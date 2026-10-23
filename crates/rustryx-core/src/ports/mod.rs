//! Ports: traits implemented by infrastructure crates and consumed by services,
//! formats and the HTTP layer.
//!
//! Conventions for every port:
//! - Methods return a typed error from [`crate::error`]; the error variants are the
//!   contract, callers never parse messages.
//! - Each async method documents whether cancelling its future is safe. Writes
//!   either complete atomically or leave nothing behind.

pub mod auth;
pub mod blob;
pub mod infra;
pub mod repos;

pub use auth::{Authenticator, Decision, ExternalGroups, GroupResolver, RbacChecker, ResourceRef};
pub use blob::{BlobCommit, BlobKey, BlobMeta, BlobRead, BlobStore, BlobWriter, ByteRange};
pub use infra::{
    AuditSink, Clock, IdGen, InvalidationBus, InvalidationEvent, LockBackend, LockGuard,
    SystemClock, UuidV7IdGen,
};
pub use repos::{
    AssetRepo, BlobRepo, BlobStoreRepo, ComponentFilter, ComponentRepo, IdpMappingRepo,
    LoginAttemptRepo, LoginKey, LoginState, Page, PageRequest, PrivilegeRepo, RepositoryRepo,
    RoleRepo, SelectorRepo, TokenRepo, UserRepo,
};
