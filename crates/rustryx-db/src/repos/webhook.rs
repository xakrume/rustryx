//! Webhooks. Secrets are sealed; an update never replaces the signing secret.

use std::sync::Arc;

use rustryx_core::error::StoreError;
use rustryx_core::events::EventKind;
use rustryx_core::ids::{ProjectId, RepositoryId, UserId, WebhookId};
use rustryx_core::ports::WebhookRepo;
use rustryx_core::secret::Secret;
use rustryx_core::webhook::{NotifyScope, Webhook, WebhookKind};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{to_i64, to_u64};
use crate::error::{backend, map_err};
use crate::sealer::SecretSealer;

/// Webhooks.
#[derive(Clone)]
pub struct PgWebhookRepo {
    pool: PgPool,
    sealer: Arc<dyn SecretSealer>,
}

impl PgWebhookRepo {
    /// A repo that seals secrets with `sealer`.
    #[must_use]
    pub fn new(pool: PgPool, sealer: Arc<dyn SecretSealer>) -> Self {
        Self { pool, sealer }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    project_id: Option<Uuid>,
    repository_id: Option<Uuid>,
    system: bool,
    name: String,
    kind: String,
    url: String,
    secret: String,
    auth_header: Option<String>,
    events: Vec<String>,
    enabled: bool,
    suspended_at: Option<OffsetDateTime>,
    consecutive_failures: i32,
    retry_max: i16,
    timeout_secs: i16,
    version: i64,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
}

#[async_trait::async_trait]
impl WebhookRepo for PgWebhookRepo {
    async fn create(&self, webhook: &Webhook) -> Result<(), StoreError> {
        webhook
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let (system, project, repository) = scope_columns(webhook.scope);
        let secret = self
            .sealer
            .seal(&secret_aad(webhook.id), webhook.secret.expose())?;
        let auth = seal_auth(
            self.sealer.as_ref(),
            webhook.id,
            webhook.auth_header.as_ref(),
        )?;
        let events = event_names(&webhook.events)?;
        sqlx::query(
            "INSERT INTO webhooks (id, project_id, repository_id, system, name, kind, url, \
             secret, auth_header, events, enabled, suspended_at, consecutive_failures, \
             retry_max, timeout_secs, version, created_by, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        )
        .bind(webhook.id.as_uuid())
        .bind(project)
        .bind(repository)
        .bind(system)
        .bind(&webhook.name)
        .bind(kind_label(webhook.kind))
        .bind(&webhook.url)
        .bind(secret)
        .bind(auth)
        .bind(events)
        .bind(webhook.enabled)
        .bind(webhook.suspended_at)
        .bind(i32::try_from(webhook.consecutive_failures).unwrap_or(i32::MAX))
        .bind(i16::from(webhook.retry_max))
        .bind(i16::from(webhook.timeout_secs))
        .bind(webhook.version)
        .bind(webhook.created_by.map(|id| id.as_uuid()))
        .bind(webhook.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create webhook", e))?;
        Ok(())
    }

    async fn get(&self, id: WebhookId) -> Result<Option<Webhook>, StoreError> {
        let row: Option<Row> = sqlx::query_as("SELECT * FROM webhooks WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read webhook", e))?;
        row.map(|row| row.into_webhook(self.sealer.as_ref()))
            .transpose()
    }

    async fn list(&self, scope: Option<NotifyScope>) -> Result<Vec<Webhook>, StoreError> {
        let (kind, id) = scope_filter(scope);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT * FROM webhooks \
             WHERE $1::text IS NULL \
                OR ($1 = 'system' AND system) \
                OR ($1 = 'project' AND project_id = $2) \
                OR ($1 = 'repository' AND repository_id = $2) \
             ORDER BY name, id",
        )
        .bind(kind)
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list webhooks", e))?;
        rows.into_iter()
            .map(|row| row.into_webhook(self.sealer.as_ref()))
            .collect()
    }

    async fn update(&self, webhook: &Webhook, expected_version: i64) -> Result<(), StoreError> {
        let existing = self
            .get(webhook.id)
            .await?
            .ok_or_else(|| StoreError::NotFound("webhook".to_owned()))?;
        if existing.version != expected_version {
            return Err(StoreError::Conflict(
                "webhook was changed by someone else".to_owned(),
            ));
        }
        let mut next = webhook.clone();
        next.secret = existing.secret;
        next.scope = existing.scope;
        next.created_by = existing.created_by;
        next.created_at = existing.created_at;
        next.version = expected_version + 1;
        next.validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let auth = seal_auth(self.sealer.as_ref(), next.id, next.auth_header.as_ref())?;
        let events = event_names(&next.events)?;
        let updated = sqlx::query(
            "UPDATE webhooks SET name = $2, kind = $3, url = $4, auth_header = $5, events = $6, \
             enabled = $7, suspended_at = $8, consecutive_failures = $9, retry_max = $10, \
             timeout_secs = $11, version = $12 \
             WHERE id = $1 AND version = $13",
        )
        .bind(next.id.as_uuid())
        .bind(&next.name)
        .bind(kind_label(next.kind))
        .bind(&next.url)
        .bind(auth)
        .bind(events)
        .bind(next.enabled)
        .bind(next.suspended_at)
        .bind(i32::try_from(next.consecutive_failures).unwrap_or(i32::MAX))
        .bind(i16::from(next.retry_max))
        .bind(i16::from(next.timeout_secs))
        .bind(next.version)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update webhook", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::Conflict(
                "webhook was changed by someone else".to_owned(),
            ));
        }
        Ok(())
    }

    async fn delete(&self, id: WebhookId) -> Result<(), StoreError> {
        let deleted = sqlx::query("DELETE FROM webhooks WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete webhook", e))?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound("webhook".to_owned()));
        }
        Ok(())
    }

    async fn rotate_secret(
        &self,
        id: WebhookId,
        secret: &Secret,
        expected_version: i64,
    ) -> Result<i64, StoreError> {
        if secret.is_empty() {
            return Err(StoreError::Conflict(
                "a webhook needs a signing secret".to_owned(),
            ));
        }
        let sealed = self.sealer.seal(&secret_aad(id), secret.expose())?;
        let version = expected_version + 1;
        let updated = sqlx::query(
            "UPDATE webhooks SET secret = $2, version = $3 WHERE id = $1 AND version = $4",
        )
        .bind(id.as_uuid())
        .bind(sealed)
        .bind(version)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("rotate webhook secret", e))?;
        if updated.rows_affected() == 0 {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM webhooks WHERE id = $1)")
                    .bind(id.as_uuid())
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| map_err("rotate webhook secret", e))?;
            if exists {
                return Err(StoreError::Conflict(
                    "webhook was changed by someone else".to_owned(),
                ));
            }
            return Err(StoreError::NotFound("webhook".to_owned()));
        }
        Ok(version)
    }

    async fn matching(
        &self,
        kind: EventKind,
        project: Option<ProjectId>,
        repository: Option<RepositoryId>,
    ) -> Result<Vec<Webhook>, StoreError> {
        let name = event_name(kind)?;
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT * FROM webhooks \
             WHERE enabled AND suspended_at IS NULL AND $1 = ANY(events) \
               AND (system \
                    OR ($2::uuid IS NOT NULL AND project_id = $2) \
                    OR ($3::uuid IS NOT NULL AND repository_id = $3)) \
             ORDER BY id",
        )
        .bind(name)
        .bind(project.map(|id| id.as_uuid()))
        .bind(repository.map(|id| id.as_uuid()))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("match webhooks", e))?;
        rows.into_iter()
            .map(|row| row.into_webhook(self.sealer.as_ref()))
            .collect()
    }

    async fn any_subscribes_to(&self, kind: EventKind) -> Result<bool, StoreError> {
        let name = event_name(kind)?;
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM webhooks WHERE enabled AND $1 = ANY(events))",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("read webhook subscriptions", e))
    }

    async fn record_outcome(
        &self,
        id: WebhookId,
        success: bool,
        suspend_after: u32,
    ) -> Result<(), StoreError> {
        let after = to_i64(u64::from(suspend_after))?;
        sqlx::query(
            "UPDATE webhooks SET \
               consecutive_failures = CASE WHEN $2 THEN 0 ELSE consecutive_failures + 1 END, \
               suspended_at = CASE \
                 WHEN $2 OR suspended_at IS NOT NULL OR $3 <= 0 THEN suspended_at \
                 WHEN consecutive_failures + 1 >= $3 THEN now() \
                 ELSE suspended_at END \
             WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(success)
        .bind(after)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("record webhook outcome", e))?;
        Ok(())
    }
}

impl Row {
    fn into_webhook(self, sealer: &dyn SecretSealer) -> Result<Webhook, StoreError> {
        let id = WebhookId::from_uuid(self.id);
        let mut webhook = Webhook::new(
            id,
            scope_of(self.system, self.project_id, self.repository_id)?,
            self.name,
            self.url,
            sealer.open(&secret_aad(id), &self.secret)?,
            self.events
                .iter()
                .map(|name| parse_event(name))
                .collect::<Result<Vec<_>, _>>()?,
            self.created_at,
        );
        webhook.kind = parse_kind(&self.kind)?;
        webhook.auth_header = self
            .auth_header
            .map(|envelope| sealer.open(&auth_aad(id), &envelope))
            .transpose()?;
        webhook.enabled = self.enabled;
        webhook.suspended_at = self.suspended_at;
        webhook.consecutive_failures =
            to_u64(i64::from(self.consecutive_failures), "read webhook")?
                .try_into()
                .map_err(|_| backend("read webhook", "failure counter does not fit"))?;
        webhook.retry_max = u8::try_from(self.retry_max)
            .map_err(|_| backend("read webhook", "retry_max does not fit"))?;
        webhook.timeout_secs = u8::try_from(self.timeout_secs)
            .map_err(|_| backend("read webhook", "timeout does not fit"))?;
        webhook.version = self.version;
        webhook.created_by = self.created_by.map(UserId::from_uuid);
        Ok(webhook)
    }
}

fn scope_columns(scope: NotifyScope) -> (bool, Option<Uuid>, Option<Uuid>) {
    match scope {
        NotifyScope::System => (true, None, None),
        NotifyScope::Project(id) => (false, Some(id.as_uuid()), None),
        NotifyScope::Repository(id) => (false, None, Some(id.as_uuid())),
    }
}

fn scope_of(
    system: bool,
    project: Option<Uuid>,
    repository: Option<Uuid>,
) -> Result<NotifyScope, StoreError> {
    match (system, project, repository) {
        (true, None, None) => Ok(NotifyScope::System),
        (false, Some(id), None) => Ok(NotifyScope::Project(ProjectId::from_uuid(id))),
        (false, None, Some(id)) => Ok(NotifyScope::Repository(RepositoryId::from_uuid(id))),
        _ => Err(backend("read webhook", "scope columns disagree")),
    }
}

fn scope_filter(scope: Option<NotifyScope>) -> (Option<&'static str>, Option<Uuid>) {
    match scope {
        None => (None, None),
        Some(NotifyScope::System) => (Some("system"), None),
        Some(NotifyScope::Project(id)) => (Some("project"), Some(id.as_uuid())),
        Some(NotifyScope::Repository(id)) => (Some("repository"), Some(id.as_uuid())),
    }
}

fn kind_label(kind: WebhookKind) -> &'static str {
    match kind {
        WebhookKind::Http => "http",
        WebhookKind::CloudEvents => "cloudevents",
    }
}

fn parse_kind(label: &str) -> Result<WebhookKind, StoreError> {
    match label {
        "http" => Ok(WebhookKind::Http),
        "cloudevents" => Ok(WebhookKind::CloudEvents),
        _ => Err(backend("read webhook", "unknown webhook kind")),
    }
}

fn event_names(events: &[EventKind]) -> Result<Vec<String>, StoreError> {
    events.iter().copied().map(event_name).collect()
}

fn event_name(kind: EventKind) -> Result<String, StoreError> {
    match serde_json::to_value(kind).map_err(|_| backend("write webhook", "event name"))? {
        serde_json::Value::String(name) => Ok(name),
        _ => Err(backend("write webhook", "event name")),
    }
}

fn parse_event(name: &str) -> Result<EventKind, StoreError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| backend("read webhook", "unknown event"))
}

fn secret_aad(id: WebhookId) -> Vec<u8> {
    let mut aad = b"rustryx/webhook/secret/v1/".to_vec();
    aad.extend_from_slice(id.as_uuid().as_bytes());
    aad
}

fn auth_aad(id: WebhookId) -> Vec<u8> {
    let mut aad = b"rustryx/webhook/auth/v1/".to_vec();
    aad.extend_from_slice(id.as_uuid().as_bytes());
    aad
}

fn seal_auth(
    sealer: &dyn SecretSealer,
    id: WebhookId,
    header: Option<&Secret>,
) -> Result<Option<String>, StoreError> {
    header
        .map(|secret| sealer.seal(&auth_aad(id), secret.expose()))
        .transpose()
}
