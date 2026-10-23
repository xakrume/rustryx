//! The second half of an authentication: from an identified principal to its roles.
//!
//! Where the groups of each kind of caller come from:
//!
//! | caller | account | groups | roles |
//! |---|---|---|---|
//! | local (Basic, token, JWT) | stored | none | rows of origin `manual` |
//! | OIDC bearer | JIT by `(iss, sub)` | the token's `groups` claim, via the OIDC resolver | mapped, current request only |
//! | OIDC session (internal JWT) | stored | the snapshot inside the JWT, at most 5 minutes old | mapped |
//! | OIDC owner of an API token | stored | none can be confirmed | `manual` rows only if supplement is on |
//! | LDAP (Basic, token, JWT) | JIT by username | the LDAP resolver, per request | mapped |
//!
//! Roles are computed from what the current request proves. Persisted `user_roles`
//! rows are written for the UI and audit and are never read back as a grant.

use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthMethod, IdpProvider, Principal, UserSource};
use rustryx_core::ids::UserId;
use time::OffsetDateTime;

use super::AuthService;
use crate::groups::MappingOutcome;
use crate::passwords::errmap::backend_failure;

/// A group set the directory confirmed, and when.
#[derive(Debug, Clone)]
pub(super) struct ConfirmedGroups {
    groups: Vec<String>,
    at: OffsetDateTime,
}

/// What a request proved about group membership.
struct Membership {
    groups: Vec<String>,
    confirmed_at: OffsetDateTime,
}

impl AuthService {
    /// Provisions the account when needed and fills in `roles` and `external_groups`.
    pub(super) async fn finish(&self, mut principal: Principal) -> Result<Principal, AuthError> {
        if principal.user_id.is_none() {
            let user = self.jit.provision(&principal).await?;
            principal.user_id = Some(user.id);
            // The stored name is canonical: the provider's spelling may change.
            principal.username = user.username;
        }
        let Some(user_id) = principal.user_id else {
            return Err(backend_failure(
                "role resolution",
                "principal without an account",
            ));
        };
        match principal.source {
            UserSource::Local => {
                principal.roles = self.jit.manual_roles(user_id).await?;
                Ok(principal)
            }
            UserSource::Oidc => {
                self.external_roles(principal, user_id, IdpProvider::Oidc)
                    .await
            }
            UserSource::Ldap => {
                self.external_roles(principal, user_id, IdpProvider::Ldap)
                    .await
            }
        }
    }

    async fn external_roles(
        &self,
        mut principal: Principal,
        user_id: UserId,
        provider: IdpProvider,
    ) -> Result<Principal, AuthError> {
        let membership = match provider {
            IdpProvider::Oidc => self.oidc_membership(&principal).await?,
            IdpProvider::Ldap => Some(self.ldap_membership(&principal, user_id).await?),
        };
        let manual = if self.mapper.allows_local_role_supplement() {
            self.jit.manual_roles(user_id).await?
        } else {
            Vec::new()
        };
        let Some(membership) = membership else {
            // Nothing confirms group membership (an API token of an OIDC user). The groups
            // grant nothing, and the persisted rows are left alone: this request proved
            // nothing about them.
            principal.roles = self.mapper.compose(&[], &manual);
            principal.external_groups = Vec::new();
            principal.groups_confirmed_at = None;
            return Ok(principal);
        };
        let outcome = self.mapper.map_groups(provider, &membership.groups).await?;
        match outcome {
            MappingOutcome::Granted(external) => {
                self.jit
                    .persist_external_roles(user_id, provider, &external.roles)
                    .await;
                principal.roles = self.mapper.compose(&external.roles, &manual);
                principal.external_groups = external.matched_groups;
                principal.groups_confirmed_at = Some(membership.confirmed_at);
                Ok(principal)
            }
            MappingOutcome::Denied => {
                // The identity is real but maps to nothing and there is no default role:
                // access is denied (never `nx-anonymous`). The persisted rows are cleared
                // so the UI does not keep showing roles the user no longer has.
                self.jit
                    .persist_external_roles(user_id, provider, &[])
                    .await;
                Err(AuthError::GroupLookupFailed(
                    "access denied: no mapped group and no default role".into(),
                ))
            }
        }
    }

    /// OIDC: the groups the request itself carries. `None` when it carries none that
    /// can be confirmed.
    async fn oidc_membership(
        &self,
        principal: &Principal,
    ) -> Result<Option<Membership>, AuthError> {
        let now = self.clock.now();
        match principal.auth_method {
            AuthMethod::OidcBearer => {
                let Some(idp) = &self.oidc else {
                    return Err(AuthError::GroupLookupFailed(
                        "OIDC is not configured".into(),
                    ));
                };
                let external = idp.resolver.external_groups(principal).await?;
                if external.provider != IdpProvider::Oidc {
                    return Err(backend_failure(
                        "group resolution",
                        "resolver answered for another provider",
                    ));
                }
                Ok(Some(Membership {
                    groups: external.groups,
                    confirmed_at: now,
                }))
            }
            AuthMethod::InternalJwt => {
                // The session token is the only record of what the login was confirmed
                // with. Without a timestamp the membership cannot be aged, and an unaged
                // set must not grant anything.
                let Some(confirmed_at) = principal.groups_confirmed_at else {
                    return Err(AuthError::GroupLookupFailed(
                        "session carries no group confirmation".into(),
                    ));
                };
                self.ensure_fresh(confirmed_at, now)?;
                Ok(Some(Membership {
                    groups: principal.external_groups.clone(),
                    confirmed_at,
                }))
            }
            _ => Ok(None),
        }
    }

    fn ensure_fresh(
        &self,
        confirmed_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), AuthError> {
        let max = time::Duration::try_from(self.groups_max_age).unwrap_or(time::Duration::ZERO);
        if now - confirmed_at > max {
            return Err(AuthError::GroupLookupFailed(
                "group confirmation is older than the allowed maximum".into(),
            ));
        }
        Ok(())
    }

    /// LDAP: a fresh search per request. When the search fails, the last successful
    /// result is reused only while it is younger than the maximum age; past that the
    /// request is refused. A failed lookup is never an empty group list.
    async fn ldap_membership(
        &self,
        principal: &Principal,
        user_id: UserId,
    ) -> Result<Membership, AuthError> {
        let Some(idp) = &self.ldap else {
            return Err(AuthError::GroupLookupFailed(
                "LDAP is not configured".into(),
            ));
        };
        let now = self.clock.now();
        match idp.resolver.external_groups(principal).await {
            Ok(external) => {
                if external.provider != IdpProvider::Ldap {
                    return Err(backend_failure(
                        "group resolution",
                        "resolver answered for another provider",
                    ));
                }
                self.ldap_confirmed
                    .insert(
                        user_id,
                        ConfirmedGroups {
                            groups: external.groups.clone(),
                            at: now,
                        },
                    )
                    .await;
                Ok(Membership {
                    groups: external.groups,
                    confirmed_at: now,
                })
            }
            Err(AuthError::Overloaded) => Err(AuthError::Overloaded),
            Err(error) => {
                let earlier = self.ldap_confirmed.get(&user_id).await;
                match earlier {
                    Some(previous) if self.ensure_fresh(previous.at, now).is_ok() => {
                        tracing::warn!(
                            user_id = %user_id,
                            %error,
                            "LDAP group search failed, reusing the previous result within its maximum age"
                        );
                        Ok(Membership {
                            groups: previous.groups,
                            confirmed_at: previous.at,
                        })
                    }
                    _ => {
                        tracing::warn!(user_id = %user_id, %error, "LDAP group search failed and no recent result exists");
                        Err(AuthError::GroupLookupFailed(
                            "group lookup failed and no recent confirmation exists".into(),
                        ))
                    }
                }
            }
        }
    }
}
