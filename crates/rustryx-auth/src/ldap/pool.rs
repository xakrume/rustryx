//! Pool of service account connections.
//!
//! The pool holds connections that are bound as the service account and nothing
//! else. A connection that carried a USER bind never gets here: user binds are made
//! by `LdapService` on a connection it opens itself and closes afterwards, and there
//! is no function in this module that accepts a connection from outside. That is how
//! the rule "a user bound connection is never returned to the service pool" is held:
//! by construction, not by a flag someone could forget to check.
//!
//! Invariants:
//! - at most `size` connections exist at once (idle plus checked out): the semaphore
//!   permit travels with the checked out connection;
//! - a connection is returned only if the last operation on it finished normally.
//!   One that was cancelled (timeout, dropped future) or failed at the transport level
//!   may have a pending response on the wire and is discarded;
//! - a connection older than `max_lifetime` is retired on checkout and on return;
//! - the health check runs on every checkout of an idle connection.
//!
//! No mutex guard is held across an `.await`: the idle list is only touched in short
//! synchronous sections.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustryx_core::Secret;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, timeout};

use super::breaker::{BREAKER_COOLDOWN, BREAKER_FAILURES, CircuitBreaker};
use super::config::Timeouts;
use super::error::{LdapError, Operation};
use super::stats::{Counter, LdapStats};
use super::transport::{
    LdapConnection, LdapTransport, SearchRequest, SearchResponse, TransportError,
};

struct IdleConnection {
    conn: Box<dyn LdapConnection>,
    opened: Instant,
}

/// The pool. Cheap to share behind an `Arc`.
pub(crate) struct ServicePool {
    transport: Arc<dyn LdapTransport>,
    bind_dn: String,
    bind_password: Secret,
    timeouts: Timeouts,
    max_lifetime: Duration,
    size: usize,
    permits: Arc<Semaphore>,
    idle: Mutex<Vec<IdleConnection>>,
    breaker: CircuitBreaker,
    stats: Arc<LdapStats>,
}

/// A checked out service connection. Dropping it returns it to the pool when that is
/// safe, and discards it otherwise.
pub(crate) struct PooledConnection {
    conn: Option<Box<dyn LdapConnection>>,
    opened: Instant,
    /// True while an operation is in flight or ended in a transport failure.
    broken: bool,
    pool: Arc<ServicePool>,
    _permit: OwnedSemaphorePermit,
}

impl ServicePool {
    pub(crate) fn new(
        transport: Arc<dyn LdapTransport>,
        bind_dn: String,
        bind_password: Secret,
        timeouts: Timeouts,
        size: usize,
        max_lifetime: Duration,
        stats: Arc<LdapStats>,
    ) -> Arc<Self> {
        Arc::new(Self {
            transport,
            bind_dn,
            bind_password,
            timeouts,
            max_lifetime,
            size,
            permits: Arc::new(Semaphore::new(size)),
            idle: Mutex::new(Vec::with_capacity(size)),
            breaker: CircuitBreaker::new(BREAKER_FAILURES, BREAKER_COOLDOWN),
            stats,
        })
    }

    /// Takes a bound connection: an idle one that passes the health check, or a new one.
    ///
    /// Waiting for a free slot is bounded only by the caller's deadline, which the
    /// callers always apply.
    pub(crate) async fn checkout(self: &Arc<Self>) -> Result<PooledConnection, LdapError> {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| LdapError::PoolClosed)?;
        while let Some(candidate) = self.pop_idle() {
            if candidate.opened.elapsed() >= self.max_lifetime {
                continue;
            }
            let mut conn = candidate.conn;
            if self.is_alive(conn.as_mut()).await {
                return Ok(self.wrap(conn, candidate.opened, permit));
            }
            self.stats.bump(Counter::PoolUnhealthy);
        }
        let conn = self.open().await?;
        Ok(self.wrap(conn, Instant::now(), permit))
    }

    fn wrap(
        self: &Arc<Self>,
        conn: Box<dyn LdapConnection>,
        opened: Instant,
        permit: OwnedSemaphorePermit,
    ) -> PooledConnection {
        PooledConnection {
            conn: Some(conn),
            opened,
            broken: false,
            pool: Arc::clone(self),
            _permit: permit,
        }
    }

    fn pop_idle(&self) -> Option<IdleConnection> {
        // The guard is dropped at the end of the statement, before any await.
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
    }

    async fn is_alive(&self, conn: &mut dyn LdapConnection) -> bool {
        matches!(
            timeout(self.timeouts.search, conn.is_healthy()).await,
            Ok(true)
        )
    }

    /// Opens and binds a new connection, unless the breaker says the directory has been
    /// failing: then it refuses at once instead of spending another timeout (and another
    /// attempt with a service password that may be wrong).
    async fn open(&self) -> Result<Box<dyn LdapConnection>, LdapError> {
        let Some(ticket) = self.breaker.admit() else {
            self.stats.bump(Counter::BreakerRejected);
            return Err(LdapError::CircuitOpen);
        };
        let result = self.open_unguarded().await;
        match &result {
            Ok(_) => ticket.succeed(),
            Err(error) => {
                if ticket.fail() {
                    self.stats.bump(Counter::BreakerTrip);
                    tracing::warn!(
                        %error,
                        failures = BREAKER_FAILURES,
                        cooldown_secs = BREAKER_COOLDOWN.as_secs(),
                        "ldap circuit breaker opened: new service connections are refused for the cooldown"
                    );
                }
            }
        }
        result
    }

    async fn open_unguarded(&self) -> Result<Box<dyn LdapConnection>, LdapError> {
        let mut conn = timeout(self.timeouts.connect, self.transport.connect())
            .await
            .map_err(|_| LdapError::Timeout(Operation::Connect))??;
        let bound = timeout(
            self.timeouts.bind,
            conn.simple_bind(&self.bind_dn, self.bind_password.expose()),
        )
        .await;
        match bound {
            Ok(Ok(())) => Ok(conn),
            Ok(Err(error)) => Err(LdapError::ServiceBind(error)),
            Err(_) => Err(LdapError::Timeout(Operation::Bind)),
        }
    }

    fn give_back(&self, conn: Box<dyn LdapConnection>, opened: Instant) {
        if opened.elapsed() >= self.max_lifetime {
            return;
        }
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        if idle.len() < self.size {
            idle.push(IdleConnection { conn, opened });
        }
    }

    #[cfg(test)]
    pub(crate) fn idle_count(&self) -> usize {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl PooledConnection {
    /// Runs one search under the search timeout.
    ///
    /// A server answer with a result code (for example `noSuchObject`) leaves the
    /// connection usable. Anything else, and a timeout above all, marks it broken.
    pub(crate) async fn search(
        &mut self,
        request: &SearchRequest,
    ) -> Result<SearchResponse, LdapError> {
        let limit = self.pool.timeouts.search;
        let Some(conn) = self.conn.as_mut() else {
            return Err(LdapError::Transport(TransportError::Closed));
        };
        self.broken = true;
        match timeout(limit, conn.search(request)).await {
            Ok(Ok(response)) => {
                self.broken = false;
                Ok(response)
            }
            Ok(Err(error)) => {
                self.broken = !matches!(error, TransportError::Server { .. });
                Err(LdapError::Transport(error))
            }
            Err(_) => Err(LdapError::Timeout(Operation::Search)),
        }
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if self.broken {
            self.pool.stats.bump(Counter::PoolDiscarded);
            return;
        }
        self.pool.give_back(conn, self.opened);
    }
}
