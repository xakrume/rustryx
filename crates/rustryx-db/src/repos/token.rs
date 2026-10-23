//! [`TokenRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::identity::ApiToken;
use rustryx_core::ids::{ApiTokenId, UserId};
use rustryx_core::ports::{InvalidationEvent, TokenRepo};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::notify;
use crate::error::map_err;

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, user_id, name, token_hash, display_prefix, scopes, expires_at, revoked_at, \
             last_used_at, created_at FROM user_tokens ",
            $($tail),*
        )
    };
}

/// A write of `last_used_at` is skipped when the stored value is this recent: the column is
/// for humans (stale token audits), and one `UPDATE` per request would make every
/// authenticated request a write.
const LAST_USED_COALESCE_SECS: f64 = 60.0;

/// API tokens (hashes only).
#[derive(Debug, Clone)]
pub struct PgTokenRepo {
    pool: PgPool,
}

impl PgTokenRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    name: String,
    token_hash: String,
    display_prefix: String,
    scopes: Vec<String>,
    expires_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
    last_used_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

impl From<Row> for ApiToken {
    fn from(row: Row) -> Self {
        Self {
            id: ApiTokenId::from_uuid(row.id),
            user_id: UserId::from_uuid(row.user_id),
            name: row.name,
            token_hash: row.token_hash,
            display_prefix: row.display_prefix,
            scopes: row.scopes,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
            last_used_at: row.last_used_at,
            created_at: row.created_at,
        }
    }
}

#[async_trait]
impl TokenRepo for PgTokenRepo {
    /// `Conflict` for a duplicate hash and for a hash that is not 64 lowercase hex
    /// characters (check constraint): a malformed value would never match a lookup.
    async fn create(&self, token: &ApiToken) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO user_tokens (id, user_id, name, token_hash, display_prefix, scopes, \
             expires_at, revoked_at, last_used_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(token.id.as_uuid())
        .bind(token.user_id.as_uuid())
        .bind(&token.name)
        .bind(&token.token_hash)
        .bind(&token.display_prefix)
        .bind(&token.scopes)
        .bind(token.expires_at)
        .bind(token.revoked_at)
        .bind(token.last_used_at)
        .bind(token.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create token", e))?;
        Ok(())
    }

    /// Returns the record whatever its state; expiry and revocation are the caller's checks
    /// (the caller maps them to distinct, logged reasons).
    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<ApiToken>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("WHERE token_hash = $1"))
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read token", e))?
            .map(ApiToken::from))
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<ApiToken>, StoreError> {
        Ok(
            sqlx::query_as::<_, Row>(select!("WHERE user_id = $1 ORDER BY created_at, id"))
                .bind(user_id.as_uuid())
                .fetch_all(&self.pool)
                .await
                .map_err(|e| map_err("list tokens", e))?
                .into_iter()
                .map(ApiToken::from)
                .collect(),
        )
    }

    /// Keeps the first revocation time if the token is already revoked, and announces the
    /// change so other nodes drop a principal cached for this token.
    async fn revoke(&self, id: ApiTokenId, at: OffsetDateTime) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("revoke token", e))?;
        let owner: Option<Uuid> = sqlx::query_scalar(
            "UPDATE user_tokens SET revoked_at = COALESCE(revoked_at, $2) WHERE id = $1 RETURNING user_id",
        )
        .bind(id.as_uuid())
        .bind(at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("revoke token", e))?;
        let Some(owner) = owner else {
            return Err(StoreError::NotFound("token".to_owned()));
        };
        notify(&mut tx, &InvalidationEvent::User(owner)).await?;
        tx.commit().await.map_err(|e| map_err("revoke token", e))
    }

    /// Coalesced: no write when the stored time is within a minute of `at` (or newer). An
    /// unknown id is not an error either, the call is a hint, not a state change.
    async fn touch_last_used(&self, id: ApiTokenId, at: OffsetDateTime) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE user_tokens SET last_used_at = $2 WHERE id = $1 \
             AND (last_used_at IS NULL OR last_used_at < $2 - make_interval(secs => $3))",
        )
        .bind(id.as_uuid())
        .bind(at)
        .bind(LAST_USED_COALESCE_SECS)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("touch token", e))?;
        Ok(())
    }
}
