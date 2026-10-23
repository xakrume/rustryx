//! In-memory implementation of every metadata port.
//!
//! One `Mutex<State>` guards all tables, so an operation that touches several of them
//! (publishing an asset, deleting a repository with its content) is atomic exactly like
//! the single PostgreSQL transaction it stands for. No method awaits while holding the
//! lock: they are `async` only to satisfy the port signatures.

mod cascade;
mod catalog;
mod config;
mod content;
mod guards;
mod infra;
mod labels;
mod lifecycle;
mod outbox;
mod paging;
mod policies;
mod projects;
mod promotion;
mod publish;
mod quota;
mod rbac;
mod registries;
mod robots;
mod scans;
mod security;
mod seed;
mod state;
mod storage;
mod support;
mod system;
mod tasks;
mod users;
mod webhooks;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rustryx_core::error::StoreError;
use rustryx_core::model::BlobRecord;
use rustryx_core::ports::{Clock, InvalidationEvent, SystemClock};
use time::OffsetDateTime;

pub use self::paging::MAX_PAGE_LIMIT;
pub(crate) use self::paging::paginate;
pub use self::seed::builtin_role_id;
use self::state::State;
use crate::audit::MemAuditSink;
use crate::bus::MemInvalidationBus;
use crate::fixtures;
use crate::support::lock;

pub(crate) fn not_found(what: &str, id: impl fmt::Display) -> StoreError {
    StoreError::NotFound(format!("{what} {id}"))
}

pub(crate) fn conflict(message: impl Into<String>) -> StoreError {
    StoreError::Conflict(message.into())
}

/// All metadata ports over plain maps.
///
/// Cheap to clone: clones share the tables, the availability switch and the bus, so
/// one value can be handed to several services as different trait objects. Reference
/// integrity is deliberately looser than PostgreSQL where tests would otherwise need
/// ceremony (a repository need not exist before its assets); see `docs/testkit.md`.
#[derive(Clone)]
pub struct MemStore {
    state: Arc<Mutex<State>>,
    unavailable: Arc<AtomicBool>,
    clock: Arc<dyn Clock>,
    bus: Option<MemInvalidationBus>,
    audit: MemAuditSink,
}

impl MemStore {
    /// Creates an empty store timestamping with the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            unavailable: Arc::new(AtomicBool::new(false)),
            clock: Arc::new(SystemClock),
            bus: None,
            audit: MemAuditSink::new(),
        }
    }

    /// Creates a store seeded with the built-in roles `nx-admin`, `nx-developer` and
    /// `nx-anonymous` and the privileges they carry. Their ids are fixed, see
    /// [`builtin_role_id`].
    #[must_use]
    pub fn with_builtin_roles() -> Self {
        let store = Self::new();
        seed::insert_builtin_roles(&mut lock(&store.state));
        store
    }

    /// Replaces the clock used for `created_at`, `updated_at` and similar stamps.
    /// Call it before cloning the store: clones made earlier keep the old clock.
    #[must_use]
    pub fn with_clock(self, clock: Arc<dyn Clock>) -> Self {
        Self { clock, ..self }
    }

    /// Makes the store publish [`InvalidationEvent`]s on `bus` after RBAC relevant
    /// writes, the way the PostgreSQL implementation notifies other nodes.
    #[must_use]
    pub fn with_invalidation_bus(self, bus: MemInvalidationBus) -> Self {
        Self {
            bus: Some(bus),
            ..self
        }
    }

    /// Replaces the audit log the store reads (`AuditReadRepo`) and writes (`AuditSink`), so a
    /// store and a service that records through its own sink look at the same events.
    #[must_use]
    pub fn with_audit_sink(self, audit: MemAuditSink) -> Self {
        Self { audit, ..self }
    }

    /// The audit log of this store. The sink and the read side are one object: what a service
    /// records through it comes back through `AuditReadRepo` on the store.
    #[must_use]
    pub fn audit_sink(&self) -> MemAuditSink {
        self.audit.clone()
    }

    /// Seeds the blob store definition that [`fixtures::repository`] points at.
    #[must_use]
    pub fn with_default_blob_store(self) -> Self {
        let definition = fixtures::blob_store_def();
        lock(&self.state)
            .blob_stores
            .insert(definition.id, definition);
        self
    }

    /// Makes every port call fail with `StoreError::Unavailable` until set back to
    /// `false`. Lets tests drive the fail-closed paths (login guard, readiness).
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Inserts a blob row that no asset references, for garbage collection tests.
    /// Published blobs arrive through `AssetRepo::publish` instead.
    ///
    /// # Errors
    /// `Conflict` when a blob with that id exists, `Unavailable` when switched off.
    pub fn insert_blob(&self, record: BlobRecord) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let taken = state.blobs.values().any(|b| {
            b.id == record.id || (b.blob_store_id == record.blob_store_id && b.key == record.key)
        });
        if taken {
            return Err(conflict(format!("blob {} already exists", record.id)));
        }
        state.blobs.insert(record.id, record);
        Ok(())
    }

    /// Locks the tables, or fails when the store is switched off. Every port method
    /// goes through here, which is what makes `set_unavailable` total.
    fn tables(&self) -> Result<MutexGuard<'_, State>, StoreError> {
        self.check_available()?;
        let now = self.now();
        let mut state = lock(&self.state);
        state.now = state::Stamp(now);
        Ok(state)
    }

    /// The availability switch alone, for calls that touch no table of `State` (the audit
    /// log lives in its own object) but must still fail while the store is switched off.
    fn check_available(&self) -> Result<(), StoreError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable(
                "memory store marked unavailable".to_owned(),
            ));
        }
        Ok(())
    }

    fn now(&self) -> OffsetDateTime {
        self.clock.now()
    }

    fn notify(&self, event: InvalidationEvent) {
        if let Some(bus) = &self.bus {
            bus.emit(event);
        }
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemStore").finish_non_exhaustive()
    }
}
