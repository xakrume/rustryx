//! `PostgreSQL` metadata store of Rustryx.
//!
//! This crate implements the store ports of `rustryx-core` on `sqlx`/`PostgreSQL` 16:
//! repositories, blob and component metadata (including the policy enforcing writes of core
//! revision 2 and the garbage collection step that deletes the row before the object), identity
//! and RBAC data, login counters, the audit sink and its read side, durable job queues, revoked
//! token ids, routing rules, sealed runtime settings, distributed locks (advisory locks) and the
//! cache invalidation bus (`LISTEN`/`NOTIFY`). It also owns the schema: forward-only migrations
//! applied by [`migrate::migrate`] (the `rustryx migrate` command) and never by `serve`.
//!
//! Business rules do not live here. Constraints that protect an invariant even against
//! a buggy caller (uniqueness, role nesting cycles, read-only built-ins, append-only
//! audit) are enforced by the schema; everything else is the services' job.
//!
//! See `docs/db.md` for the schema, the ADRs and the operational notes.

// Errors of every port are documented once in `rustryx_core::ports` and per error enum;
// repeating `# Errors` on each method adds noise without information.
#![allow(clippy::missing_errors_doc)]
// Unit tests may unwrap and expect: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit;
pub mod audit_read;
pub mod bootstrap;
pub mod config;
pub mod error;
pub mod invalidation;
pub mod lease;
pub mod lock;
pub mod maintenance;
pub mod migrate;
pub mod outbox;
pub mod pool;
pub mod repos;
pub mod revoked;
pub mod sealer;
pub mod store;
pub mod system_config;

mod cursor;

pub use audit::PgAuditSink;
pub use audit_read::PgAuditRead;
pub use bootstrap::bootstrap_admin;
pub use config::PgConfig;
pub use invalidation::{PgInvalidationBus, RBAC_CHANNEL};
pub use lease::PgJobLeases;
pub use lock::PgLockBackend;
pub use maintenance::{AuditMaintenance, AuditMaintenanceReport};
pub use migrate::{MigrateReport, SchemaStatus, migrate, schema_status};
pub use pool::{connect, connect_lazy, ping};
pub use repos::{PgImmutabilityRepo, PgRetentionPolicyRepo};
pub use revoked::PgRevokedTokens;
pub use sealer::{AeadSealer, KeyEntry, SecretSealer};
pub use store::PgStore;
pub use system_config::PgSystemConfig;
