//! Errors of the harness itself (never of the code under test).

use rustryx_core::error::StoreError;

/// Why a harness helper could not do what a test asked.
///
/// The infallible helpers (`add_repository`, `add_user`, `start`) turn these into a panic with
/// the message below, because a test that cannot even build its fixture has nothing to assert;
/// the `try_*` variants return them for tests that want to look at the failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HarnessError {
    /// The repository name is not a valid `RepoName`.
    #[error("invalid repository name {name:?}: {reason}")]
    InvalidRepositoryName {
        /// The rejected name.
        name: String,
        /// Why `RepoName::parse` refused it.
        reason: String,
    },
    /// A role named in `add_user` or a group member does not exist.
    #[error("unknown role {0:?}; built in roles are nx-admin, nx-developer and nx-anonymous")]
    UnknownRole(String),
    /// The selector expression does not compile.
    #[error("selector expression refused: {0}")]
    InvalidSelector(String),
    /// A format handler could not be registered (duplicate format, no format).
    #[error("cannot register the format handler: {0}")]
    Registry(String),
    /// The engine could not be built.
    #[error("cannot build the format engine: {0}")]
    Engine(String),
    /// The in-memory store refused the change.
    #[error("the store refused the change: {0}")]
    Store(#[from] StoreError),
    /// The listener could not be bound.
    #[error("cannot bind the listener: {0}")]
    Bind(#[from] std::io::Error),
}
