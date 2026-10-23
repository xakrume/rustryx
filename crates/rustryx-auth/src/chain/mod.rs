//! The authentication chain: one [`Authenticator`] for the HTTP layer.
//!
//! Order, fixed: local Basic, API token, internal JWT, OIDC bearer, LDAP. Each
//! member returns `Ok(None)` for credentials that are not its kind, so the first
//! member that claims them decides, and an `Err` from a member that claimed them
//! is final (a wrong password is never retried against LDAP).
//!
//! Around that sequence this service adds what no single member can:
//!
//! - the login guard around password Basic credentials;
//! - the dummy argon2id when nobody claims Basic credentials, so an unknown account
//!   costs what a known one with a wrong password costs;
//! - just-in-time provisioning of external accounts;
//! - group resolution and the group to role mapping, for both providers;
//! - the principal cache for JWT bearers and the 60 second cache of verified Basic passwords
//!   of local accounts (`basic_cache`);
//! - the request deadline of `AuthContext`, passed to every blocking hash;
//! - the metrics of an authentication: `auth_attempts_total` with the method that refused,
//!   and `auth_breakglass_logins_total` (see [`crate::telemetry`]).
//!
//! The service never returns `Ok(None)`: anonymous credentials yield
//! `Principal::anonymous()`, anything else yields a principal or an error. The HTTP
//! layer therefore needs no fallback of its own.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError` variants documented on the type

mod basic_cache;
mod cache;
mod roles;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests_attempts;
#[cfg(test)]
mod tests_basic;
#[cfg(test)]
mod tests_basic_cache;
#[cfg(test)]
mod tests_bearer;
#[cfg(test)]
mod tests_roles;
#[cfg(test)]
mod tests_scoped;
#[cfg(test)]
mod tests_window;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use moka::future::Cache;
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    API_TOKEN_PREFIX, AuthContext, AuthFailure, AuthMethod, Credentials, Principal, UserSource,
};
use rustryx_core::ids::UserId;
use rustryx_core::ports::{
    Authenticator, Clock, GroupResolver, InvalidationBus, InvalidationEvent, UserRepo,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

pub use self::basic_cache::MAX_BASIC_CACHE_TTL;
use self::basic_cache::{BasicCache, Snapshot};
pub use self::cache::{DEFAULT_CACHE_CAPACITY, MAX_CACHE_TTL};
use self::cache::{PrincipalCache, token_key};
use self::roles::ConfirmedGroups;
use crate::groups::{JitProvisioner, RoleMapper};
use crate::login_guard::{LoginGuard, fold_account};
use crate::passwords::{Passwords, is_usable_password};
use crate::telemetry;

/// How old a confirmation of external group membership may be (README 8.5: 5 minutes).
/// A configured value can only lower it.
pub const MAX_GROUPS_AGE: Duration = Duration::from_mins(5);

/// Settings of the [`AuthService`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthServiceConfig {
    /// Oldest group confirmation that still counts, clamped to [`MAX_GROUPS_AGE`].
    pub groups_max_age: Duration,
    /// Capacity of the principal cache and of the Basic cache.
    pub principal_cache_capacity: u64,
    /// How long a verified Basic password of a local account is remembered, clamped to
    /// [`MAX_BASIC_CACHE_TTL`] (60 seconds). Zero turns the cache off.
    pub basic_cache_ttl: Duration,
    /// Login of the break-glass administrator (`auth.bootstrap_admin.username`). Every
    /// verification of this local account's password raises
    /// `auth_breakglass_logins_total` and a warning in the log. `None` counts nothing.
    pub breakglass_username: Option<String>,
    /// Emit `auth_attempts_total`. The HTTP middleware counts attempts too; exactly one of the
    /// two must, or every request counts twice. Turn this off when the middleware does.
    pub record_attempt_metrics: bool,
}

impl Default for AuthServiceConfig {
    fn default() -> Self {
        Self {
            groups_max_age: MAX_GROUPS_AGE,
            principal_cache_capacity: DEFAULT_CACHE_CAPACITY,
            basic_cache_ttl: MAX_BASIC_CACHE_TTL,
            breakglass_username: None,
            record_attempt_metrics: true,
        }
    }
}

/// An identity provider as the chain sees it: how to authenticate and how to ask
/// for the groups of whoever it authenticated.
#[derive(Clone)]
pub struct ExternalIdp {
    /// OIDC: claims `Bearer` access tokens. LDAP: claims `Basic` credentials.
    pub authenticator: Arc<dyn Authenticator>,
    /// Reports the group ids of a principal of this provider.
    pub resolver: Arc<dyn GroupResolver>,
}

/// Everything the service is built from. All parts are shared (`Arc`), so the same
/// instances can be handed to the HTTP layer where it needs them (for example
/// `Passwords` for password changes and `InternalJwtService` for issuing sessions).
pub struct AuthServiceDeps {
    /// Accounts.
    pub users: Arc<dyn UserRepo>,
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// Local Basic (`LocalPasswordAuthenticator`).
    pub local_basic: Arc<dyn Authenticator>,
    /// API tokens (`TokenAuthenticator`).
    pub api_token: Arc<dyn Authenticator>,
    /// Internal JWT (`InternalJwtAuthenticator`).
    pub internal_jwt: Arc<dyn Authenticator>,
    /// Keycloak, when configured.
    pub oidc: Option<ExternalIdp>,
    /// LDAP, when configured.
    pub ldap: Option<ExternalIdp>,
    /// Failed-login accounting.
    pub guard: Arc<LoginGuard>,
    /// Password hashing, for the dummy verification.
    pub passwords: Arc<Passwords>,
    /// Group to role mapping.
    pub mapper: Arc<RoleMapper>,
    /// Account provisioning and role persistence.
    pub jit: Arc<JitProvisioner>,
}

/// The composed authenticator. See the module documentation.
pub struct AuthService {
    users: Arc<dyn UserRepo>,
    clock: Arc<dyn Clock>,
    local_basic: Arc<dyn Authenticator>,
    api_token: Arc<dyn Authenticator>,
    internal_jwt: Arc<dyn Authenticator>,
    oidc: Option<ExternalIdp>,
    ldap: Option<ExternalIdp>,
    guard: Arc<LoginGuard>,
    passwords: Arc<Passwords>,
    mapper: Arc<RoleMapper>,
    jit: Arc<JitProvisioner>,
    groups_max_age: Duration,
    principals: PrincipalCache,
    basic: BasicCache,
    /// Folded login of the break-glass administrator.
    breakglass: Option<String>,
    record_attempts: bool,
    /// Last successful LDAP group search per user: what may be reused for at most
    /// `groups_max_age`, counted from the directory's answer, when the directory fails.
    ldap_confirmed: Cache<UserId, ConfirmedGroups>,
}

impl std::fmt::Debug for AuthService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthService")
            .field("oidc", &self.oidc.is_some())
            .field("ldap", &self.ldap.is_some())
            .finish_non_exhaustive()
    }
}

impl AuthService {
    /// Assembles the service.
    #[must_use]
    pub fn new(deps: AuthServiceDeps, config: AuthServiceConfig) -> Self {
        let groups_max_age = config.groups_max_age.min(MAX_GROUPS_AGE);
        Self {
            users: deps.users,
            clock: deps.clock,
            local_basic: deps.local_basic,
            api_token: deps.api_token,
            internal_jwt: deps.internal_jwt,
            oidc: deps.oidc,
            ldap: deps.ldap,
            guard: deps.guard,
            passwords: deps.passwords,
            mapper: deps.mapper,
            jit: deps.jit,
            groups_max_age,
            principals: PrincipalCache::new(config.principal_cache_capacity),
            basic: BasicCache::new(config.basic_cache_ttl, config.principal_cache_capacity),
            breakglass: config
                .breakglass_username
                .map(|name| fold_account(&name))
                .filter(|name| !name.is_empty()),
            record_attempts: config.record_attempt_metrics,
            ldap_confirmed: Cache::builder()
                .max_capacity(config.principal_cache_capacity)
                .time_to_live(groups_max_age)
                .build(),
        }
    }

    /// Forgets cached principals of one user (their tokens, roles or state changed).
    pub fn invalidate_user(&self, user: UserId) {
        self.principals.invalidate_user(user);
        self.basic.invalidate_user(user);
    }

    /// Forgets every cached principal and the cached mapping tables (RBAC data changed,
    /// or the invalidation stream was re-established after a gap).
    pub fn invalidate_all(&self) {
        self.principals.invalidate_all();
        self.basic.invalidate_all();
        self.mapper.invalidate();
    }

    /// Returns a future that applies cross-node invalidation events until `shutdown`
    /// fires or the stream ends. Spawn it as a supervised task (a `JoinSet` member).
    ///
    /// The subscription is taken here, before the future is returned, so an event
    /// published between this call and the first poll is not lost. When the stream
    /// ends (the bus is gone) the future completes; a caller that wants to survive
    /// that calls [`Self::invalidate_all`] and then this method again.
    pub fn invalidation_task(
        self: &Arc<Self>,
        bus: &dyn InvalidationBus,
        shutdown: CancellationToken,
    ) -> impl Future<Output = ()> + Send + 'static {
        let mut events = bus.subscribe();
        let service = Arc::clone(self);
        async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    event = events.next() => match event {
                        Some(InvalidationEvent::User(id)) => {
                            service.invalidate_user(UserId::from_uuid(id));
                        }
                        Some(InvalidationEvent::Rbac | InvalidationEvent::Reset) => {
                            service.invalidate_all();
                        }
                        // Neither a project nor a settings change alters who a principal is
                        // or which roles it holds, so a cached principal stays correct.
                        Some(InvalidationEvent::Projects | InvalidationEvent::Config(_)) => {}
                        // A variant added later: the safe reading of an event this build
                        // cannot interpret is the one of `Reset`, drop everything.
                        Some(_) => service.invalidate_all(),
                        None => return,
                    },
                }
            }
        }
    }

    /// Members in their fixed order, skipping providers that are not configured, each with
    /// the method it authenticates by (what a failure of it is counted as).
    fn members(&self) -> impl Iterator<Item = (AuthMethod, &Arc<dyn Authenticator>)> {
        [
            Some((AuthMethod::BasicLocal, &self.local_basic)),
            Some((AuthMethod::ApiToken, &self.api_token)),
            Some((AuthMethod::InternalJwt, &self.internal_jwt)),
            self.oidc
                .as_ref()
                .map(|idp| (AuthMethod::OidcBearer, &idp.authenticator)),
            self.ldap
                .as_ref()
                .map(|idp| (AuthMethod::BasicLdap, &idp.authenticator)),
        ]
        .into_iter()
        .flatten()
    }

    /// Runs the members until one claims the credentials. A refusal carries the method of the
    /// member that claimed them.
    async fn claim(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthFailure> {
        for (method, member) in self.members() {
            match member.authenticate(credentials, context).await {
                Ok(Some(principal)) => return Ok(Some(principal)),
                Ok(None) => {}
                Err(error) => return Err(AuthFailure::new(Some(method), error)),
            }
        }
        Ok(None)
    }

    /// `finish` with the failure labelled by the method of the principal being finished.
    async fn finish_as(&self, principal: Principal) -> Result<Principal, AuthFailure> {
        let method = principal.auth_method;
        self.finish(principal)
            .await
            .map_err(|error| AuthFailure::new(Some(method), error))
    }

    async fn bearer(
        &self,
        secret: &Secret,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Principal, AuthFailure> {
        let token = secret.expose();
        let cacheable = !token.starts_with(API_TOKEN_PREFIX);
        let key = token_key(token);
        let now = self.clock.now();
        if cacheable && let Some(hit) = self.principals.get(&key, now).await {
            return Ok(hit);
        }
        let Some(claimed) = self.claim(credentials, context).await? else {
            return Err(AuthFailure::new(
                None,
                AuthError::InvalidToken("unsupported bearer token"),
            ));
        };
        let principal = self.finish_as(claimed).await?;
        if cacheable {
            self.remember(key, &principal, now).await;
        }
        Ok(principal)
    }

    /// Caches a principal for as long as its credential and its group confirmation
    /// stay valid. A principal whose credential carries no expiry is not cached.
    async fn remember(&self, key: [u8; 32], principal: &Principal, now: OffsetDateTime) {
        let Some(mut valid_until) = principal.expires_at else {
            return;
        };
        if let Some(confirmed) = principal.groups_confirmed_at {
            let age = time::Duration::try_from(self.groups_max_age).unwrap_or(time::Duration::ZERO);
            valid_until = valid_until.min(confirmed.saturating_add(age));
        }
        self.principals.put(key, principal, valid_until, now).await;
    }

    async fn basic(
        &self,
        username: &str,
        password: &Secret,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Principal, AuthFailure> {
        // The token namespace: API tokens have 256 bits of entropy, nothing to guess,
        // and counting their failures would let a stale CI token lock a person out.
        if password.expose().starts_with(API_TOKEN_PREFIX) {
            let claimed = self.claim(credentials, context).await?;
            return match claimed {
                Some(principal) => self.finish_as(principal).await,
                None => Err(AuthFailure::new(None, AuthError::InvalidCredentials)),
            };
        }
        // Blank credentials end here, before the guard, the database and any hash.
        if username.trim().is_empty() || !is_usable_password(password.expose()) {
            return Err(AuthFailure::new(None, AuthError::InvalidCredentials));
        }
        // Checked before the password: a locked account must not reveal whether the
        // password was right. It also runs before the Basic cache, so a cached password
        // never outlives a lockout.
        let ticket = self
            .guard
            .check(username, context.client_ip)
            .await
            .map_err(|error| AuthFailure::new(None, error))?;
        let digest = self.basic.digest(username, password.expose());
        if let Some(principal) = self.recall_basic(&digest).await {
            self.guard
                .record_success(&ticket)
                .await
                .map_err(|error| AuthFailure::new(Some(AuthMethod::BasicLocal), error))?;
            return self.finish_as(principal).await;
        }
        let before = self.basic_snapshot(username).await;
        match self.claim(credentials, context).await {
            Ok(Some(principal)) => {
                self.guard
                    .record_success(&ticket)
                    .await
                    .map_err(|error| AuthFailure::new(Some(principal.auth_method), error))?;
                self.accept_basic(digest, before, principal).await
            }
            Ok(None) => {
                // Nobody knows this account. Pay the cost of a real verification under the
                // same semaphore, then fail exactly like a wrong password.
                self.passwords
                    .verify_dummy_before(password, context.deadline)
                    .await
                    .map_err(|error| AuthFailure::new(None, error))?;
                self.record_failure(username, context).await?;
                Err(AuthFailure::new(None, AuthError::InvalidCredentials))
            }
            Err(failure) if matches!(failure.error, AuthError::InvalidCredentials) => {
                self.record_failure(username, context).await?;
                Err(failure)
            }
            Err(failure) => Err(failure),
        }
    }

    async fn record_failure(
        &self,
        username: &str,
        context: &AuthContext,
    ) -> Result<(), AuthFailure> {
        self.guard
            .record_failure(username, context.client_ip)
            .await
            .map_err(|error| AuthFailure::new(None, error))
    }

    /// A verification that just succeeded: finish it, count a break-glass login, and
    /// remember it when the account row proves the verification used the row it describes.
    async fn accept_basic(
        &self,
        digest: [u8; 32],
        before: Option<Snapshot>,
        verified: Principal,
    ) -> Result<Principal, AuthFailure> {
        let finished = self.finish_as(verified.clone()).await?;
        if finished.auth_method == AuthMethod::BasicLocal {
            self.note_breakglass(&finished);
            self.remember_basic(digest, before, &verified).await;
        }
        Ok(finished)
    }

    fn note_breakglass(&self, principal: &Principal) {
        let Some(breakglass) = &self.breakglass else {
            return;
        };
        if principal.source == UserSource::Local && fold_account(&principal.username) == *breakglass
        {
            telemetry::breakglass_login();
            tracing::warn!(
                user = %principal.username,
                "break-glass administrator logged in with a password"
            );
        }
    }

    /// The account row as it is before a Basic verification, for the cache. `None` when the
    /// cache is off, the typed name is not a local account with a password, or the store could
    /// not be read: the cache is bookkeeping and never the reason a request fails (if the store
    /// is down, the verification that follows reports it with the right method).
    async fn basic_snapshot(&self, username: &str) -> Option<Snapshot> {
        if !self.basic.enabled() {
            return None;
        }
        match self.users.find_by_username(username).await {
            Ok(user) => user.as_ref().and_then(Snapshot::of),
            Err(error) => {
                tracing::warn!(%error, "basic cache skipped: the account could not be read");
                None
            }
        }
    }

    /// Stores a verification when the row read after it agrees with the row read before it.
    /// A failed read stores nothing and fails nothing.
    async fn remember_basic(
        &self,
        digest: [u8; 32],
        before: Option<Snapshot>,
        verified: &Principal,
    ) {
        let (Some(before), Some(user_id)) = (before, verified.user_id) else {
            return;
        };
        let after = match self.users.get(user_id).await {
            Ok(user) => user,
            Err(error) => {
                tracing::warn!(%error, "basic cache skipped: the account could not be read");
                return;
            }
        };
        if let Some(after) = after.as_ref().and_then(Snapshot::of) {
            self.basic
                .remember(
                    digest,
                    verified,
                    user_id,
                    (&before, &after),
                    self.clock.now(),
                )
                .await;
        }
    }

    /// The remembered principal of this credential, after the account row confirmed that
    /// nothing it depends on has changed. `None` sends the request down the full path, which
    /// is also where a store outage is reported: a hit that cannot check the row is a miss.
    async fn recall_basic(&self, digest: &[u8; 32]) -> Option<Principal> {
        let verified = self.basic.recall(digest, self.clock.now()).await?;
        let Some(user_id) = verified.principal.user_id else {
            self.basic.forget(digest).await;
            return None;
        };
        let user = match self.users.get(user_id).await {
            Ok(user) => user,
            Err(error) => {
                tracing::warn!(%error, "basic cache skipped: the account could not be read");
                return None;
            }
        };
        match user {
            Some(user) if BasicCache::still_valid(&verified, &user) => {
                // The account as it is now, not as it was: a rename or an e-mail change shows.
                let mut principal =
                    Principal::new(user.username, UserSource::Local, AuthMethod::BasicLocal);
                principal.user_id = Some(user.id);
                principal.email = user.email;
                principal.display_name = user.display_name;
                Some(principal)
            }
            _ => {
                self.basic.forget(digest).await;
                None
            }
        }
    }

    /// Everything but the metrics.
    async fn resolve(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Principal, AuthFailure> {
        match credentials {
            Credentials::Anonymous => Ok(Principal::anonymous()),
            Credentials::Bearer(secret) => self.bearer(secret, credentials, context).await,
            Credentials::Basic { username, password } => {
                self.basic(username, password, credentials, context).await
            }
        }
    }

    /// `resolve` plus `auth_attempts_total`, with the method that refused. Anonymous requests
    /// are not attempts: nothing was presented.
    async fn counted(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Principal, AuthFailure> {
        let outcome = self.resolve(credentials, context).await;
        if self.record_attempts && !matches!(credentials, Credentials::Anonymous) {
            match &outcome {
                Ok(principal) => telemetry::auth_attempt(Some(principal.auth_method), None),
                Err(failure) => telemetry::auth_attempt(failure.method, Some(&failure.error)),
            }
        }
        outcome
    }
}

#[async_trait]
impl Authenticator for AuthService {
    /// Cancel safe. Never returns `Ok(None)`: see the module documentation.
    async fn authenticate(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        self.counted(credentials, context)
            .await
            .map(Some)
            .map_err(|failure| failure.error)
    }

    /// Cancel safe. The same decision as [`Authenticator::authenticate`], and a refusal names
    /// the method that refused (`None` when the credentials were refused before any member
    /// looked at them: a locked account, blank credentials, nobody claimed them).
    async fn authenticate_detailed(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthFailure> {
        self.counted(credentials, context).await.map(Some)
    }
}
