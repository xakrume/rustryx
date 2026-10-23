//! Webhooks, their deliveries, e-mail subscriptions and e-mail deliveries.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventKind};
use rustryx_core::ids::{
    EmailDeliveryId, EventId, ProjectId, RepositoryId, SubscriptionId, WebhookDeliveryId, WebhookId,
};
use rustryx_core::notification::{EmailDelivery, EmailStatus, NotificationSubscription};
use rustryx_core::ports::{
    DeliveryOutcome, NewEmail, NotificationRepo, Page, PageRequest, WebhookDeliveryRepo,
    WebhookRepo,
};
use rustryx_core::secret::Secret;
use rustryx_core::webhook::{
    DeliveryStatus, MAX_DELIVERY_ERROR_CHARS, MAX_RESPONSE_EXCERPT_CHARS, MAX_WEBHOOKS_PER_SCOPE,
    NotifyScope, Webhook, WebhookDelivery,
};
use time::OffsetDateTime;

use super::paging::paginate;
use super::state::{DeliveryRow, EmailRow, State};
use super::support::bump_version;
use super::{MemStore, conflict, not_found};
use crate::support::add_std;

/// Identical `(address, event kind, subject key)` inside this window are suppressed.
const EMAIL_SUPPRESSION_WINDOW: Duration = Duration::from_secs(600);

impl State {
    fn notify_scope_exists(&self, scope: NotifyScope) -> bool {
        match scope {
            NotifyScope::System => true,
            NotifyScope::Project(id) => self.projects.contains_key(&id),
            NotifyScope::Repository(id) => self.repositories.contains_key(&id),
        }
    }

    /// Compare and set of an outbox cursor, the atomic hand-off that the consumers' batch
    /// methods share. `false` means another consumer moved it first.
    fn cursor_matches(&self, advance: Option<&CursorAdvance>) -> bool {
        advance.is_none_or(|advance| {
            self.outbox_cursors
                .get(advance.consumer)
                .copied()
                .unwrap_or_default()
                == advance.from
        })
    }

    fn move_cursor(&mut self, advance: Option<&CursorAdvance>) {
        if let Some(advance) = advance {
            self.outbox_cursors
                .insert(advance.consumer.to_owned(), advance.to);
        }
    }

    fn held_delivery(&mut self, id: WebhookDeliveryId, owner: &str) -> Option<&mut DeliveryRow> {
        self.webhook_deliveries.iter_mut().find(|row| {
            row.delivery.id == id
                && row.delivery.status == DeliveryStatus::Delivering
                && row.owner.as_deref() == Some(owner)
        })
    }
}

/// True when an event of `project` and `repository` falls inside `scope`.
fn scope_applies(
    scope: NotifyScope,
    project: Option<ProjectId>,
    repository: Option<RepositoryId>,
) -> bool {
    match scope {
        NotifyScope::System => true,
        NotifyScope::Project(id) => project == Some(id),
        NotifyScope::Repository(id) => repository == Some(id),
    }
}

#[async_trait]
impl WebhookRepo for MemStore {
    async fn create(&self, webhook: &Webhook) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        webhook
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.notify_scope_exists(webhook.scope) {
            return Err(conflict("the scope of the webhook does not exist"));
        }
        if state.webhooks.contains_key(&webhook.id) {
            return Err(conflict(format!("webhook {} already exists", webhook.id)));
        }
        let in_scope = state
            .webhooks
            .values()
            .filter(|w| w.scope == webhook.scope)
            .count();
        if in_scope >= MAX_WEBHOOKS_PER_SCOPE {
            return Err(conflict("a scope has at most 20 webhooks"));
        }
        state.webhooks.insert(webhook.id, webhook.clone());
        Ok(())
    }

    async fn get(&self, id: WebhookId) -> Result<Option<Webhook>, StoreError> {
        Ok(self.tables()?.webhooks.get(&id).cloned())
    }

    async fn list(&self, scope: Option<NotifyScope>) -> Result<Vec<Webhook>, StoreError> {
        let mut all: Vec<Webhook> = self
            .tables()?
            .webhooks
            .values()
            .filter(|w| scope.is_none_or(|s| w.scope == s))
            .cloned()
            .collect();
        all.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
        Ok(all)
    }

    async fn update(&self, webhook: &Webhook, expected_version: i64) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .webhooks
            .get(&webhook.id)
            .ok_or_else(|| not_found("webhook", webhook.id))?;
        let version = bump_version(existing.version, expected_version)?;
        // The stored secret stays, whatever the caller's copy holds: only `rotate_secret`
        // changes it, so an edit from a stale read cannot resurrect an old secret.
        let mut candidate = webhook.clone();
        candidate.secret = existing.secret.clone();
        candidate
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        candidate.scope = existing.scope;
        candidate.created_by = existing.created_by;
        candidate.created_at = existing.created_at;
        candidate.version = version;
        state.webhooks.insert(candidate.id, candidate);
        Ok(())
    }

    async fn delete(&self, id: WebhookId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.webhooks.remove(&id).is_none() {
            return Err(not_found("webhook", id));
        }
        state
            .webhook_deliveries
            .retain(|row| row.delivery.webhook_id != id);
        Ok(())
    }

    async fn rotate_secret(
        &self,
        id: WebhookId,
        secret: &Secret,
        expected_version: i64,
    ) -> Result<i64, StoreError> {
        let mut state = self.tables()?;
        let webhook = state
            .webhooks
            .get_mut(&id)
            .ok_or_else(|| not_found("webhook", id))?;
        let version = bump_version(webhook.version, expected_version)?;
        if secret.is_empty() {
            return Err(conflict("a webhook needs a signing secret"));
        }
        // One stored secret, so the old one stops signing in the same write.
        webhook.secret = secret.clone();
        webhook.version = version;
        Ok(version)
    }

    async fn matching(
        &self,
        kind: EventKind,
        project: Option<ProjectId>,
        repository: Option<RepositoryId>,
    ) -> Result<Vec<Webhook>, StoreError> {
        let state = self.tables()?;
        let mut hooks: Vec<Webhook> = state
            .webhooks
            .values()
            .filter(|w| w.is_deliverable() && w.subscribes_to(kind))
            .filter(|w| scope_applies(w.scope, project, repository))
            .cloned()
            .collect();
        hooks.sort_by_key(|w| w.id);
        Ok(hooks)
    }

    async fn any_subscribes_to(&self, kind: EventKind) -> Result<bool, StoreError> {
        let state = self.tables()?;
        Ok(state
            .webhooks
            .values()
            .any(|w| w.enabled && w.subscribes_to(kind)))
    }

    async fn record_outcome(
        &self,
        id: WebhookId,
        success: bool,
        suspend_after: u32,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // A webhook deleted while a delivery was in flight is not an error for the worker.
        let Some(webhook) = state.webhooks.get_mut(&id) else {
            return Ok(());
        };
        if success {
            webhook.consecutive_failures = 0;
            return Ok(());
        }
        webhook.consecutive_failures = webhook.consecutive_failures.saturating_add(1);
        if suspend_after > 0
            && webhook.consecutive_failures >= suspend_after
            && webhook.suspended_at.is_none()
        {
            webhook.suspended_at = Some(now);
        }
        Ok(())
    }
}

fn delivery_key(seq: u64) -> String {
    format!("{:020}", u64::MAX - seq)
}

/// Control characters removed and the length limit applied, as the excerpt column requires.
fn clean_excerpt(excerpt: &str) -> String {
    excerpt
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_RESPONSE_EXCERPT_CHARS)
        .collect()
}

#[async_trait]
impl WebhookDeliveryRepo for MemStore {
    async fn create_for_events(
        &self,
        rows: &[(WebhookId, EventId, EventKind)],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if !state.cursor_matches(advance) {
            return Ok(0);
        }
        let mut created = 0_u64;
        for (webhook, event, kind) in rows {
            // A webhook deleted since the consumer matched it has nobody to deliver to.
            if !state.webhooks.contains_key(webhook) {
                continue;
            }
            // One pending delivery per `(webhook, event)`: a replayed batch adds nothing.
            let exists = state
                .webhook_deliveries
                .iter()
                .any(|r| r.delivery.webhook_id == *webhook && r.delivery.event_id == *event);
            if exists {
                continue;
            }
            state.run_seq += 1;
            let seq = state.run_seq;
            state.webhook_deliveries.push(DeliveryRow {
                seq,
                delivery: WebhookDelivery::new(
                    WebhookDeliveryId::generate(),
                    *webhook,
                    *event,
                    *kind,
                    now,
                ),
                owner: None,
                lease_expires_at: None,
            });
            created += 1;
        }
        state.move_cursor(advance);
        Ok(created)
    }

    async fn claim(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<WebhookDelivery>, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let candidate = state
            .webhook_deliveries
            .iter_mut()
            .filter(|row| match row.delivery.status {
                DeliveryStatus::Pending => row.delivery.next_attempt_at.is_none_or(|at| at <= now),
                DeliveryStatus::Delivering => row.lease_expires_at.is_some_and(|at| at <= now),
                _ => false,
            })
            .min_by_key(|row| row.seq);
        let Some(row) = candidate else {
            return Ok(None);
        };
        row.delivery.status = DeliveryStatus::Delivering;
        row.delivery.attempt = row.delivery.attempt.saturating_add(1);
        row.owner = Some(owner.to_owned());
        row.lease_expires_at = Some(add_std(now, lease));
        Ok(Some(row.delivery.clone()))
    }

    async fn finish(
        &self,
        id: WebhookDeliveryId,
        owner: &str,
        outcome: &DeliveryOutcome,
    ) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let Some(row) = state.held_delivery(id, owner) else {
            return Ok(false);
        };
        row.owner = None;
        row.lease_expires_at = None;
        let delivery = &mut row.delivery;
        delivery.status = outcome.status;
        delivery.last_status_code = outcome.status_code;
        delivery.last_latency_ms = outcome.latency_ms;
        delivery.last_error = outcome
            .error
            .as_ref()
            .map(|e| e.chars().take(MAX_DELIVERY_ERROR_CHARS).collect());
        delivery.response_excerpt = outcome.excerpt.as_deref().map(clean_excerpt);
        delivery.next_attempt_at = outcome.next_attempt_at;
        if outcome.status == DeliveryStatus::Succeeded {
            delivery.delivered_at = Some(now);
        }
        Ok(true)
    }

    async fn list(
        &self,
        webhook: WebhookId,
        page: &PageRequest,
    ) -> Result<Page<WebhookDelivery>, StoreError> {
        let rows: Vec<(String, WebhookDelivery)> = self
            .tables()?
            .webhook_deliveries
            .iter()
            .filter(|row| row.delivery.webhook_id == webhook)
            .map(|row| (delivery_key(row.seq), row.delivery.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn pending_count(&self) -> Result<u64, StoreError> {
        let state = self.tables()?;
        let count = state
            .webhook_deliveries
            .iter()
            .filter(|row| row.delivery.status == DeliveryStatus::Pending)
            .count();
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let before = state.webhook_deliveries.len();
        state
            .webhook_deliveries
            .retain(|row| row.delivery.created_at >= created_before);
        Ok(u64::try_from(before - state.webhook_deliveries.len()).unwrap_or(u64::MAX))
    }
}

#[async_trait]
impl NotificationRepo for MemStore {
    async fn upsert(
        &self,
        subscription: &NotificationSubscription,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        subscription
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.notify_scope_exists(subscription.scope) {
            return Err(conflict("the scope of the subscription does not exist"));
        }
        let existing = state.subscriptions.get(&subscription.id).map(|s| s.version);
        let mut next = subscription.clone();
        match (existing, expected_version) {
            (Some(current), Some(expected)) => next.version = bump_version(current, expected)?,
            (Some(_), None) => return Err(conflict("the subscription already exists")),
            (None, Some(_)) => {
                return Err(conflict("stale version: the subscription does not exist"));
            }
            (None, None) => {}
        }
        state.subscriptions.insert(next.id, next);
        Ok(())
    }

    async fn list(
        &self,
        scope: Option<NotifyScope>,
    ) -> Result<Vec<NotificationSubscription>, StoreError> {
        let mut all: Vec<NotificationSubscription> = self
            .tables()?
            .subscriptions
            .values()
            .filter(|s| scope.is_none_or(|wanted| s.scope == wanted))
            .cloned()
            .collect();
        all.sort_by_key(|s| s.id);
        Ok(all)
    }

    async fn delete(&self, id: SubscriptionId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.subscriptions.remove(&id).is_none() {
            return Err(not_found("notification subscription", id));
        }
        Ok(())
    }

    async fn matching(
        &self,
        kind: EventKind,
        project: Option<ProjectId>,
        repository: Option<RepositoryId>,
    ) -> Result<Vec<NotificationSubscription>, StoreError> {
        let state = self.tables()?;
        let mut subscriptions: Vec<NotificationSubscription> = state
            .subscriptions
            .values()
            .filter(|s| s.enabled && s.events.contains(&kind))
            .filter(|s| scope_applies(s.scope, project, repository))
            .cloned()
            .collect();
        subscriptions.sort_by_key(|s| s.id);
        Ok(subscriptions)
    }

    async fn create_for_events(
        &self,
        rows: &[NewEmail],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if !state.cursor_matches(advance) {
            return Ok(0);
        }
        let window_start = now
            - time::Duration::seconds(
                i64::try_from(EMAIL_SUPPRESSION_WINDOW.as_secs()).unwrap_or(i64::MAX),
            );
        let mut created = 0_u64;
        for row in rows {
            // A recent identical message (same address, kind and subject) is recorded as
            // suppressed, so the audit shows the mail that was not sent.
            let duplicate = state.emails.iter().any(|existing| {
                existing.delivery.to == row.to
                    && existing.kind == row.kind
                    && existing.subject_key == row.subject_key
                    && existing.delivery.created_at >= window_start
                    && existing.delivery.status != EmailStatus::Suppressed
            });
            let (status, next_attempt_at) = if duplicate {
                (EmailStatus::Suppressed, None)
            } else {
                (EmailStatus::Pending, Some(now))
            };
            state.emails.push(EmailRow {
                delivery: EmailDelivery {
                    id: EmailDeliveryId::generate(),
                    subscription_id: row.subscription_id,
                    event_id: row.event_id,
                    to: row.to.clone(),
                    status,
                    attempts: 0,
                    next_attempt_at,
                    last_error: None,
                    created_at: now,
                    sent_at: None,
                },
                kind: row.kind,
                subject_key: row.subject_key.clone(),
                owner: None,
                lease_expires_at: None,
            });
            created += 1;
        }
        state.move_cursor(advance);
        Ok(created)
    }

    async fn claim(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<EmailDelivery>, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let candidate = state
            .emails
            .iter_mut()
            .filter(|row| match row.delivery.status {
                EmailStatus::Pending => row.delivery.next_attempt_at.is_none_or(|at| at <= now),
                EmailStatus::Sending => row.lease_expires_at.is_some_and(|at| at <= now),
                _ => false,
            })
            .min_by_key(|row| (row.delivery.created_at, row.delivery.id));
        let Some(row) = candidate else {
            return Ok(None);
        };
        row.delivery.status = EmailStatus::Sending;
        row.delivery.attempts = row.delivery.attempts.saturating_add(1);
        row.owner = Some(owner.to_owned());
        row.lease_expires_at = Some(add_std(now, lease));
        Ok(Some(row.delivery.clone()))
    }

    async fn finish(
        &self,
        id: EmailDeliveryId,
        owner: &str,
        status: EmailStatus,
        error: Option<&str>,
        retry_in: Option<Duration>,
    ) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // A mail ends as sent, failed or dead, or goes back to pending with a delay; the
        // other states are the repository's own and would leave the row claimed.
        let ends = matches!(
            status,
            EmailStatus::Sent | EmailStatus::Failed | EmailStatus::Dead
        );
        if !ends && retry_in.is_none() {
            return Err(StoreError::Invalid(
                "a mail ends as sent, failed or dead, or goes back with a retry delay".to_owned(),
            ));
        }
        let Some(row) = state.emails.iter_mut().find(|row| {
            row.delivery.id == id
                && row.delivery.status == EmailStatus::Sending
                && row.owner.as_deref() == Some(owner)
        }) else {
            return Ok(false);
        };
        row.owner = None;
        row.lease_expires_at = None;
        row.delivery.last_error = error.map(|e| e.chars().take(255).collect());
        if let Some(delay) = retry_in {
            row.delivery.status = EmailStatus::Pending;
            row.delivery.next_attempt_at = Some(add_std(now, delay));
        } else {
            row.delivery.status = status;
            row.delivery.next_attempt_at = None;
            if status == EmailStatus::Sent {
                row.delivery.sent_at = Some(now);
            }
        }
        Ok(true)
    }

    async fn count_sent_since(&self, since: OffsetDateTime) -> Result<u64, StoreError> {
        let state = self.tables()?;
        let count = state
            .emails
            .iter()
            .filter(|row| row.delivery.sent_at.is_some_and(|at| at >= since))
            .count();
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let before = state.emails.len();
        state
            .emails
            .retain(|row| row.delivery.created_at >= created_before);
        Ok(u64::try_from(before - state.emails.len()).unwrap_or(u64::MAX))
    }
}
