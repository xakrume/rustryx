//! Local filesystem blob store.
//!
//! Layout under the root (see also `docs/blob.md`):
//!
//! ```text
//! <root>/_staging/<uuidv7>.part     in-flight writes, never served
//! <root>/<b0>/<b1>/<key>            committed blobs (b0 b1 = shard, see crate::key)
//! ```
//!
//! A write goes to a temporary file in `_staging/`, which lives under the SAME root
//! so that the final `rename` stays on one file system and is atomic. Data is
//! fsynced before the rename and the directory is fsynced after it, so an
//! acknowledged commit survives a crash. Readers either see the whole previous state
//! (no file) or the whole new one; there is no moment at which a partial blob is
//! reachable by key.
//!
//! All file work uses `tokio::fs`. `tokio::fs` itself runs each operation on the
//! runtime's blocking pool, which is the sanctioned way to touch files from async
//! code; nothing in this module blocks a worker thread directly.

use std::future::Future;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::StreamExt;
use rustryx_core::BlobError;
use rustryx_core::model::BlobStoreKind;
use rustryx_core::ports::{
    BlobKey, BlobMeta, BlobRead, BlobStore, BlobStoreCapabilities, BlobWriter, ByteRange, KeyPage,
};
use time::OffsetDateTime;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio_util::io::ReaderStream;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::disk::{DEFAULT_PROBE_INTERVAL, DiskGuard, DiskMonitor, DiskProbe};
use crate::error::{backend, invalid_key, io_read, io_write, is_absent};
use crate::hashing::{BlobSink, HashingWriter};
use crate::key::{STAGING_DIR, shard_of, validate_key};
use crate::metrics::{MeteredWriter, Op, StoreMetrics};
use crate::read::{READ_CHUNK_BYTES, resolve_range};

mod list;

/// Buffer between the 64 KiB slices and the file. Fewer, larger writes mean fewer
/// hops to the blocking pool; 256 KiB is the whole per-upload memory of this store.
const WRITE_BUFFER_BYTES: usize = 256 * 1024;

const STAGING_SUFFIX: &str = ".part";

/// How a [`LocalBlobStore`] is opened: its name for the metrics and the free space floor.
///
/// A plain [`LocalBlobStore::open`] has no floor and no disk monitor: it is the right
/// choice for tests and for tools that only read. The registry builds production stores
/// with [`LocalOptions::with_disk_floor`].
#[derive(Clone)]
#[non_exhaustive]
pub struct LocalOptions {
    /// Value of the `store` label on every series of this store.
    pub store_name: String,
    /// Writes are refused below this percentage of free space. Zero means no floor.
    pub min_free_percent: u8,
    /// Where capacity comes from. `None` disables the monitor, the floor and the disk gauges.
    pub probe: Option<Arc<dyn DiskProbe>>,
    /// Time between two samples of the file system.
    pub probe_interval: Duration,
}

impl LocalOptions {
    /// Options with the given metrics name, no floor and no monitor.
    #[must_use]
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
            min_free_percent: 0,
            probe: None,
            probe_interval: DEFAULT_PROBE_INTERVAL,
        }
    }

    /// Samples the disk with `probe` (feeding `blob_store_free_bytes` and
    /// `blob_store_total_bytes`) and refuses writes below `min_free_percent`. With zero the
    /// disk is still monitored, only the refusal is off.
    #[must_use]
    pub fn with_disk_floor(self, probe: Arc<dyn DiskProbe>, min_free_percent: u8) -> Self {
        Self {
            probe: Some(probe),
            min_free_percent,
            ..self
        }
    }

    /// Changes the time between samples.
    #[must_use]
    pub fn with_probe_interval(self, probe_interval: Duration) -> Self {
        Self {
            probe_interval,
            ..self
        }
    }
}

impl std::fmt::Debug for LocalOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalOptions")
            .field("store_name", &self.store_name)
            .field("min_free_percent", &self.min_free_percent)
            .field("monitored", &self.probe.is_some())
            .field("probe_interval", &self.probe_interval)
            .finish()
    }
}

/// A blob store backed by a directory.
///
/// Cheap to clone (shared state behind an `Arc`). For HA the root must be a shared
/// (RWX) volume; every node may then list staging, which is why nothing here deletes
/// staging files at startup: another node may be writing into them.
#[derive(Clone)]
pub struct LocalBlobStore {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    staging: PathBuf,
    /// Cleanup of abandoned writes runs here. `Drop` cannot await, so it spawns on
    /// this tracker; the tracker is what lets shutdown (and tests) wait for the
    /// removals instead of fire-and-forget spawning.
    cleanup: TaskTracker,
    /// The write floor. Open (no floor) when the store was opened without options.
    guard: Arc<DiskGuard>,
    /// Keeps the guard current. Dropped with the store, which ends its thread.
    monitor: Option<DiskMonitor>,
    metrics: StoreMetrics,
}

impl std::fmt::Debug for LocalBlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalBlobStore")
            .field("root", &self.inner.root)
            .finish_non_exhaustive()
    }
}

impl LocalBlobStore {
    /// Opens (creating when needed) a store rooted at `root`, without a free space floor.
    ///
    /// Creates `root` and `root/_staging`. Not cancel safe in the sense that a
    /// cancelled call may leave the directories created, which is harmless.
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, BlobError> {
        Self::open_with(root, LocalOptions::new("local")).await
    }

    /// Opens a store with a metrics name and, when `options` carry a probe, the disk
    /// monitor and the free space floor. Waits briefly for the first sample so that the
    /// floor is in force from the first write (a file system that does not answer in time
    /// leaves the floor open until it does).
    pub async fn open_with(
        root: impl AsRef<Path>,
        options: LocalOptions,
    ) -> Result<Self, BlobError> {
        let requested = root.as_ref();
        fs::create_dir_all(requested.join(STAGING_DIR))
            .await
            .map_err(|e| io_write("create blob store root", e))?;
        let root = fs::canonicalize(requested)
            .await
            .map_err(|e| backend("resolve blob store root", e))?;
        let staging = root.join(STAGING_DIR);
        let metrics = StoreMetrics::new(options.store_name);
        let guard = Arc::new(DiskGuard::new(options.min_free_percent));
        let monitor = match options.probe {
            Some(probe) => {
                let monitor = DiskMonitor::start(
                    root.clone(),
                    probe,
                    Arc::clone(&guard),
                    metrics.clone(),
                    options.probe_interval,
                )
                .map_err(|e| backend("start the disk monitor", e))?;
                monitor.first_sample().await;
                Some(monitor)
            }
            None => None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                staging,
                cleanup: TaskTracker::new(),
                guard,
                monitor,
                metrics,
            }),
        })
    }

    /// Canonical root directory of the store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Directory holding in-flight writes.
    #[must_use]
    pub fn staging_dir(&self) -> &Path {
        &self.inner.staging
    }

    /// Samples the disk now and waits for the sample, so that the floor and the gauges
    /// reflect a change that just happened. A no-op without a monitor. Cancel safe.
    pub async fn refresh_disk(&self) {
        if let Some(monitor) = &self.inner.monitor {
            monitor.refresh().await;
        }
    }

    /// Absolute path of a committed blob. The only place a key becomes a path:
    /// the key is validated first (strict allowlist, no `..`), then every component
    /// is appended individually, and the result is checked to still lie under the
    /// root as a second, independent layer.
    fn blob_path(&self, key: &BlobKey) -> Result<PathBuf, BlobError> {
        let text = validate_key(key).map_err(invalid_key)?;
        let (first, second) = shard_of(text);
        let mut path = self.inner.root.join(first).join(second);
        path.extend(text.split('/'));
        if !path.starts_with(&self.inner.root) {
            return Err(BlobError::InvalidKey("resolved path left the store root"));
        }
        Ok(path)
    }

    /// Path of a staging file named by a key returned from `list_stale_staging`.
    fn staging_path(&self, key: &BlobKey) -> Option<Result<PathBuf, BlobError>> {
        let name = key.as_str().strip_prefix(STAGING_DIR)?.strip_prefix('/')?;
        if is_staging_name(name) {
            Some(Ok(self.inner.staging.join(name)))
        } else {
            Some(Err(BlobError::InvalidKey("malformed staging identifier")))
        }
    }

    async fn create_staging_file(&self, path: &Path) -> Result<File, BlobError> {
        let mut file = match open_exclusive(path).await {
            Ok(file) => file,
            // An operator may have wiped staging while the process runs.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.inner.staging)
                    .await
                    .map_err(|e| io_write("recreate staging directory", e))?;
                open_exclusive(path)
                    .await
                    .map_err(|e| io_write("create staging file", e))?
            }
            Err(err) => return Err(io_write("create staging file", err)),
        };
        file.set_max_buf_size(WRITE_BUFFER_BYTES);
        Ok(file)
    }

    /// Counts the result of `work` under `op` and passes it through.
    async fn metered<T>(
        &self,
        op: Op,
        work: impl Future<Output = Result<T, BlobError>>,
    ) -> Result<T, BlobError> {
        let result = work.await;
        self.inner.metrics.result(op, &result);
        result
    }

    /// `begin` without the metering: the floor check, the staging file, the sink.
    async fn begin_write(
        &self,
        key: &BlobKey,
        size_hint: Option<u64>,
    ) -> Result<HashingWriter<LocalSink>, BlobError> {
        let final_path = self.blob_path(key)?;
        // A write that announced its size is refused before a byte is transferred when it
        // would cross the floor; one that did not is stopped by the check in `write`.
        self.inner.guard.check(size_hint.unwrap_or(0))?;
        let name = format!("{}{STAGING_SUFFIX}", Uuid::now_v7().simple());
        let temp_path = self.inner.staging.join(name);
        // The sink owns the temporary name BEFORE the file exists. The open runs on the
        // blocking pool, so a future cancelled right after the file was created never
        // sees the result; with the sink already built, its `Drop` still schedules the
        // removal. (A removal that wins the race against that in-flight open misses the
        // file; the staging listing for GC is the backstop for that window.)
        let mut sink = LocalSink {
            writer: None,
            temp_path: temp_path.clone(),
            final_path,
            root: self.inner.root.clone(),
            cleanup: self.inner.cleanup.clone(),
            guard: Arc::clone(&self.inner.guard),
            settled: false,
        };
        let file = match self.create_staging_file(&temp_path).await {
            Ok(file) => file,
            Err(err) => {
                // Nothing was created: no removal task is needed.
                sink.settled = true;
                return Err(err);
            }
        };
        sink.writer = Some(BufWriter::with_capacity(WRITE_BUFFER_BYTES, file));
        Ok(HashingWriter::new(sink))
    }
}

/// Creates a new file and fails if the name exists (`O_EXCL`), so a staging name is
/// never shared and a planted symlink at that name is never followed.
async fn open_exclusive(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
}

fn is_staging_name(name: &str) -> bool {
    name.strip_suffix(STAGING_SUFFIX).is_some_and(|stem| {
        stem.len() == 32 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn to_offset(time: SystemTime) -> OffsetDateTime {
    OffsetDateTime::from(time)
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    fn kind(&self) -> BlobStoreKind {
        BlobStoreKind::Local
    }

    /// Starts a write into `_staging/`. The object stays invisible until `commit`.
    /// Cancel safe: dropping the writer (or the future) removes the temporary file.
    /// `size_hint` is only used for the free space floor; a file system needs no part size.
    ///
    /// Errors: `InsufficientStorage` below the floor or on a full disk, `InvalidKey`.
    async fn begin(
        &self,
        key: &BlobKey,
        size_hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        match self.begin_write(key, size_hint).await {
            Ok(writer) => Ok(Box::new(MeteredWriter::new(
                writer,
                self.inner.metrics.clone(),
            ))),
            Err(err) => {
                self.inner.metrics.failed(Op::Put, &err);
                Err(err)
            }
        }
    }

    /// Opens a streamed read of at most 64 KiB chunks, pulled on demand. Cancel safe.
    /// Reads never consult the free space floor: a full disk still serves what it has.
    async fn get(&self, key: &BlobKey, range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
        let read = self
            .metered(Op::Get, async {
                let path = self.blob_path(key)?;
                let mut file = File::open(&path)
                    .await
                    .map_err(|e| io_read("open blob", e))?;
                let meta = file.metadata().await.map_err(|e| io_read("stat blob", e))?;
                if !meta.is_file() {
                    return Err(BlobError::NotFound);
                }
                let total = meta.len();
                let (start, len) = resolve_range(total, range)?;
                if start > 0 {
                    file.seek(SeekFrom::Start(start))
                        .await
                        .map_err(|e| backend("seek blob", e))?;
                }
                let stream = ReaderStream::with_capacity(file.take(len), READ_CHUNK_BYTES)
                    .map(|item| item.map_err(|e| backend("read blob", e)))
                    .boxed();
                Ok(BlobRead {
                    total_size: total,
                    stream,
                })
            })
            .await?;
        Ok(BlobRead {
            total_size: read.total_size,
            stream: self.inner.metrics.meter_read(read.stream),
        })
    }

    /// Size and modification time. Cancel safe.
    async fn head(&self, key: &BlobKey) -> Result<Option<BlobMeta>, BlobError> {
        self.metered(Op::Head, async {
            let path = self.blob_path(key)?;
            match fs::metadata(&path).await {
                Ok(meta) if meta.is_file() => Ok(Some(blob_meta(meta.len(), meta.modified()))),
                Ok(_) => Ok(None),
                Err(err) if is_absent(err.kind()) => Ok(None),
                Err(err) => Err(backend("stat blob", err)),
            }
        })
        .await
    }

    /// Deletes a committed blob, or a stale staging file named by a key from
    /// `list_stale_staging`. A missing target is success. Cancel safe. Never refused by
    /// the free space floor: deleting is how a full disk recovers.
    async fn delete(&self, key: &BlobKey) -> Result<(), BlobError> {
        self.metered(Op::Delete, async {
            let path = match self.staging_path(key) {
                Some(staging) => staging?,
                None => self.blob_path(key)?,
            };
            match fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(err) if is_absent(err.kind()) => Ok(()),
                Err(err) => Err(backend("delete blob", err)),
            }
        })
        .await
    }

    /// Lists staging files that were not touched for `min_age`, as keys of the form
    /// `_staging/<id>.part` that `delete` accepts. An upload that is merely slow keeps
    /// refreshing its modification time; one that stalls for longer than `min_age`
    /// is indistinguishable from an abandoned one, and deleting it makes its commit
    /// fail cleanly. Cancel safe.
    async fn list_stale_staging(&self, min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        self.metered(Op::List, self.stale_staging(min_age)).await
    }

    /// Creates and removes a probe file in staging, which proves the root exists and
    /// is writable. Does not touch other nodes' data. Cancel safe.
    ///
    /// A low disk is not unhealthy: the store still serves reads and accepts deletes, and
    /// it reports the condition through the free space gauges and `InsufficientStorage`.
    async fn health(&self) -> Result<(), BlobError> {
        self.metered(Op::Health, self.probe_root()).await
    }

    /// Staging listing and key listing are both real on a file system.
    fn capabilities(&self) -> BlobStoreCapabilities {
        let mut capabilities = BlobStoreCapabilities::default();
        capabilities.lists_staging = true;
        capabilities.lists_keys = true;
        capabilities
    }

    /// Waits for the removals that dropped writers left behind and stops the disk monitor.
    /// Idempotent and safe to call concurrently. Cancel safe.
    async fn shutdown(&self) {
        // Left closed on purpose: a closed tracker still accepts tasks, and `wait`
        // then resolves as soon as it is empty, so repeated or concurrent calls are
        // safe. Reopening would let one caller hang another.
        self.inner.cleanup.close();
        self.inner.cleanup.wait().await;
        if let Some(monitor) = &self.inner.monitor {
            monitor.stop();
        }
    }

    /// Walks the shard directories in name order, at most 1000 keys per page. See
    /// `docs/blob.md` for the cursor and for what the walk skips. Cancel safe.
    async fn list_keys(&self, after: Option<&str>, limit: u32) -> Result<KeyPage, BlobError> {
        self.metered(Op::List, list::list_keys(&self.inner.root, after, limit))
            .await
    }
}

impl LocalBlobStore {
    async fn stale_staging(&self, min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        let mut entries = fs::read_dir(&self.inner.staging)
            .await
            .map_err(|e| backend("list staging directory", e))?;
        let now = SystemTime::now();
        let mut names = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| backend("list staging directory", e))?
        {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str().filter(|n| is_staging_name(n)) else {
                continue;
            };
            let meta = match entry.metadata().await {
                Ok(meta) => meta,
                // Committed or deleted between readdir and stat.
                Err(err) if is_absent(err.kind()) => continue,
                Err(err) => return Err(backend("stat staging file", err)),
            };
            let modified = meta
                .modified()
                .map_err(|e| backend("read staging mtime", e))?;
            // A modification time in the future (clock step) counts as fresh.
            if meta.is_file() && now.duration_since(modified).is_ok_and(|age| age >= min_age) {
                names.push(name.to_owned());
            }
        }
        names.sort_unstable();
        Ok(names
            .into_iter()
            .map(|n| BlobKey::new(format!("{STAGING_DIR}/{n}")))
            .collect())
    }

    async fn probe_root(&self) -> Result<(), BlobError> {
        let probe = self
            .inner
            .staging
            .join(format!(".health-{}", Uuid::now_v7().simple()));
        let unavailable =
            |what: &str, err: std::io::Error| BlobError::Unavailable(format!("{what}: {err}"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .await
            .map_err(|e| unavailable("blob store root is not writable", e))?;
        let written = file.write_all(b"ok").await;
        drop(file);
        fs::remove_file(&probe)
            .await
            .map_err(|e| unavailable("health probe cleanup failed", e))?;
        written.map_err(|e| unavailable("blob store root rejected a write", e))
    }
}

/// Size and modification time of a file, from the pieces of its metadata. Taking the pieces
/// instead of the metadata type keeps the blocking-construct scan of `tests/hard_rules.rs`
/// from needing an exception for a type name.
fn blob_meta(len: u64, modified: std::io::Result<SystemTime>) -> BlobMeta {
    let mut blob = BlobMeta::new(len);
    blob.modified = modified.ok().map(to_offset);
    blob
}

/// Local half of a write: the open staging file plus where it must end up.
struct LocalSink {
    writer: Option<BufWriter<File>>,
    temp_path: PathBuf,
    final_path: PathBuf,
    root: PathBuf,
    cleanup: TaskTracker,
    guard: Arc<DiskGuard>,
    /// Set once `finish` or `discard` completed (successfully or not). `Drop` removes
    /// the temporary file only while this is false.
    settled: bool,
}

impl LocalSink {
    async fn publish(&mut self) -> Result<(), BlobError> {
        let mut writer = self.writer.take().ok_or(BlobError::WriterClosed)?;
        writer
            .flush()
            .await
            .map_err(|e| io_write("flush staging file", e))?;
        let file = writer.into_inner();
        // Data must be on disk before the name appears; otherwise a crash after the
        // rename could leave a visible, truncated blob.
        file.sync_all()
            .await
            .map_err(|e| io_write("fsync staging file", e))?;
        drop(file);

        let parent = self
            .final_path
            .parent()
            .ok_or_else(|| backend("locate blob directory", "blob path has no parent"))?;
        let created_dirs = !fs::try_exists(parent)
            .await
            .map_err(|e| backend("stat blob directory", e))?;
        fs::create_dir_all(parent)
            .await
            .map_err(|e| io_write("create blob directory", e))?;
        fs::rename(&self.temp_path, &self.final_path)
            .await
            .map_err(|e| io_write("rename staging file", e))?;

        // The rename and any freshly created shard directories only survive a crash
        // once their parent directories are synced. The blob is already visible, so
        // a failure here is logged, not returned: failing the commit now would
        // orphan a good object.
        sync_dir(parent).await;
        if created_dirs {
            for dir in parent
                .ancestors()
                .skip(1)
                .take_while(|d| d.starts_with(&self.root))
            {
                sync_dir(dir).await;
            }
        }
        Ok(())
    }

    async fn remove_temp(&self) -> Result<(), BlobError> {
        match fs::remove_file(&self.temp_path).await {
            Ok(()) => Ok(()),
            Err(err) if is_absent(err.kind()) => Ok(()),
            Err(err) => Err(backend("remove staging file", err)),
        }
    }
}

impl BlobSink for LocalSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), BlobError> {
        let len = data.len() as u64;
        // Checked on every slice, not only at `begin`: an upload of unknown length, or one
        // that began above the floor, must stop when the disk crosses it. The check reads
        // two atomics and a sample, never the file system.
        self.guard.check(len)?;
        let writer = self.writer.as_mut().ok_or(BlobError::WriterClosed)?;
        writer
            .write_all(data)
            .await
            .map_err(|e| io_write("write staging file", e))?;
        self.guard.note_written(len);
        Ok(())
    }

    async fn finish(mut self) -> Result<(), BlobError> {
        let outcome = self.publish().await;
        if outcome.is_err() {
            // Whatever failed, the temporary name must not survive. A removal error
            // here is secondary; the original error is what the caller needs.
            let _ = self.remove_temp().await;
        }
        self.settled = true;
        outcome
    }

    async fn discard(mut self) -> Result<(), BlobError> {
        // Close the file before unlinking it.
        self.writer = None;
        let outcome = self.remove_temp().await;
        self.settled = true;
        outcome
    }
}

impl Drop for LocalSink {
    fn drop(&mut self) {
        if !self.settled {
            schedule_removal(&self.cleanup, std::mem::take(&mut self.temp_path));
        }
    }
}

/// Removes an abandoned temporary file from `Drop`, which cannot await.
///
/// Invariant: only the unique temporary name is ever removed here, never the final
/// path, so running this after a successful rename is a harmless no-op.
fn schedule_removal(cleanup: &TaskTracker, path: PathBuf) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("no runtime to remove an abandoned staging file; GC will list it");
        return;
    };
    cleanup.spawn_on(
        async move {
            match fs::remove_file(&path).await {
                Ok(()) => {}
                Err(err) if is_absent(err.kind()) => {}
                Err(err) => {
                    tracing::warn!(error = %err, path = %path.display(), "failed to remove staging file");
                }
            }
        },
        &handle,
    );
}

async fn sync_dir(path: &Path) {
    let synced = match File::open(path).await {
        Ok(dir) => dir.sync_all().await,
        Err(err) => Err(err),
    };
    if let Err(err) = synced {
        tracing::warn!(error = %err, path = %path.display(), "directory fsync failed after commit");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_names_are_32_hex_digits_with_part_suffix() {
        assert!(is_staging_name("0192f3a47b1c7d2e8f00123456789abc.part"));
        assert!(!is_staging_name("0192f3a47b1c7d2e8f00123456789abc"));
        assert!(!is_staging_name("0192F3A47B1C7D2E8F00123456789ABC.part"));
        assert!(!is_staging_name("short.part"));
        assert!(!is_staging_name("../0192f3a47b1c7d2e8f00123456789abc.part"));
        assert!(!is_staging_name(".health-0192f3a47b1c7d2e8f00123456789abc"));
    }
}
