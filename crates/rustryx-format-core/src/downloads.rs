//! Batched download counters.
//!
//! A download must not cost a database write: a busy repository serves thousands of reads a
//! second, and the number a project owner reads is a statistic, not an account. The download
//! path announces each client read as `ContentEvent::AssetDownloaded`; [`DownloadBatcher`] is
//! the [`ContentEvents`] sink that folds those announcements into one pending count per asset
//! and writes them with `AssetRepo::record_downloads` once per interval.
//!
//! Wiring, in the composition root:
//!
//! ```ignore
//! let batcher = DownloadBatcher::new(existing_sink, DownloadBatcher::DEFAULT_MAX_PENDING);
//! ports.events = batcher.clone();
//! batcher.spawn_flusher(ports.assets.clone(), Duration::from_secs(10),
//!                       engine.shutdown_token().clone(), engine.tasks());
//! ```
//!
//! Properties, and what holds them:
//!
//! * **Only client reads count.** The download path does not announce scanner, replication or
//!   internal reads, and the batcher checks `DownloadSource::counts_as_download` again: a
//!   scanner that walks every artifact must not keep an unused one alive (SCN-21).
//! * **Bounded memory.** At most `max_pending` assets are tracked. A new asset beyond the cap
//!   is dropped and counted in `download_counter_dropped_total`; assets already tracked keep
//!   counting. A burst over a million distinct assets costs precision, never memory.
//! * **A failed flush loses nothing.** The pending map is taken out under the lock, written
//!   without the lock, and merged back on failure (counts added, the later time kept), so the
//!   next flush retries it together with what arrived meanwhile.
//! * **No double counting.** A batch that was written is gone from the map; the port does not
//!   promise idempotence per batch, so the batcher never sends one twice after a success.
//! * **A crash loses at most one interval** of counts, the accepted precision of a statistic.
//! * **The final flush runs at shutdown**, inside the task tracker that `drain` waits for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use metrics::{counter, gauge};
use rustryx_core::error::StoreError;
use rustryx_core::ids::AssetId;
use rustryx_core::model::DownloadTick;
use rustryx_core::ports::AssetRepo;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::context::{ContentEvent, ContentEvents};
use crate::metrics::{
    DOWNLOAD_COUNTER_DROPPED_TOTAL, DOWNLOAD_COUNTER_FLUSH_TOTAL, DOWNLOAD_COUNTER_PENDING,
};

/// Longest the final flush at shutdown may take.
const FINAL_FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// Ticks per `record_downloads` call. A flush of a full map is several calls, so that no single
/// statement grows with the number of distinct assets.
const FLUSH_CHUNK: usize = 5_000;

#[allow(
    clippy::cast_precision_loss,
    reason = "a gauge of a map size; precision beyond 2^52 entries is irrelevant"
)]
fn publish_pending(len: usize) {
    gauge!(DOWNLOAD_COUNTER_PENDING).set(len as f64);
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    count: u32,
    last: OffsetDateTime,
}

/// Folds download announcements into batches. See the module docs.
pub struct DownloadBatcher {
    inner: Arc<dyn ContentEvents>,
    max_pending: usize,
    pending: Mutex<HashMap<AssetId, Pending>>,
}

impl DownloadBatcher {
    /// The default cap on distinct assets tracked between two flushes (design: 1 000 000).
    pub const DEFAULT_MAX_PENDING: usize = 1_000_000;

    /// A batcher that forwards every event to `inner` (so it can sit in front of an existing
    /// sink) and counts the downloads among them.
    #[must_use]
    pub fn new(inner: Arc<dyn ContentEvents>, max_pending: usize) -> Arc<Self> {
        Arc::new(Self {
            inner,
            max_pending,
            pending: Mutex::new(HashMap::new()),
        })
    }

    /// The map is plain data and every critical section below is short and cannot panic, but a
    /// poisoned lock must not stop the counters (or the download path that feeds them).
    fn lock(&self) -> MutexGuard<'_, HashMap<AssetId, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Assets with downloads waiting for the next flush.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.lock().len()
    }

    fn record(&self, asset: AssetId, at: OffsetDateTime) {
        let mut pending = self.lock();
        if let Some(entry) = pending.get_mut(&asset) {
            entry.count = entry.count.saturating_add(1);
            entry.last = entry.last.max(at);
        } else if pending.len() < self.max_pending {
            pending.insert(asset, Pending { count: 1, last: at });
        } else {
            drop(pending);
            counter!(DOWNLOAD_COUNTER_DROPPED_TOTAL).increment(1);
            return;
        }
        publish_pending(pending.len());
    }

    /// Writes everything pending with `record_downloads`, in chunks. Returns how many assets
    /// were written. On a failure the part that was not written is merged back and the error
    /// is returned: the caller retries at the next interval.
    ///
    /// Cancel safe in the sense that matters: a batch is out of the map only while its call is
    /// in flight, and a dropped future puts nothing back, so a cancelled flush loses at most
    /// that one chunk (the accepted precision), never counts a chunk twice.
    pub async fn flush(&self, assets: &dyn AssetRepo) -> Result<usize, StoreError> {
        let taken = std::mem::take(&mut *self.lock());
        publish_pending(0);
        let ticks: Vec<DownloadTick> = taken
            .into_iter()
            .map(|(asset_id, pending)| DownloadTick {
                asset_id,
                at: pending.last,
                count: pending.count,
            })
            .collect();
        let mut written = 0;
        let mut chunks = ticks.chunks(FLUSH_CHUNK);
        while let Some(chunk) = chunks.next() {
            if let Err(error) = assets.record_downloads(chunk).await {
                counter!(DOWNLOAD_COUNTER_FLUSH_TOTAL, "result" => "error").increment(1);
                // This chunk and every chunk after it were not written.
                self.put_back(chunk);
                for rest in chunks {
                    self.put_back(rest);
                }
                return Err(error);
            }
            written += chunk.len();
        }
        if written > 0 {
            counter!(DOWNLOAD_COUNTER_FLUSH_TOTAL, "result" => "ok").increment(1);
        }
        Ok(written)
    }

    /// Merges ticks that were not written back into the map: counts are added and the later
    /// time wins, so what was recorded while the failed call ran is kept.
    fn put_back(&self, ticks: &[DownloadTick]) {
        let mut pending = self.lock();
        for tick in ticks {
            if let Some(entry) = pending.get_mut(&tick.asset_id) {
                entry.count = entry.count.saturating_add(tick.count);
                entry.last = entry.last.max(tick.at);
            } else if pending.len() < self.max_pending {
                pending.insert(
                    tick.asset_id,
                    Pending {
                        count: tick.count,
                        last: tick.at,
                    },
                );
            } else {
                counter!(DOWNLOAD_COUNTER_DROPPED_TOTAL).increment(1);
            }
        }
        publish_pending(pending.len());
    }

    /// Flushes every `interval` until `shutdown` is cancelled, then once more. The task runs on
    /// `tracker`, so the engine's `drain` waits for the final flush. A failed flush is logged and
    /// retried at the next tick with the counts it kept.
    pub fn spawn_flusher(
        self: &Arc<Self>,
        assets: Arc<dyn AssetRepo>,
        interval: Duration,
        shutdown: CancellationToken,
        tracker: &TaskTracker,
    ) {
        let batcher = Arc::clone(self);
        drop(tracker.spawn(async move {
            let mut ticker = tokio::time::interval(interval.max(Duration::from_millis(1)));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick of an interval fires at once; nothing is pending yet.
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(error) = batcher.flush(assets.as_ref()).await {
                            tracing::warn!(%error, "download counter flush failed; the counts are kept for the next one");
                        }
                    }
                }
            }
            // Bounded: a database that hangs must not hold the shutdown of the whole node.
            match tokio::time::timeout(FINAL_FLUSH_TIMEOUT, batcher.flush(assets.as_ref())).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "final download counter flush failed; the last counts are lost");
                }
                Err(_) => {
                    tracing::warn!("final download counter flush timed out; the last counts are lost");
                }
            }
        }));
    }
}

impl ContentEvents for DownloadBatcher {
    fn emit(&self, event: ContentEvent) {
        if let ContentEvent::AssetDownloaded {
            asset, source, at, ..
        } = &event
            && source.counts_as_download()
        {
            self.record(*asset, *at);
        }
        self.inner.emit(event);
    }
}

impl std::fmt::Debug for DownloadBatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadBatcher")
            .field("max_pending", &self.max_pending)
            .field("pending", &self.pending())
            .finish_non_exhaustive()
    }
}
