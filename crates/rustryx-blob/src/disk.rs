//! Free space of the file system under a local store: the probe, the write floor and
//! the thread that keeps both current.
//!
//! SRE-34 asks for writes to be rejected below 10% free space while reads and deletes
//! go on. Three decisions shape this module:
//!
//! 1. **`statvfs` through `rustix`, never `unsafe`.** The workspace forbids `unsafe`;
//!    `rustix::fs::statvfs` is the safe wrapper (Linux and macOS, glibc and musl).
//! 2. **The syscall runs on a dedicated OS thread, not on the runtime.** `statvfs` has no
//!    async form and on a shared RWX volume (NFS, `CephFS`) it can hang for as long as the
//!    mount does. The blocking pool is reserved for hashing and parsing work behind a
//!    semaphore (CONTRIBUTING rule 2), and a hung probe must never occupy it. The thread
//!    publishes a sample; the write path only reads that sample, so no request ever waits
//!    for the file system to answer a capacity question.
//! 3. **The sample is a snapshot, so the guard subtracts what was written since.** A
//!    5 second old sample would otherwise let a burst of uploads run far past the floor.
//!    Bytes accepted since the last sample are counted against the free space, which is
//!    pessimistic (deletes are not credited until the next sample) and therefore safe.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::Thread;
use std::time::Duration;

use rustryx_core::BlobError;
use tokio::sync::watch;

use crate::metrics::StoreMetrics;

/// How often the monitor samples the file system when nobody asks for a refresh.
pub const DEFAULT_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// How long opening a store waits for the first sample. A file system that cannot answer
/// within this time is probably hung; the store opens anyway and the guard stays open
/// (it cannot judge without a sample) until the monitor catches up.
const FIRST_SAMPLE_WAIT: Duration = Duration::from_secs(2);

/// Capacity of the file system that holds a store root, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    /// Size of the file system.
    pub total_bytes: u64,
    /// Bytes a process without root privileges can still write. This is `f_bavail`, not
    /// `f_bfree`: the blocks that ext4 and XFS reserve for root are not free for us, and
    /// it is the number a node exporter alert compares against.
    pub free_bytes: u64,
}

impl DiskUsage {
    /// Whether `free_bytes` is below `percent` of the file system. Exactly at the floor is
    /// not below it. An unknown size (`total_bytes == 0`, some pseudo file systems) is
    /// never below: there is nothing to compare against.
    #[must_use]
    pub fn below_floor(&self, free_bytes: u64, percent: u8) -> bool {
        u128::from(free_bytes) * 100 < u128::from(self.total_bytes) * u128::from(percent)
    }
}

/// Reads the capacity of a file system. A trait so that tests (and a deployment that wants
/// another source) can substitute it; the call blocks by nature and is only ever made from
/// the monitor thread.
pub trait DiskProbe: Send + Sync + 'static {
    /// Capacity of the file system that holds `path`.
    fn usage(&self, path: &Path) -> std::io::Result<DiskUsage>;
}

/// The production probe: `statvfs(3)` through the safe `rustix` wrapper.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatvfsProbe;

impl DiskProbe for StatvfsProbe {
    fn usage(&self, path: &Path) -> std::io::Result<DiskUsage> {
        let stat = rustix::fs::statvfs(path)?;
        // Block counts are in units of the fragment size; POSIX allows it to be zero on a
        // broken driver, in which case the block size is the best remaining unit.
        let unit = if stat.f_frsize == 0 {
            stat.f_bsize
        } else {
            stat.f_frsize
        };
        Ok(DiskUsage {
            total_bytes: stat.f_blocks.saturating_mul(unit),
            free_bytes: stat.f_bavail.saturating_mul(unit),
        })
    }
}

/// The write floor. Shared between the monitor thread (which feeds it) and every writer
/// (which asks it).
#[derive(Debug)]
pub(crate) struct DiskGuard {
    min_free_percent: u8,
    // Held for the instant of a copy, never across an await.
    sample: Mutex<Option<DiskUsage>>,
    /// Bytes accepted since `sample` was taken.
    written_since: AtomicU64,
}

impl DiskGuard {
    pub(crate) fn new(min_free_percent: u8) -> Self {
        Self {
            min_free_percent,
            sample: Mutex::new(None),
            written_since: AtomicU64::new(0),
        }
    }

    pub(crate) fn record(&self, usage: DiskUsage) {
        *self.sample.lock().unwrap_or_else(PoisonError::into_inner) = Some(usage);
        // Bytes written while the probe ran are forgotten twice over (the sample may or may
        // not include them); a small under-count here is bounded by one probe interval and
        // the floor itself leaves far more headroom than that.
        self.written_since.store(0, Ordering::Relaxed);
    }

    /// Fails with `InsufficientStorage` when accepting `incoming` more bytes would leave
    /// less than the floor free. Without a sample, or with the floor switched off, it
    /// allows: a guard that cannot see must not block the whole store.
    pub(crate) fn check(&self, incoming: u64) -> Result<(), BlobError> {
        if self.min_free_percent == 0 {
            return Ok(());
        }
        let Some(usage) = *self.sample.lock().unwrap_or_else(PoisonError::into_inner) else {
            return Ok(());
        };
        let free = usage
            .free_bytes
            .saturating_sub(self.written_since.load(Ordering::Relaxed))
            .saturating_sub(incoming);
        if usage.below_floor(free, self.min_free_percent) {
            Err(BlobError::InsufficientStorage)
        } else {
            Ok(())
        }
    }

    pub(crate) fn note_written(&self, bytes: u64) {
        self.written_since.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// What the monitor thread has done so far, published on a `watch` channel so that the
/// async side can wait for it without polling.
#[derive(Debug, Clone, Copy, Default)]
struct Progress {
    /// Probes that finished (successfully or not).
    probes: u64,
    /// The highest refresh ticket that a probe which STARTED after the request has served.
    served: u64,
}

struct Shared {
    stop: AtomicBool,
    /// Refresh tickets handed out by `refresh`.
    requested: AtomicU64,
    progress: watch::Sender<Progress>,
}

/// Handle of the thread that samples the file system. Dropping it (or calling `stop`)
/// ends the thread without joining it: joining would block the caller for as long as a
/// hung mount keeps `statvfs` stuck.
pub(crate) struct DiskMonitor {
    shared: Arc<Shared>,
    thread: Thread,
}

impl DiskMonitor {
    pub(crate) fn start(
        root: PathBuf,
        probe: Arc<dyn DiskProbe>,
        guard: Arc<DiskGuard>,
        metrics: StoreMetrics,
        interval: Duration,
    ) -> std::io::Result<Self> {
        let (progress, _unused) = watch::channel(Progress::default());
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            requested: AtomicU64::new(0),
            progress,
        });
        let worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("rustryx-disk-probe".into())
            .spawn(move || run(&worker, &root, probe.as_ref(), &guard, &metrics, interval))?;
        Ok(Self {
            shared,
            thread: handle.thread().clone(),
        })
    }

    /// Waits for the first sample, at most [`FIRST_SAMPLE_WAIT`].
    pub(crate) async fn first_sample(&self) {
        let mut progress = self.shared.progress.subscribe();
        let _ = tokio::time::timeout(
            FIRST_SAMPLE_WAIT,
            progress.wait_for(|state| state.probes >= 1),
        )
        .await;
    }

    /// Probes now and waits until a probe that started after this call has finished.
    /// Gives up after [`FIRST_SAMPLE_WAIT`] times two, so a hung mount cannot hang a
    /// caller. Cancel safe.
    pub(crate) async fn refresh(&self) {
        // A stopped monitor will never serve a ticket; waiting for one would only burn the
        // timeout.
        if self.shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let ticket = self.shared.requested.fetch_add(1, Ordering::SeqCst) + 1;
        let mut progress = self.shared.progress.subscribe();
        self.thread.unpark();
        let _ = tokio::time::timeout(
            FIRST_SAMPLE_WAIT * 2,
            progress.wait_for(|state| state.served >= ticket),
        )
        .await;
    }

    /// Ends the thread. Idempotent.
    pub(crate) fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.thread.unpark();
    }
}

impl Drop for DiskMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    shared: &Shared,
    root: &Path,
    probe: &dyn DiskProbe,
    guard: &DiskGuard,
    metrics: &StoreMetrics,
    interval: Duration,
) {
    let mut failing = false;
    while !shared.stop.load(Ordering::SeqCst) {
        // Read the ticket BEFORE probing: only a probe that starts after a `refresh` call
        // is allowed to answer it, otherwise a sample taken just before the caller changed
        // the disk would be taken for a fresh one.
        let ticket = shared.requested.load(Ordering::SeqCst);
        match probe.usage(root) {
            Ok(usage) => {
                guard.record(usage);
                metrics.disk(usage);
                if failing {
                    tracing::info!(path = %root.display(), "disk probe recovered");
                    failing = false;
                }
            }
            Err(err) => {
                // One line per failure episode, not one per interval.
                if !failing {
                    tracing::warn!(error = %err, path = %root.display(), "disk probe failed; the free space floor keeps its last sample");
                    failing = true;
                }
            }
        }
        shared.progress.send_modify(|state| {
            state.probes += 1;
            state.served = state.served.max(ticket);
        });
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::park_timeout(interval);
    }
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
