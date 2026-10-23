//! Cross-cutting ports: clock, id generation, audit, distributed locks and
//! cache invalidation.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::AuditEvent;
use crate::error::StoreError;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidationEvent {
    /// Roles, privileges, selectors, mappings or user roles changed.
    Rbac,
    /// One user's tokens or roles changed.
    User(Uuid),
    /// The subscription was re-established after a gap: drop everything.
    Reset,
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
