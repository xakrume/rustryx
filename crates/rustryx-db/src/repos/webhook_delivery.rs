//! Webhook deliveries. One row per webhook and event. A lost lease can be claimed again.

use std::time::Duration;

use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventKind};
use rustryx_core::ids::{EventId, WebhookDeliveryId, WebhookId};
use rustryx_core::ports::{DeliveryOutcome, Page, PageRequest, WebhookDeliveryRepo};
use rustryx_core::webhook::{DeliveryStatus, WebhookDelivery};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{backend, map_err};
use crate::outbox;

/// Webhook deliveries.
#[derive(Clone)]
pub struct PgWebhookDeliveryRepo {
    pool: PgPool,
}

impl PgWebhookDeliveryRepo {
    /// A repo on `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    webhook_id: Uuid,
    event_id: Uuid,
    event_kind: String,
    attempt: i16,
    status: String,
    next_attempt_at: Option<OffsetDateTime>,
    last_status_code: Option<i16>,
    last_latency_ms: Option<i32>,
    last_error: Option<String>,
    response_excerpt: Option<String>,
    created_at: OffsetDateTime,
    delivered_at: Option<OffsetDateTime>,
}

#[async_trait::async_trait]
impl WebhookDeliveryRepo for PgWebhookDeliveryRepo {
    async fn create_for_events(
        &self,
        rows: &[(WebhookId, EventId, EventKind)],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("create webhook deliveries", e))?;
        if let Some(advance) = advance
            && !outbox::advance_cursor(&mut tx, advance).await?
        {
            return Ok(0);
        }
        let mut created = 0_u64;
        for (webhook, event, kind) in rows {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM webhooks WHERE id = $1)")
                    .bind(webhook.as_uuid())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| map_err("read webhook", e))?;
            if !exists {
                continue;
            }
            let inserted = sqlx::query(
                "INSERT INTO webhook_deliveries (id, webhook_id, event_id, event_kind, attempt, \
                 status, next_attempt_at, created_at) \
                 VALUES ($1,$2,$3,$4,0,'pending',now(),now()) \
                 ON CONFLICT (webhook_id, event_id) DO NOTHING",
            )
            .bind(Uuid::now_v7())
            .bind(webhook.as_uuid())
            .bind(event.as_uuid())
            .bind(kind.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("create webhook delivery", e))?;
            created += inserted.rows_affected();
        }
        tx.commit()
            .await
            .map_err(|e| map_err("create webhook deliveries", e))?;
        Ok(created)
    }

    async fn claim(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<WebhookDelivery>, StoreError> {
        let seconds = i32::try_from(lease.as_secs()).unwrap_or(i32::MAX);
        let row: Option<Row> = sqlx::query_as(
            "WITH candidate AS ( \
                SELECT id FROM webhook_deliveries \
                WHERE (status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= now())) \
                   OR (status = 'delivering' AND lease_expires_at <= now()) \
                ORDER BY created_at, id \
                FOR UPDATE SKIP LOCKED LIMIT 1), \
             claimed AS ( \
                UPDATE webhook_deliveries SET status = 'delivering', attempt = attempt + 1, \
                    lease_owner = $1, lease_expires_at = now() + make_interval(secs => $2) \
                WHERE id = (SELECT id FROM candidate) \
                RETURNING id, webhook_id, event_id, event_kind, attempt, status, next_attempt_at, \
                          last_status_code, last_latency_ms, last_error, response_excerpt, \
                          created_at, delivered_at) \
             SELECT * FROM claimed",
        )
        .bind(owner)
        .bind(seconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("claim webhook delivery", e))?;
        row.map(decode).transpose()
    }

    async fn finish(
        &self,
        id: WebhookDeliveryId,
        owner: &str,
        outcome: &DeliveryOutcome,
    ) -> Result<bool, StoreError> {
        let status = status_label(outcome.status)?;
        let delivered = (outcome.status == DeliveryStatus::Succeeded).then(OffsetDateTime::now_utc);
        let updated = sqlx::query(
            "UPDATE webhook_deliveries SET status = $3, last_status_code = $4, last_latency_ms = $5, \
             last_error = $6, response_excerpt = $7, next_attempt_at = $8, \
             delivered_at = CASE WHEN $3 = 'succeeded' THEN $9 ELSE delivered_at END, \
             lease_owner = NULL, lease_expires_at = NULL \
             WHERE id = $1 AND lease_owner = $2 AND status = 'delivering'",
        )
        .bind(id.as_uuid())
        .bind(owner)
        .bind(status)
        .bind(outcome.status_code.map(i16::try_from).transpose().unwrap_or(None))
        .bind(outcome.latency_ms.map(i32::try_from).transpose().unwrap_or(None))
        .bind(outcome.error.as_ref().map(|text| clip(text, 64)))
        .bind(outcome.excerpt.as_deref().map(clean_excerpt))
        .bind(outcome.next_attempt_at)
        .bind(delivered)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("finish webhook delivery", e))?;
        Ok(updated.rows_affected() == 1)
    }

    async fn list(
        &self,
        webhook: WebhookId,
        page: &PageRequest,
    ) -> Result<Page<WebhookDelivery>, StoreError> {
        let limit = page.limit.clamp(1, 100);
        let after = match page.after.as_deref() {
            None => None,
            Some(token) => Some(
                Uuid::parse_str(token)
                    .map_err(|_| StoreError::Invalid("page token is not a delivery id".to_owned()))?,
            ),
        };
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, webhook_id, event_id, event_kind, attempt, status, next_attempt_at, \
             last_status_code, last_latency_ms, last_error, response_excerpt, created_at, \
             delivered_at FROM webhook_deliveries \
             WHERE webhook_id = $1 AND ($2::uuid IS NULL OR (created_at, id) < ( \
                 SELECT created_at, id FROM webhook_deliveries WHERE id = $2)) \
             ORDER BY created_at DESC, id DESC LIMIT $3",
        )
        .bind(webhook.as_uuid())
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list webhook deliveries", e))?;
        let items = rows.into_iter().map(decode).collect::<Result<Vec<_>, _>>()?;
        Ok(Page { items, next: None })
    }

    async fn pending_count(&self) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM webhook_deliveries WHERE status = 'pending'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("count webhook deliveries", e))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError> {
        let deleted = sqlx::query("DELETE FROM webhook_deliveries WHERE created_at < $1")
            .bind(created_before)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("purge webhook deliveries", e))?;
        Ok(deleted.rows_affected())
    }
}

fn decode(row: Row) -> Result<WebhookDelivery, StoreError> {
    let kind = EventKind::parse(&row.event_kind)
        .ok_or_else(|| backend("read webhook delivery", "event kind is unknown"))?;
    let mut delivery = WebhookDelivery::new(
        WebhookDeliveryId::from_uuid(row.id),
        WebhookId::from_uuid(row.webhook_id),
        EventId::from_uuid(row.event_id),
        kind,
        row.created_at,
    );
    delivery.attempt = u8::try_from(row.attempt).unwrap_or(u8::MAX);
    delivery.status = decode_status(&row.status)?;
    delivery.next_attempt_at = row.next_attempt_at;
    delivery.last_status_code = row.last_status_code.map(|code| u16::try_from(code).unwrap_or(0));
    delivery.last_latency_ms = row.last_latency_ms.map(|ms| u32::try_from(ms).unwrap_or(0));
    delivery.last_error = row.last_error;
    delivery.response_excerpt = row.response_excerpt;
    delivery.delivered_at = row.delivered_at;
    Ok(delivery)
}

fn status_label(status: DeliveryStatus) -> Result<&'static str, StoreError> {
    Ok(match status {
        DeliveryStatus::Pending => "pending",
        DeliveryStatus::Delivering => "delivering",
        DeliveryStatus::Succeeded => "succeeded",
        DeliveryStatus::Failed => "failed",
        DeliveryStatus::Dead => "dead",
    })
}

fn decode_status(label: &str) -> Result<DeliveryStatus, StoreError> {
    Ok(match label {
        "pending" => DeliveryStatus::Pending,
        "delivering" => DeliveryStatus::Delivering,
        "succeeded" => DeliveryStatus::Succeeded,
        "failed" => DeliveryStatus::Failed,
        "dead" => DeliveryStatus::Dead,
        _ => return Err(backend("read webhook delivery", "status is unknown")),
    })
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn clean_excerpt(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control()).take(1024).collect()
}
