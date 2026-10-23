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
//! - the principal cache for JWT bearers.
//!
//! The service never returns `Ok(None)`: anonymous credentials yield
//! `Principal::anonymous()`, anything else yields a principal or an error. The HTTP
//! layer therefore needs no fallback of its own.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError` variants documented on the type

mod cache;
mod roles;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests_basic;
#[cfg(test)]
mod tests_bearer;
#[cfg(test)]
mod tests_roles;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use moka::future::Cache;
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{API_TOKEN_PREFIX, AuthContext, Credentials, Principal};
use rustryx_core::ids::UserId;
use rustryx_core::ports::{
    Authenticator, Clock, GroupResolver, InvalidationBus, InvalidationEvent, UserRepo,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

pub use self::cache::{DEFAULT_CACHE_CAPACITY, MAX_CACHE_TTL};
use self::cache::{PrincipalCache, token_key};
use self::roles::ConfirmedGroups;
use crate::groups::{JitProvisioner, RoleMapper};
use crate::internal_jwt::unverified_expiry;
use crate::login_guard::LoginGuard;
use crate::passwords::{Passwords, is_usable_password};

/// How old a confirmation of external group membership may be (README 8.5: 5 minutes).
/// A configured value can only lower it.
pub const MAX_GROUPS_AGE: Duration = Duration::from_mins(5);

/// Settings of the [`AuthService`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthServiceConfig {
    /// Oldest group confirmation that still counts, clamped to [`MAX_GROUPS_AGE`].
    pub groups_max_age: Duration,
    /// Capacity of the principal cache.
    pub principal_cache_capacity: u64,
}

impl Default for AuthServiceConfig {
    fn default() -> Self {
        Self {
            groups_max_age: MAX_GROUPS_AGE,
            principal_cache_capacity: DEFAULT_CACHE_CAPACITY,
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
    /// Last successful LDAP group search per user: what may be reused for at most
    /// `groups_max_age` when the directory fails.
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
            ldap_confirmed: Cache::builder()
                .max_capacity(config.principal_cache_capacity)
                .time_to_live(groups_max_age)
                .build(),
        }
    }

    /// Forgets cached principals of one user (their tokens, roles or state changed).
    pub fn invalidate_user(&self, user: UserId) {
        self.principals.invalidate_user(user);
    }

    /// Forgets every cached principal and the cached mapping tables (RBAC data changed,
    /// or the invalidation stream was re-established after a gap).
    pub fn invalidate_all(&self) {
        self.principals.invalidate_all();
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
                        None => return,
                    },
                }
            }
        }
    }

    /// Members in their fixed order, skipping providers that are not configured.
    fn members(&self) -> impl Iterator<Item = &Arc<dyn Authenticator>> {
        [
            Some(&self.local_basic),
            Some(&self.api_token),
            Some(&self.internal_jwt),
            self.oidc.as_ref().map(|idp| &idp.authenticator),
            self.ldap.as_ref().map(|idp| &idp.authenticator),
        ]
        .into_iter()
        .flatten()
    }

    /// Runs the members until one claims the credentials.
    async fn claim(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        for member in self.members() {
            if let Some(principal) = member.authenticate(credentials, context).await? {
                return Ok(Some(principal));
            }
        }
        Ok(None)
    }

    async fn bearer(
        &self,
        secret: &Secret,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Principal, AuthError> {
        let token = secret.expose();
        let cacheable = !token.starts_with(API_TOKEN_PREFIX);
        let key = token_key(token);
        let now = self.clock.now();
        if cacheable && let Some(hit) = self.principals.get(&key, now).await {
            return Ok(hit);
        }
        let Some(claimed) = self.claim(credentials, context).await? else {
            return Err(AuthError::InvalidToken("unsupported bearer token"));
        };
        let principal = self.finish(claimed).await?;
        if cacheable {
            self.remember(key, token, &principal, now).await;
        }
        Ok(principal)
    }

    /// Caches a principal for as long as its credential and its group confirmation
    /// stay valid. Tokens whose expiry cannot be read are not cached.
    async fn remember(
        &self,
        key: [u8; 32],
        token: &str,
        principal: &Principal,
        now: OffsetDateTime,
    ) {
        let Some(mut valid_until) = unverified_expiry(token) else {
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
    ) -> Result<Principal, AuthError> {
        // The token namespace: API tokens have 256 bits of entropy, nothing to guess,
        // and counting their failures would let a stale CI token lock a person out.
        if password.expose().starts_with(API_TOKEN_PREFIX) {
            let claimed = self.claim(credentials, context).await?;
            return match claimed {
                Some(principal) => self.finish(principal).await,
                None => Err(AuthError::InvalidCredentials),
            };
        }
        // Blank credentials end here, before the guard, the database and any hash.
        if username.trim().is_empty() || !is_usable_password(password.expose()) {
            return Err(AuthError::InvalidCredentials);
        }
        // Checked before the password: a locked account must not reveal whether the
        // password was right.
        let ticket = self.guard.check(username, context.client_ip).await?;
        match self.claim(credentials, context).await {
            Ok(Some(principal)) => {
                self.guard.record_success(&ticket).await?;
                self.finish(principal).await
            }
            Ok(None) => {
                // Nobody knows this account. Pay the cost of a real verification under the
                // same semaphore, then fail exactly like a wrong password.
                self.passwords.verify_dummy(password).await?;
                self.guard
                    .record_failure(username, context.client_ip)
                    .await?;
                Err(AuthError::InvalidCredentials)
            }
            Err(AuthError::InvalidCredentials) => {
                self.guard
                    .record_failure(username, context.client_ip)
                    .await?;
                Err(AuthError::InvalidCredentials)
            }
            Err(other) => Err(other),
        }
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
        let principal = match credentials {
            Credentials::Anonymous => Principal::anonymous(),
            Credentials::Bearer(secret) => self.bearer(secret, credentials, context).await?,
            Credentials::Basic { username, password } => {
                self.basic(username, password, credentials, context).await?
            }
        };
        Ok(Some(principal))
    }
}
