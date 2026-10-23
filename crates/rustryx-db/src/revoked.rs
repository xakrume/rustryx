//! [`RevokedTokenRepo`] on `PostgreSQL`: the revocation list of internal JWT ids.
//!
//! An in-memory list is per node: a token revoked on node A would still be accepted by node B
//! until it expired. The table is the shared list; the `tid` primary key makes a revocation
//! idempotent and a lookup one index probe.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ports::RevokedTokenRepo;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::map_err;

/// Revoked internal JWT ids.
#[derive(Debug, Clone)]
pub struct PgRevokedTokens {
    pool: PgPool,
}

impl PgRevokedTokens {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RevokedTokenRepo for PgRevokedTokens {
    /// Idempotent. A second revocation of the same id keeps the LATER expiry: the row exists to
    /// outlive the token, and an earlier `expires_at` would let the purge remove it while the
    /// token still validates.
    async fn revoke(&self, tid: Uuid, expires_at: OffsetDateTime) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO revoked_tokens (tid, expires_at) VALUES ($1, $2) \
             ON CONFLICT (tid) DO UPDATE \
             SET expires_at = GREATEST(revoked_tokens.expires_at, EXCLUDED.expires_at)",
        )
        .bind(tid)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("revoke token", e))?;
        Ok(())
    }

    /// A row that expired but was not purged yet still counts as revoked: nothing is gained by
    /// a time check here, the token's own `exp` rejects it, and the answer stays a pure
    /// membership test.
    async fn is_revoked(&self, tid: Uuid) -> Result<bool, StoreError> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM revoked_tokens WHERE tid = $1)")
            .bind(tid)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("check revoked token", e))
    }

    /// Removes the rows whose expiry lies STRICTLY before `now`. A row that expires exactly at
    /// `now` stays: a token with `exp == now` is still inside the clock skew leeway of the
    /// validator, so the purge must not be the one that makes it valid again.
    async fn purge_expired(&self, now: OffsetDateTime) -> Result<u64, StoreError> {
        let result = sqlx::query("DELETE FROM revoked_tokens WHERE expires_at < $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("purge revoked tokens", e))?;
        Ok(result.rows_affected())
    }
}
