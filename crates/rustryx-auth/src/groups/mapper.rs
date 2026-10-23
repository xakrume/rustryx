//! Mapping of external groups (Keycloak paths, LDAP DNs) onto Rustryx roles.
//!
//! One mapper serves both providers; only the policy differs. Order of evaluation:
//!
//! 1. `admin_groups` of the provider, exact match: the ONLY way to obtain `nx-admin`;
//! 2. rows of `idp_group_mappings` (`provider`, `external_group` -> role);
//! 3. optional "group name = role name" auto-mapping, off by default and never able
//!    to yield `nx-admin` (or `nx-anonymous`);
//! 4. when nothing matched, the provider's `default_role`, otherwise the user is denied.
//!
//! Matching is exact and case sensitive on the whole value. Nothing is trimmed (the
//! leading slash of a Keycloak path is part of the identity), nothing is matched as
//! a substring, nothing is case folded. `/rustryx/admins2`, `/Rustryx/Admins` and
//! `rustryx/admins` are three different groups from `/rustryx/admins`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moka::future::Cache;
use rustryx_core::StoreError;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{IdpProvider, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, RoleRef};
use rustryx_core::ports::{IdpMappingRepo, RoleRepo};

use crate::passwords::errmap::backend_failure;

/// Most external groups one identity may present. A bigger set is refused rather
/// than truncated: truncation would make the outcome depend on claim order.
pub const MAX_EXTERNAL_GROUPS: usize = 1024;
/// Default time a loaded mapping table is reused before it is read again.
pub const DEFAULT_SNAPSHOT_TTL: Duration = Duration::from_secs(30);

/// Per provider settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderPolicy {
    /// Groups whose members get `nx-admin`. Exact full path (OIDC) or DN (LDAP).
    pub admin_groups: Vec<String>,
    /// Role name granted when no group matched. `None` means "deny".
    pub default_role: Option<String>,
    /// Treat a group whose last path segment (OIDC) or first RDN value (LDAP) equals a
    /// role name as a grant of that role. Off by default; never yields `nx-admin`.
    pub auto_map_group_name: bool,
}

/// Settings of the [`RoleMapper`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleMapperConfig {
    /// Keycloak side.
    pub oidc: ProviderPolicy,
    /// LDAP side.
    pub ldap: ProviderPolicy,
    /// Add roles of origin `manual` to externally derived roles. Off by default: the
    /// identity provider is then the only source of an external user's rights.
    pub allow_local_role_supplement: bool,
    /// How long a loaded mapping table is reused.
    pub snapshot_ttl: Duration,
}

impl Default for RoleMapperConfig {
    fn default() -> Self {
        Self {
            oidc: ProviderPolicy::default(),
            ldap: ProviderPolicy::default(),
            allow_local_role_supplement: false,
            snapshot_ttl: DEFAULT_SNAPSHOT_TTL,
        }
    }
}

/// Roles derived from external groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRoles {
    /// Roles without duplicates, `nx-admin` first when present.
    pub roles: Vec<RoleRef>,
    /// The groups that produced `roles`. Empty when the default role applied.
    pub matched_groups: Vec<String>,
}

/// Result of mapping a group set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingOutcome {
    /// At least one role: from groups, or the default role.
    Granted(ExternalRoles),
    /// No group matched and no default role is configured.
    Denied,
}

/// The mapping table and the role catalogue, read together.
struct Snapshot {
    /// `(external_group, role)` by ascending priority. Rows that point at `nx-admin`
    /// or at a role that no longer exists are already removed.
    mappings: Vec<(String, RoleRef)>,
    roles_by_name: HashMap<String, RoleRef>,
}

/// Shared group to role mapping for OIDC and LDAP.
pub struct RoleMapper {
    mappings: Arc<dyn IdpMappingRepo>,
    roles: Arc<dyn RoleRepo>,
    config: RoleMapperConfig,
    snapshots: Cache<IdpProvider, Arc<Snapshot>>,
    unknown_groups: AtomicU64,
}

impl std::fmt::Debug for RoleMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleMapper")
            .field(
                "allow_local_role_supplement",
                &self.config.allow_local_role_supplement,
            )
            .finish_non_exhaustive()
    }
}

impl RoleMapper {
    /// Builds a mapper over the shared repositories.
    #[must_use]
    pub fn new(
        mappings: Arc<dyn IdpMappingRepo>,
        roles: Arc<dyn RoleRepo>,
        config: RoleMapperConfig,
    ) -> Self {
        let snapshots = Cache::builder()
            .max_capacity(4)
            .time_to_live(config.snapshot_ttl)
            .build();
        Self {
            mappings,
            roles,
            config,
            snapshots,
            unknown_groups: AtomicU64::new(0),
        }
    }

    /// Whether `manual` roles are added to external ones.
    #[must_use]
    pub const fn allows_local_role_supplement(&self) -> bool {
        self.config.allow_local_role_supplement
    }

    /// Groups seen so far that matched nothing (also exported as a metric).
    #[must_use]
    pub fn unknown_group_count(&self) -> u64 {
        self.unknown_groups.load(Ordering::Relaxed)
    }

    /// Drops the cached mapping tables. Call on `InvalidationEvent::Rbac` and `Reset`.
    pub fn invalidate(&self) {
        self.snapshots.invalidate_all();
    }

    const fn policy(&self, provider: IdpProvider) -> &ProviderPolicy {
        match provider {
            IdpProvider::Oidc => &self.config.oidc,
            IdpProvider::Ldap => &self.config.ldap,
        }
    }

    async fn snapshot(&self, provider: IdpProvider) -> Result<Arc<Snapshot>, AuthError> {
        self.snapshots
            .try_get_with(provider, self.load(provider))
            .await
            .map_err(|shared| match &*shared {
                StoreError::Unavailable(_) => {
                    tracing::warn!(error = %shared, "role mapping tables unavailable");
                    AuthError::ProviderUnavailable("role mapping tables unavailable".into())
                }
                _ => AuthError::Backend {
                    context: "role mapping load",
                    source: Box::new(shared),
                },
            })
    }

    async fn load(&self, provider: IdpProvider) -> Result<Arc<Snapshot>, StoreError> {
        let (mut rows, roles) =
            futures::future::try_join(self.mappings.list(provider), self.roles.list()).await?;
        rows.sort_by(|a, b| (a.priority, &a.external_group).cmp(&(b.priority, &b.external_group)));
        let roles_by_id: HashMap<_, _> = roles
            .iter()
            .map(|role| {
                (
                    role.id,
                    RoleRef {
                        id: role.id,
                        name: role.name.clone(),
                    },
                )
            })
            .collect();
        let roles_by_name = roles_by_id
            .values()
            .map(|role| (role.name.clone(), role.clone()))
            .collect();
        let mut mappings = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(role) = roles_by_id.get(&row.role_id) else {
                tracing::warn!(group = ?row.external_group, "mapping points at a role that does not exist");
                continue;
            };
            if role.name == ROLE_NX_ADMIN {
                // `admin_groups` is the only door to nx-admin. A table row must not be
                // a second one: whoever can write the table (an admin API, a seed file)
                // could otherwise grant administration without touching the audited setting.
                tracing::error!(group = ?row.external_group, "ignoring mapping to nx-admin: use admin_groups");
                continue;
            }
            mappings.push((row.external_group, role.clone()));
        }
        Ok(Arc::new(Snapshot {
            mappings,
            roles_by_name,
        }))
    }

    /// Maps an identity's external groups onto roles.
    pub async fn map_groups(
        &self,
        provider: IdpProvider,
        groups: &[String],
    ) -> Result<MappingOutcome, AuthError> {
        if groups.len() > MAX_EXTERNAL_GROUPS {
            return Err(AuthError::GroupLookupFailed("too many groups".into()));
        }
        let snapshot = self.snapshot(provider).await?;
        let policy = self.policy(provider);
        let present: HashSet<&str> = groups.iter().map(String::as_str).collect();
        let mut roles: Vec<RoleRef> = Vec::new();
        let mut matched: HashSet<&str> = HashSet::new();

        for admin_group in &policy.admin_groups {
            // An empty configured value must never match an empty group name.
            if !admin_group.is_empty() && present.contains(admin_group.as_str()) {
                let Some(admin) = snapshot.roles_by_name.get(ROLE_NX_ADMIN) else {
                    return Err(backend_failure(
                        "role mapping",
                        "built-in role nx-admin is missing",
                    ));
                };
                roles.push(admin.clone());
                matched.insert(admin_group.as_str());
            }
        }
        for (external_group, role) in &snapshot.mappings {
            if present.contains(external_group.as_str()) {
                roles.push(role.clone());
                matched.insert(external_group.as_str());
            }
        }
        if policy.auto_map_group_name {
            for group in groups {
                if matched.contains(group.as_str()) {
                    continue;
                }
                let Some(name) = group_name(provider, group) else {
                    continue;
                };
                // Neither special role may be reached by naming a group after it.
                if name == ROLE_NX_ADMIN || name == ROLE_NX_ANONYMOUS {
                    continue;
                }
                if let Some(role) = snapshot.roles_by_name.get(name) {
                    roles.push(role.clone());
                    matched.insert(group.as_str());
                }
            }
        }
        self.record_unknown(provider, groups, &matched);

        let mut seen = HashSet::new();
        roles.retain(|role| seen.insert(role.id));
        if !roles.is_empty() {
            let matched_groups = groups
                .iter()
                .filter(|group| matched.contains(group.as_str()))
                .cloned()
                .collect();
            return Ok(MappingOutcome::Granted(ExternalRoles {
                roles,
                matched_groups,
            }));
        }
        Ok(Self::fallback(policy, &snapshot))
    }

    fn fallback(policy: &ProviderPolicy, snapshot: &Snapshot) -> MappingOutcome {
        let Some(name) = policy.default_role.as_deref() else {
            return MappingOutcome::Denied;
        };
        // The fallback must never be a privileged or anonymous role: it is handed to
        // every authenticated identity that matched nothing.
        if name == ROLE_NX_ADMIN || name == ROLE_NX_ANONYMOUS {
            tracing::error!(
                role = name,
                "default_role must not be nx-admin or nx-anonymous; denying"
            );
            return MappingOutcome::Denied;
        }
        if let Some(role) = snapshot.roles_by_name.get(name) {
            return MappingOutcome::Granted(ExternalRoles {
                roles: vec![role.clone()],
                matched_groups: Vec::new(),
            });
        }
        tracing::error!(role = name, "default_role does not exist; denying");
        MappingOutcome::Denied
    }

    fn record_unknown(&self, provider: IdpProvider, groups: &[String], matched: &HashSet<&str>) {
        let label = match provider {
            IdpProvider::Oidc => "oidc",
            IdpProvider::Ldap => "ldap",
        };
        for group in groups.iter().filter(|g| !matched.contains(g.as_str())) {
            self.unknown_groups.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("rustryx_auth_unknown_groups_total", "provider" => label)
                .increment(1);
            tracing::debug!(provider = label, group = ?group, "ignoring group without a mapping");
        }
    }

    /// Adds `manual` roles to `external` ones when the supplement switch is on.
    ///
    /// `nx-admin` is never taken from `manual` rows for an external user: an external
    /// identity becomes an administrator through `admin_groups` and nothing else.
    #[must_use]
    pub fn compose(&self, external: &[RoleRef], manual: &[RoleRef]) -> Vec<RoleRef> {
        let mut roles = external.to_vec();
        if self.config.allow_local_role_supplement {
            roles.extend(manual.iter().filter(|r| r.name != ROLE_NX_ADMIN).cloned());
        }
        let mut seen = HashSet::new();
        roles.retain(|role| seen.insert(role.id));
        roles
    }
}

/// The part of a group id that "group name = role name" compares with a role name.
fn group_name(provider: IdpProvider, group: &str) -> Option<&str> {
    let name = match provider {
        IdpProvider::Oidc => group.rsplit('/').find(|segment| !segment.is_empty())?,
        IdpProvider::Ldap => first_rdn_value(group)?,
    };
    (!name.is_empty()).then_some(name)
}

/// Value of the first RDN of a DN (`cn=Foo,ou=x` gives `Foo`). `None` for
/// multi-valued or escaped RDNs: guessing at their meaning would be worse than skipping.
fn first_rdn_value(dn: &str) -> Option<&str> {
    let first = dn.split(',').next()?;
    let (_, value) = first.split_once('=')?;
    if value.contains(['\\', '+']) || dn.contains('\\') {
        return None;
    }
    Some(value.trim())
}

#[cfg(test)]
mod tests;
