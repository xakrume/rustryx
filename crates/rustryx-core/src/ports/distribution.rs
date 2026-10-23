//! Ports of registries, replication, webhooks, notifications and promotion.
//!
//! Contract as in [`crate::ports`]: every method is one statement or one transaction and
//! therefore cancel safe (a cancelled future leaves the old or the new state, never a mix).
//! Methods that take `advance` move an outbox cursor in the same transaction (the atomic
//! hand-off of the event outbox). When `advance` is given and its
//! compare and set fails, because a duplicate consumer or a consumer that lost its lock moved
//! the cursor first, the method writes nothing and returns zero: the batch belongs to whoever
//! moved the cursor, and the caller reads the next batch from the new position.

use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::error::StoreError;
use crate::events::{CursorAdvance, EventDraft, EventKind};
use crate::ids::{
    ComponentId, EmailDeliveryId, EventId, ProjectId, PromotionId, RegistryId,
    ReplicationExecutionId, ReplicationRuleId, ReplicationTaskId, RepositoryId, SubscriptionId,
    UserId, WebhookDeliveryId, WebhookId,
};
use crate::notification::{EmailDelivery, EmailStatus, NotificationSubscription};
use crate::ports::repos::{Page, PageRequest};
use crate::promotion::{
    PromotionDecision, PromotionItem, PromotionItemStatus, PromotionRequest, PromotionStatus,
    PromotionTarget,
};
use crate::registry::{Registry, RegistryTest};
use crate::replication::{
    ReplicationExecution, ReplicationRule, ReplicationTask, ReplicationTaskStatus,
};
use crate::secret::Secret;
use crate::tasks::TaskLogLine;
use crate::webhook::{DeliveryStatus, NotifyScope, Webhook, WebhookDelivery};

/// What still depends on a registry.
#[derive(Debug, Clone)]
pub struct RegistryDependents {
    /// Proxy repositories that resolve their upstream through it.
    pub proxy_repositories: Vec<RepositoryId>,
    /// Replication rules that use it.
    pub replication_rules: Vec<ReplicationRuleId>,
}

/// Registries.
#[async_trait]
pub trait RegistryRepo: Send + Sync {
    /// Inserts. Credentials are sealed (purpose `registry.credential`). `Conflict` when the
    /// name exists.
    async fn create(&self, registry: &Registry) -> Result<(), StoreError>;
    /// Finds by id. Credentials are unsealed for runtime use; API responses must not embed
    /// the returned type.
    async fn get(&self, id: RegistryId) -> Result<Option<Registry>, StoreError>;
    /// Lists all registries.
    async fn list(&self) -> Result<Vec<Registry>, StoreError>;
    /// Replaces a registry and rewrites the inline copy of every bound proxy repository in the
    /// same transaction. `Conflict` on a stale `expected_version`.
    async fn update(&self, registry: &Registry, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while a proxy repository, a replication rule or a scanner uses it;
    /// `dependents` names them.
    async fn delete(&self, id: RegistryId) -> Result<(), StoreError>;
    /// What still depends on the registry.
    async fn dependents(&self, id: RegistryId) -> Result<RegistryDependents, StoreError>;
    /// Stores the result of a connection test.
    async fn record_test(&self, id: RegistryId, test: &RegistryTest) -> Result<(), StoreError>;
}

/// Replication rules, executions and tasks.
#[async_trait]
pub trait ReplicationRepo: Send + Sync {
    /// Inserts a rule. `Conflict` when the name exists.
    async fn create_rule(&self, rule: &ReplicationRule) -> Result<(), StoreError>;
    /// Finds a rule.
    async fn get_rule(&self, id: ReplicationRuleId) -> Result<Option<ReplicationRule>, StoreError>;
    /// Lists all rules.
    async fn list_rules(&self) -> Result<Vec<ReplicationRule>, StoreError>;
    /// `Conflict` while an execution of the rule is queued or running (the rule cannot change
    /// under a run), and on a stale `expected_version`.
    async fn update_rule(
        &self,
        rule: &ReplicationRule,
        expected_version: i64,
    ) -> Result<(), StoreError>;
    /// Deletes a rule.
    async fn delete_rule(&self, id: ReplicationRuleId) -> Result<(), StoreError>;
    /// `None` when an execution of that rule is queued or running (the partial unique index):
    /// one active run per rule.
    async fn start_execution(
        &self,
        execution: &ReplicationExecution,
    ) -> Result<Option<ReplicationExecution>, StoreError>;
    /// Finds an execution.
    async fn get_execution(
        &self,
        id: ReplicationExecutionId,
    ) -> Result<Option<ReplicationExecution>, StoreError>;
    /// Lists executions, newest first, optionally of one rule.
    async fn list_executions(
        &self,
        rule: Option<ReplicationRuleId>,
        page: &PageRequest,
    ) -> Result<Page<ReplicationExecution>, StoreError>;
    /// Sets the stop flag. `false` for a finished execution.
    async fn request_stop(&self, id: ReplicationExecutionId) -> Result<bool, StoreError>;
    /// Adds tasks to an execution (at most 1000 per call; more is `Invalid`).
    async fn add_tasks(&self, tasks: &[ReplicationTask]) -> Result<(), StoreError>;
    /// Event triggered rules: creates tasks for matching events in the rule's open event
    /// execution (created if none was open in the last minute) and advances the cursor, in
    /// one transaction. Returns how many tasks were created.
    async fn create_event_tasks(
        &self,
        rule: ReplicationRuleId,
        tasks: &[ReplicationTask],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError>;
    /// `SKIP LOCKED` claim of a due task of a running execution; lease as given.
    async fn claim_task(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<ReplicationTask>, StoreError>;
    /// Extends the lease. `false` once it was lost.
    async fn heartbeat_task(
        &self,
        id: ReplicationTaskId,
        owner: &str,
        lease: Duration,
    ) -> Result<bool, StoreError>;
    /// Ends or retries a task. `retry_in` puts it back as pending. The events are written in
    /// the same transaction. `false` when the lease was lost.
    #[allow(
        clippy::too_many_arguments,
        reason = "the signature is the contract of the design document; a parameter struct would only move the same eight values"
    )]
    async fn finish_task(
        &self,
        id: ReplicationTaskId,
        owner: &str,
        status: ReplicationTaskStatus,
        bytes: u64,
        error: Option<&str>,
        retry_in: Option<Duration>,
        events: &[EventDraft],
    ) -> Result<bool, StoreError>;
    /// Lists the tasks of an execution.
    async fn tasks(
        &self,
        execution: ReplicationExecutionId,
        page: &PageRequest,
    ) -> Result<Page<ReplicationTask>, StoreError>;
    /// Appends log lines to a task.
    async fn append_task_log(
        &self,
        id: ReplicationTaskId,
        lines: &[TaskLogLine],
    ) -> Result<(), StoreError>;
    /// Reads the log of a task.
    async fn read_task_log(
        &self,
        id: ReplicationTaskId,
        after_seq: i32,
        limit: u32,
    ) -> Result<Vec<TaskLogLine>, StoreError>;
    /// Marks executions with no unfinished task as finished and writes the
    /// `replication.finished` event in the same transaction. Returns how many.
    async fn settle_executions(&self, now: OffsetDateTime) -> Result<u64, StoreError>;
    /// Removes executions finished before the cutoff; returns how many.
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Webhooks.
#[async_trait]
pub trait WebhookRepo: Send + Sync {
    /// Inserts. The secret and the auth header are sealed. At most 20 per scope: `Conflict`
    /// beyond.
    async fn create(&self, webhook: &Webhook) -> Result<(), StoreError>;
    /// Finds by id. Secrets are unsealed for runtime use.
    async fn get(&self, id: WebhookId) -> Result<Option<Webhook>, StoreError>;
    /// Lists the webhooks of a scope, or all.
    async fn list(&self, scope: Option<NotifyScope>) -> Result<Vec<Webhook>, StoreError>;
    /// Replaces a webhook (the secret is changed only by `rotate_secret`). `Conflict` on a
    /// stale `expected_version`.
    async fn update(&self, webhook: &Webhook, expected_version: i64) -> Result<(), StoreError>;
    /// Deletes a webhook.
    async fn delete(&self, id: WebhookId) -> Result<(), StoreError>;
    /// New secret, the old one invalid at once. Returns the new version.
    async fn rotate_secret(
        &self,
        id: WebhookId,
        secret: &Secret,
        expected_version: i64,
    ) -> Result<i64, StoreError>;
    /// Enabled, not suspended webhooks that would receive an event (system, or its project,
    /// or its repository).
    async fn matching(
        &self,
        kind: EventKind,
        project: Option<ProjectId>,
        repository: Option<RepositoryId>,
    ) -> Result<Vec<Webhook>, StoreError>;
    /// True when any enabled webhook subscribes to `kind`; cached 60 s by the caller.
    async fn any_subscribes_to(&self, kind: EventKind) -> Result<bool, StoreError>;
    /// Records a delivery result: resets or increments `consecutive_failures` and suspends the
    /// webhook after `suspend_after` dead deliveries in a row.
    async fn record_outcome(
        &self,
        id: WebhookId,
        success: bool,
        suspend_after: u32,
    ) -> Result<(), StoreError>;
}

/// How a delivery attempt ended, handed to `WebhookDeliveryRepo::finish`.
#[derive(Debug, Clone)]
pub struct DeliveryOutcome {
    /// The new state.
    pub status: DeliveryStatus,
    /// HTTP status of the attempt.
    pub status_code: Option<u16>,
    /// Latency of the attempt.
    pub latency_ms: Option<u32>,
    /// Stable error code.
    pub error: Option<String>,
    /// Response excerpt, control characters removed.
    pub excerpt: Option<String>,
    /// When the next attempt is due.
    pub next_attempt_at: Option<OffsetDateTime>,
}

/// Webhook deliveries.
#[async_trait]
pub trait WebhookDeliveryRepo: Send + Sync {
    /// One pending delivery per `(webhook, event)` and the cursor advance, in one transaction
    /// (the outbox hand-off). Returns how many rows were created.
    async fn create_for_events(
        &self,
        rows: &[(WebhookId, EventId, EventKind)],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError>;
    /// `SKIP LOCKED` claim of a due `pending` row, or a `delivering` row whose lease expired.
    async fn claim(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<WebhookDelivery>, StoreError>;
    /// Records the outcome of an attempt. `false` when the lease was lost.
    async fn finish(
        &self,
        id: WebhookDeliveryId,
        owner: &str,
        outcome: &DeliveryOutcome,
    ) -> Result<bool, StoreError>;
    /// Lists the deliveries of a webhook, newest first.
    async fn list(
        &self,
        webhook: WebhookId,
        page: &PageRequest,
    ) -> Result<Page<WebhookDelivery>, StoreError>;
    /// Counts for `webhook_deliveries_pending`.
    async fn pending_count(&self) -> Result<u64, StoreError>;
    /// Removes deliveries created before the cutoff; returns how many.
    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError>;
}

/// An e-mail the consumer asks the repository to create.
#[derive(Debug, Clone)]
pub struct NewEmail {
    /// The subscription that produced it.
    pub subscription_id: Option<SubscriptionId>,
    /// The event.
    pub event_id: EventId,
    /// The resolved address.
    pub to: String,
    /// The kind of the event.
    pub kind: EventKind,
    /// What the message is about (a repository, a token), for suppression of duplicates.
    pub subject_key: String,
}

/// Notification subscriptions and e-mail deliveries.
#[async_trait]
pub trait NotificationRepo: Send + Sync {
    /// Inserts or replaces a subscription.
    async fn upsert(
        &self,
        subscription: &NotificationSubscription,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError>;
    /// Lists the subscriptions of a scope, or all.
    async fn list(
        &self,
        scope: Option<NotifyScope>,
    ) -> Result<Vec<NotificationSubscription>, StoreError>;
    /// Deletes a subscription.
    async fn delete(&self, id: SubscriptionId) -> Result<(), StoreError>;
    /// Enabled subscriptions that would receive an event.
    async fn matching(
        &self,
        kind: EventKind,
        project: Option<ProjectId>,
        repository: Option<RepositoryId>,
    ) -> Result<Vec<NotificationSubscription>, StoreError>;
    /// Resolved recipients as addresses, with the cursor advance in one transaction; identical
    /// `(address, event kind, subject key)` within 10 minutes are `Suppressed` rows. Returns
    /// how many rows were created.
    async fn create_for_events(
        &self,
        rows: &[NewEmail],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError>;
    /// `SKIP LOCKED` claim of a due e-mail.
    async fn claim(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<EmailDelivery>, StoreError>;
    /// Records the outcome of an attempt. `false` when the lease was lost.
    async fn finish(
        &self,
        id: EmailDeliveryId,
        owner: &str,
        status: EmailStatus,
        error: Option<&str>,
        retry_in: Option<Duration>,
    ) -> Result<bool, StoreError>;
    /// E-mails sent since `since`, for the hourly cap.
    async fn count_sent_since(&self, since: OffsetDateTime) -> Result<u64, StoreError>;
    /// Removes e-mails created before the cutoff; returns how many.
    async fn purge(&self, created_before: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Promotion.
#[async_trait]
pub trait PromotionRepo: Send + Sync {
    /// Inserts or replaces the promotion rule of a target repository.
    async fn set_target(
        &self,
        target: &PromotionTarget,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError>;
    /// The rule of a target repository.
    async fn get_target(
        &self,
        repository: RepositoryId,
    ) -> Result<Option<PromotionTarget>, StoreError>;
    /// Creates the request and its items (at most 10 000) in one transaction, writes the
    /// `promotion.requested` event.
    async fn create(
        &self,
        request: &PromotionRequest,
        items: &[ComponentId],
        events: &[EventDraft],
    ) -> Result<(), StoreError>;
    /// Finds a request.
    async fn get(&self, id: PromotionId) -> Result<Option<PromotionRequest>, StoreError>;
    /// Lists requests, newest first.
    async fn list(
        &self,
        repository: Option<RepositoryId>,
        status: Option<PromotionStatus>,
        page: &PageRequest,
    ) -> Result<Page<PromotionRequest>, StoreError>;
    /// Stores the decision (marking whether it counts) and, when the counted approvals reach
    /// the requirement, moves the request to `Approved` and enqueues the executor job
    /// `promotion:<id>`, all in one transaction. A rejection ends the request. Returns the new
    /// status.
    async fn decide(
        &self,
        decision: &PromotionDecision,
        events: &[EventDraft],
    ) -> Result<PromotionStatus, StoreError>;
    /// The decisions of a request.
    async fn decisions(&self, id: PromotionId) -> Result<Vec<PromotionDecision>, StoreError>;
    /// Cancels a pending request; only the requester may.
    async fn cancel(&self, id: PromotionId, by: UserId) -> Result<(), StoreError>;
    /// Lists the items of a request.
    async fn items(
        &self,
        id: PromotionId,
        page: &PageRequest,
    ) -> Result<Page<PromotionItem>, StoreError>;
    /// Marks the request running. `false` when it already runs or is not approved.
    async fn mark_running(&self, id: PromotionId, at: OffsetDateTime) -> Result<bool, StoreError>;
    /// Records the state of one item.
    async fn set_item(
        &self,
        id: PromotionId,
        component: ComponentId,
        status: PromotionItemStatus,
        reason: Option<&str>,
    ) -> Result<(), StoreError>;
    /// Ends the request, with its events in the same transaction.
    async fn finish(
        &self,
        id: PromotionId,
        status: PromotionStatus,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError>;
    /// Expires pending requests past their expiry; returns how many.
    async fn expire(&self, now: OffsetDateTime) -> Result<u64, StoreError>;
}
