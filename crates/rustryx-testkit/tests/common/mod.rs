//! Shared setup of the `MemStore` integration tests.
//!
//! `MemStore` implements dozens of traits with methods of the same name (`create`, `get`,
//! `list`), so a plain method call on it is ambiguous. The `Ports` extension trait hands out
//! the store as one trait object per port, which is also how the composition root holds it.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared by several test binaries, each of which uses a part; a panic is the failure signal"
)]

use std::sync::Arc;

use rustryx_core::ids::{BlobId, ProjectId, RepositoryId};
use rustryx_core::model::{NewAsset, RepoFormat, RepoType, Repository};
use rustryx_core::ports::{
    AssetRepo, AuditReadRepo, AuditSink, BlobMoveRepo, BlobRepo, BlobStoreMemberRepo,
    BlobStoreRepo, BlobStoreStatsRepo, CleanupPolicyRepo, Clock, ComponentRepo, CveAllowlistRepo,
    DownloadStatsRepo, ExportRepo, GcRunRepo, ImmutabilityRepo, JobLeaseRepo, LabelRepo,
    LifecycleHoldRepo, LifecycleRepo, LifecycleRunRepo, NodeRegistryRepo, NotificationRepo,
    OutboxRepo, PageRequest, ProjectAccessRepo, ProjectMemberRepo, ProjectRepo, PromotionRepo,
    QuotaRepo, RegistryRepo, ReplicationRepo, RepositoryRepo, RetentionPolicyRepo,
    RevokedTokenRepo, RobotTokenRepo, RoutingRuleRepo, ScanRepo, ScannerDataRepo, ScannerRepo,
    SecurityHubRepo, SecurityPolicyRepo, SystemConfigRepo, TaskDefinitionRepo, TaskRunRepo,
    TokenRepo, TrustRepo, TrustedCertificateRepo, UserRepo, VerifyRunRepo, WebhookDeliveryRepo,
    WebhookRepo,
};
use rustryx_core::project::{Project, ProjectName, ProjectVisibility};
use rustryx_testkit::fixtures::{blob_record, repository};
use rustryx_testkit::{FixedClock, MemStore};
use time::OffsetDateTime;
use time::macros::datetime;

macro_rules! ports {
    ($($method:ident: $port:ident),* $(,)?) => {
        /// One accessor per port: the store as `Arc<dyn Port>`.
        pub trait Ports {
            $(fn $method(&self) -> Arc<dyn $port>;)*
        }

        impl Ports for MemStore {
            $(fn $method(&self) -> Arc<dyn $port> {
                Arc::new(self.clone())
            })*
        }
    };
}

ports! {
    repos: RepositoryRepo,
    blob_stores: BlobStoreRepo,
    blobs: BlobRepo,
    components: ComponentRepo,
    assets: AssetRepo,
    users: UserRepo,
    tokens: TokenRepo,
    audit_log: AuditReadRepo,
    audit_sink: AuditSink,
    revoked: RevokedTokenRepo,
    routing_rules: RoutingRuleRepo,
    jobs: JobLeaseRepo,
    projects: ProjectRepo,
    members: ProjectMemberRepo,
    access: ProjectAccessRepo,
    labels: LabelRepo,
    robots: RobotTokenRepo,
    quotas: QuotaRepo,
    immutability: ImmutabilityRepo,
    holds: LifecycleHoldRepo,
    cleanup_policies: CleanupPolicyRepo,
    retention: RetentionPolicyRepo,
    lifecycle: LifecycleRepo,
    lifecycle_runs: LifecycleRunRepo,
    gc_runs: GcRunRepo,
    download_stats: DownloadStatsRepo,
    scanners: ScannerRepo,
    scans: ScanRepo,
    allowlist: CveAllowlistRepo,
    security_policies: SecurityPolicyRepo,
    trust: TrustRepo,
    hub: SecurityHubRepo,
    exports: ExportRepo,
    datasets: ScannerDataRepo,
    registries: RegistryRepo,
    replication: ReplicationRepo,
    webhooks: WebhookRepo,
    deliveries: WebhookDeliveryRepo,
    notifications: NotificationRepo,
    promotions: PromotionRepo,
    store_members: BlobStoreMemberRepo,
    blob_moves: BlobMoveRepo,
    verify_runs: VerifyRunRepo,
    store_stats: BlobStoreStatsRepo,
    outbox: OutboxRepo,
    task_definitions: TaskDefinitionRepo,
    task_runs: TaskRunRepo,
    nodes: NodeRegistryRepo,
    config: SystemConfigRepo,
    certificates: TrustedCertificateRepo,
}

/// A store on a clock the test moves, with the fixture blob store in place.
pub struct Env {
    pub store: MemStore,
    pub clock: FixedClock,
}

impl Env {
    /// The instant the store stamps rows with right now.
    pub fn clock_now(&self) -> OffsetDateTime {
        self.clock.now()
    }
}

pub fn env() -> Env {
    let clock = FixedClock::default();
    let store = MemStore::new()
        .with_clock(Arc::new(clock.clone()))
        .with_default_blob_store();
    Env { store, clock }
}

/// The instant `FixedClock::default()` stands at.
pub const T0: OffsetDateTime = datetime!(2026-01-01 00:00:00 UTC);

pub fn page(limit: u32) -> PageRequest {
    PageRequest { limit, after: None }
}

/// Creates a hosted raw repository in the store.
pub async fn hosted(store: &MemStore, name: &str) -> Repository {
    let created = repository(name, RepoFormat::Raw, RepoType::Hosted);
    store.repos().create(&created).await.unwrap();
    created
}

/// Creates a private project in the store, without a first admin.
pub async fn project(store: &MemStore, name: &str) -> Project {
    let created = Project::new(ProjectId::generate(), ProjectName::parse(name).unwrap(), T0);
    store.projects().create(&created, None).await.unwrap();
    created
}

pub async fn public_project(store: &MemStore, name: &str) -> Project {
    let mut created = project(store, name).await;
    created.visibility = ProjectVisibility::Public;
    store
        .projects()
        .update(&created, created.version)
        .await
        .unwrap();
    store.projects().get(created.id).await.unwrap().unwrap()
}

/// A publish of `content` at `path` with a blob key that is unique per call.
pub fn asset(repo: RepositoryId, path: &str, content: &[u8]) -> NewAsset {
    NewAsset::new(
        repo,
        path,
        "application/octet-stream",
        blob_record(&format!("key-{path}-{}", BlobId::generate()), content),
    )
}
