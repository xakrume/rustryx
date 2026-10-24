//! In-process HTTP harness for running real package clients against the Rustryx format
//! handlers.
//!
//! [`TestServer`] mounts the REAL `FormatEngine` over the testkit's in-memory ports, with the
//! REAL `Rbac` for authorization and a small token authenticator, behind an HTTP listener on
//! an ephemeral port. A format crate registers its handler, creates repositories and users,
//! and points `mvn`, `npm`, `pip`, `twine`, `skopeo`, `docker`, `helm` or `dart pub` at
//! [`TestServer::repository_url`]. Authorization behaves like production: deny by default,
//! the anonymous gates, roles, privileges and content selectors.
//!
//! Real-client tests run only with `RUSTRYX_IT=1` and print `SKIP` otherwise; see the
//! [`tools`] module and `docs/testserver.md`.
//!
//! Nothing here is for production: the crate is a dev-dependency.

// Errors are documented once on `HarnessError`; repeating them on every helper adds noise.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod auth;
mod builder;
mod client;
mod data;
mod error;
mod http_util;
mod oci;
mod policy;
mod project;
mod raw;
mod serve;
mod store;
mod synthetic;
pub mod tools;
mod upstream;
mod venv;

pub use builder::{TestServer, TestServerBuilder};
pub use client::http_client;
pub use data::{ALL_ACTIONS, NewTestRepo, READ, READ_WRITE, TestPrivilege, TestUser};
pub use error::HarnessError;
pub use policy::{fast_executor_config, loopback_friendly_policy};
pub use project::TempProject;
pub use raw::RawLikeHandler;
pub use reqwest;
pub use rustryx_format_core::testing::{FakeUpstream, Reply, synthetic_range, synthetic_stream};
pub use rustryx_testkit::fixtures;
pub use synthetic::SyntheticBlobStore;
pub use tools::{
    Output, Tool, it_enabled, require, run, run_program, run_with_timeout, skip_reason,
};
pub use upstream::{RecordedCall, UpstreamStub};
