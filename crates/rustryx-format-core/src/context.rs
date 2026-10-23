//! What a [`crate::FormatHandler`] gets to work with: [`FormatContext`], the
//! repository [`Ports`], the [`BlobStoreRegistry`] and the tunable [`Limits`].

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::audit::{AuditEvent, AuditKind};
use rustryx_core::error::BlobError;
use rustryx_core::identity::Principal;
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::{Asset, BlobRecord, Repository};
use rustryx_core::ports::{
    AssetRepo, AuditSink, BlobRepo, BlobStore, BlobStoreRepo, Clock, ComponentRepo, IdGen,
    RbacChecker, RepositoryRepo, UuidV7IdGen,
};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::error::FormatError;
use crate::group::GroupPipeline;
use crate::hosted::{HostedPipeline, UploadReservations};
use crate::proxy::executor::UpstreamExecutor;
use crate::proxy::flight::Flights;
use crate::proxy::negative::NegativeCache;
use crate::proxy::{FillOutcome, ProxyPipeline};
use crate::registry::FormatRegistry;
use crate::request::ArtifactRequest;

/// Maps a repository to the blob store that holds its content.
///
/// Implemented by the blob crate (it owns the open stores); this crate only
/// consumes it. The lookup is synchronous: the registry hands out stores that
/// were opened at start-up or on configuration change.
pub trait BlobStoreRegistry: Send + Sync {
    /// The store for `repository` (its `blob_store_id`).
    ///
    /// # Errors
    /// [`BlobError::Unavailable`] when the store is unknown or failed to open.
    /// Returning an error instead of panicking is required: a repository can
    /// reference a store that was deleted or never came up.
    fn store_for(&self, repository: &Repository) -> Result<Arc<dyn BlobStore>, BlobError>;
}

/// Decision of the routing-rule gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingDecision {
    /// The path may be requested from this repository.
    Allow,
    /// A routing rule blocks the path: answer 404 and, for group members, skip the member.
    Block,
}

/// Evaluates routing rules (allow or block lists of paths per repository).
///
/// The core has no routing-rule port yet, so this is the seam: whoever owns the
/// rule store implements it. The default [`AllowAllRouting`] allows everything.
#[async_trait]
pub trait RoutingGate: Send + Sync {
    /// Decides for a repository and a classified request. Cancel safe.
    async fn check(
        &self,
        repository: &Repository,
        request: &ArtifactRequest,
    ) -> Result<RoutingDecision, FormatError>;
}

/// Routing gate that allows everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAllRouting;

#[async_trait]
impl RoutingGate for AllowAllRouting {
    async fn check(
        &self,
        _repository: &Repository,
        _request: &ArtifactRequest,
    ) -> Result<RoutingDecision, FormatError> {
        Ok(RoutingDecision::Allow)
    }
}

/// Why a blob lost its last reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanCause {
    /// A path was re-published and the previous blob was replaced.
    Replaced,
    /// The asset was deleted.
    Deleted,
}

/// Content lifecycle events for consumers such as the GC scheduler.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ContentEvent {
    /// A blob no asset references any more. The database already treats it as
    /// unreferenced (`BlobRepo::list_unreferenced` finds it); the event only lets a
    /// consumer collect it earlier than the next GC pass.
    BlobOrphaned {
        /// Repository the blob belonged to.
        repository: RepositoryId,
        /// The unreferenced blob.
        blob: BlobRecord,
        /// Why it is unreferenced.
        cause: OrphanCause,
    },
    /// An asset was published (hosted upload or proxy cache fill).
    AssetPublished {
        /// Repository.
        repository: RepositoryId,
        /// The asset after the commit.
        asset: Asset,
        /// True when the path existed before.
        replaced: bool,
    },
}

/// Receiver of [`ContentEvent`]s. Must not block: hand the event to a channel.
pub trait ContentEvents: Send + Sync {
    /// Called after the corresponding database commit.
    fn emit(&self, event: ContentEvent);
}

/// Event sink that drops everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopEvents;

impl ContentEvents for NoopEvents {
    fn emit(&self, _event: ContentEvent) {}
}

/// The ports the pipelines need, as `Arc<dyn Trait>`s. All fields are public so the
/// composition root (or a test) can swap any one of them.
#[derive(Clone)]
pub struct Ports {
    /// Repositories, used to resolve group members.
    pub repositories: Arc<dyn RepositoryRepo>,
    /// Blob store definitions, used for blob store quotas.
    pub blob_store_defs: Arc<dyn BlobStoreRepo>,
    /// Assets: the visibility commit of every upload and cache fill.
    pub assets: Arc<dyn AssetRepo>,
    /// Components: coordinates and group name claims.
    pub components: Arc<dyn ComponentRepo>,
    /// Blob metadata and usage sums for quotas.
    pub blobs: Arc<dyn BlobRepo>,
    /// Authorization of group members.
    pub rbac: Arc<dyn RbacChecker>,
    /// Audit sink for content changes.
    pub audit: Arc<dyn AuditSink>,
    /// Clock, for freshness decisions and timestamps.
    pub clock: Arc<dyn Clock>,
    /// Id source for blob ids and keys.
    pub ids: Arc<dyn IdGen>,
    /// Repository to blob store mapping.
    pub blob_stores: Arc<dyn BlobStoreRegistry>,
    /// Routing rules.
    pub routing: Arc<dyn RoutingGate>,
    /// Content lifecycle events.
    pub events: Arc<dyn ContentEvents>,
}

impl Ports {
    /// Builds the ports from one object that implements all the metadata repositories
    /// (the PostgreSQL store in production, the in-memory store in tests). Ids are
    /// `UUIDv7`, routing allows everything and events are dropped; replace the public
    /// fields to change that.
    #[must_use]
    pub fn from_store<S>(
        store: Arc<S>,
        rbac: Arc<dyn RbacChecker>,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
        blob_stores: Arc<dyn BlobStoreRegistry>,
    ) -> Self
    where
        S: RepositoryRepo + BlobStoreRepo + AssetRepo + ComponentRepo + BlobRepo + 'static,
    {
        Self {
            repositories: store.clone(),
            blob_store_defs: store.clone(),
            assets: store.clone(),
            components: store.clone(),
            blobs: store,
            rbac,
            audit,
            clock,
            ids: Arc::new(UuidV7IdGen),
            blob_stores,
            routing: Arc::new(AllowAllRouting),
            events: Arc::new(NoopEvents),
        }
    }
}

/// Tunable limits and timeouts. Defaults follow the normative numbers of the spec.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest blob accepted or cached (SRE-30: 5 GiB). Over it: 413.
    pub max_blob_bytes: u64,
    /// Largest index document the proxy caches and the largest member document a
    /// group reads. Index documents are the only thing ever buffered.
    pub max_document_bytes: u64,
    /// Total memory a group merge may hold across all member documents (64 MiB).
    pub group_merge_cap_bytes: u64,
    /// Deepest nesting of group-in-group resolution.
    pub max_group_depth: usize,
    /// Most members a group resolves.
    pub max_group_members: usize,
    /// Members queried concurrently when merging indexes.
    pub group_fanout: usize,
    /// Idle limit on a client request body (slowloris on uploads).
    pub body_idle_timeout: Duration,
    /// How long the proxy tee waits for a stalled client before it stops feeding
    /// it and carries on filling the cache alone.
    pub client_stall_timeout: Duration,
    /// Safety ceiling for a follower waiting on a singleflight fill. The fill
    /// itself ends by idle timeout, so this is only a backstop.
    pub singleflight_max_wait: Duration,
    /// Oldest cached copy that may be served stale when the upstream is down.
    pub stale_max_age: Duration,
    /// Capacity of the negative cache.
    pub negative_cache_capacity: u64,
    /// Permits for blocking work (index merges); permit before spawn, no queue.
    pub blocking_permits: usize,
    /// How long to wait for a blocking permit before answering 503.
    pub blocking_permit_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 5 * 1024 * 1024 * 1024,
            max_document_bytes: 64 * 1024 * 1024,
            group_merge_cap_bytes: 64 * 1024 * 1024,
            max_group_depth: 4,
            max_group_members: 64,
            group_fanout: 8,
            body_idle_timeout: Duration::from_secs(60),
            client_stall_timeout: Duration::from_secs(60),
            singleflight_max_wait: Duration::from_mins(30),
            stale_max_age: Duration::from_hours(720),
            negative_cache_capacity: 100_000,
            blocking_permits: 8,
            blocking_permit_wait: Duration::from_secs(5),
        }
    }
}

/// Engine-wide shared state. One instance per [`crate::FormatEngine`].
pub(crate) struct Shared {
    pub(crate) ports: Ports,
    pub(crate) registry: FormatRegistry,
    pub(crate) limits: Limits,
    pub(crate) upstream: UpstreamExecutor,
    pub(crate) flights: Flights<FillOutcome>,
    pub(crate) negative: NegativeCache,
    pub(crate) reservations: UploadReservations,
    pub(crate) blocking: Arc<Semaphore>,
    pub(crate) tasks: TaskTracker,
    pub(crate) shutdown: CancellationToken,
}

/// Request-scoped correlation data for audit records.
#[derive(Debug, Clone, Default)]
pub struct RequestMeta {
    /// Request id.
    pub request_id: Option<String>,
    /// Peer address.
    pub client_ip: Option<IpAddr>,
}

/// Everything a handler needs for one request.
///
/// Cheap to clone (a few `Arc`s). The pipelines borrow it: [`FormatContext::hosted`],
/// [`FormatContext::proxy`], [`FormatContext::group`]. Long-lived tasks (cache fills)
/// take a clone, never a reference.
#[derive(Clone)]
pub struct FormatContext {
    pub(crate) shared: Arc<Shared>,
    repository: Arc<Repository>,
    origin: Arc<Repository>,
    principal: Arc<Principal>,
    meta: Arc<RequestMeta>,
    trail: Arc<[RepositoryId]>,
}

impl FormatContext {
    pub(crate) fn new(
        shared: Arc<Shared>,
        repository: Arc<Repository>,
        principal: Arc<Principal>,
        meta: RequestMeta,
    ) -> Self {
        Self {
            shared,
            origin: Arc::clone(&repository),
            repository,
            principal,
            meta: Arc::new(meta),
            trail: Arc::from(Vec::new()),
        }
    }

    /// Context for resolving `member` on behalf of this (group) context.
    pub(crate) fn for_member(&self, member: Arc<Repository>) -> Self {
        let mut trail = self.trail.to_vec();
        trail.push(self.repository.id);
        Self {
            shared: Arc::clone(&self.shared),
            repository: member,
            origin: Arc::clone(&self.origin),
            principal: Arc::clone(&self.principal),
            meta: Arc::clone(&self.meta),
            trail: Arc::from(trail),
        }
    }

    /// The repository whose content is being read or written. While a group
    /// resolves a member this is the member.
    #[must_use]
    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    /// The repository the client addressed. Equal to [`FormatContext::repository`]
    /// except while a group resolves one of its members. Use this for every URL
    /// written into a document (archive URLs, tarball URLs): the client must keep
    /// talking to the repository it chose, not to a member behind it.
    #[must_use]
    pub fn origin_repository(&self) -> &Repository {
        &self.origin
    }

    /// The authenticated caller.
    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Request id for correlation, if the HTTP layer assigned one.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.meta.request_id.as_deref()
    }

    /// The repository ports.
    #[must_use]
    pub fn ports(&self) -> &Ports {
        &self.shared.ports
    }

    /// The tunable limits.
    #[must_use]
    pub fn limits(&self) -> &Limits {
        &self.shared.limits
    }

    /// The clock.
    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        self.shared.ports.clock.as_ref()
    }

    /// Current time from the injected clock.
    #[must_use]
    pub fn now(&self) -> OffsetDateTime {
        self.shared.ports.clock.now()
    }

    /// The blob store of this repository.
    ///
    /// # Errors
    /// [`FormatError::Blob`] when the registry cannot provide the store.
    pub fn store(&self) -> Result<Arc<dyn BlobStore>, FormatError> {
        Ok(self.shared.ports.blob_stores.store_for(&self.repository)?)
    }

    /// Token cancelled at shutdown. Long-lived work must select on it.
    #[must_use]
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.shared.shutdown
    }

    /// Tracker for tasks that must outlive the request but not the process.
    /// Spawn through it (never a bare `tokio::spawn`) so shutdown can wait for them.
    #[must_use]
    pub fn tasks(&self) -> &TaskTracker {
        &self.shared.tasks
    }

    /// The resilient upstream executor (SSRF policy, circuit breaker, concurrency cap,
    /// timeouts, retries, redirect re-validation) for calls that do not fit
    /// [`ProxyFetch`](crate::ProxyFetch), such as an OCI registry's token handshake.
    /// Build an [`UpstreamCall`](crate::proxy::executor::UpstreamCall) and run it here; never
    /// talk to an upstream with a client of your own.
    #[must_use]
    pub fn upstream(&self) -> &UpstreamExecutor {
        &self.shared.upstream
    }

    /// Hosted pipeline: upload, download, delete on this repository.
    #[must_use]
    pub fn hosted(&self) -> HostedPipeline<'_> {
        HostedPipeline::new(self)
    }

    /// Proxy pipeline: cached, deduplicated, resilient fetches from the upstream.
    #[must_use]
    pub fn proxy(&self) -> ProxyPipeline<'_> {
        ProxyPipeline::new(self)
    }

    /// Group pipeline: ordered resolution across members, index merging, name claims.
    #[must_use]
    pub fn group(&self) -> GroupPipeline<'_> {
        GroupPipeline::new(self)
    }

    pub(crate) fn trail(&self) -> &[RepositoryId] {
        &self.trail
    }

    /// Records a content-change audit event. A failing sink is logged, never
    /// propagated: the change is already committed and cannot be undone here.
    pub(crate) async fn audit_content(
        &self,
        action: &str,
        target: String,
        outcome: &str,
        detail: serde_json::Value,
    ) {
        let event = AuditEvent {
            at: self.now(),
            kind: AuditKind::ContentChange,
            actor_id: self.principal.user_id,
            actor_name: self.principal.username.clone(),
            client_ip: self.meta.client_ip.map(|ip| ip.to_string()),
            request_id: self.meta.request_id.clone(),
            action: action.to_owned(),
            target: Some(target),
            outcome: outcome.to_owned(),
            detail,
        };
        if let Err(error) = self.shared.ports.audit.record(event).await {
            tracing::error!(%error, action, "audit record for a content change failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_match_the_normative_numbers() {
        let limits = Limits::default();
        assert_eq!(limits.max_blob_bytes, 5 * 1024 * 1024 * 1024);
        assert_eq!(limits.group_merge_cap_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.body_idle_timeout, Duration::from_secs(60));
        assert!(limits.max_group_depth >= 2);
    }

    #[test]
    fn noop_events_accept_everything() {
        NoopEvents.emit(ContentEvent::BlobOrphaned {
            repository: RepositoryId::generate(),
            blob: BlobRecord {
                id: rustryx_core::ids::BlobId::generate(),
                blob_store_id: rustryx_core::ids::BlobStoreId::generate(),
                key: "k".into(),
                size_bytes: 1,
                checksums: rustryx_core::model::Checksums {
                    sha1: String::new(),
                    sha256: String::new(),
                    md5: String::new(),
                },
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
            cause: OrphanCause::Deleted,
        });
    }
}
