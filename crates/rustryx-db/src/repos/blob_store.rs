//! [`BlobStoreRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::BlobStoreRepo;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, to_i64, to_u64};
use crate::error::map_err;

/// Static `SELECT` with a literal tail. `concat!` keeps the text `'static`, which is what
/// `sqlx` 0.9 demands of anything that is not explicitly asserted safe.
macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, name, kind, config, quota_bytes, created_at FROM blob_stores ",
            $($tail),*
        )
    };
}

/// Blob store definitions.
#[derive(Debug, Clone)]
pub struct PgBlobStoreRepo {
    pool: PgPool,
}

impl PgBlobStoreRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    name: String,
    kind: String,
    config: serde_json::Value,
    quota_bytes: Option<i64>,
    created_at: OffsetDateTime,
}

const fn kind_label(kind: BlobStoreKind) -> &'static str {
    match kind {
        BlobStoreKind::Local => "local",
        BlobStoreKind::S3 => "s3",
    }
}

impl Row {
    fn into_def(self) -> Result<BlobStoreDef, StoreError> {
        let kind = match self.kind.as_str() {
            "local" => BlobStoreKind::Local,
            "s3" => BlobStoreKind::S3,
            _ => return Err(corrupt("read blob store")),
        };
        Ok(BlobStoreDef {
            id: BlobStoreId::from_uuid(self.id),
            name: self.name,
            kind,
            config: self.config,
            quota_bytes: self
                .quota_bytes
                .map(|q| to_u64(q, "read blob store"))
                .transpose()?,
            created_at: self.created_at,
        })
    }
}

#[async_trait]
impl BlobStoreRepo for PgBlobStoreRepo {
    async fn create(&self, store: &BlobStoreDef) -> Result<(), StoreError> {
        let quota = store.quota_bytes.map(to_i64).transpose()?;
        sqlx::query(
            "INSERT INTO blob_stores (id, name, kind, config, quota_bytes, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(store.id.as_uuid())
        .bind(&store.name)
        .bind(kind_label(store.kind))
        .bind(&store.config)
        .bind(quota)
        .bind(store.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create blob store", e))?;
        Ok(())
    }

    async fn get(&self, id: BlobStoreId) -> Result<Option<BlobStoreDef>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read blob store", e))?
            .map(Row::into_def)
            .transpose()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<BlobStoreDef>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE name = $1"))
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read blob store", e))?
            .map(Row::into_def)
            .transpose()
    }

    async fn list(&self) -> Result<Vec<BlobStoreDef>, StoreError> {
        sqlx::query_as::<_, Row>(select!("ORDER BY name"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list blob stores", e))?
            .into_iter()
            .map(Row::into_def)
            .collect()
    }

    /// `Conflict` while a repository or a blob still refers to the store (foreign keys are
    /// `RESTRICT`); `NotFound` when absent.
    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM blob_stores WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete blob store", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("blob store".to_owned()));
        }
        Ok(())
    }
}
