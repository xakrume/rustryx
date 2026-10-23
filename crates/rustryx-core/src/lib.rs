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
pub mod identity;
pub mod ids;
pub mod model;
pub mod names;
pub mod ports;
pub mod secret;

pub use error::{AuthError, BlobError, DomainError, StoreError};
pub use secret::Secret;
