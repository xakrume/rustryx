//! Domain types, ports (traits) and domain errors of Rustryx.
//!
//! This crate performs no I/O and knows nothing about HTTP, SQL or any concrete
//! blob store. Infrastructure crates implement the traits in [`ports`]; the
//! composition root (`rustryx-server`) is the only place that wires them.
//!
//! Dependency direction: `api -> services/formats -> core <- db, blob, auth, rbac`.

// Errors of every port are documented once in `ports` (module docs) and per error enum;
// repeating `# Errors` on each trait method adds noise without information.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap and expect: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit;
pub mod error;
pub mod events;
pub mod glob;
pub mod identity;
pub mod ids;
pub mod label;
pub mod lifecycle;
pub mod model;
pub mod names;
pub mod node;
pub mod notification;
pub mod ports;
pub mod project;
pub mod promotion;
pub mod quota;
pub mod registry;
pub mod replication;
pub mod rfc3339;
pub mod robot;
pub mod routing;
pub mod scan;
pub mod secret;
pub mod settings;
pub mod status;
pub mod storage;
pub mod tasks;
pub mod webhook;

#[cfg(test)]
mod testing;

pub use error::{
    AuthError, BlobError, DomainError, MutationError, PolicyViolation, StoreError, UpstreamError,
};
pub use secret::{Secret, SecretStatus};
