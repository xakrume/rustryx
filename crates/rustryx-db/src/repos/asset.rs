//! [`AssetRepo`] on `PostgreSQL`.

use std::collections::HashMap;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{AssetId, ComponentId, RepositoryId};
use rustryx_core::model::{Asset, BlobRecord, NewAsset, PublishOutcome};
use rustryx_core::ports::{AssetRepo, Page, PageRequest};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{escape_like, to_i64};
use super::rows::{AssetRow, BlobRow, asset_select};
use crate::cursor;
use crate::error::map_err;

const KIND: &str = "asset";

/// Assets.
#[derive(Debug, Clone)]
pub struct PgAssetRepo {
    pool: PgPool,
}

impl PgAssetRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Upper bound of publish attempts when a concurrent publisher keeps winning the insert.
const PUBLISH_ATTEMPTS: u32 = 5;

async fn insert_blob(conn: &mut PgConnection, new: &NewAsset) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO blobs (id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(new.blob.id.as_uuid())
    .bind(new.blob.blob_store_id.as_uuid())
    .bind(&new.blob.key)
    .bind(to_i64(new.blob.size_bytes)?)
    .bind(&new.blob.checksums.sha1)
    .bind(&new.blob.checksums.sha256)
    .bind(&new.blob.checksums.md5)
    .bind(new.blob.created_at)
    .execute(conn)
    .await
    .map_err(|e| map_err("insert blob", e))?;
    Ok(())
}

/// Points the asset at the new blob, creating it when the path is new. Returns the asset id
/// and the blob it displaced.
///
/// `SELECT .. FOR UPDATE` first, then `UPDATE` or `INSERT .. ON CONFLICT DO NOTHING`:
/// - a concurrent publisher of the same path blocks on the row lock and, when it wakes,
///   reads the row the winner committed, so each publisher reports exactly the blob IT
///   displaced (a single `INSERT .. ON CONFLICT DO UPDATE` cannot return the old value);
/// - for a new path nothing can be locked, so two publishers may both reach the insert; the
///   loser's `DO NOTHING` returns no row and the loop retries into the update branch.
async fn upsert_asset(
    conn: &mut PgConnection,
    new: &NewAsset,
) -> Result<(Uuid, Option<Uuid>), StoreError> {
    for _ in 0..PUBLISH_ATTEMPTS {
        let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, blob_id FROM assets WHERE repository_id = $1 AND path = $2 FOR UPDATE",
        )
        .bind(new.repository_id.as_uuid())
        .bind(&new.path)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| map_err("lock asset", e))?;
        if let Some((id, old_blob)) = existing {
            sqlx::query(
                "UPDATE assets SET blob_id = $2, content_type = $3, component_id = $4, \
                 updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(new.blob.id.as_uuid())
            .bind(&new.content_type)
            .bind(new.component_id.map(|c| c.as_uuid()))
            .execute(&mut *conn)
            .await
            .map_err(|e| map_err("switch asset blob", e))?;
            return Ok((id, Some(old_blob)));
        }
        let id = AssetId::generate().as_uuid();
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO assets (id, repository_id, component_id, path, blob_id, content_type) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT ON CONSTRAINT uq_assets_repo_path DO NOTHING RETURNING id",
        )
        .bind(id)
        .bind(new.repository_id.as_uuid())
        .bind(new.component_id.map(|c| c.as_uuid()))
        .bind(&new.path)
        .bind(new.blob.id.as_uuid())
        .bind(&new.content_type)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| map_err("insert asset", e))?;
        if let Some(id) = inserted {
            return Ok((id, None));
        }
    }
    Err(StoreError::Unavailable(
        "publish kept losing a race, retry".to_owned(),
    ))
}

#[async_trait]
impl AssetRepo for PgAssetRepo {
    async fn get_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<Asset>, StoreError> {
        sqlx::query_as::<_, AssetRow>(asset_select!("WHERE a.repository_id = $1 AND a.path = $2"))
            .bind(repository_id.as_uuid())
            .bind(path)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read asset", e))?
            .map(AssetRow::into_asset)
            .transpose()
    }

    async fn get(&self, id: AssetId) -> Result<Option<Asset>, StoreError> {
        sqlx::query_as::<_, AssetRow>(asset_select!("WHERE a.id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read asset", e))?
            .map(AssetRow::into_asset)
            .transpose()
    }

    /// One transaction: insert the blob row, create or switch the asset. A failure anywhere
    /// rolls the blob row back too, so no unreferenced row is left by a failed publish, and
    /// a cancelled future leaves the old state (dropping an open `sqlx` transaction rolls it
    /// back).
    async fn publish(&self, new: &NewAsset) -> Result<PublishOutcome, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("begin publish", e))?;
        insert_blob(&mut tx, new).await?;
        let (asset_id, replaced_blob_id) = upsert_asset(&mut tx, new).await?;
        let replaced = match replaced_blob_id {
            Some(blob_id) => sqlx::query_as::<_, BlobRow>(
                "SELECT id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at \
                 FROM blobs WHERE id = $1",
            )
            .bind(blob_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| map_err("read replaced blob", e))?
            .map(BlobRow::into_record)
            .transpose()?,
            None => None,
        };
        let asset = sqlx::query_as::<_, AssetRow>(asset_select!("WHERE a.id = $1"))
            .bind(asset_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_err("read published asset", e))?
            .into_asset()?;
        tx.commit()
            .await
            .map_err(|e| map_err("commit publish", e))?;
        Ok(PublishOutcome { asset, replaced })
    }

    /// Ordered by path in byte order (`COLLATE "C"`), which is also the keyset order.
    async fn list(
        &self,
        repository_id: RepositoryId,
        prefix: &str,
        page: &PageRequest,
    ) -> Result<Page<Asset>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let mut builder = QueryBuilder::<Postgres>::new(asset_select!());
        builder
            .push(" WHERE a.repository_id = ")
            .push_bind(repository_id.as_uuid());
        if !prefix.is_empty() {
            builder
                .push(" AND a.path LIKE ")
                .push_bind(format!("{}%", escape_like(prefix)));
            builder.push(" ESCAPE '\\'");
        }
        if let Some(after) = &page.after {
            builder
                .push(" AND a.path > ")
                .push_bind(cursor::decode(KIND, after)?);
        }
        builder.push(" ORDER BY a.path LIMIT ").push_bind(fetch);
        let rows = builder
            .build_query_as::<AssetRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list assets", e))?;
        let items = rows
            .into_iter()
            .map(AssetRow::into_asset)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, KIND, |a| a.path.clone()))
    }

    async fn list_by_component(&self, id: ComponentId) -> Result<Vec<Asset>, StoreError> {
        let mut builder = QueryBuilder::<Postgres>::new(asset_select!());
        builder
            .push(" WHERE a.component_id = ")
            .push_bind(id.as_uuid());
        builder.push(" ORDER BY a.path");
        builder
            .build_query_as::<AssetRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list component assets", e))?
            .into_iter()
            .map(AssetRow::into_asset)
            .collect()
    }

    /// Returns the blob only when no other asset references it any more, so GC candidates are
    /// exactly the blobs that lost their last reference.
    async fn delete_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<BlobRecord>, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("begin delete asset", e))?;
        let deleted: Option<Uuid> = sqlx::query_scalar(
            "DELETE FROM assets WHERE repository_id = $1 AND path = $2 RETURNING blob_id",
        )
        .bind(repository_id.as_uuid())
        .bind(path)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("delete asset", e))?;
        let Some(blob_id) = deleted else {
            return Ok(None);
        };
        let record = sqlx::query_as::<_, BlobRow>(
            "SELECT b.id, b.blob_store_id, b.key, b.size_bytes, b.sha1, b.sha256, b.md5, b.created_at \
             FROM blobs b WHERE b.id = $1 \
               AND NOT EXISTS (SELECT 1 FROM assets a WHERE a.blob_id = b.id)",
        )
        .bind(blob_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("read freed blob", e))?
        .map(BlobRow::into_record)
        .transpose()?;
        tx.commit()
            .await
            .map_err(|e| map_err("commit delete asset", e))?;
        Ok(record)
    }

    /// One `UPDATE .. FROM unnest(..)` for the whole batch. Duplicate ids in the batch are
    /// reduced to the newest time first: `UPDATE .. FROM` with several source rows for one
    /// target row applies an arbitrary one of them. A time older than the stored one never
    /// moves `last_downloaded_at` backwards.
    async fn touch_downloaded(
        &self,
        batch: &[(AssetId, OffsetDateTime)],
    ) -> Result<(), StoreError> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut newest: HashMap<Uuid, OffsetDateTime> = HashMap::with_capacity(batch.len());
        for (id, at) in batch {
            newest
                .entry(id.as_uuid())
                .and_modify(|current| {
                    if *at > *current {
                        *current = *at;
                    }
                })
                .or_insert(*at);
        }
        let (ids, times): (Vec<Uuid>, Vec<OffsetDateTime>) = newest.into_iter().unzip();
        sqlx::query(
            "UPDATE assets a SET last_downloaded_at = v.at \
             FROM unnest($1::uuid[], $2::timestamptz[]) AS v(id, at) \
             WHERE a.id = v.id AND (a.last_downloaded_at IS NULL OR a.last_downloaded_at < v.at)",
        )
        .bind(&ids)
        .bind(&times)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("record downloads", e))?;
        Ok(())
    }
}
