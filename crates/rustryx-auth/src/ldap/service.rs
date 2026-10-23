//! The LDAP service shared by [`LdapAuthenticator`](super::LdapAuthenticator) and
//! [`LdapGroupResolver`](super::LdapGroupResolver): configuration, pool, transport,
//! clock, counters and the group cache.
//!
//! Primitive operations (user search, user bind, decoy bind) live here; the
//! authentication sequence is in `authenticator.rs` and the group refresh policy in
//! `resolver.rs`.

use std::fmt;
use std::sync::Arc;

use rustryx_core::ports::Clock;
use tokio::sync::Semaphore;
use tokio::time::timeout;

use super::cache::GroupCache;
use super::config::{LdapConfig, LdapConfigError, LdapGroupStrategy, LdapSettings};
use super::error::{LdapError, Operation};
use super::filter::Substitution;
use super::net::NetTransport;
use super::pool::{PooledConnection, ServicePool};
use super::stats::{Counter, LdapStats};
use super::tls::TlsSetupError;
use super::transport::{
    LdapConnection, LdapTransport, SearchRequest, SearchResponse, SearchScope, TransportError,
    result_code,
};
use super::user::{DirectoryUser, UserLookup};
use crate::telemetry::{self, RequestResult};

/// RDN of the account the decoy bind targets. It is never expected to exist, and the
/// result of that bind is ignored even if it does.
const DECOY_RDN: &str = "cn=rustryx-decoy";

/// Why the service could not be built at startup.
#[derive(Debug, thiserror::Error)]
pub enum LdapInitError {
    /// The configuration is invalid (including a plaintext URL).
    #[error(transparent)]
    Config(#[from] LdapConfigError),
    /// The TLS trust material could not be loaded.
    #[error(transparent)]
    Tls(#[from] TlsSetupError),
}

/// Outcome of a user bind that reached the server and was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindVerdict {
    Accepted,
    Rejected,
}

/// Everything an enabled service needs.
pub(crate) struct Active {
    pub(crate) settings: LdapSettings,
    pub(crate) transport: Arc<dyn LdapTransport>,
    pub(crate) pool: Arc<ServicePool>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) stats: Arc<LdapStats>,
    pub(crate) cache: GroupCache,
    /// Bounds the user bind connections open at the same time.
    user_binds: Semaphore,
}

/// Shared LDAP state. Build it once at startup and hand it to
/// [`LdapAuthenticator::new`](super::LdapAuthenticator::new) and
/// [`LdapGroupResolver::new`](super::LdapGroupResolver::new).
pub struct LdapService {
    stats: Arc<LdapStats>,
    /// The injected clock, kept even by a disabled service: the resolver stamps the
    /// confirmation time of the (empty) answer for a principal that is not an LDAP one.
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) active: Option<Active>,
}

impl LdapService {
    /// Validates `config`, loads the TLS trust material and builds the service.
    ///
    /// Nothing is connected: an unreachable directory must not stop the node from
    /// starting (README 8.2), the first login finds out. A bad configuration, a
    /// plaintext URL or an unreadable `ca_file` does stop it.
    pub async fn new(
        config: &LdapConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, LdapInitError> {
        config.validate()?;
        if !config.enabled {
            return Ok(Arc::new(Self::disabled(clock)));
        }
        let settings = config.settings()?;
        let transport = NetTransport::from_settings(&settings).await?;
        Ok(Arc::new(Self::enabled(
            settings,
            Arc::new(transport),
            clock,
        )))
    }

    /// Builds the service on a caller supplied transport (tests, or a transport with
    /// a different connection policy). The configuration rules still apply.
    pub fn with_transport(
        config: &LdapConfig,
        transport: Arc<dyn LdapTransport>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, LdapConfigError> {
        config.validate()?;
        if !config.enabled {
            return Ok(Arc::new(Self::disabled(clock)));
        }
        let settings = config.settings()?;
        Ok(Arc::new(Self::enabled(settings, transport, clock)))
    }

    fn disabled(clock: Arc<dyn Clock>) -> Self {
        Self {
            stats: Arc::new(LdapStats::default()),
            clock,
            active: None,
        }
    }

    fn enabled(
        settings: LdapSettings,
        transport: Arc<dyn LdapTransport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let stats = Arc::new(LdapStats::default());
        let pool = ServicePool::new(
            Arc::clone(&transport),
            settings.bind_dn.clone(),
            settings.bind_password.clone(),
            settings.timeouts,
            settings.pool_size,
            settings.pool_max_lifetime,
            Arc::clone(&stats),
        );
        let cache = GroupCache::new(settings.groups.cache_ttl);
        let user_binds = Semaphore::new(settings.max_user_binds);
        Self {
            stats: Arc::clone(&stats),
            clock: Arc::clone(&clock),
            active: Some(Active {
                settings,
                transport,
                pool,
                clock,
                stats,
                cache,
                user_binds,
            }),
        }
    }

    /// True when `auth.ldap.enabled` is set. The authentication chain asks this to
    /// decide whether a user can be handled by LDAP at all.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.active.is_some()
    }

    /// Counters for exporting as metrics.
    #[must_use]
    pub fn stats(&self) -> &LdapStats {
        &self.stats
    }
}

impl fmt::Debug for LdapService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LdapService")
            .field("enabled", &self.is_enabled())
            .finish_non_exhaustive()
    }
}

impl Active {
    /// Searches for the user with the configured filter.
    ///
    /// The search is capped at two entries: one is the answer, two prove the filter is
    /// ambiguous, and nothing more is worth transferring. A result that was cut by a
    /// size limit cannot prove uniqueness, so it counts as ambiguous too.
    pub(crate) async fn find_user(
        &self,
        conn: &mut PooledConnection,
        login: &str,
    ) -> Result<UserLookup, LdapError> {
        let request = self.user_request(
            self.settings.user_base_dn.clone(),
            SearchScope::Subtree,
            login,
        );
        let response = conn.search(&request).await?;
        Ok(self.interpret_user_response(response, login))
    }

    /// Reads the user entry at a DN learned earlier (a login, or the last refresh) instead of
    /// searching the subtree for it.
    ///
    /// The DN is only a hint and the filter still decides: the request is the very same user
    /// filter, rendered with this login, but with the entry as the base and the scope `Base`.
    /// An account that was disabled, deleted or renamed since, or an entry that never was this
    /// user's (a forged hint), therefore answers `NotFound` exactly like a subtree search
    /// would, and "the user search is the place to exclude disabled accounts" (docs/ldap.md)
    /// keeps holding at every refresh. `noSuchObject` is the same answer. The caller falls
    /// back to the subtree search on `NotFound`, because a moved entry is not a gone user.
    pub(crate) async fn find_user_at(
        &self,
        conn: &mut PooledConnection,
        dn: &str,
        login: &str,
    ) -> Result<UserLookup, LdapError> {
        let request = self.user_request(dn.to_owned(), SearchScope::Base, login);
        match conn.search(&request).await {
            Ok(response) => Ok(self.interpret_user_response(response, login)),
            Err(LdapError::Transport(TransportError::Server { code, .. }))
                if code == result_code::NO_SUCH_OBJECT =>
            {
                Ok(UserLookup::NotFound)
            }
            Err(error) => Err(error),
        }
    }

    fn user_request(&self, base: String, scope: SearchScope, login: &str) -> SearchRequest {
        let settings = &self.settings;
        let mut attributes = vec![settings.username_attribute.clone()];
        if settings.groups.strategy == LdapGroupStrategy::MemberOf {
            attributes.push(settings.groups.member_of_attribute.clone());
        }
        SearchRequest {
            base,
            scope,
            filter: settings.user_filter.render(Substitution {
                username: login,
                dn: "",
            }),
            attributes,
            size_limit: Some(2),
        }
    }

    fn interpret_user_response(&self, response: SearchResponse, login: &str) -> UserLookup {
        if response.entries.is_empty() {
            return UserLookup::NotFound;
        }
        if response.entries.len() != 1 || response.size_limit_exceeded {
            self.stats.bump(Counter::AmbiguousUser);
            return UserLookup::Ambiguous;
        }
        let entry = response.entries.into_iter().next().unwrap_or_default();
        // An entry without a DN cannot be bound as, and an empty DN plus a password is
        // the shape of an anonymous bind: treat it as no user.
        if entry.dn.trim().is_empty() {
            return UserLookup::NotFound;
        }
        UserLookup::Found(DirectoryUser::from_entry(
            entry,
            login,
            &self.settings.username_attribute,
        ))
    }

    /// Verifies a password by binding as `dn` on a brand new connection.
    ///
    /// The connection is opened here, used for this one bind and closed on every path.
    /// It is not obtained from, and is never handed to, the service account pool: a
    /// connection that has carried a user's credentials must not later search on the
    /// service account's behalf.
    pub(crate) async fn bind_as_user(
        &self,
        dn: &str,
        password: &str,
    ) -> Result<BindVerdict, LdapError> {
        // Every login opens its own connection, so the number of them in flight is
        // capped: a storm of logins must queue here (inside the authentication deadline)
        // instead of opening that many TLS connections to the directory.
        let _slot = self
            .user_binds
            .acquire()
            .await
            .map_err(|_| LdapError::PoolClosed)?;
        let verdict = self.bind_on_new_connection(dn, password).await;
        // A refused password is the directory doing its job: only a failure to get an answer
        // counts as an error.
        telemetry::ldap_request(
            verdict
                .as_ref()
                .map_or_else(LdapError::request_result, |_| RequestResult::Ok),
        );
        verdict
    }

    async fn bind_on_new_connection(
        &self,
        dn: &str,
        password: &str,
    ) -> Result<BindVerdict, LdapError> {
        let mut conn = self.connect().await?;
        let outcome = timeout(self.settings.timeouts.bind, conn.simple_bind(dn, password)).await;
        self.close(conn).await;
        match outcome {
            Ok(Ok(())) => Ok(BindVerdict::Accepted),
            Ok(Err(TransportError::Server { code, .. })) if is_credential_rejection(code) => {
                Ok(BindVerdict::Rejected)
            }
            Ok(Err(other)) => Err(LdapError::Transport(other)),
            Err(_) => Err(LdapError::Timeout(Operation::Bind)),
        }
    }

    /// Spends the time of a user bind when there is nobody to bind as.
    ///
    /// "No such user" would otherwise answer one connect and one bind sooner than
    /// "wrong password", and the difference is measurable from outside. The bind goes
    /// to a DN that does not exist and its result, whatever it is, is discarded: a
    /// decoy can never authenticate anyone.
    pub(crate) async fn decoy_bind(&self, password: &str) {
        let dn = format!("{DECOY_RDN},{}", self.settings.user_base_dn);
        if let Err(error) = self.bind_as_user(&dn, password).await {
            tracing::debug!(%error, "decoy bind failed");
        }
    }

    async fn connect(&self) -> Result<Box<dyn LdapConnection>, LdapError> {
        timeout(self.settings.timeouts.connect, self.transport.connect())
            .await
            .map_err(|_| LdapError::Timeout(Operation::Connect))?
            .map_err(LdapError::from)
    }

    async fn close(&self, conn: Box<dyn LdapConnection>) {
        // Best effort: a connection that cannot be closed in time is dropped, which
        // closes the socket anyway.
        let _ = timeout(self.settings.timeouts.bind, conn.close()).await;
    }
}

/// Result codes that mean "this identity and password pair is refused".
///
/// Codes that mean the server could not decide (busy, unavailable, protocol or
/// configuration problems such as `confidentialityRequired`) are NOT in the list: they
/// are outages and must not be reported to the user as a wrong password.
const fn is_credential_rejection(code: u32) -> bool {
    matches!(
        code,
        19 // constraintViolation (password policy)
        | 32 // noSuchObject
        | 34 // invalidDNSyntax
        | 48 // inappropriateAuthentication
        | 49 // invalidCredentials
        | 50 // insufficientAccessRights
        | 53 // unwillingToPerform
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_rejection_codes_are_exactly_the_documented_set() {
        for code in [19, 32, 34, 48, 49, 50, 53] {
            assert!(is_credential_rejection(code), "{code}");
        }
        for code in [0, 1, 2, 3, 7, 8, 11, 13, 51, 52, 80] {
            assert!(!is_credential_rejection(code), "{code}");
        }
    }
}
