//! [`BlobRepo`] on `PostgreSQL`: blob metadata and garbage collection support.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{BlobId, BlobStoreId, RepositoryId};
use rustryx_core::model::BlobRecord;
use rustryx_core::ports::BlobRepo;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::common::to_u64;
use super::rows::BlobRow;
use crate::error::map_err;

/// Blob metadata.
#[derive(Debug, Clone)]
pub struct PgBlobRepo {
    pool: PgPool,
}

impl PgBlobRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl BlobRepo for PgBlobRepo {
    async fn get(&self, id: BlobId) -> Result<Option<BlobRecord>, StoreError> {
        sqlx::query_as::<_, BlobRow>(
            "SELECT id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at \
             FROM blobs WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read blob", e))?
        .map(BlobRow::into_record)
        .transpose()
    }

    async fn used_bytes_by_repository(&self, id: RepositoryId) -> Result<u64, StoreError> {
        // DISTINCT blobs: two assets sharing one blob occupy the space once.
        let total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(b.size_bytes), 0)::bigint FROM blobs b \
             WHERE b.id IN (SELECT a.blob_id FROM assets a WHERE a.repository_id = $1)",
        )
        .bind(id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("sum repository usage", e))?;
        to_u64(total, "sum repository usage")
    }

    async fn used_bytes_by_store(&self, id: BlobStoreId) -> Result<u64, StoreError> {
        let total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(size_bytes), 0)::bigint FROM blobs WHERE blob_store_id = $1",
        )
        .bind(id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("sum store usage", e))?;
        to_u64(total, "sum store usage")
    }

    /// Age is measured from the moment the blob lost its last reference (`orphaned_at`),
    /// falling back to its creation time for a blob that was never referenced. A replaced
    /// blob may still be streaming to a client; its own, older creation time says nothing
    /// about that.
    async fn list_unreferenced(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        sqlx::query_as::<_, BlobRow>(
            "SELECT b.id, b.blob_store_id, b.key, b.size_bytes, b.sha1, b.sha256, b.md5, b.created_at \
             FROM blobs b \
             WHERE NOT EXISTS (SELECT 1 FROM assets a WHERE a.blob_id = b.id) \
               AND COALESCE(b.orphaned_at, b.created_at) < $1 \
             ORDER BY COALESCE(b.orphaned_at, b.created_at), b.id \
             LIMIT $2",
        )
        .bind(older_than)
        .bind(i64::from(limit.min(10_000)))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list unreferenced blobs", e))?
        .into_iter()
        .map(BlobRow::into_record)
        .collect()
    }

    /// Idempotent: a second GC pass (or a second node) deleting the same row succeeds. A
    /// blob that an asset still references cannot be deleted (`Conflict`, foreign key).
    async fn delete(&self, id: BlobId) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM blobs WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete blob", e))?;
        Ok(())
    }
}
