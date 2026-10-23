//! Soft quotas of blob stores and garbage collection runs.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{BlobStoreId, GcRunId, TaskRunId, UserId};
use crate::tasks::{TaskRunStatus, TaskTrigger};

/// Which side of the limit a soft quota alerts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoftQuotaKind {
    /// Alert when used bytes exceed the value.
    MaxUsed,
    /// Alert when free bytes fall below the value.
    MinFree,
}

/// Alert only quota of a blob store. The hard limit stays `BlobStoreDef.quota_bytes`; a soft
/// quota never blocks a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftQuota {
    /// Which side alerts.
    pub kind: SoftQuotaKind,
    /// Threshold in bytes.
    pub bytes: u64,
}

impl SoftQuota {
    /// True when the store is over the soft quota. `free_bytes` is `None` when the store does
    /// not know its free space; `MinFree` then reports "not over" because it cannot tell, and
    /// the status check says so.
    #[must_use]
    pub fn is_exceeded(&self, used_bytes: u64, free_bytes: Option<u64>) -> bool {
        match self.kind {
            SoftQuotaKind::MaxUsed => used_bytes > self.bytes,
            SoftQuotaKind::MinFree => free_bytes.is_some_and(|free| free < self.bytes),
        }
    }
}

/// Options of a garbage collection run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcOptions {
    /// Mark only, store counts and a sample, delete nothing.
    pub dry_run: bool,
    /// OCI: also delete manifests that have no tag for longer than `min_age`, are not a
    /// subject's referrer and are not a child of an index.
    pub delete_untagged: bool,
    /// Never below `gc.min_age_secs` of the configuration (validated), default equal to it.
    pub min_age_secs: u64,
    /// Parallel sweeps, 1 to 8, default 2.
    pub workers: u8,
    /// Restrict the run to one blob store.
    pub blob_store: Option<BlobStoreId>,
}

/// Default number of parallel sweeps.
pub const DEFAULT_GC_WORKERS: u8 = 2;
/// Most parallel sweeps.
pub const MAX_GC_WORKERS: u8 = 8;

impl GcOptions {
    /// A real run over all stores with the default worker count. `min_age_secs` is the
    /// configured `gc.min_age_secs`.
    #[must_use]
    pub const fn new(min_age_secs: u64) -> Self {
        Self {
            dry_run: false,
            delete_untagged: false,
            min_age_secs,
            workers: DEFAULT_GC_WORKERS,
            blob_store: None,
        }
    }

    /// Checks the worker count and that the run does not go below the configured minimum age.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for 0 or more than 8 workers and for a minimum age
    /// below `configured_min_age_secs`.
    pub fn validate(&self, configured_min_age_secs: u64) -> Result<(), DomainError> {
        if !(1..=MAX_GC_WORKERS).contains(&self.workers) {
            return Err(DomainError::invalid("gc workers must be 1 to 8"));
        }
        if self.min_age_secs < configured_min_age_secs {
            return Err(DomainError::invalid(
                "gc minimum age must not be below the configured minimum",
            ));
        }
        Ok(())
    }
}

/// Counters of a garbage collection run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcCounts {
    /// Blobs examined.
    pub examined: u64,
    /// Blobs that qualified for deletion.
    pub candidates: u64,
    /// Blobs deleted.
    pub deleted: u64,
    /// Deletions that failed.
    pub failed: u64,
    /// Bytes of the candidates.
    pub bytes_candidates: u64,
    /// Bytes of the deleted blobs.
    pub bytes_deleted: u64,
    /// Untagged OCI components deleted first.
    pub untagged_deleted: u64,
}

/// One garbage collection run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GcRun {
    /// Identifier.
    pub id: GcRunId,
    /// How it started.
    pub trigger: TaskTrigger,
    /// Options.
    pub options: GcOptions,
    /// State.
    pub status: TaskRunStatus,
    /// Counters.
    pub counts: GcCounts,
    /// Per blob store breakdown of `counts`, keyed by store name.
    pub per_store: serde_json::Value,
    /// Set by a stop request; the sweep reads it before every batch.
    pub stop_requested: bool,
    /// The task run that executes it.
    pub task_run_id: Option<TaskRunId>,
    /// Who asked for it, `None` for a scheduled run.
    pub requested_by: Option<UserId>,
    /// Start time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// End time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

impl GcRun {
    /// A queued run with zero counters.
    #[must_use]
    pub fn new(id: GcRunId, trigger: TaskTrigger, options: GcOptions) -> Self {
        Self {
            id,
            trigger,
            options,
            status: TaskRunStatus::Queued,
            counts: GcCounts::default(),
            per_store: serde_json::json!({}),
            stop_requested: false,
            task_run_id: None,
            requested_by: None,
            started_at: None,
            finished_at: None,
        }
    }
}

/// A blob a dry run would delete (the report keeps a sample of 1000 per run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcSample {
    /// The run.
    pub run_id: GcRunId,
    /// Blob store name.
    pub blob_store: String,
    /// Object key.
    pub key: String,
    /// Size.
    pub size_bytes: u64,
    /// When the blob lost its last reference.
    #[serde(with = "crate::rfc3339")]
    pub orphaned_at: OffsetDateTime,
}
