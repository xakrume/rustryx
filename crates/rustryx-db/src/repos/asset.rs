//! [`AssetRepo`] on `PostgreSQL`: reads, listings and the batched statistics. The state
//! changing transactions (publish, link, delete) are in [`super::asset_write`].

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::error::MutationError;
use rustryx_core::ids::{AssetId, ComponentId, RepositoryId};
use rustryx_core::model::{
    Asset, BlobRecord, DownloadTick, NewAsset, NewAssetLink, PublishOutcome, WriteOptions,
};
use rustryx_core::ports::{AssetRepo, Page, PageRequest};
use sqlx::{PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use super::asset_write;
use super::common::escape_like;
use super::rows::{AssetRow, asset_select};
use crate::cursor;
use crate::error::map_err;

const KIND: &str = "asset";

/// How far a stored `last_downloaded_at` must lag a new download before it is rewritten.
///
/// A hot artifact downloaded every second would otherwise be rewritten by every flush, one
/// dead tuple per flush per asset for a statistic whose consumers (retention by last pull,
/// the "last downloaded" column) work in days. A minute keeps the column honest to the
/// minute and cuts the writes of a hot asset to one per minute.
pub const DEFAULT_DOWNLOAD_WRITE_INTERVAL: Duration = Duration::from_secs(60);

/// Assets.
#[derive(Debug, Clone)]
pub struct PgAssetRepo {
    pool: PgPool,
    download_write_interval: Duration,
}

impl PgAssetRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            download_write_interval: DEFAULT_DOWNLOAD_WRITE_INTERVAL,
        }
    }

    /// Sets how far a stored download time must lag before `record_downloads` rewrites it
    /// (default [`DEFAULT_DOWNLOAD_WRITE_INTERVAL`]; zero rewrites on every newer time).
    #[must_use]
    pub fn with_download_write_interval(mut self, interval: Duration) -> Self {
        self.download_write_interval = interval;
        self
    }
}

/// The newest time per asset: `UPDATE .. FROM` with several source rows for one target row
/// applies an arbitrary one of them.
fn newest_per_asset(
    ticks: impl Iterator<Item = (AssetId, OffsetDateTime)>,
) -> (Vec<Uuid>, Vec<OffsetDateTime>) {
    let mut newest: HashMap<Uuid, OffsetDateTime> = HashMap::new();
    for (id, at) in ticks {
        newest
            .entry(id.as_uuid())
            .and_modify(|current| {
                if at > *current {
                    *current = at;
                }
            })
            .or_insert(at);
    }
    let mut pairs: Vec<(Uuid, OffsetDateTime)> = newest.into_iter().collect();
    // Ascending ids: the lock order of every flush, see `record_downloads`.
    pairs.sort_unstable_by_key(|(id, _)| *id);
    pairs.into_iter().unzip()
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

    async fn publish_with(
        &self,
        new: &NewAsset,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        asset_write::publish(&self.pool, new, options).await
    }

    async fn link(
        &self,
        link: &NewAssetLink,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        asset_write::link(&self.pool, link, options).await
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

    /// The phase 1 delete is the typed one with no options: a guard refuses it as a `Conflict`.
    /// Returns the blob only when no other asset references it any more, so GC candidates are
    /// exactly the blobs that lost their last reference.
    async fn delete_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<BlobRecord>, StoreError> {
        asset_write::delete_by_path(&self.pool, repository_id, path, &WriteOptions::new())
            .await
            .map_err(MutationError::into_store_error)
    }

    async fn delete_by_path_checked(
        &self,
        repository_id: RepositoryId,
        path: &str,
        options: &WriteOptions,
    ) -> Result<Option<BlobRecord>, MutationError> {
        asset_write::delete_by_path(&self.pool, repository_id, path, options).await
    }

    /// One `UPDATE .. FROM unnest(..)` for the whole batch, a time older than the stored one
    /// never moves `last_downloaded_at` backwards.
    async fn touch_downloaded(
        &self,
        batch: &[(AssetId, OffsetDateTime)],
    ) -> Result<(), StoreError> {
        if batch.is_empty() {
            return Ok(());
        }
        let (ids, times) = newest_per_asset(batch.iter().copied());
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

    /// Moves `updated_at` forward only: a stale revalidation (an older `at`) never rewinds the
    /// freshness of an entry another node refreshed meanwhile. The common case is one
    /// statement; the existence read happens only when nothing was updated.
    async fn touch_fresh(&self, id: AssetId, at: OffsetDateTime) -> Result<(), StoreError> {
        let updated =
            sqlx::query("UPDATE assets SET updated_at = $2 WHERE id = $1 AND updated_at < $2")
                .bind(id.as_uuid())
                .bind(at)
                .execute(&self.pool)
                .await
                .map_err(|e| map_err("touch asset", e))?;
        if updated.rows_affected() > 0 {
            return Ok(());
        }
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assets WHERE id = $1)")
            .bind(id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("touch asset", e))?;
        if exists {
            Ok(())
        } else {
            Err(StoreError::NotFound("asset".to_owned()))
        }
    }

    /// Sets `last_downloaded_at` of the assets and of their components, in one transaction,
    /// only where the stored time lags the new one by more than the write interval.
    ///
    /// Row locks are taken first, in ascending id order, by a materialized
    /// `SELECT .. FOR UPDATE .. ORDER BY id`: two nodes flushing overlapping sets then wait
    /// for each other instead of deadlocking (an `UPDATE .. FROM` locks in the order of its
    /// join, which differs between plans). Counts of assets that still exist are added to
    /// `asset_downloads_daily` in the same transaction, keyed by the UTC date of each tick.
    async fn record_downloads(&self, batch: &[DownloadTick]) -> Result<(), StoreError> {
        if batch.is_empty() {
            return Ok(());
        }
        let (ids, times) = newest_per_asset(batch.iter().map(|t| (t.asset_id, t.at)));
        let lag = self.download_write_interval.as_secs_f64();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("record downloads", e))?;
        sqlx::query(
            "WITH locked AS MATERIALIZED ( \
                 SELECT id FROM assets WHERE id = ANY($1) ORDER BY id FOR UPDATE) \
             UPDATE assets a SET last_downloaded_at = v.at \
             FROM unnest($1::uuid[], $2::timestamptz[]) AS v(id, at) \
             WHERE a.id = v.id AND a.id IN (SELECT id FROM locked) \
               AND (a.last_downloaded_at IS NULL \
                    OR a.last_downloaded_at < v.at - make_interval(secs => $3))",
        )
        .bind(&ids)
        .bind(&times)
        .bind(lag)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("record asset downloads", e))?;
        sqlx::query(
            "WITH newest AS ( \
                 SELECT a.component_id AS id, max(v.at) AS at \
                 FROM unnest($1::uuid[], $2::timestamptz[]) AS v(id, at) \
                 JOIN assets a ON a.id = v.id WHERE a.component_id IS NOT NULL \
                 GROUP BY a.component_id), \
             locked AS MATERIALIZED ( \
                 SELECT c.id FROM components c WHERE c.id IN (SELECT id FROM newest) \
                 ORDER BY c.id FOR UPDATE) \
             UPDATE components c SET last_downloaded_at = n.at \
             FROM newest n \
             WHERE c.id = n.id AND c.id IN (SELECT id FROM locked) \
               AND (c.last_downloaded_at IS NULL \
                    OR c.last_downloaded_at < n.at - make_interval(secs => $3))",
        )
        .bind(&ids)
        .bind(&times)
        .bind(lag)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("record component downloads", e))?;
        let (tick_ids, tick_at, tick_count) = download_ticks(batch);
        let earliest = tick_at.iter().copied().min();
        if let Some(at) = earliest {
            sqlx::query("SELECT ensure_download_partitions($1, 1)")
                .bind(at)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("ensure download partitions", e))?;
        }
        sqlx::query(
            "INSERT INTO asset_downloads_daily (asset_id, day, count) \
             SELECT v.id, (v.at AT TIME ZONE 'UTC')::date, sum(v.count)::bigint \
             FROM unnest($1::uuid[], $2::timestamptz[], $3::integer[]) AS v(id, at, count) \
             JOIN assets a ON a.id = v.id \
             GROUP BY v.id, (v.at AT TIME ZONE 'UTC')::date \
             HAVING sum(v.count) > 0 \
             ON CONFLICT (asset_id, day) DO UPDATE \
             SET count = asset_downloads_daily.count + EXCLUDED.count",
        )
        .bind(&tick_ids)
        .bind(&tick_at)
        .bind(&tick_count)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("record download counts", e))?;
        tx.commit()
            .await
            .map_err(|e| map_err("record downloads", e))
    }
}

fn download_ticks(batch: &[DownloadTick]) -> (Vec<Uuid>, Vec<OffsetDateTime>, Vec<i32>) {
    let mut ids = Vec::with_capacity(batch.len());
    let mut times = Vec::with_capacity(batch.len());
    let mut counts = Vec::with_capacity(batch.len());
    for tick in batch {
        ids.push(tick.asset_id.as_uuid());
        times.push(tick.at);
        counts.push(i32::try_from(tick.count).unwrap_or(i32::MAX));
    }
    (ids, times, counts)
}
