//! Test doubles for the wave 2 pieces: a spy over the asset port, scripted policy hooks, blob
//! stores that spy on or fake the resumable and group behaviour, and an in-memory routing rule
//! repository.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rustryx_core::error::{BlobError, MutationError, PolicyViolation, StoreError};
use rustryx_core::ids::{AssetId, BlobStoreId, ComponentId, RepositoryId, RoutingRuleId};
use rustryx_core::model::{
    Asset, BlobRecord, BlobStoreKind, DownloadTick, NewAsset, NewAssetLink, PublishOutcome,
    WriteOptions,
};
use rustryx_core::ports::{
    AssetRepo, BlobCommit, BlobKey, BlobRead, BlobStore, BlobStoreCapabilities, BlobWriter,
    ByteRange, Page, PageRequest, ResumableBlobStore, RoutingRuleRepo, UploadHandle,
};
use rustryx_core::routing::RoutingRule;
use rustryx_format_core::{BlobStoreRegistry, ContentPolicy, DeleteIntent, WriteIntent};
use rustryx_testkit::MemBlobStore;
use time::OffsetDateTime;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a port call should fail, since `StoreError` is not `Clone`.
#[derive(Debug, Clone, Copy)]
pub enum Fault {
    NotFound,
    Unavailable,
}

impl Fault {
    pub fn error(self) -> StoreError {
        match self {
            Self::NotFound => StoreError::NotFound("asset".into()),
            Self::Unavailable => StoreError::Unavailable("asset store down".into()),
        }
    }
}

/// Wraps the asset port: records what the pipeline asks, and refuses or fails on request.
#[derive(Default)]
pub struct AssetSpy {
    inner: OnceLock<Arc<dyn AssetRepo>>,
    pub publish_options: Mutex<Vec<WriteOptions>>,
    pub delete_options: Mutex<Vec<WriteOptions>>,
    pub touch_fresh_calls: Mutex<Vec<(AssetId, OffsetDateTime)>>,
    pub download_batches: Mutex<Vec<Vec<DownloadTick>>>,
    pub refuse_publish: Mutex<Option<PolicyViolation>>,
    pub refuse_delete: Mutex<Option<PolicyViolation>>,
    pub touch_fresh_fault: Mutex<Option<Fault>>,
    pub record_downloads_fault: AtomicBool,
    /// Makes `record_downloads` never return, like a database that stopped answering.
    pub record_downloads_hang: AtomicBool,
    /// Fails `record_downloads` once the batch counter reaches this call number (1 based).
    pub fail_record_call: AtomicUsize,
    pub record_calls: AtomicUsize,
}

impl AssetSpy {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Puts the spy in front of the asset port `ports` already has.
    pub fn install(self: &Arc<Self>, ports: &mut rustryx_format_core::Ports) {
        let _ = self.inner.set(Arc::clone(&ports.assets));
        ports.assets = Arc::clone(self) as Arc<dyn AssetRepo>;
    }

    fn inner(&self) -> &Arc<dyn AssetRepo> {
        self.inner.get().expect("the spy was installed")
    }

    pub fn touches(&self) -> usize {
        lock(&self.touch_fresh_calls).len()
    }

    pub fn publishes(&self) -> usize {
        lock(&self.publish_options).len()
    }
}

#[async_trait]
impl AssetRepo for AssetSpy {
    async fn get_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<Asset>, StoreError> {
        self.inner().get_by_path(repository_id, path).await
    }

    async fn get(&self, id: AssetId) -> Result<Option<Asset>, StoreError> {
        self.inner().get(id).await
    }

    async fn publish_with(
        &self,
        new: &NewAsset,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        lock(&self.publish_options).push(options.clone());
        if let Some(violation) = lock(&self.refuse_publish).clone() {
            return Err(violation.into());
        }
        self.inner().publish_with(new, options).await
    }

    async fn link(
        &self,
        link: &NewAssetLink,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        self.inner().link(link, options).await
    }

    async fn list(
        &self,
        repository_id: RepositoryId,
        prefix: &str,
        page: &PageRequest,
    ) -> Result<Page<Asset>, StoreError> {
        self.inner().list(repository_id, prefix, page).await
    }

    async fn list_by_component(&self, id: ComponentId) -> Result<Vec<Asset>, StoreError> {
        self.inner().list_by_component(id).await
    }

    async fn delete_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<BlobRecord>, StoreError> {
        self.inner().delete_by_path(repository_id, path).await
    }

    async fn delete_by_path_checked(
        &self,
        repository_id: RepositoryId,
        path: &str,
        options: &WriteOptions,
    ) -> Result<Option<BlobRecord>, MutationError> {
        lock(&self.delete_options).push(options.clone());
        if let Some(violation) = lock(&self.refuse_delete).clone() {
            return Err(violation.into());
        }
        self.inner()
            .delete_by_path_checked(repository_id, path, options)
            .await
    }

    async fn touch_downloaded(
        &self,
        batch: &[(AssetId, OffsetDateTime)],
    ) -> Result<(), StoreError> {
        self.inner().touch_downloaded(batch).await
    }

    async fn touch_fresh(&self, id: AssetId, at: OffsetDateTime) -> Result<(), StoreError> {
        lock(&self.touch_fresh_calls).push((id, at));
        if let Some(fault) = *lock(&self.touch_fresh_fault) {
            return Err(fault.error());
        }
        self.inner().touch_fresh(id, at).await
    }

    async fn record_downloads(&self, batch: &[DownloadTick]) -> Result<(), StoreError> {
        let call = self.record_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.record_downloads_hang.load(Ordering::SeqCst) {
            futures::future::pending::<()>().await;
        }
        if self.record_downloads_fault.load(Ordering::SeqCst)
            || self.fail_record_call.load(Ordering::SeqCst) == call
        {
            return Err(StoreError::Unavailable("download counters down".into()));
        }
        lock(&self.download_batches).push(batch.to_vec());
        self.inner().record_downloads(batch).await
    }
}

/// What a scripted hook answers.
#[derive(Debug, Clone)]
pub enum Verdict {
    Allow,
    Refuse(PolicyViolation),
    /// The store behind the hook is down.
    CannotDecide,
}

impl Verdict {
    fn result(&self) -> Result<(), MutationError> {
        match self {
            Self::Allow => Ok(()),
            Self::Refuse(violation) => Err(violation.clone().into()),
            Self::CannotDecide => Err(StoreError::Unavailable("policy store down".into()).into()),
        }
    }
}

/// What the hook was asked about a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenWrite {
    pub repository: String,
    pub principal: String,
    pub path: String,
    pub declared_length: Option<u64>,
    pub existing: bool,
    pub component: Option<String>,
}

/// A policy hook with scripted answers that remembers the intents it saw.
pub struct ScriptedPolicy {
    pub write: Mutex<Verdict>,
    pub delete: Mutex<Verdict>,
    pub writes: Mutex<Vec<SeenWrite>>,
    pub deletes: Mutex<Vec<(String, u64)>>,
}

impl ScriptedPolicy {
    pub fn new(write: Verdict, delete: Verdict) -> Arc<Self> {
        Arc::new(Self {
            write: Mutex::new(write),
            delete: Mutex::new(delete),
            writes: Mutex::default(),
            deletes: Mutex::default(),
        })
    }

    pub fn allowing() -> Arc<Self> {
        Self::new(Verdict::Allow, Verdict::Allow)
    }

    pub fn set_write(&self, verdict: Verdict) {
        *lock(&self.write) = verdict;
    }

    pub fn set_delete(&self, verdict: Verdict) {
        *lock(&self.delete) = verdict;
    }
}

#[async_trait]
impl ContentPolicy for ScriptedPolicy {
    async fn check_write(&self, intent: &WriteIntent<'_>) -> Result<(), MutationError> {
        lock(&self.writes).push(SeenWrite {
            repository: intent.repository.name.to_string(),
            principal: intent.principal.username.clone(),
            path: intent.path.as_str().to_owned(),
            declared_length: intent.declared_length,
            existing: intent.existing.is_some(),
            component: intent.component.map(|c| c.name.clone()),
        });
        lock(&self.write).result()
    }

    async fn check_delete(&self, intent: &DeleteIntent<'_>) -> Result<(), MutationError> {
        lock(&self.deletes).push((intent.path.as_str().to_owned(), intent.asset.size_bytes));
        lock(&self.delete).result()
    }
}

/// A store that wraps a [`MemBlobStore`], optionally without the resumable capability, and
/// records and faults the resumable calls.
pub struct SpyStore {
    pub inner: MemBlobStore,
    resumable: bool,
    /// `(offset, size)` of every `append`.
    pub appends: Mutex<Vec<(u64, usize)>>,
    pub finalizes: AtomicUsize,
    pub aborts: AtomicUsize,
    pub fail_finalize: AtomicBool,
    pub finalize_delay: Mutex<Duration>,
    pub placement: Mutex<Option<BlobStoreId>>,
}

impl SpyStore {
    /// A store with the resumable capability.
    pub fn resumable(inner: MemBlobStore) -> Arc<Self> {
        Arc::new(Self::build(inner, true))
    }

    /// A store without it, like a plain object store adapter.
    pub fn plain(inner: MemBlobStore) -> Arc<Self> {
        Arc::new(Self::build(inner, false))
    }

    fn build(inner: MemBlobStore, resumable: bool) -> Self {
        Self {
            inner,
            resumable,
            appends: Mutex::default(),
            finalizes: AtomicUsize::new(0),
            aborts: AtomicUsize::new(0),
            fail_finalize: AtomicBool::new(false),
            finalize_delay: Mutex::new(Duration::ZERO),
            placement: Mutex::new(None),
        }
    }

    fn base(&self) -> Result<&dyn ResumableBlobStore, BlobError> {
        self.inner
            .resumable()
            .ok_or(BlobError::Unsupported("resumable"))
    }
}

struct PlacedWriter {
    inner: Box<dyn BlobWriter>,
    placement: Option<BlobStoreId>,
}

#[async_trait]
impl BlobWriter for PlacedWriter {
    async fn write(&mut self, chunk: Bytes) -> Result<(), BlobError> {
        self.inner.write(chunk).await
    }

    async fn commit(self: Box<Self>) -> Result<BlobCommit, BlobError> {
        let placement = self.placement;
        let mut commit = self.inner.commit().await?;
        commit.placement = placement;
        Ok(commit)
    }

    async fn abort(self: Box<Self>) -> Result<(), BlobError> {
        self.inner.abort().await
    }
}

#[async_trait]
impl BlobStore for SpyStore {
    fn kind(&self) -> BlobStoreKind {
        self.inner.kind()
    }

    async fn begin(
        &self,
        key: &BlobKey,
        size_hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        let inner = self.inner.begin(key, size_hint).await?;
        Ok(Box::new(PlacedWriter {
            inner,
            placement: *lock(&self.placement),
        }))
    }

    async fn get(&self, key: &BlobKey, range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
        self.inner.get(key, range).await
    }

    async fn head(
        &self,
        key: &BlobKey,
    ) -> Result<Option<rustryx_core::ports::BlobMeta>, BlobError> {
        self.inner.head(key).await
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), BlobError> {
        self.inner.delete(key).await
    }

    async fn list_stale_staging(&self, min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        self.inner.list_stale_staging(min_age).await
    }

    async fn health(&self) -> Result<(), BlobError> {
        self.inner.health().await
    }

    fn capabilities(&self) -> BlobStoreCapabilities {
        let mut caps = self.inner.capabilities();
        caps.resumable = self.resumable;
        caps
    }

    fn resumable(&self) -> Option<&dyn ResumableBlobStore> {
        self.resumable.then_some(self as &dyn ResumableBlobStore)
    }
}

#[async_trait]
impl ResumableBlobStore for SpyStore {
    async fn begin_resumable(&self, key: &BlobKey) -> Result<UploadHandle, BlobError> {
        self.base()?.begin_resumable(key).await
    }

    async fn append(
        &self,
        handle: &UploadHandle,
        offset: u64,
        chunk: Bytes,
    ) -> Result<(), BlobError> {
        let size = chunk.len();
        let result = self.base()?.append(handle, offset, chunk).await;
        if result.is_ok() {
            lock(&self.appends).push((offset, size));
        }
        result
    }

    async fn length(&self, handle: &UploadHandle) -> Result<u64, BlobError> {
        self.base()?.length(handle).await
    }

    async fn finalize(&self, handle: UploadHandle) -> Result<BlobCommit, BlobError> {
        self.finalizes.fetch_add(1, Ordering::SeqCst);
        let delay = *lock(&self.finalize_delay);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if self.fail_finalize.load(Ordering::SeqCst) {
            return Err(BlobError::Unavailable("finalize refused".into()));
        }
        let mut commit = self.base()?.finalize(handle).await?;
        commit.placement = *lock(&self.placement);
        Ok(commit)
    }

    async fn abort(&self, handle: UploadHandle) -> Result<(), BlobError> {
        self.aborts.fetch_add(1, Ordering::SeqCst);
        self.base()?.abort(handle).await
    }
}

/// Stands for a group store: it takes writes and reports the member that took the bytes, and it
/// refuses every read and delete, as the group adapter of the storage stream does.
pub struct GroupLikeStore {
    pub member: MemBlobStore,
    pub member_id: BlobStoreId,
    pub refused_calls: AtomicUsize,
}

impl GroupLikeStore {
    pub fn new(member: MemBlobStore, member_id: BlobStoreId) -> Arc<Self> {
        Arc::new(Self {
            member,
            member_id,
            refused_calls: AtomicUsize::new(0),
        })
    }

    fn refuse<T>(&self) -> Result<T, BlobError> {
        self.refused_calls.fetch_add(1, Ordering::SeqCst);
        Err(BlobError::Unsupported("group"))
    }
}

#[async_trait]
impl BlobStore for GroupLikeStore {
    fn kind(&self) -> BlobStoreKind {
        BlobStoreKind::Group
    }

    async fn begin(
        &self,
        key: &BlobKey,
        size_hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        let inner = self.member.begin(key, size_hint).await?;
        Ok(Box::new(PlacedWriter {
            inner,
            placement: Some(self.member_id),
        }))
    }

    async fn get(&self, _key: &BlobKey, _range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
        self.refuse()
    }

    async fn head(
        &self,
        _key: &BlobKey,
    ) -> Result<Option<rustryx_core::ports::BlobMeta>, BlobError> {
        self.refuse()
    }

    async fn delete(&self, _key: &BlobKey) -> Result<(), BlobError> {
        self.refuse()
    }

    async fn list_stale_staging(&self, _min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        self.refuse()
    }

    async fn health(&self) -> Result<(), BlobError> {
        Ok(())
    }
}

/// A registry over named stores that remembers which ids were asked for.
pub struct MapRegistry {
    stores: HashMap<BlobStoreId, Arc<dyn BlobStore>>,
    pub requested: Mutex<Vec<BlobStoreId>>,
}

impl MapRegistry {
    pub fn new(stores: Vec<(BlobStoreId, Arc<dyn BlobStore>)>) -> Arc<Self> {
        Arc::new(Self {
            stores: stores.into_iter().collect(),
            requested: Mutex::default(),
        })
    }

    pub fn requested(&self) -> Vec<BlobStoreId> {
        lock(&self.requested).clone()
    }
}

#[async_trait]
impl BlobStoreRegistry for MapRegistry {
    async fn store_by_id(&self, id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError> {
        lock(&self.requested).push(id);
        self.stores
            .get(&id)
            .cloned()
            .ok_or_else(|| BlobError::Unavailable("unknown blob store".into()))
    }
}

/// An in-memory routing rule repository that counts reads and can fail or be slow.
#[derive(Default)]
pub struct FakeRules {
    rules: Mutex<HashMap<RoutingRuleId, RoutingRule>>,
    pub gets: AtomicUsize,
    pub fail: AtomicBool,
    pub get_delay: Mutex<Duration>,
}

impl FakeRules {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn put(&self, rule: &RoutingRule) {
        lock(&self.rules).insert(rule.id, rule.clone());
    }

    pub fn remove(&self, id: RoutingRuleId) {
        lock(&self.rules).remove(&id);
    }

    pub fn reads(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RoutingRuleRepo for FakeRules {
    async fn create(&self, rule: &RoutingRule) -> Result<(), StoreError> {
        self.put(rule);
        Ok(())
    }

    async fn get(&self, id: RoutingRuleId) -> Result<Option<RoutingRule>, StoreError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let delay = *lock(&self.get_delay);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("rules down".into()));
        }
        Ok(lock(&self.rules).get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<RoutingRule>, StoreError> {
        Ok(lock(&self.rules).values().cloned().collect())
    }

    async fn update(&self, rule: &RoutingRule, _expected_version: i64) -> Result<(), StoreError> {
        self.put(rule);
        Ok(())
    }

    async fn delete(&self, id: RoutingRuleId) -> Result<(), StoreError> {
        self.remove(id);
        Ok(())
    }

    async fn assigned_to(&self, _id: RoutingRuleId) -> Result<Vec<RepositoryId>, StoreError> {
        Ok(Vec::new())
    }
}
