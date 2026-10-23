//! Just-in-time provisioning of external accounts and persistence of their roles.
//!
//! Identity rules (SEC-23), all enforced here:
//!
//! - an OIDC account is found ONLY by `(issuer, subject)`, which live in separate
//!   columns. Neither the username nor the e-mail address ever selects an existing
//!   account, so an identity provider cannot take over a local administrator by
//!   presenting a token with the same e-mail or name;
//! - a username already used by another identity is a conflict
//!   ([`AuthError::UsernameConflict`], HTTP 409). Accounts are never merged
//!   automatically; only an explicit administrator action can link them;
//! - LDAP accounts are found by username with source `ldap`: the directory has no
//!   stable subject to key on, and usernames are unique across all sources, so a
//!   match with another source is again a conflict.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use rustryx_core::StoreError;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    IdpProvider, NewUser, Principal, RoleOrigin, RoleRef, User, UserSource,
};
use rustryx_core::ids::UserId;
use rustryx_core::ports::{Clock, UserRepo};
use time::OffsetDateTime;

use crate::passwords::errmap::{backend_failure, store_failure};

/// Minimum spacing of `user_roles` rewrites for one user on one node.
pub const DEFAULT_PERSIST_INTERVAL: Duration = Duration::from_mins(5);
/// Longest username accepted for a new external account.
const MAX_USERNAME_CHARS: usize = 256;
/// Upper bound of users tracked for write coalescing.
const PERSIST_CACHE_CAPACITY: u64 = 100_000;

/// When an unknown external identity may get an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitMode {
    /// Create the account on first login.
    Jit,
    /// Create it only for usernames on the allowlist.
    Allowlist,
    /// Never create: an administrator provisions accounts in advance.
    Manual,
}

/// Provisioning settings.
#[derive(Debug, Clone)]
pub struct JitConfig {
    /// Provisioning mode.
    pub mode: JitMode,
    /// Usernames allowed in [`JitMode::Allowlist`], matched exactly and case sensitively.
    pub allowlist: HashSet<String>,
    /// Minimum spacing of role row rewrites per user.
    pub persist_interval: Duration,
}

impl Default for JitConfig {
    /// `jit` with no allowlist and the 5 minute persistence interval.
    fn default() -> Self {
        Self {
            mode: JitMode::Jit,
            allowlist: HashSet::new(),
            persist_interval: DEFAULT_PERSIST_INTERVAL,
        }
    }
}

/// Finds or creates external accounts and keeps their role rows current.
pub struct JitProvisioner {
    users: Arc<dyn UserRepo>,
    clock: Arc<dyn Clock>,
    config: JitConfig,
    /// When this node last rewrote a user's rows. Eviction by age only frees memory:
    /// an entry older than the interval never suppresses a write.
    persisted: Cache<UserId, OffsetDateTime>,
}

impl std::fmt::Debug for JitProvisioner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitProvisioner")
            .field("mode", &self.config.mode)
            .finish_non_exhaustive()
    }
}

impl JitProvisioner {
    /// Builds a provisioner.
    #[must_use]
    pub fn new(users: Arc<dyn UserRepo>, clock: Arc<dyn Clock>, config: JitConfig) -> Self {
        let persisted = Cache::builder()
            .max_capacity(PERSIST_CACHE_CAPACITY)
            .time_to_live(
                config
                    .persist_interval
                    .saturating_mul(2)
                    .max(Duration::from_secs(1)),
            )
            .build();
        Self {
            users,
            clock,
            config,
            persisted,
        }
    }

    /// Returns the account of an externally authenticated principal, creating it when
    /// the mode allows. Fails with `AccountDisabled` for a disabled account or an
    /// identity that may not be provisioned, and `UsernameConflict` on a name clash.
    pub async fn provision(&self, principal: &Principal) -> Result<User, AuthError> {
        match principal.source {
            UserSource::Oidc => self.provision_oidc(principal).await,
            UserSource::Ldap => self.provision_ldap(principal).await,
            UserSource::Local => Err(backend_failure(
                "jit provisioning",
                "local accounts are never provisioned just in time",
            )),
        }
    }

    async fn provision_oidc(&self, principal: &Principal) -> Result<User, AuthError> {
        let (Some(issuer), Some(subject)) = (
            non_empty(principal.issuer.as_deref()),
            non_empty(principal.subject.as_deref()),
        ) else {
            return Err(AuthError::InvalidToken("issuer or subject missing"));
        };
        let username = checked_username(principal)?;
        let found = self
            .users
            .find_external(UserSource::Oidc, Some(issuer), Some(subject))
            .await
            .map_err(|error| store_failure("jit find external", error))?;
        if let Some(user) = found {
            return ensure_enabled(user);
        }
        self.admit(username)?;
        // The identity is new. If the username is taken it is taken by someone else,
        // unless a concurrent first login of this very identity just created it.
        let clash = self
            .users
            .find_by_username(username)
            .await
            .map_err(|error| store_failure("jit find by username", error))?;
        if clash.is_some() {
            return self.same_identity_now(issuer, subject).await;
        }
        let new = NewUser {
            username: username.to_owned(),
            source: UserSource::Oidc,
            issuer: Some(issuer.to_owned()),
            subject: Some(subject.to_owned()),
            email: None,
            display_name: None,
            password_hash: None,
        };
        match self.users.create(&new).await {
            Ok(user) => Ok(user),
            // Another request created it between our lookup and our insert.
            Err(StoreError::Conflict(_)) => self.same_identity_now(issuer, subject).await,
            Err(error) => Err(store_failure("jit create", error)),
        }
    }

    /// The account of `(issuer, subject)` if it exists now, otherwise a username clash.
    /// Never falls back to a lookup by username: that would be account linking.
    async fn same_identity_now(&self, issuer: &str, subject: &str) -> Result<User, AuthError> {
        let found = self
            .users
            .find_external(UserSource::Oidc, Some(issuer), Some(subject))
            .await
            .map_err(|error| store_failure("jit find external", error))?;
        found.map_or(Err(AuthError::UsernameConflict), ensure_enabled)
    }

    async fn provision_ldap(&self, principal: &Principal) -> Result<User, AuthError> {
        let username = checked_username(principal)?;
        let found = self
            .users
            .find_by_username(username)
            .await
            .map_err(|error| store_failure("jit find by username", error))?;
        if let Some(user) = found {
            return Self::ldap_account(user);
        }
        self.admit(username)?;
        let new = NewUser {
            username: username.to_owned(),
            source: UserSource::Ldap,
            issuer: None,
            subject: None,
            email: None,
            display_name: None,
            password_hash: None,
        };
        match self.users.create(&new).await {
            Ok(user) => Ok(user),
            Err(StoreError::Conflict(_)) => {
                let again = self
                    .users
                    .find_by_username(username)
                    .await
                    .map_err(|error| store_failure("jit find by username", error))?;
                again.map_or(Err(AuthError::UsernameConflict), Self::ldap_account)
            }
            Err(error) => Err(store_failure("jit create", error)),
        }
    }

    /// An existing account that carries an LDAP login must itself be an LDAP account.
    fn ldap_account(user: User) -> Result<User, AuthError> {
        if user.source == UserSource::Ldap {
            ensure_enabled(user)
        } else {
            Err(AuthError::UsernameConflict)
        }
    }

    fn admit(&self, username: &str) -> Result<(), AuthError> {
        let allowed = match self.config.mode {
            JitMode::Jit => true,
            JitMode::Allowlist => self.config.allowlist.contains(username),
            JitMode::Manual => false,
        };
        if allowed {
            return Ok(());
        }
        tracing::info!(
            username = ?username,
            mode = ?self.config.mode,
            "external login refused: the account is not provisioned"
        );
        Err(AuthError::AccountDisabled)
    }

    /// Roles of origin `manual` assigned to the user.
    pub async fn manual_roles(&self, user: UserId) -> Result<Vec<RoleRef>, AuthError> {
        let all = self
            .users
            .roles_of(user)
            .await
            .map_err(|error| store_failure("jit roles of", error))?;
        Ok(all
            .into_iter()
            .filter(|(_, origin)| *origin == RoleOrigin::Manual)
            .map(|(role, _)| role)
            .collect())
    }

    /// Rewrites the rows of the provider's origin, at most once per interval per user.
    ///
    /// The rows exist for the UI and for audit. A request never reads them back: its
    /// roles come from the groups it presented, so a failed or skipped write changes
    /// nothing about access, and a failure here is only logged.
    pub async fn persist_external_roles(
        &self,
        user: UserId,
        provider: IdpProvider,
        roles: &[RoleRef],
    ) {
        let now = self.clock.now();
        let interval =
            time::Duration::try_from(self.config.persist_interval).unwrap_or(time::Duration::MAX);
        // Claiming the slot before the write folds concurrent requests into one write.
        let entry = self.persisted.entry(user).or_insert(now).await;
        let fresh = entry.is_fresh();
        if !fresh {
            if now - *entry.value() < interval {
                return;
            }
            self.persisted.insert(user, now).await;
        }
        let ids: Vec<_> = roles.iter().map(|role| role.id).collect();
        if let Err(error) = self
            .users
            .replace_roles_for_origin(user, provider.origin(), &ids)
            .await
        {
            tracing::warn!(user_id = %user, %error, "could not persist external roles");
            // Let the next request retry instead of waiting out the interval.
            self.persisted.invalidate(&user).await;
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

fn checked_username(principal: &Principal) -> Result<&str, AuthError> {
    let name = principal.username.as_str();
    let valid = !name.trim().is_empty()
        && name.chars().count() <= MAX_USERNAME_CHARS
        && !name.chars().any(char::is_control);
    if valid {
        Ok(name)
    } else {
        Err(AuthError::InvalidToken("username missing or malformed"))
    }
}

fn ensure_enabled(user: User) -> Result<User, AuthError> {
    if user.enabled {
        Ok(user)
    } else {
        Err(AuthError::AccountDisabled)
    }
}

#[cfg(test)]
mod tests;
