//! Notification subscriptions and the e-mails they produce.

use std::time::Duration;

use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventKind};
use rustryx_core::ids::{
    EmailDeliveryId, EventId, ProjectId, RepositoryId, SubscriptionId, UserId,
};
use rustryx_core::notification::{EmailDelivery, EmailStatus, NotificationSubscription};
use rustryx_core::ports::{NewEmail, NotificationRepo};
use rustryx_core::webhook::NotifyScope;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{backend, map_err};
use crate::outbox;

/// Subscriptions and e-mail deliveries.
#[derive(Clone)]
pub struct PgNotificationRepo {
    pool: PgPool,
}

impl PgNotificationRepo {
    /// A repo on `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct SubRow {
    id: Uuid,
    project_id: Option<Uuid>,
    repository_id: Option<Uuid>,
    system: bool,
    events: Vec<String>,
    recipients: serde_json::Value,
    enabled: bool,
    version: i64,
    created_by: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct MailRow {
    id: Uuid,
    subscription_id: Option<Uuid>,
    event_id: Uuid,
    to_address: String,
    status: String,
    attempts: i16,
    next_attempt_at: Option<OffsetDateTime>,
    last_error: Option<String>,
    created_at: OffsetDateTime,
    sent_at: Option<OffsetDateTime>,
}

#[async_trait::async_trait]
impl NotificationRepo for PgNotificationRepo {
    async fn upsert(
        &self,
        subscription: &NotificationSubscription,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        subscription
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        scope_exists(&self.pool, subscription.scope).await?;
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM notification_subscriptions WHERE id = $1",
        )
        .bind(subscription.id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read notification subscription", e))?;
        let version = match (current, expected_version) {
            (Some(current), Some(expected)) if current == expected => current.saturating_add(1),
            (Some(current), Some(expected)) => {
                return Err(StoreError::Conflict(format!(
                    "stale version: expected {expected}, found {current}"
                )));
            }
            (Some(_), None) => {
                return Err(StoreError::Conflict(
                    "the subscription already exists".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(StoreError::Conflict(
                    "stale version: the subscription does not exist".to_owned(),
                ));
            }
            (None, None) => subscription.version,
        };
        let (system, project, repository) = scope_columns(subscription.scope);
        let events = subscription
            .events
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>();
        let recipients = serde_json::to_value(&subscription.recipients)
            .map_err(|_| backend("write notification subscription", "recipients"))?;
        if current.is_some() {
            sqlx::query(
                "UPDATE notification_subscriptions SET project_id = $2, repository_id = $3, \
                 system = $4, events = $5, recipients = $6, enabled = $7, version = $8, \
                 created_by = $9 WHERE id = $1",
            )
            .bind(subscription.id.as_uuid())
            .bind(project)
            .bind(repository)
            .bind(system)
            .bind(&events)
            .bind(recipients)
            .bind(subscription.enabled)
            .bind(version)
            .bind(subscription.created_by.map(|id| id.as_uuid()))
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("update notification subscription", e))?;
        } else {
            sqlx::query(
                "INSERT INTO notification_subscriptions (id, project_id, repository_id, system, \
                 events, recipients, enabled, version, created_by) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            )
            .bind(subscription.id.as_uuid())
            .bind(project)
            .bind(repository)
            .bind(system)
            .bind(&events)
            .bind(recipients)
            .bind(subscription.enabled)
            .bind(version)
            .bind(subscription.created_by.map(|id| id.as_uuid()))
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("create notification subscription", e))?;
        }
        Ok(())
    }

    async fn list(
        &self,
        scope: Option<NotifyScope>,
    ) -> Result<Vec<NotificationSubscription>, StoreError> {
        let rows: Vec<SubRow> = match scope {
            None => {
                sqlx::query_as("SELECT * FROM notification_subscriptions ORDER BY id")
                    .fetch_all(&self.pool)
                    .await
            }
            Some(NotifyScope::System) => {
                sqlx::query_as("SELECT * FROM notification_subscriptions WHERE system ORDER BY id")
                    .fetch_all(&self.pool)
                    .await
            }
            Some(NotifyScope::Project(id)) => {
                sqlx::query_as(
                    "SELECT * FROM notification_subscriptions WHERE project_id = $1 ORDER BY id",
                )
                .bind(id.as_uuid())
                .fetch_all(&self.pool)
                .await
            }
            Some(NotifyScope::Repository(id)) => {
                sqlx::query_as(
                    "SELECT * FROM notification_subscriptions WHERE repository_id = $1 ORDER BY id",
                )
                .bind(id.as_uuid())
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| map_err("list notification subscriptions", e))?;
        rows.into_iter().map(decode_sub).collect()
    }

    async fn delete(&self, id: SubscriptionId) -> Result<(), StoreError> {
        let deleted = sqlx::query("DELETE FROM notification_subscriptions WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete notification subscription", e))?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound("notification subscription".to_owned()));
        }
        Ok(())
    }

    async fn matching(
        &self,
        kind: EventKind,
        project: Option<ProjectId>,
        repository: Option<RepositoryId>,
    ) -> Result<Vec<NotificationSubscription>, StoreError> {
        let rows: Vec<SubRow> = sqlx::query_as(
            "SELECT * FROM notification_subscriptions WHERE enabled AND $1 = ANY (events) \
             AND (system OR project_id = $2 OR repository_id = $3) ORDER BY id",
        )
        .bind(kind.as_str())
        .bind(project.map(|id| id.as_uuid()))
        .bind(repository.map(|id| id.as_uuid()))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("match notification subscriptions", e))?;
        rows.into_iter().map(decode_sub).collect()
    }

    async fn create_for_events(
        &self,
        rows: &[NewEmail],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("create email deliveries", e))?;
        if let Some(advance) = advance
            && !outbox::advance_cursor(&mut tx, advance).await?
        {
            return Ok(0);
        }
        let now = OffsetDateTime::now_utc();
        let window = now - time::Duration::minutes(10);
        for row in rows {
            let duplicate: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM email_deliveries \
                 WHERE to_address = $1 AND event_kind = $2 AND subject_key = $3 \
                   AND created_at >= $4 AND status <> 'suppressed')",
            )
            .bind(&row.to)
            .bind(row.kind.as_str())
            .bind(&row.subject_key)
            .bind(window)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_err("read email deliveries", e))?;
            let (status, next) = if duplicate {
                ("suppressed", None)
            } else {
                ("pending", Some(now))
            };
            sqlx::query(
                "INSERT INTO email_deliveries (id, subscription_id, event_id, event_kind, \
                 subject_key, to_address, status, attempts, next_attempt_at, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,0,$8,$9)",
            )
            .bind(Uuid::now_v7())
            .bind(row.subscription_id.map(|id| id.as_uuid()))
            .bind(row.event_id.as_uuid())
            .bind(row.kind.as_str())
            .bind(&row.subject_key)
            .bind(&row.to)
            .bind(status)
            .bind(next)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("create email delivery", e))?;
        }
        tx.commit()
            .await
            .map_err(|e| map_err("create email deliveries", e))?;
        Ok(u64::try_from(rows.len()).unwrap_or(u64::MAX))
    }

    async fn claim(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<EmailDelivery>, StoreError> {
        let seconds = i32::try_from(lease.as_secs()).unwrap_or(i32::MAX);
        let row: Option<MailRow> = sqlx::query_as(
            "WITH candidate AS ( \
                SELECT id FROM email_deliveries \
                WHERE (status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= now())) \
                   OR (status = 'sending' AND lease_expires_at <= now()) \
                ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1), \
             claimed AS ( \
                UPDATE email_deliveries SET status = 'sending', attempts = attempts + 1, \
                    lease_owner = $1, lease_expires_at = now() + make_interval(secs => $2) \
                WHERE id = (SELECT id FROM candidate) \
                RETURNING id, subscription_id, event_id, to_address, status, attempts, \
                          next_attempt_at, last_error, created_at, sent_at) \
             SELECT * FROM claimed",
        )
        .bind(owner)
        .bind(seconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("claim email delivery", e))?;
        row.map(decode_mail).transpose()
    }

    async fn finish(
        &self,
        id: EmailDeliveryId,
        owner: &str,
        status: EmailStatus,
        error: Option<&str>,
        retry_in: Option<Duration>,
    ) -> Result<bool, StoreError> {
        let ends = matches!(status, EmailStatus::Sent | EmailStatus::Failed | EmailStatus::Dead);
        if retry_in.is_none() && !ends {
            return Err(StoreError::Invalid(
                "a mail ends as sent, failed or dead, or goes back with a retry delay".to_owned(),
            ));
        }
        let code = error.map(|text| text.chars().take(64).collect::<String>());
        let updated = if let Some(delay) = retry_in {
            let seconds = i32::try_from(delay.as_secs()).unwrap_or(i32::MAX);
            sqlx::query(
                "UPDATE email_deliveries SET status = 'pending', last_error = $3, \
                 lease_owner = NULL, lease_expires_at = NULL, \
                 next_attempt_at = now() + make_interval(secs => $4) \
                 WHERE id = $1 AND lease_owner = $2 AND status = 'sending'",
            )
            .bind(id.as_uuid())
            .bind(owner)
            .bind(code)
            .bind(seconds)
            .execute(&self.pool)
            .await
        } else {
            sqlx::query(
                "UPDATE email_deliveries SET status = $3, last_error = $4, \
                 lease_owner = NULL, lease_expires_at = NULL, next_attempt_at = NULL, \
                 sent_at = CASE WHEN $3 = 'sent' THEN now() ELSE sent_at END \
                 WHERE id = $1 AND lease_owner = $2 AND status = 'sending'",
            )
            .bind(id.as_uuid())
            .bind(owner)
            .bind(mail_status(status)?)
            .bind(code)
            .execute(&self.pool)
            .await
        }
        .map_err(|e| map_err("finish email delivery", e))?;
        Ok(updated.rows_affected() == 1)
    }

    async fn count_sent_since(&self, since: OffsetDateTime) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM email_deliveries WHERE sent_at >= $1",
        )
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("count sent email", e))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError> {
        let deleted = sqlx::query("DELETE FROM email_deliveries WHERE created_at < $1")
            .bind(created_before)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("purge email deliveries", e))?;
        Ok(deleted.rows_affected())
    }
}

async fn scope_exists(pool: &PgPool, scope: NotifyScope) -> Result<(), StoreError> {
    let exists: bool = match scope {
        NotifyScope::System => return Ok(()),
        NotifyScope::Project(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(pool)
                .await
        }
        NotifyScope::Repository(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM repositories WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(pool)
                .await
        }
    }
    .map_err(|e| map_err("read notification scope", e))?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "the scope of the subscription does not exist".to_owned(),
        ))
    }
}

fn scope_columns(scope: NotifyScope) -> (bool, Option<Uuid>, Option<Uuid>) {
    match scope {
        NotifyScope::System => (true, None, None),
        NotifyScope::Project(id) => (false, Some(id.as_uuid()), None),
        NotifyScope::Repository(id) => (false, None, Some(id.as_uuid())),
    }
}

fn decode_sub(row: SubRow) -> Result<NotificationSubscription, StoreError> {
    let scope = match (row.system, row.project_id, row.repository_id) {
        (true, None, None) => NotifyScope::System,
        (false, Some(id), None) => NotifyScope::Project(ProjectId::from_uuid(id)),
        (false, None, Some(id)) => NotifyScope::Repository(RepositoryId::from_uuid(id)),
        _ => return Err(backend("read notification subscription", "scope is ambiguous")),
    };
    let events = row
        .events
        .iter()
        .map(|label| {
            EventKind::parse(label)
                .ok_or_else(|| backend("read notification subscription", "event kind is unknown"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let recipients = serde_json::from_value(row.recipients)
        .map_err(|_| backend("read notification subscription", "recipients are not recipients"))?;
    let mut subscription = NotificationSubscription::new(SubscriptionId::from_uuid(row.id), scope, events, recipients);
    subscription.enabled = row.enabled;
    subscription.version = row.version;
    subscription.created_by = row.created_by.map(UserId::from_uuid);
    Ok(subscription)
}

fn decode_mail(row: MailRow) -> Result<EmailDelivery, StoreError> {
    Ok(EmailDelivery {
        id: EmailDeliveryId::from_uuid(row.id),
        subscription_id: row.subscription_id.map(SubscriptionId::from_uuid),
        event_id: EventId::from_uuid(row.event_id),
        to: row.to_address,
        status: decode_mail_status(&row.status)?,
        attempts: u8::try_from(row.attempts).unwrap_or(u8::MAX),
        next_attempt_at: row.next_attempt_at,
        last_error: row.last_error,
        created_at: row.created_at,
        sent_at: row.sent_at,
    })
}

fn mail_status(status: EmailStatus) -> Result<&'static str, StoreError> {
    Ok(match status {
        EmailStatus::Pending => "pending",
        EmailStatus::Sending => "sending",
        EmailStatus::Sent => "sent",
        EmailStatus::Failed => "failed",
        EmailStatus::Dead => "dead",
        EmailStatus::Suppressed => "suppressed",
    })
}

fn decode_mail_status(label: &str) -> Result<EmailStatus, StoreError> {
    Ok(match label {
        "pending" => EmailStatus::Pending,
        "sending" => EmailStatus::Sending,
        "sent" => EmailStatus::Sent,
        "failed" => EmailStatus::Failed,
        "dead" => EmailStatus::Dead,
        "suppressed" => EmailStatus::Suppressed,
        _ => return Err(backend("read email delivery", "status is unknown")),
    })
}
