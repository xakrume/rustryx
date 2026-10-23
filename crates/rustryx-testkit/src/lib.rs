//! In-memory implementations of the Rustryx core ports and shared test fixtures.
//!
//! Other crates (`rbac`, `auth`, `tasks`, `formats`, `api`) depend on this crate in
//! `[dev-dependencies]` so their tests run without PostgreSQL or S3:
//!
//! - [`MemStore`] implements every metadata port of `rustryx_core::ports` (core revision 2:
//!   content, identity, projects, lifecycle, supply chain, distribution, storage and the
//!   system ports), with the guards that PostgreSQL runs as triggers: immutability, holds,
//!   quota and read only mode.
//! - [`MemBlobStore`] implements `BlobStore` and `ResumableBlobStore` with real streaming
//!   semantics.
//! - [`FixedClock`], [`MemLockBackend`], [`MemAuditSink`] and [`MemInvalidationBus`]
//!   cover the cross-cutting ports.
//! - [`fixtures`] builds repositories, projects, users, principals and blob rows.
//!
//! The crate is a test utility, but it follows the port documentation to the letter, so
//! a test that passes here is evidence about the contract and not about the double.
//! Where it is looser than PostgreSQL the difference is listed in `docs/testkit.md`.

// Errors of every port are documented once on the port (see `rustryx_core::ports`);
// repeating them on the in-memory helpers adds noise without information.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap and expect: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod audit;
mod blob_store;
mod bus;
mod clock;
pub mod fixtures;
mod lock;
mod store;
mod support;

pub use audit::MemAuditSink;
pub use blob_store::MemBlobStore;
pub use bus::MemInvalidationBus;
pub use clock::FixedClock;
pub use lock::MemLockBackend;
pub use store::{MAX_PAGE_LIMIT, MemStore, builtin_role_id};
