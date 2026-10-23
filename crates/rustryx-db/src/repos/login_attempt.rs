//! [`LoginAttemptRepo`] on `PostgreSQL`.
//!
//! PostgreSQL is the single source of truth for failed-login counters, shared by every node
//! and every worker thread. There is no process-local counter anywhere.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ports::{LoginAttemptRepo, LoginKey, LoginState};
use sqlx::PgPool;
use time::OffsetDateTime;

use super::common::corrupt;
use crate::error::map_err;

/// Failure counts stop growing here. A counter is compared with a threshold of a few
/// attempts; saturating keeps an attacker from overflowing the `integer` column.
const MAX_FAILURES: i32 = 1_000_000;

/// Failed-login counters.
#[derive(Debug, Clone)]
pub struct PgLoginAttemptRepo {
    pool: PgPool,
}

impl PgLoginAttemptRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// `''` selects the per-account counter, an address the `(account, ip)` counter.
fn ip_column(key: &LoginKey) -> String {
    key.ip.map(|ip| ip.to_string()).unwrap_or_default()
}

fn state(failures: i32, at: Option<OffsetDateTime>) -> Result<LoginState, StoreError> {
    let failures = u32::try_from(failures).map_err(|_| corrupt("read login attempts"))?;
    Ok(LoginState {
        failures,
        last_failure_at: at,
    })
}

#[async_trait]
impl LoginAttemptRepo for PgLoginAttemptRepo {
    /// One `INSERT .. ON CONFLICT DO UPDATE`: the increment happens inside the statement under
    /// the row lock, so concurrent failures from any number of nodes never lose a count (a
    /// read-modify-write in application code would).
    async fn register_failure(
        &self,
        key: &LoginKey,
        at: OffsetDateTime,
    ) -> Result<LoginState, StoreError> {
        let (failures, last): (i32, OffsetDateTime) = sqlx::query_as(
            "INSERT INTO login_attempts (account, ip, failures, last_failure_at) \
             VALUES ($1, $2, 1, $3) \
             ON CONFLICT (account, ip) DO UPDATE SET \
                 failures = LEAST(login_attempts.failures + 1, $4), \
                 last_failure_at = GREATEST(login_attempts.last_failure_at, EXCLUDED.last_failure_at) \
             RETURNING failures, last_failure_at",
        )
        .bind(&key.account)
        .bind(ip_column(key))
        .bind(at)
        .bind(MAX_FAILURES)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("register login failure", e))?;
        state(failures, Some(last))
    }

    async fn state(&self, key: &LoginKey) -> Result<LoginState, StoreError> {
        let row: Option<(i32, OffsetDateTime)> = sqlx::query_as(
            "SELECT failures, last_failure_at FROM login_attempts WHERE account = $1 AND ip = $2",
        )
        .bind(&key.account)
        .bind(ip_column(key))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read login attempts", e))?;
        match row {
            Some((failures, last)) => state(failures, Some(last)),
            None => Ok(LoginState {
                failures: 0,
                last_failure_at: None,
            }),
        }
    }

    async fn reset(&self, key: &LoginKey) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM login_attempts WHERE account = $1 AND ip = $2")
            .bind(&key.account)
            .bind(ip_column(key))
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("reset login attempts", e))?;
        Ok(())
    }
}
