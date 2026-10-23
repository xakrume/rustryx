//! Authorization for Rustryx: who may do what to which content.
//!
//! This crate decides, it never authenticates (README section 8.6). See
//! `docs/rbac.md` for the decision model, the selector language and the
//! decision records.
//!
//! # Wiring
//!
//! The composition root builds one [`Rbac`] over the role and selector ports,
//! starts its invalidation listener on the bus and hands the authorizer to the
//! HTTP layer as `Arc<dyn RbacChecker>`:
//!
//! ```
//! use std::sync::Arc;
//!
//! use rustryx_core::identity::{Action, Principal};
//! use rustryx_core::model::RepoFormat;
//! use rustryx_core::ports::RbacChecker;
//! use rustryx_core::ports::auth::{Decision, ResourceRef};
//! use rustryx_rbac::{Rbac, RbacConfig};
//! use rustryx_testkit::{MemInvalidationBus, MemStore};
//! use tokio_util::sync::CancellationToken;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let store = MemStore::with_builtin_roles();
//! let bus = MemInvalidationBus::new();
//! let rbac = Rbac::new(
//!     Arc::new(store.clone()),
//!     Arc::new(store),
//!     &RbacConfig::default(),
//! );
//!
//! // Keep the handle and await it on shutdown.
//! let shutdown = CancellationToken::new();
//! let listener = rbac.spawn_invalidation_listener(&bus, shutdown.child_token());
//!
//! let checker: Arc<dyn RbacChecker> = Arc::new(rbac);
//! let resource = ResourceRef {
//!     repository: "public".into(),
//!     format: RepoFormat::Raw,
//!     path: "docs/readme.txt".into(),
//!     coordinate: None,
//! };
//! // `nx-anonymous` may read but never write.
//! let anonymous = Principal::anonymous();
//! assert_eq!(
//!     checker.check(&anonymous, Action::Read, &resource).await.unwrap(),
//!     Decision::Allow
//! );
//! assert_eq!(
//!     checker.check(&anonymous, Action::Write, &resource).await.unwrap(),
//!     Decision::Deny
//! );
//!
//! shutdown.cancel();
//! listener.join().await.unwrap();
//! # }
//! ```

#![allow(
    // Errors of the `RbacChecker` port are documented once on the trait and on
    // `Rbac`; repeating a `# Errors` section on every method adds noise.
    clippy::missing_errors_doc
)]
// Tests may unwrap and expect: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cache;
mod config;
mod invalidation;
mod path;
mod rbac;
pub mod scope;
pub mod selector;
mod state;

pub use cache::MAX_DECISION_TTL;
pub use config::RbacConfig;
pub use invalidation::InvalidationListener;
pub use path::selector_path;
pub use rbac::Rbac;
pub use scope::{Scope, ScopeError, TokenScopes, parse_scope};
pub use selector::{
    Program, RestrictedEngine, SelectorCompileError, SelectorEngine, SelectorEvalError,
    SelectorInput, SelectorLimits,
};
