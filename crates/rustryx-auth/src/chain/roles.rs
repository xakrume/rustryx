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
//! | robot or scan token | none | none | none: rights are the token scopes alone |
//!
//! Roles are computed from what the current request proves. Persisted `user_roles`
//! rows are written for the UI and audit and are never read back as a grant.

use std::time::Duration;

use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthMethod, IdpProvider, Principal, UserSource};
use rustryx_core::ids::UserId;
use rustryx_core::ports::ExternalGroups;
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

/// How far ahead of this node's clock a confirmation time may lie. A confirmation from the
/// future (another node's clock runs ahead, or this one was stepped back) cannot be aged, and
/// an unaged group set must not grant anything; the allowance only absorbs ordinary skew.
const GROUPS_CLOCK_SKEW: Duration = Duration::from_secs(5);

/// What a request proved about group membership.
struct Membership {
    groups: Vec<String>,
    confirmed_at: OffsetDateTime,
}

impl AuthService {
    /// Provisions the account when needed and fills in `roles` and `external_groups`.
    pub(super) async fn finish(&self, mut principal: Principal) -> Result<Principal, AuthError> {
        match principal.auth_method {
            AuthMethod::BasicLocal
            | AuthMethod::BasicLdap
            | AuthMethod::ApiToken
            | AuthMethod::InternalJwt
            | AuthMethod::OidcBearer => {}
            // Robot and scan tokens have no account and no roles: their rights are their scopes
            // and nothing else (decision order rule 4 of rbac), so there is nothing to
            // provision and nothing to map.
            AuthMethod::RobotToken | AuthMethod::ScanToken => {
                return Self::finish_scoped(principal);
            }
            // `Anonymous` never reaches here (the service answers it itself), and a method
            // this build does not know cannot be given roles safely. Both are refused rather
            // than treated as "an account without roles", which a caller could read as a
            // valid, merely unprivileged identity.
            _ => {
                return Err(backend_failure(
                    "role resolution",
                    "authentication method cannot be resolved to roles",
                ));
            }
        }
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

    /// A robot or scan principal: no roles, no groups, and the scopes it was authenticated
    /// with are mandatory. A token principal without `token_scopes` would read as an
    /// unrestricted personal token to the authorizer, so it is refused here, where the broken
    /// authenticator is still identifiable.
    fn finish_scoped(mut principal: Principal) -> Result<Principal, AuthError> {
        if principal.token_scopes.is_none() {
            return Err(backend_failure(
                "role resolution",
                "robot or scan principal without token scopes",
            ));
        }
        principal.roles = Vec::new();
        principal.external_groups = Vec::new();
        principal.groups_confirmed_at = None;
        Ok(principal)
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
                Err(AuthError::AccessDenied(
                    "no mapped group and no default role",
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
                Self::ensure_provider(&external, IdpProvider::Oidc)?;
                // The resolver says when the groups were confirmed (the verification of this
                // request's token); the same age rule applies to every source.
                self.ensure_fresh(external.confirmed_at, now)?;
                Ok(Some(Membership {
                    groups: external.groups,
                    confirmed_at: external.confirmed_at,
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

    fn ensure_provider(external: &ExternalGroups, expected: IdpProvider) -> Result<(), AuthError> {
        if external.provider == expected {
            return Ok(());
        }
        Err(backend_failure(
            "group resolution",
            "resolver answered for another provider",
        ))
    }

    /// A confirmation counts while it is at most `groups_max_age` old at `now` and does not
    /// lie in the future beyond the clock skew allowance.
    fn ensure_fresh(
        &self,
        confirmed_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), AuthError> {
        let max = time::Duration::try_from(self.groups_max_age).unwrap_or(time::Duration::ZERO);
        let skew = time::Duration::try_from(GROUPS_CLOCK_SKEW).unwrap_or(time::Duration::ZERO);
        if confirmed_at > now.saturating_add(skew) {
            return Err(AuthError::GroupLookupFailed(
                "group confirmation lies in the future".into(),
            ));
        }
        if now - confirmed_at > max {
            return Err(AuthError::GroupLookupFailed(
                "group confirmation is older than the allowed maximum".into(),
            ));
        }
        Ok(())
    }

    /// LDAP: the resolver per request. Its answer carries the time the DIRECTORY confirmed
    /// it, and every age decision here is made from that time, never from the time the answer
    /// was received: a set served from the resolver's 60 second cache is already up to 60
    /// seconds old when it arrives, and stamping "now" on it would let the two graces add up.
    ///
    /// When the lookup fails, the last successful result is reused only while it is at most
    /// the maximum age old, counted from ITS confirmation; past that the request is refused.
    /// The total staleness of a group set is therefore at most five minutes from the
    /// directory's answer, whichever layer serves it (README 8.5). A failed lookup is never an
    /// empty group list.
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
                Self::ensure_provider(&external, IdpProvider::Ldap)?;
                match self.ensure_fresh(external.confirmed_at, now) {
                    Ok(()) => {
                        self.remember_ldap(user_id, &external).await;
                        Ok(Membership {
                            groups: external.groups,
                            confirmed_at: external.confirmed_at,
                        })
                    }
                    // An answer older than the window (a resolver that serves stale sets for
                    // too long) or from the future is as good as no answer.
                    Err(error) => self.reuse_ldap(user_id, now, &error).await,
                }
            }
            Err(AuthError::Overloaded) => Err(AuthError::Overloaded),
            Err(error) => self.reuse_ldap(user_id, now, &error).await,
        }
    }

    /// Keeps the newest confirmed set of a user. An answer that is not newer than the one
    /// held (the resolver served its cache again) does not replace it, so the stored
    /// confirmation time never moves backwards and never forwards without the directory.
    async fn remember_ldap(&self, user_id: UserId, external: &ExternalGroups) {
        let newer = self
            .ldap_confirmed
            .get(&user_id)
            .await
            .is_none_or(|held| held.at < external.confirmed_at);
        if newer {
            self.ldap_confirmed
                .insert(
                    user_id,
                    ConfirmedGroups {
                        groups: external.groups.clone(),
                        at: external.confirmed_at,
                    },
                )
                .await;
        }
    }

    /// The directory could not answer: serve the last confirmed set if it is still inside
    /// the window, otherwise fail closed.
    async fn reuse_ldap(
        &self,
        user_id: UserId,
        now: OffsetDateTime,
        cause: &AuthError,
    ) -> Result<Membership, AuthError> {
        match self.ldap_confirmed.get(&user_id).await {
            Some(previous) if self.ensure_fresh(previous.at, now).is_ok() => {
                tracing::warn!(
                    user_id = %user_id,
                    error = %cause,
                    confirmed_at = %previous.at,
                    "LDAP group search failed, reusing the previous result within its maximum age"
                );
                Ok(Membership {
                    groups: previous.groups,
                    confirmed_at: previous.at,
                })
            }
            _ => {
                tracing::warn!(user_id = %user_id, error = %cause, "LDAP group search failed and no recent result exists");
                Err(AuthError::GroupLookupFailed(
                    "group lookup failed and no recent confirmation exists".into(),
                ))
            }
        }
    }
}
