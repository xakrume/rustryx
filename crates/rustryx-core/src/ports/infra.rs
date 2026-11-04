//! Cross-cutting ports: clock, id generation, audit, distributed locks, cache invalidation,
//! revoked tokens, durable job queues, the audit read side and routing rules.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{AuditEvent, AuditKind};
use crate::error::StoreError;
use crate::events::CursorAdvance;
use crate::ids::{ProjectId, UserId};
use crate::ports::repos::{Page, PageRequest};

/// Source of the current time, injectable for tests.
pub trait Clock: Send + Sync {
    /// Current UTC time.
    fn now(&self) -> OffsetDateTime;
}

/// Wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Source of fresh identifiers, injectable for tests.
pub trait IdGen: Send + Sync {
    /// A new `UUIDv7`.
    fn next_uuid(&self) -> Uuid;
}

/// `UUIDv7` generator.
#[derive(Debug, Clone, Copy, Default)]
pub struct UuidV7IdGen;

impl IdGen for UuidV7IdGen {
    fn next_uuid(&self) -> Uuid {
        Uuid::now_v7()
    }
}

/// Destination of audit events. Implementations write structured JSON to stdout
/// and, when configured, to the database (monthly partitions).
#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Cancel safe. Failure to record must be logged by the caller; for security
    /// relevant events the caller decides whether to fail the request.
    async fn record(&self, event: AuditEvent) -> Result<(), StoreError>;
}

/// A held distributed lock.
#[async_trait]
pub trait LockGuard: Send {
    /// Extends the TTL. Returns `false` once the lock is lost (taken over after
    /// expiry); the holder must then stop writing. Cancel safe.
    async fn renew(&mut self) -> Result<bool, StoreError>;
    /// True when a renewal already observed loss of the lock.
    fn is_lost(&self) -> bool;
    /// Releases the lock. Idempotent.
    async fn release(self: Box<Self>) -> Result<(), StoreError>;
}

/// Distributed lock backend for singleton jobs. One backend per process for its
/// whole life: PostgreSQL advisory locks on a dedicated direct connection (default)
/// or Redis `SET NX PX`. There is no automatic switch between them.
#[async_trait]
pub trait LockBackend: Send + Sync {
    /// Tries once to take the lock. `None` means someone else holds it.
    /// Cancel safe. Errors mean the backend is unavailable: the caller does not
    /// start the singleton job.
    async fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<Box<dyn LockGuard>>, StoreError>;
}

/// What changed, so that nodes drop only the affected cache entries.
///
/// A node that does not know a payload decodes it as `Reset` (db.md section 9), so an N-1 node
/// stays correct when a newer node publishes a variant it has never heard of.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidationEvent {
    /// Roles, privileges, selectors, mappings or user roles changed.
    Rbac,
    /// One user's tokens or roles changed.
    User(Uuid),
    /// The subscription was re-established after a gap: drop everything.
    Reset,
    /// Project, membership, group mapping, label scope or visibility changed.
    Projects,
    /// A settings section or a security policy changed (settings, gate, allowlist, trust
    /// policy). Carries the `system_config` revision so a node knows what it has seen.
    Config(i64),
}

/// Cross-node cache invalidation (PostgreSQL `LISTEN`/`NOTIFY` channel
/// `rustryx_rbac`). After the listening connection drops and comes back the
/// stream yields [`InvalidationEvent::Reset`] first.
#[async_trait]
pub trait InvalidationBus: Send + Sync {
    /// Announces a change to all nodes, including this one.
    async fn publish(&self, event: InvalidationEvent) -> Result<(), StoreError>;
    /// Stream of events. Lagging subscribers get `Reset` rather than missing events silently.
    fn subscribe(&self) -> BoxStream<'static, InvalidationEvent>;
}

/// Persisted revocation of internal JWT ids. Replaces the in-memory list of
/// `rustryx-auth::internal_jwt`, which is per node and breaks revocation in a cluster.
#[async_trait]
pub trait RevokedTokenRepo: Send + Sync {
    /// Idempotent. `expires_at` is the JWT `exp`, after which the row is useless and may be
    /// purged. Cancel safe (one statement).
    async fn revoke(&self, tid: Uuid, expires_at: OffsetDateTime) -> Result<(), StoreError>;
    /// True when the id was revoked. Cancel safe.
    async fn is_revoked(&self, tid: Uuid) -> Result<bool, StoreError>;
    /// Removes the rows whose `expires_at` lies before `now`; returns how many. Cancel safe.
    async fn purge_expired(&self, now: OffsetDateTime) -> Result<u64, StoreError>;
}

/// A claimed row of a durable queue.
#[derive(Debug, Clone)]
pub struct JobLease {
    /// Row id.
    pub id: Uuid,
    /// Queue name.
    pub queue: String,
    /// Payload.
    pub payload: serde_json::Value,
    /// Claims so far, this one included.
    pub attempts: i32,
    /// When the lease runs out unless renewed.
    pub lease_expires_at: OffsetDateTime,
}

/// A job to insert in a batch.
#[derive(Debug, Clone)]
pub struct NewJob {
    /// Queue name.
    pub queue: String,
    /// A queued or running row with the same `(queue, dedupe_key)` makes the insert a no-op.
    pub dedupe_key: Option<String>,
    /// Payload.
    pub payload: serde_json::Value,
    /// Available after this delay.
    pub delay: Duration,
}

/// Durable queues with leases. Exact semantics of `rustryx-db::PgJobLeases`; time comes from
/// the database, never from the caller. Every method is one statement or one transaction and
/// cancel safe.
#[async_trait]
pub trait JobLeaseRepo: Send + Sync {
    /// `true` when a row was inserted; a queued or running row with the same
    /// `(queue, dedupe_key)` makes it a no-op.
    async fn enqueue(
        &self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError>;
    /// Like `enqueue`, available after `delay`.
    async fn enqueue_after(
        &self,
        queue: &str,
        dedupe_key: Option<&str>,
        payload: &serde_json::Value,
        delay: Duration,
    ) -> Result<bool, StoreError>;
    /// Claims the oldest available row (`SKIP LOCKED`), or `None` when the queue is empty.
    async fn claim(
        &self,
        queue: &str,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<JobLease>, StoreError>;
    /// Extends the lease. `false` once the lease was lost: stop and write nothing more.
    async fn heartbeat(&self, id: Uuid, owner: &str, lease: Duration) -> Result<bool, StoreError>;
    /// Marks the job done. `false` when the lease was lost.
    async fn complete(&self, id: Uuid, owner: &str) -> Result<bool, StoreError>;
    /// Puts the job back, available after `retry_in`, and records the error. `false` when the
    /// lease was lost.
    async fn release_for_retry(
        &self,
        id: Uuid,
        owner: &str,
        retry_in: Duration,
        error: &str,
    ) -> Result<bool, StoreError>;
    /// Terminal failure (state `failed`), keeps `error` (at most 500 characters). `false` when
    /// the lease was lost.
    async fn fail(&self, id: Uuid, owner: &str, error: &str) -> Result<bool, StoreError>;
    /// Inserts several jobs and, when `advance` is given, moves an outbox cursor in the same
    /// transaction (the atomic hand-off). Dedupe keys behave as in `enqueue`. Returns how many
    /// rows were inserted. A cursor that moved meanwhile makes the whole call a no-op that
    /// returns 0 and inserts nothing.
    async fn enqueue_batch(
        &self,
        jobs: &[NewJob],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError>;
    /// Queued plus running rows, for the queue depth gauge.
    async fn depth(&self, queue: &str) -> Result<u64, StoreError>;
    /// Removes `done` and `failed` rows older than `older_than`; returns how many.
    async fn purge_finished(&self, older_than: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Filter of the audit read side (NDJSON export, viewer, project log view). The sink stays
/// write only.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AuditQuery {
    /// Events at or after this time.
    pub from: Option<OffsetDateTime>,
    /// Events before this time.
    pub to: Option<OffsetDateTime>,
    /// Only these kinds; empty means all.
    pub kinds: Vec<AuditKind>,
    /// Only this actor id.
    pub actor_id: Option<UserId>,
    /// Only this actor name.
    pub actor_name: Option<String>,
    /// Only actions that start with this text.
    pub action_prefix: Option<String>,
    /// Only this target.
    pub target: Option<String>,
    /// Events for one repository: `target` is the name, or `name:` plus a path.
    pub repository: Option<String>,
    /// Only events scoped to this project.
    pub project_id: Option<ProjectId>,
    /// Only this outcome (`success` or `failure`).
    pub outcome: Option<String>,
}

impl AuditQuery {
    /// A query that matches every event.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// An audit event with the id the table gave it.
#[derive(Debug, Clone)]
pub struct StoredAuditEvent {
    /// Row id.
    pub id: Uuid,
    /// The event.
    pub event: AuditEvent,
}

/// Read side of the audit log.
#[async_trait]
pub trait AuditReadRepo: Send + Sync {
    /// Keyset paging newest first by `(event_time, id)`. A malformed token is
    /// `StoreError::Invalid`. Cancel safe, read only.
    async fn query(
        &self,
        query: &AuditQuery,
        page: &PageRequest,
    ) -> Result<Page<StoredAuditEvent>, StoreError>;
    /// Streams every match oldest first for NDJSON export; backpressured, cancel safe (a
    /// dropped stream ends the query).
    fn export(
        &self,
        query: &AuditQuery,
    ) -> BoxStream<'static, Result<StoredAuditEvent, StoreError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidation_events_compare_by_payload() {
        assert_eq!(InvalidationEvent::Config(7), InvalidationEvent::Config(7));
        assert_ne!(InvalidationEvent::Config(7), InvalidationEvent::Config(8));
        assert_ne!(InvalidationEvent::Projects, InvalidationEvent::Rbac);
        let user = Uuid::now_v7();
        assert_eq!(InvalidationEvent::User(user), InvalidationEvent::User(user));
    }

    #[test]
    fn audit_query_new_matches_everything() {
        let query = AuditQuery::new();
        assert!(query.from.is_none() && query.to.is_none() && query.kinds.is_empty());
        assert!(
            query.actor_id.is_none() && query.actor_name.is_none() && query.action_prefix.is_none()
        );
        assert!(query.target.is_none() && query.repository.is_none());
        assert!(query.project_id.is_none() && query.outcome.is_none());
    }

    #[test]
    fn system_clock_and_id_generator_work() {
        let first = UuidV7IdGen.next_uuid();
        assert_ne!(first, UuidV7IdGen.next_uuid());
        let before = OffsetDateTime::now_utc();
        assert!(SystemClock.now() >= before);
    }
}
