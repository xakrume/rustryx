//! In-memory blob store with real streaming semantics.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::error::BlobError;
use rustryx_core::model::{BlobStoreKind, Checksums};
use rustryx_core::ports::{
    BlobCommit, BlobKey, BlobMeta, BlobRead, BlobStore, BlobWriter, ByteRange, Clock, SystemClock,
};
use time::OffsetDateTime;

use crate::support::{StreamHasher, add_std, lock};

/// Size of the chunks a read yields. Small on purpose: tests with a few kilobytes
/// already see several chunks and exercise the consumer's streaming path.
const READ_CHUNK: usize = 8 * 1024;

struct Staged {
    key: String,
    started_at: OffsetDateTime,
}

#[derive(Default)]
struct State {
    objects: HashMap<String, Bytes>,
    staging: BTreeMap<u64, Staged>,
    next_staging_id: u64,
}

struct Shared {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    unavailable: AtomicBool,
    keep_abandoned: AtomicBool,
}

impl Shared {
    fn check_available(&self) -> Result<(), BlobError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(BlobError::Unavailable(
                "memory blob store marked unavailable".to_owned(),
            ));
        }
        Ok(())
    }
}

/// [`BlobStore`] that keeps objects in memory.
///
/// Behaves like the real stores where it matters to callers: a write hashes the bytes
/// as they arrive, the object is invisible until `commit`, `abort` discards it, reads
/// are chunked, and ranges follow HTTP semantics. Clones share the same objects.
#[derive(Clone)]
pub struct MemBlobStore {
    shared: Arc<Shared>,
}

impl MemBlobStore {
    /// Creates an empty store that stamps staging entries with the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    /// Creates an empty store using `clock` for staging ages, so a test can make an
    /// abandoned upload "stale" by advancing a `FixedClock`.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                clock,
                unavailable: AtomicBool::new(false),
                keep_abandoned: AtomicBool::new(false),
            }),
        }
    }

    /// Number of committed objects.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.shared.state).objects.len()
    }

    /// True when no object is committed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Keys of committed objects, sorted.
    #[must_use]
    pub fn keys(&self) -> Vec<BlobKey> {
        let state = lock(&self.shared.state);
        let sorted: BTreeSet<&String> = state.objects.keys().collect();
        sorted
            .into_iter()
            .map(|k| BlobKey::new(k.as_str()))
            .collect()
    }

    /// True when `key` holds a committed object.
    #[must_use]
    pub fn contains(&self, key: &BlobKey) -> bool {
        lock(&self.shared.state).objects.contains_key(key.as_str())
    }

    /// Number of writes that began and neither committed nor aborted nor (by default)
    /// were dropped.
    #[must_use]
    pub fn staging_count(&self) -> usize {
        lock(&self.shared.state).staging.len()
    }

    /// Seeds a committed object without going through a writer and returns its digests.
    pub fn put(&self, key: &BlobKey, data: impl Into<Bytes>) -> BlobCommit {
        let data: Bytes = data.into();
        let mut hasher = StreamHasher::new();
        hasher.update(&data);
        let (size_bytes, checksums) = hasher.finish();
        lock(&self.shared.state)
            .objects
            .insert(key.as_str().to_owned(), data);
        BlobCommit {
            size_bytes,
            checksums,
        }
    }

    /// Whole content of a committed object, for assertions. Production code must stream.
    #[must_use]
    pub fn snapshot(&self, key: &BlobKey) -> Option<Bytes> {
        lock(&self.shared.state).objects.get(key.as_str()).cloned()
    }

    /// Makes every operation fail with `BlobError::Unavailable` (and a commit in
    /// flight fail without publishing its object) until set back to `false`.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.shared.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// When `true`, a writer dropped without commit or abort leaves its staging entry
    /// behind, like a crashed process leaves a temp file. `list_stale_staging` then
    /// reports it and `delete` of its key clears it. Off by default: a dropped writer
    /// is cleaned up at once, so tests do not leak staging by accident.
    pub fn set_keep_abandoned_staging(&self, keep: bool) {
        self.shared.keep_abandoned.store(keep, Ordering::SeqCst);
    }
}

impl Default for MemBlobStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemBlobStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemBlobStore")
            .field("objects", &self.len())
            .field("staging", &self.staging_count())
            .finish_non_exhaustive()
    }
}

/// Selects the requested bytes of `data` with HTTP range semantics: an `end` past the
/// last byte is clamped, a `start` at or past the end is not satisfiable.
fn select_range(data: &Bytes, range: Option<ByteRange>) -> Result<Bytes, BlobError> {
    let Some(range) = range else {
        return Ok(data.clone());
    };
    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
    if range.start >= len {
        return Err(BlobError::RangeNotSatisfiable);
    }
    let last = range.end.map_or(len - 1, |end| end.min(len - 1));
    if last < range.start {
        return Err(BlobError::RangeNotSatisfiable);
    }
    let start = usize::try_from(range.start).map_err(|_| BlobError::RangeNotSatisfiable)?;
    let end = usize::try_from(last).map_err(|_| BlobError::RangeNotSatisfiable)?;
    Ok(data.slice(start..=end))
}

/// Yields `data` in [`READ_CHUNK`] pieces, one per poll. Slicing a `Bytes` is a
/// reference-count bump, so the stream holds the whole object without copying it.
fn chunked(data: Bytes) -> BoxStream<'static, Result<Bytes, BlobError>> {
    futures::stream::unfold(data, |mut rest| async move {
        if rest.is_empty() {
            return None;
        }
        let take = rest.len().min(READ_CHUNK);
        let chunk = rest.split_to(take);
        Some((Ok(chunk), rest))
    })
    .boxed()
}

#[async_trait]
impl BlobStore for MemBlobStore {
    fn kind(&self) -> BlobStoreKind {
        BlobStoreKind::Local
    }

    async fn begin(
        &self,
        key: &BlobKey,
        _size_hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        self.shared.check_available()?;
        let started_at = self.shared.clock.now();
        let staging_id = {
            let mut state = lock(&self.shared.state);
            state.next_staging_id += 1;
            let id = state.next_staging_id;
            state.staging.insert(
                id,
                Staged {
                    key: key.as_str().to_owned(),
                    started_at,
                },
            );
            id
        };
        Ok(Box::new(MemBlobWriter {
            shared: Arc::clone(&self.shared),
            key: key.clone(),
            staging_id,
            buffer: Vec::new(),
            hasher: StreamHasher::new(),
            finished: false,
        }))
    }

    async fn get(&self, key: &BlobKey, range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
        self.shared.check_available()?;
        let data = lock(&self.shared.state)
            .objects
            .get(key.as_str())
            .cloned()
            .ok_or(BlobError::NotFound)?;
        let total_size = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let selected = select_range(&data, range)?;
        Ok(BlobRead {
            total_size,
            stream: chunked(selected),
        })
    }

    async fn head(&self, key: &BlobKey) -> Result<Option<BlobMeta>, BlobError> {
        self.shared.check_available()?;
        let size = lock(&self.shared.state)
            .objects
            .get(key.as_str())
            .map(Bytes::len);
        Ok(size.map(|len| BlobMeta {
            size_bytes: u64::try_from(len).unwrap_or(u64::MAX),
        }))
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), BlobError> {
        self.shared.check_available()?;
        let mut state = lock(&self.shared.state);
        state.objects.remove(key.as_str());
        // GC deletes by the keys `list_stale_staging` returned, so the same call has to
        // clear abandoned staging for the key, not only the committed object.
        state.staging.retain(|_, staged| staged.key != key.as_str());
        Ok(())
    }

    async fn list_stale_staging(&self, min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        self.shared.check_available()?;
        let now = self.shared.clock.now();
        let state = lock(&self.shared.state);
        let keys: BTreeSet<&str> = state
            .staging
            .values()
            .filter(|staged| add_std(staged.started_at, min_age) <= now)
            .map(|staged| staged.key.as_str())
            .collect();
        Ok(keys.into_iter().map(BlobKey::new).collect())
    }

    async fn health(&self) -> Result<(), BlobError> {
        self.shared.check_available()
    }
}

/// Writer returned by [`MemBlobStore::begin`].
///
/// Data lives in a private buffer until `commit`, which is what makes the object
/// invisible beforehand: nothing touches the shared object map earlier.
struct MemBlobWriter {
    shared: Arc<Shared>,
    key: BlobKey,
    staging_id: u64,
    buffer: Vec<u8>,
    hasher: StreamHasher,
    /// Set by `commit` and `abort` so that `Drop` knows the entry is already settled.
    finished: bool,
}

impl MemBlobWriter {
    fn leave_staging(&mut self) {
        self.finished = true;
        lock(&self.shared.state).staging.remove(&self.staging_id);
    }
}

impl Drop for MemBlobWriter {
    fn drop(&mut self) {
        if !self.finished && !self.shared.keep_abandoned.load(Ordering::SeqCst) {
            lock(&self.shared.state).staging.remove(&self.staging_id);
        }
    }
}

#[async_trait]
impl BlobWriter for MemBlobWriter {
    async fn write(&mut self, chunk: Bytes) -> Result<(), BlobError> {
        self.shared.check_available()?;
        self.hasher.update(&chunk);
        self.buffer.extend_from_slice(&chunk);
        Ok(())
    }

    async fn commit(mut self: Box<Self>) -> Result<BlobCommit, BlobError> {
        // A failed commit must leave nothing visible, so check before publishing and let
        // `Drop` clean the staging entry.
        self.shared.check_available()?;
        let hasher = std::mem::replace(&mut self.hasher, StreamHasher::new());
        let (size_bytes, checksums): (u64, Checksums) = hasher.finish();
        let data = Bytes::from(std::mem::take(&mut self.buffer));
        {
            let mut state = lock(&self.shared.state);
            // One map insert under one lock: readers see the old object or the new one.
            state.objects.insert(self.key.as_str().to_owned(), data);
        }
        self.leave_staging();
        Ok(BlobCommit {
            size_bytes,
            checksums,
        })
    }

    async fn abort(mut self: Box<Self>) -> Result<(), BlobError> {
        self.leave_staging();
        Ok(())
    }
}
