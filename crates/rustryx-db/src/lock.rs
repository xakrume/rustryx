//! Distributed locks on `PostgreSQL` session advisory locks.
//!
//! One DEDICATED, DIRECT connection per held lock. A session advisory lock lives exactly as
//! long as the session: if the node dies or the connection breaks, the server releases the
//! lock, which is the property that makes a singleton job safe to take over. A transaction
//! mode pooler would break it, because consecutive statements can land on different server
//! sessions, so the URL given here must bypass any pooler.
//!
//! # TTL semantics
//!
//! An advisory lock has no expiry. To behave like the Redis backend, whose lock expires
//! unless renewed, the guard emulates a TTL: it is valid for `ttl` after acquisition and
//! after every successful `renew`. A holder that misses its renewals for longer than `ttl`
//! (a stalled task, a long pause) is treated as having lost the lock: `is_lost()` turns
//! true, `renew()` returns `false` and the connection is closed, which frees the lock for
//! someone else. A job written against the port therefore sees the same lifecycle on both
//! backends. Loss detection by `renew` also covers a dead connection and a lock that is no
//! longer held by the session.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::ports::{LockBackend, LockGuard};
use rustryx_core::{Secret, StoreError};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use tokio::time::Instant;

use crate::error::map_err;
use crate::pool::parse_options;

/// Connect timeout of the dedicated lock connection (README 8.2: connect 3 s).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Stable 64 bit advisory lock key of a job key.
///
/// SHA-256 of a namespaced string, first eight bytes. Two different job keys sharing a lock
/// would need a 64 bit collision; the `rustryx:lock:` prefix keeps keys apart from advisory
/// locks other applications might take on the same database.
#[must_use]
pub fn advisory_key(name: &str) -> i64 {
    let digest = Sha256::digest(format!("rustryx:lock:{name}").as_bytes());
    let mut first = [0_u8; 8];
    first.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(first)
}

/// [`LockBackend`] on advisory locks.
#[derive(Debug, Clone)]
pub struct PgLockBackend {
    options: PgConnectOptions,
}

impl PgLockBackend {
    /// `direct_url` must point at the server itself (no transaction-mode pooler).
    pub fn new(direct_url: &Secret) -> Result<Self, StoreError> {
        let options = parse_options(direct_url)?.application_name("rustryx-lock");
        Ok(Self { options })
    }
}

#[async_trait]
impl LockBackend for PgLockBackend {
    async fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<Box<dyn LockGuard>>, StoreError> {
        if ttl.is_zero() {
            return Err(StoreError::Conflict(
                "lock ttl must be greater than zero".to_owned(),
            ));
        }
        let mut conn =
            tokio::time::timeout(CONNECT_TIMEOUT, PgConnection::connect_with(&self.options))
                .await
                .map_err(|_| StoreError::Unavailable("lock connection timed out".to_owned()))?
                .map_err(|e| map_err("connect for lock", e))?;
        let advisory = advisory_key(key);
        let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(advisory)
            .fetch_one(&mut conn)
            .await;
        match acquired {
            Ok(true) => Ok(Some(Box::new(PgLockGuard {
                conn: Some(conn),
                advisory,
                ttl,
                deadline: Instant::now() + ttl,
                lost: false,
            }))),
            Ok(false) => {
                // Do not keep a connection per failed attempt: pollers retry often.
                let _ = conn.close().await;
                Ok(None)
            }
            Err(e) => {
                let _ = conn.close().await;
                Err(map_err("take advisory lock", e))
            }
        }
    }
}

struct PgLockGuard {
    /// `None` once the connection was closed (loss or release).
    conn: Option<PgConnection>,
    advisory: i64,
    ttl: Duration,
    deadline: Instant,
    lost: bool,
}

impl PgLockGuard {
    async fn mark_lost(&mut self) {
        self.lost = true;
        if let Some(conn) = self.conn.take() {
            // Closing frees the server side lock if the session is still alive.
            let _ = conn.close().await;
        }
    }
}

/// `pg_locks` stores a 64 bit key as two 32 bit halves.
const HELD_QUERY: &str = "SELECT EXISTS (SELECT 1 FROM pg_locks \
     WHERE locktype = 'advisory' AND pid = pg_backend_pid() AND granted \
     AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 1)";

#[async_trait]
impl LockGuard for PgLockGuard {
    async fn renew(&mut self) -> Result<bool, StoreError> {
        if self.lost {
            return Ok(false);
        }
        let now = Instant::now();
        if now > self.deadline {
            self.mark_lost().await;
            return Ok(false);
        }
        let Some(conn) = self.conn.as_mut() else {
            self.lost = true;
            return Ok(false);
        };
        let high = i64::from(u32::try_from(self.advisory >> 32 & 0xFFFF_FFFF).unwrap_or(0));
        let low = i64::from(u32::try_from(self.advisory & 0xFFFF_FFFF).unwrap_or(0));
        let check = sqlx::query_scalar::<_, bool>(HELD_QUERY)
            .bind(high)
            .bind(low)
            .fetch_one(conn);
        // A query that hangs (network partition) must not outlive the TTL: past it the lock
        // is lost by definition.
        match tokio::time::timeout(self.deadline - now, check).await {
            Ok(Ok(true)) => {
                self.deadline = Instant::now() + self.ttl;
                Ok(true)
            }
            Ok(Ok(false)) | Err(_) => {
                self.mark_lost().await;
                Ok(false)
            }
            Ok(Err(err)) => {
                if connection_is_gone(&err) {
                    self.mark_lost().await;
                    Ok(false)
                } else {
                    // The session is alive and still holds the lock; the caller may retry
                    // until the deadline passes.
                    Err(map_err("renew advisory lock", err))
                }
            }
        }
    }

    fn is_lost(&self) -> bool {
        self.lost || Instant::now() > self.deadline
    }

    async fn release(mut self: Box<Self>) -> Result<(), StoreError> {
        if let Some(mut conn) = self.conn.take() {
            // Best effort: closing the session releases the lock even when the explicit
            // unlock cannot run.
            let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(self.advisory)
                .execute(&mut conn)
                .await;
            let _ = conn.close().await;
        }
        self.lost = true;
        Ok(())
    }
}

fn connection_is_gone(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::WorkerCrashed
        | sqlx::Error::PoolClosed => true,
        sqlx::Error::Database(db) => db.code().is_some_and(|c| {
            c.starts_with("08") || matches!(c.as_ref(), "57P01" | "57P02" | "57P03")
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_key_is_stable_and_distinct() {
        assert_eq!(
            advisory_key("cleanup:repo-1"),
            advisory_key("cleanup:repo-1")
        );
        assert_ne!(
            advisory_key("cleanup:repo-1"),
            advisory_key("cleanup:repo-2")
        );
    }
}
