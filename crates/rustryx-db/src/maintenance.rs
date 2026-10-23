//! Periodic maintenance that belongs to the schema: audit partitions and stale counters.
//!
//! The functions are cheap and idempotent. A scheduler calls [`run_audit_maintenance`] under a
//! distributed lock; nothing here needs to run on exactly one node, the SQL functions
//! serialise themselves with an advisory transaction lock.

use rustryx_core::StoreError;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::error::map_err;

/// Default months of partitions created ahead of the current one.
pub const DEFAULT_LOOKAHEAD_MONTHS: u32 = 2;
/// Default retention of audit partitions, in days.
pub const DEFAULT_RETENTION_DAYS: u32 = 90;

/// Audit partition policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditMaintenance {
    /// Partitions are created for the current month and this many following months.
    pub lookahead_months: u32,
    /// A partition is dropped once its whole month is older than this many days.
    pub retention_days: u32,
}

impl Default for AuditMaintenance {
    fn default() -> Self {
        Self {
            lookahead_months: DEFAULT_LOOKAHEAD_MONTHS,
            retention_days: DEFAULT_RETENTION_DAYS,
        }
    }
}

/// What one maintenance pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditMaintenanceReport {
    /// Partitions created.
    pub created: u32,
    /// Partitions dropped.
    pub dropped: u32,
}

fn to_count(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// Creates the partition of the month containing `now` and `lookahead_months` after it.
/// Returns how many were created. Calls the SQL function of the same name.
pub async fn ensure_audit_partitions(
    pool: &PgPool,
    now: OffsetDateTime,
    lookahead_months: u32,
) -> Result<u32, StoreError> {
    let created: i32 = sqlx::query_scalar("SELECT ensure_audit_partitions($1, $2)")
        .bind(now)
        .bind(i32::try_from(lookahead_months).unwrap_or(i32::MAX))
        .fetch_one(pool)
        .await
        .map_err(|e| map_err("ensure audit partitions", e))?;
    Ok(to_count(created))
}

/// Drops partitions whose whole month is older than `retention_days`. Archive them first if
/// the retention policy requires it: a dropped partition is gone.
pub async fn drop_expired_audit_partitions(
    pool: &PgPool,
    now: OffsetDateTime,
    retention_days: u32,
) -> Result<u32, StoreError> {
    let dropped: i32 = sqlx::query_scalar("SELECT drop_expired_audit_partitions($1, $2)")
        .bind(now)
        .bind(i32::try_from(retention_days).unwrap_or(i32::MAX))
        .fetch_one(pool)
        .await
        .map_err(|e| map_err("drop expired audit partitions", e))?;
    Ok(to_count(dropped))
}

/// Creates the lookahead first, then applies retention, so a failure of the second step never
/// leaves the table without a partition for the next event.
pub async fn run_audit_maintenance(
    pool: &PgPool,
    policy: AuditMaintenance,
) -> Result<AuditMaintenanceReport, StoreError> {
    let now = OffsetDateTime::now_utc();
    let created = ensure_audit_partitions(pool, now, policy.lookahead_months).await?;
    let dropped = drop_expired_audit_partitions(pool, now, policy.retention_days).await?;
    Ok(AuditMaintenanceReport { created, dropped })
}

/// Deletes login counters whose last failure is older than `older_than`. A counter that old
/// no longer locks anyone out (the longest lockout is 30 minutes), so it is only dead weight.
pub async fn purge_login_attempts(
    pool: &PgPool,
    older_than: OffsetDateTime,
) -> Result<u64, StoreError> {
    let result = sqlx::query("DELETE FROM login_attempts WHERE last_failure_at < $1")
        .bind(older_than)
        .execute(pool)
        .await
        .map_err(|e| map_err("purge login attempts", e))?;
    Ok(result.rows_affected())
}
