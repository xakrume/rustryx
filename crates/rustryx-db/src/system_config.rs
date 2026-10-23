//! Key/value runtime configuration stored in the database.

use rustryx_core::StoreError;
use sqlx::PgPool;

use crate::error::map_err;

/// `system_config` accessor: small JSON values that operators change at runtime (for example
/// the global anonymous-access switch) without a redeploy.
#[derive(Debug, Clone)]
pub struct PgSystemConfig {
    pool: PgPool,
}

impl PgSystemConfig {
    /// Creates the accessor over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Value of `key`, or `None`.
    pub async fn get(&self, key: &str) -> Result<Option<serde_json::Value>, StoreError> {
        sqlx::query_scalar("SELECT value FROM system_config WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read system config", e))
    }

    /// Creates or replaces `key`.
    pub async fn set(&self, key: &str, value: &serde_json::Value) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO system_config (key, value) VALUES ($1, $2) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("write system config", e))?;
        Ok(())
    }

    /// Removes `key`. Removing a missing key succeeds.
    pub async fn delete(&self, key: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM system_config WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete system config", e))?;
        Ok(())
    }
}
