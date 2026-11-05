//! [`BlobRepo`] on `PostgreSQL`: blob metadata and garbage collection support.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{BlobId, BlobStoreId, RepositoryId};
use rustryx_core::model::BlobRecord;
use rustryx_core::ports::BlobRepo;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::common::{to_i64, to_u64};
use super::rows::BlobRow;
use crate::error::map_err;

/// Rows a GC listing may return at most, whatever the caller asks for.
const MAX_UNREFERENCED_PAGE: u32 = 10_000;

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

    async fn count_by_store(&self, id: BlobStoreId) -> Result<u64, StoreError> {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM blobs WHERE blob_store_id = $1",
        )
        .bind(id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("count store blobs", e))?;
        to_u64(total, "count store blobs")
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
        list_unreferenced(&self.pool, None, older_than, limit).await
    }

    async fn list_unreferenced_in(
        &self,
        store: BlobStoreId,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        list_unreferenced(&self.pool, Some(store), older_than, limit).await
    }

    /// `Conflict` when the id or the `(store, key)` pair exists, or the store does not exist.
    /// The row starts unreferenced, so its age for GC runs from `created_at` until an asset
    /// links to it.
    async fn insert(&self, record: &BlobRecord) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO blobs (id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(record.id.as_uuid())
        .bind(record.blob_store_id.as_uuid())
        .bind(&record.key)
        .bind(to_i64(record.size_bytes)?)
        .bind(&record.checksums.sha1)
        .bind(&record.checksums.sha256)
        .bind(&record.checksums.md5)
        .bind(record.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("insert blob", e))?;
        Ok(())
    }

    /// Row only and NOT safe for garbage collection: use `delete_if_unreferenced`, which closes
    /// the race with `AssetRepo::link`. Idempotent: a second pass (or a second node) deleting
    /// the same row succeeds. A blob that an asset still references cannot be deleted
    /// (`Conflict`, foreign key).
    async fn delete(&self, id: BlobId) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM blobs WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete blob", e))?;
        Ok(())
    }

    /// The GC sweep step. Row first, object second, and the row is decided under its lock:
    ///
    /// 1. `SELECT .. FOR UPDATE` takes the row lock. `AssetRepo::link` takes a lock on the same
    ///    row before it inserts its asset (and the foreign key check of any other insert
    ///    takes a share lock), so a reference that is being created either committed before
    ///    this lock was granted, in which case step 2 sees it, or it waits until this
    ///    transaction ends and then finds the row gone (`NotFound`).
    /// 2. The reference and age checks are separate statements on purpose: every statement
    ///    of a `READ COMMITTED` transaction takes a fresh snapshot, so after the lock wait the
    ///    check sees the winner's commit. A single `DELETE .. WHERE NOT EXISTS` would evaluate
    ///    the subquery on the snapshot of its own start and could miss a link committed while
    ///    it waited for the row.
    /// 3. The row is deleted and committed. The caller deletes the object only after `true`.
    ///
    /// A crash between the commit and the object delete leaves an orphan object, which the
    /// store sweep of `verify_blobs` finds; the reverse order would leave an asset that points
    /// at nothing.
    async fn delete_if_unreferenced(
        &self,
        id: BlobId,
        older_than: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("begin blob sweep", e))?;
        let locked: Option<bool> = sqlx::query_scalar(
            "SELECT COALESCE(orphaned_at, created_at) < $2 FROM blobs WHERE id = $1 FOR UPDATE",
        )
        .bind(id.as_uuid())
        .bind(older_than)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("lock blob", e))?;
        if locked != Some(true) {
            return Ok(false);
        }
        let referenced: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assets WHERE blob_id = $1)")
                .bind(id.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| map_err("check blob references", e))?;
        if referenced {
            return Ok(false);
        }
        sqlx::query("DELETE FROM blobs WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete swept blob", e))?;
        tx.commit()
            .await
            .map_err(|e| map_err("commit blob sweep", e))?;
        Ok(true)
    }
}

async fn list_unreferenced(
    pool: &PgPool,
    store: Option<BlobStoreId>,
    older_than: OffsetDateTime,
    limit: u32,
) -> Result<Vec<BlobRecord>, StoreError> {
    sqlx::query_as::<_, BlobRow>(
        "SELECT b.id, b.blob_store_id, b.key, b.size_bytes, b.sha1, b.sha256, b.md5, b.created_at \
         FROM blobs b \
         WHERE NOT EXISTS (SELECT 1 FROM assets a WHERE a.blob_id = b.id) \
           AND COALESCE(b.orphaned_at, b.created_at) < $1 \
           AND ($3::uuid IS NULL OR b.blob_store_id = $3) \
         ORDER BY COALESCE(b.orphaned_at, b.created_at), b.id \
         LIMIT $2",
    )
    .bind(older_than)
    .bind(i64::from(limit.min(MAX_UNREFERENCED_PAGE)))
    .bind(store.map(|s| s.as_uuid()))
    .fetch_all(pool)
    .await
    .map_err(|e| map_err("list unreferenced blobs", e))?
    .into_iter()
    .map(BlobRow::into_record)
    .collect()
}
