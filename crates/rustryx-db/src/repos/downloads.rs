//! Daily download counters.

use rustryx_core::error::StoreError;
use rustryx_core::ids::AssetId;
use rustryx_core::ports::DownloadStatsRepo;
use sqlx::PgPool;
use time::Date;

use super::common::to_u64;
use crate::error::map_err;

/// Most days one read returns. Older counters stay in the table until the partition ages out.
const MAX_STATS_DAYS: u32 = 90;

/// Read side of `asset_downloads_daily`.
#[derive(Clone)]
pub struct PgDownloadStatsRepo {
    pool: PgPool,
}

impl PgDownloadStatsRepo {
    /// A repo on `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl DownloadStatsRepo for PgDownloadStatsRepo {
    async fn daily(&self, asset: AssetId, days: u32) -> Result<Vec<(Date, u64)>, StoreError> {
        let days = days.min(MAX_STATS_DAYS);
        if days == 0 {
            return Ok(Vec::new());
        }
        let rows: Vec<(Date, i64)> = sqlx::query_as(
            "SELECT d::date, coalesce(c.count, 0)::bigint \
             FROM generate_series( \
                 (now() AT TIME ZONE 'UTC')::date - ($1::int - 1), \
                 (now() AT TIME ZONE 'UTC')::date, \
                 interval '1 day') AS d \
             LEFT JOIN asset_downloads_daily c \
               ON c.asset_id = $2 AND c.day = d::date \
             ORDER BY d",
        )
        .bind(i32::try_from(days).unwrap_or(i32::MAX))
        .bind(asset.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read download stats", e))?;
        rows.into_iter()
            .map(|(day, count)| Ok((day, to_u64(count, "read download stats")?)))
            .collect()
    }
}
