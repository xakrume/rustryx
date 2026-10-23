//! Ports: traits implemented by infrastructure crates and consumed by services,
//! formats and the HTTP layer.
//!
//! Conventions for every port:
//! - Methods return a typed error from [`crate::error`]; the error variants are the
//!   contract, callers never parse messages. A method that enforces a policy inside its
//!   transaction returns [`crate::error::MutationError`], whose `Violation` side carries the
//!   refusal and leaves nothing behind.
//! - Each async method documents whether cancelling its future is safe. Writes
//!   either complete atomically or leave nothing behind.
//! - A new method on an existing trait is provided with a fail closed default wherever a
//!   default can be safe (`Ok(false)`, `Deny`, `Unsupported`); it is required only where a
//!   default would hide a security or consistency bug.
//! - Every trait is dyn compatible: consumers hold `Arc<dyn Trait>`, never a concrete store
//!   (an in memory store implements many traits with the same method names, so a plain call
//!   on it is ambiguous).

pub mod auth;
pub mod blob;
pub mod distribution;
pub mod infra;
pub mod lifecycle;
pub mod projects;
pub mod repos;
pub mod scans;
pub mod storage;
pub mod system;

pub use crate::routing::RoutingRuleRepo;
pub use auth::{
    Authenticator, Coordinate, Decision, ExternalGroups, GroupResolver, ProjectVisibilityFilter,
    RbacChecker, ResourceRef,
};
pub use blob::{
    BlobCommit, BlobKey, BlobMeta, BlobRead, BlobStore, BlobStoreCapabilities, BlobWriter,
    ByteRange, KeyPage, PresignOptions, PresignedUrl, ResumableBlobStore, UploadHandle,
};
pub use distribution::{
    DeliveryOutcome, NewEmail, NotificationRepo, PromotionRepo, RegistryDependents, RegistryRepo,
    ReplicationRepo, WebhookDeliveryRepo, WebhookRepo,
};
pub use infra::{
    AuditQuery, AuditReadRepo, AuditSink, Clock, IdGen, InvalidationBus, InvalidationEvent,
    JobLease, JobLeaseRepo, LockBackend, LockGuard, NewJob, RevokedTokenRepo, StoredAuditEvent,
    SystemClock, UuidV7IdGen,
};
pub use lifecycle::{
    Candidate, CleanupPolicyRepo, DeleteResult, DownloadStatsRepo, GcRunRepo, ImmutabilityRepo,
    LifecycleHoldRepo, LifecycleRepo, LifecycleRunRepo, LifecycleScope, PolicyRef, QuotaRepo,
    RetentionPolicyRepo,
};
pub use projects::{
    LabelRepo, ProjectAccessRepo, ProjectDependents, ProjectMemberRepo, ProjectRepo,
    ProjectSummary, RobotFilter, RobotState, RobotTokenRepo,
};
pub use repos::{
    AssetRepo, BlobRepo, BlobStoreRepo, ComponentFilter, ComponentRepo, IdpMappingRepo,
    LoginAttemptRepo, LoginKey, LoginState, Page, PageRequest, PrivilegeRepo, RepositoryRepo,
    RoleRepo, SelectorRepo, TokenRepo, UserRepo,
};
pub use scans::{
    CveAllowlistRepo, ExportJob, ExportRepo, FindingFilter, HubFilter, HubRow, ScanRepo,
    ScannerDataRepo, ScannerRepo, SecurityHubRepo, SecurityPolicyRepo, SecurityStats, TrustRepo,
};
pub use storage::{BlobMoveRepo, BlobStoreMemberRepo, BlobStoreStatsRepo, VerifyRunRepo};
pub use system::{
    CertificateDependents, ConfigEntry, NodeRegistryRepo, OutboxRepo, SecretView, SystemConfigRepo,
    TaskDefinitionRepo, TaskRunRepo, TrustedCertificateRepo,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StatusCheckProvider;

    /// Instantiating the check for `dyn Trait` fails to compile when a trait stops being dyn
    /// compatible, which would break every `Arc<dyn Trait>` in the workspace.
    fn dyn_compatible<T: ?Sized>() {}

    #[test]
    fn every_port_is_dyn_compatible() {
        dyn_compatible::<dyn Authenticator>();
        dyn_compatible::<dyn GroupResolver>();
        dyn_compatible::<dyn RbacChecker>();
        dyn_compatible::<dyn BlobStore>();
        dyn_compatible::<dyn BlobWriter>();
        dyn_compatible::<dyn ResumableBlobStore>();
        dyn_compatible::<dyn AuditSink>();
        dyn_compatible::<dyn AuditReadRepo>();
        dyn_compatible::<dyn InvalidationBus>();
        dyn_compatible::<dyn LockBackend>();
        dyn_compatible::<dyn LockGuard>();
        dyn_compatible::<dyn RevokedTokenRepo>();
        dyn_compatible::<dyn JobLeaseRepo>();
        dyn_compatible::<dyn RoutingRuleRepo>();
        dyn_compatible::<dyn RepositoryRepo>();
        dyn_compatible::<dyn BlobStoreRepo>();
        dyn_compatible::<dyn BlobRepo>();
        dyn_compatible::<dyn ComponentRepo>();
        dyn_compatible::<dyn AssetRepo>();
        dyn_compatible::<dyn UserRepo>();
        dyn_compatible::<dyn RoleRepo>();
        dyn_compatible::<dyn PrivilegeRepo>();
        dyn_compatible::<dyn SelectorRepo>();
        dyn_compatible::<dyn TokenRepo>();
        dyn_compatible::<dyn IdpMappingRepo>();
        dyn_compatible::<dyn LoginAttemptRepo>();
        dyn_compatible::<dyn ProjectRepo>();
        dyn_compatible::<dyn ProjectMemberRepo>();
        dyn_compatible::<dyn ProjectAccessRepo>();
        dyn_compatible::<dyn LabelRepo>();
        dyn_compatible::<dyn RobotTokenRepo>();
        dyn_compatible::<dyn QuotaRepo>();
        dyn_compatible::<dyn ImmutabilityRepo>();
        dyn_compatible::<dyn LifecycleHoldRepo>();
        dyn_compatible::<dyn CleanupPolicyRepo>();
        dyn_compatible::<dyn RetentionPolicyRepo>();
        dyn_compatible::<dyn LifecycleRepo>();
        dyn_compatible::<dyn LifecycleRunRepo>();
        dyn_compatible::<dyn GcRunRepo>();
        dyn_compatible::<dyn DownloadStatsRepo>();
        dyn_compatible::<dyn ScannerRepo>();
        dyn_compatible::<dyn ScanRepo>();
        dyn_compatible::<dyn CveAllowlistRepo>();
        dyn_compatible::<dyn SecurityPolicyRepo>();
        dyn_compatible::<dyn TrustRepo>();
        dyn_compatible::<dyn SecurityHubRepo>();
        dyn_compatible::<dyn ExportRepo>();
        dyn_compatible::<dyn ScannerDataRepo>();
        dyn_compatible::<dyn RegistryRepo>();
        dyn_compatible::<dyn ReplicationRepo>();
        dyn_compatible::<dyn WebhookRepo>();
        dyn_compatible::<dyn WebhookDeliveryRepo>();
        dyn_compatible::<dyn NotificationRepo>();
        dyn_compatible::<dyn PromotionRepo>();
        dyn_compatible::<dyn BlobStoreMemberRepo>();
        dyn_compatible::<dyn BlobMoveRepo>();
        dyn_compatible::<dyn VerifyRunRepo>();
        dyn_compatible::<dyn BlobStoreStatsRepo>();
        dyn_compatible::<dyn OutboxRepo>();
        dyn_compatible::<dyn TaskDefinitionRepo>();
        dyn_compatible::<dyn TaskRunRepo>();
        dyn_compatible::<dyn NodeRegistryRepo>();
        dyn_compatible::<dyn SystemConfigRepo>();
        dyn_compatible::<dyn TrustedCertificateRepo>();
        dyn_compatible::<dyn StatusCheckProvider>();
    }
}
