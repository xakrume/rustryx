//! [`AuditSink`] writing to the monthly partitioned `audit_events` table.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::audit::AuditEvent;
use rustryx_core::ports::AuditSink;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_err;
use crate::maintenance;

/// Audit sink on `PostgreSQL`.
#[derive(Debug, Clone)]
pub struct PgAuditSink {
    pool: PgPool,
}

impl PgAuditSink {
    /// Creates the sink over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// `detail` is documented as a JSON object; anything else is wrapped so the column keeps one
/// shape and queries over `detail ->> 'x'` stay valid.
fn detail_object(detail: &serde_json::Value) -> serde_json::Value {
    match detail {
        serde_json::Value::Object(_) => detail.clone(),
        serde_json::Value::Null => serde_json::json!({}),
        other => serde_json::json!({ "value": other }),
    }
}

async fn insert(pool: &PgPool, event: &AuditEvent) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_events (id, event_time, kind, actor_id, actor_name, client_ip, \
         request_id, action, target, outcome, detail, actor_kind, project_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(Uuid::now_v7())
    .bind(event.at)
    // `as_str` is exhaustive inside core, so a kind added there is stored without a change
    // here (the column is plain text).
    .bind(event.kind.as_str())
    .bind(event.actor_id.map(|id| id.as_uuid()))
    .bind(&event.actor_name)
    .bind(&event.client_ip)
    .bind(&event.request_id)
    .bind(&event.action)
    .bind(&event.target)
    .bind(&event.outcome)
    .bind(detail_object(&event.detail))
    .bind(event.actor_kind.as_str())
    .bind(event.project_id.map(|id| id.as_uuid()))
    .execute(pool)
    .await
    .map(|_| ())
}

/// `check_violation` is what a row without a partition raises ("no partition of relation
/// found for row"); `audit_events` has no other check constraint.
fn is_missing_partition(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23514")
}

#[async_trait]
impl AuditSink for PgAuditSink {
    /// A missing partition is created on the spot and the insert retried once. The
    /// maintenance job normally keeps two months ahead, but an audit write that fails because
    /// that job was late would lose the very event that matters most.
    async fn record(&self, event: AuditEvent) -> Result<(), StoreError> {
        match insert(&self.pool, &event).await {
            Ok(()) => Ok(()),
            Err(err) if is_missing_partition(&err) => {
                maintenance::ensure_audit_partitions(&self.pool, event.at, 0).await?;
                insert(&self.pool, &event)
                    .await
                    .map_err(|e| map_err("record audit event", e))
            }
            Err(err) => Err(map_err("record audit event", err)),
        }
    }
}
