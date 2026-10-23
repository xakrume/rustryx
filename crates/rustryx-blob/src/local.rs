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

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::StreamExt;
use rustryx_core::BlobError;
use rustryx_core::model::BlobStoreKind;
use rustryx_core::ports::{BlobKey, BlobMeta, BlobRead, BlobStore, BlobWriter, ByteRange};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio_util::io::ReaderStream;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::cleanup::CleanupDrain;
use crate::error::{backend, invalid_key, io_read, is_absent};
use crate::hashing::{BlobSink, HashingWriter};
use crate::key::{STAGING_DIR, shard_of, validate_key};
use crate::read::{READ_CHUNK_BYTES, resolve_range};

/// Buffer between the 64 KiB slices and the file. Fewer, larger writes mean fewer
/// hops to the blocking pool; 256 KiB is the whole per-upload memory of this store.
const WRITE_BUFFER_BYTES: usize = 256 * 1024;

const STAGING_SUFFIX: &str = ".part";

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
}

impl std::fmt::Debug for LocalBlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalBlobStore")
            .field("root", &self.inner.root)
            .finish_non_exhaustive()
    }
}

impl LocalBlobStore {
    /// Opens (creating when needed) a store rooted at `root`.
    ///
    /// Creates `root` and `root/_staging`. Not cancel safe in the sense that a
    /// cancelled call may leave the directories created, which is harmless.
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, BlobError> {
        let requested = root.as_ref();
        fs::create_dir_all(requested.join(STAGING_DIR))
            .await
            .map_err(|e| backend("create blob store root", e))?;
        let root = fs::canonicalize(requested)
            .await
            .map_err(|e| backend("resolve blob store root", e))?;
        let staging = root.join(STAGING_DIR);
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                staging,
                cleanup: TaskTracker::new(),
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

    /// Waits until every cleanup task spawned by dropped writers has finished.
    /// Call it at graceful shutdown. Cancel safe.
    pub async fn drain_cleanup(&self) {
        // Left closed on purpose: a closed tracker still accepts tasks, and `wait`
        // then resolves as soon as it is empty, so repeated or concurrent drains are
        // safe. Reopening would let one caller hang another.
        self.inner.cleanup.close();
        self.inner.cleanup.wait().await;
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
            return Err(backend(
                "invalid blob key",
                "resolved path left the store root",
            ));
        }
        Ok(path)
    }

    /// Path of a staging file named by a key returned from `list_stale_staging`.
    fn staging_path(&self, key: &BlobKey) -> Option<Result<PathBuf, BlobError>> {
        let name = key.as_str().strip_prefix(STAGING_DIR)?.strip_prefix('/')?;
        if is_staging_name(name) {
            Some(Ok(self.inner.staging.join(name)))
        } else {
            Some(Err(backend(
                "invalid blob key",
                "malformed staging identifier",
            )))
        }
    }

    async fn create_staging_file(&self, path: &Path) -> Result<File, BlobError> {
        let mut file = match open_exclusive(path).await {
            Ok(file) => file,
            // An operator may have wiped staging while the process runs.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.inner.staging)
                    .await
                    .map_err(|e| backend("recreate staging directory", e))?;
                open_exclusive(path)
                    .await
                    .map_err(|e| backend("create staging file", e))?
            }
            Err(err) => return Err(backend("create staging file", err)),
        };
        file.set_max_buf_size(WRITE_BUFFER_BYTES);
        Ok(file)
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

#[async_trait]
impl CleanupDrain for LocalBlobStore {
    async fn drain_cleanup(&self) {
        Self::drain_cleanup(self).await;
    }
}

fn is_staging_name(name: &str) -> bool {
    name.strip_suffix(STAGING_SUFFIX).is_some_and(|stem| {
        stem.len() == 32 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    fn kind(&self) -> BlobStoreKind {
        BlobStoreKind::Local
    }

    /// Starts a write into `_staging/`. The object stays invisible until `commit`.
    /// Cancel safe: dropping the writer (or the future) removes the temporary file.
    /// `size_hint` is not needed by a file system and is ignored.
    async fn begin(
        &self,
        key: &BlobKey,
        _size_hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        let final_path = self.blob_path(key)?;
        let name = format!("{}{STAGING_SUFFIX}", Uuid::now_v7().simple());
        let temp_path = self.inner.staging.join(name);
        let file = self.create_staging_file(&temp_path).await?;
        let sink = LocalSink {
            writer: Some(BufWriter::with_capacity(WRITE_BUFFER_BYTES, file)),
            temp_path,
            final_path,
            root: self.inner.root.clone(),
            cleanup: self.inner.cleanup.clone(),
            settled: false,
        };
        Ok(Box::new(HashingWriter::new(sink)))
    }

    /// Opens a streamed read of at most 64 KiB chunks, pulled on demand. Cancel safe.
    async fn get(&self, key: &BlobKey, range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
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
    }

    /// Cancel safe.
    async fn head(&self, key: &BlobKey) -> Result<Option<BlobMeta>, BlobError> {
        let path = self.blob_path(key)?;
        match fs::metadata(&path).await {
            Ok(meta) if meta.is_file() => Ok(Some(BlobMeta {
                size_bytes: meta.len(),
            })),
            Ok(_) => Ok(None),
            Err(err) if is_absent(err.kind()) => Ok(None),
            Err(err) => Err(backend("stat blob", err)),
        }
    }

    /// Deletes a committed blob, or a stale staging file named by a key from
    /// `list_stale_staging`. A missing target is success. Cancel safe.
    async fn delete(&self, key: &BlobKey) -> Result<(), BlobError> {
        let path = match self.staging_path(key) {
            Some(staging) => staging?,
            None => self.blob_path(key)?,
        };
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if is_absent(err.kind()) => Ok(()),
            Err(err) => Err(backend("delete blob", err)),
        }
    }

    /// Lists staging files that were not touched for `min_age`, as keys of the form
    /// `_staging/<id>.part` that `delete` accepts. An upload that is merely slow keeps
    /// refreshing its modification time; one that stalls for longer than `min_age`
    /// is indistinguishable from an abandoned one, and deleting it makes its commit
    /// fail cleanly. Cancel safe.
    async fn list_stale_staging(&self, min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
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

    /// Creates and removes a probe file in staging, which proves the root exists and
    /// is writable. Does not touch other nodes' data. Cancel safe.
    async fn health(&self) -> Result<(), BlobError> {
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

/// Local half of a write: the open staging file plus where it must end up.
struct LocalSink {
    writer: Option<BufWriter<File>>,
    temp_path: PathBuf,
    final_path: PathBuf,
    root: PathBuf,
    cleanup: TaskTracker,
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
            .map_err(|e| backend("flush staging file", e))?;
        let file = writer.into_inner();
        // Data must be on disk before the name appears; otherwise a crash after the
        // rename could leave a visible, truncated blob.
        file.sync_all()
            .await
            .map_err(|e| backend("fsync staging file", e))?;
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
            .map_err(|e| backend("create blob directory", e))?;
        fs::rename(&self.temp_path, &self.final_path)
            .await
            .map_err(|e| backend("rename staging file", e))?;

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
        let writer = self.writer.as_mut().ok_or(BlobError::WriterClosed)?;
        writer
            .write_all(data)
            .await
            .map_err(|e| backend("write staging file", e))
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
