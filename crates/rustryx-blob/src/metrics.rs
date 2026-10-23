//! Metrics of the blob stores, through the `metrics` facade.
//!
//! The series are the four that `docs/slo.md` lists and that
//! `deploy/monitoring/rustryx-alerts.yaml` queries. Label names are fixed by those
//! files; changing one here silently blinds an alert, so the tests pin them:
//!
//! | Series | Type | Labels |
//! |---|---|---|
//! | `blob_store_ops_total` | counter | `store`, `op`, `result` |
//! | `blob_store_bytes_total` | counter | `store`, `direction` |
//! | `blob_store_free_bytes` | gauge | `store` |
//! | `blob_store_total_bytes` | gauge | `store` |
//!
//! Cardinality (SRE-09): `store` is the name of a blob store, which an administrator
//! creates by hand and which is a handful per installation. `op`, `result` and
//! `direction` are closed sets defined in this file. A key, a path, a repository or a
//! user never becomes a label value.

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use metrics::{counter, gauge};
use rustryx_core::BlobError;
use rustryx_core::ports::{BlobCommit, BlobWriter};

use crate::disk::DiskUsage;

/// Counter `blob_store_ops_total{store,op,result}`.
pub const OPS_TOTAL: &str = "blob_store_ops_total";
/// Counter `blob_store_bytes_total{store,direction}`.
pub const BYTES_TOTAL: &str = "blob_store_bytes_total";
/// Gauge `blob_store_free_bytes{store}`: bytes a non-root process can still write.
pub const FREE_BYTES: &str = "blob_store_free_bytes";
/// Gauge `blob_store_total_bytes{store}`: size of the file system under a local store.
pub const TOTAL_BYTES: &str = "blob_store_total_bytes";

/// The `op` label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    /// One whole write: `begin`, every `write`, `commit`. Counted once, at its end.
    Put,
    Get,
    Head,
    Delete,
    /// `list_keys` and `list_stale_staging`.
    List,
    Presign,
    Health,
}

impl Op {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Get => "get",
            Self::Head => "head",
            Self::Delete => "delete",
            Self::List => "list",
            Self::Presign => "presign",
            Self::Health => "health",
        }
    }
}

/// The `result` label. Only `error` feeds `RustryxBlobStoreErrors`, so the split decides
/// what pages somebody: a missing object or a bad range is the caller's business, a full
/// disk or an unreachable bucket is the operator's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Ok,
    /// The object does not exist.
    NotFound,
    /// A caller bug the store refused: invalid key, unsatisfiable range, unsupported call.
    Rejected,
    /// No upload slot. Backpressure, not a failure of the store.
    Overloaded,
    /// A write that was abandoned on purpose.
    Aborted,
    /// The store failed: unreachable, timed out, disk full, backend failure.
    Error,
}

impl Outcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotFound => "not_found",
            Self::Rejected => "rejected",
            Self::Overloaded => "overloaded",
            Self::Aborted => "aborted",
            Self::Error => "error",
        }
    }
}

/// Classifies an error. A variant this build does not know yet counts as `error`: the
/// alert fires on something unclassified instead of staying silent.
pub(crate) fn outcome_of_error(err: &BlobError) -> Outcome {
    match err {
        BlobError::NotFound => Outcome::NotFound,
        BlobError::RangeNotSatisfiable
        | BlobError::InvalidKey(_)
        | BlobError::Unsupported(_)
        | BlobError::WriterClosed => Outcome::Rejected,
        BlobError::Overloaded { .. } => Outcome::Overloaded,
        _ => Outcome::Error,
    }
}

/// The `direction` label of `blob_store_bytes_total`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Direction {
    /// Bytes accepted from a writer.
    Write,
    /// Bytes handed to a reader.
    Read,
}

impl Direction {
    const fn label(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
        }
    }
}

/// Emits the series of one store. Cheap to clone.
#[derive(Debug, Clone)]
pub(crate) struct StoreMetrics {
    store: String,
}

impl StoreMetrics {
    pub(crate) fn new(store: impl Into<String>) -> Self {
        ensure_test_recorder();
        Self {
            store: store.into(),
        }
    }

    pub(crate) fn op(&self, op: Op, outcome: Outcome) {
        counter!(
            OPS_TOTAL,
            "store" => self.store.clone(),
            "op" => op.label(),
            "result" => outcome.label()
        )
        .increment(1);
    }

    /// Counts the result of a call that has already produced its value.
    pub(crate) fn result<T>(&self, op: Op, result: &Result<T, BlobError>) {
        let outcome = result
            .as_ref()
            .map_or_else(outcome_of_error, |_| Outcome::Ok);
        self.op(op, outcome);
    }

    /// Counts an error under `op`.
    pub(crate) fn failed(&self, op: Op, err: &BlobError) {
        self.op(op, outcome_of_error(err));
    }

    pub(crate) fn bytes(&self, direction: Direction, bytes: u64) {
        counter!(
            BYTES_TOTAL,
            "store" => self.store.clone(),
            "direction" => direction.label()
        )
        .increment(bytes);
    }

    // A gauge is an f64. Disk sizes above 2^53 bytes (8 PiB) lose their last digits, which
    // no alert on a percentage can see.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn disk(&self, usage: DiskUsage) {
        gauge!(FREE_BYTES, "store" => self.store.clone()).set(usage.free_bytes as f64);
        gauge!(TOTAL_BYTES, "store" => self.store.clone()).set(usage.total_bytes as f64);
    }

    /// Wraps a read stream: counts the bytes it hands out and a failure in the middle of
    /// the body, which the `get` that opened the stream could not know about.
    pub(crate) fn meter_read(
        &self,
        stream: BoxStream<'static, Result<Bytes, BlobError>>,
    ) -> BoxStream<'static, Result<Bytes, BlobError>> {
        let metrics = self.clone();
        stream
            .inspect(move |item| match item {
                Ok(chunk) => metrics.bytes(Direction::Read, chunk.len() as u64),
                Err(err) => metrics.op(Op::Get, outcome_of_error(err)),
            })
            .boxed()
    }
}

/// Counts a write exactly once, at the moment its fate is known: the first failed
/// `write`, or `commit`, or `abort`. A writer that is dropped without any of them (a
/// cancelled request) is not counted: the format engine counts abandoned writers.
pub(crate) struct MeteredWriter<W> {
    inner: W,
    metrics: StoreMetrics,
    counted: bool,
}

impl<W: BlobWriter> MeteredWriter<W> {
    pub(crate) fn new(inner: W, metrics: StoreMetrics) -> Self {
        Self {
            inner,
            metrics,
            counted: false,
        }
    }

    fn count(&mut self, outcome: Outcome) {
        if !self.counted {
            self.counted = true;
            self.metrics.op(Op::Put, outcome);
        }
    }
}

#[async_trait]
impl<W: BlobWriter> BlobWriter for MeteredWriter<W> {
    async fn write(&mut self, chunk: Bytes) -> Result<(), BlobError> {
        let len = chunk.len() as u64;
        match self.inner.write(chunk).await {
            Ok(()) => {
                self.metrics.bytes(Direction::Write, len);
                Ok(())
            }
            Err(err) => {
                self.count(outcome_of_error(&err));
                Err(err)
            }
        }
    }

    async fn commit(self: Box<Self>) -> Result<BlobCommit, BlobError> {
        let Self {
            inner,
            metrics,
            counted,
        } = *self;
        let result = Box::new(inner).commit().await;
        if !counted {
            metrics.result(Op::Put, &result);
        }
        result
    }

    async fn abort(self: Box<Self>) -> Result<(), BlobError> {
        let Self {
            inner,
            metrics,
            counted,
        } = *self;
        let result = Box::new(inner).abort().await;
        if !counted {
            let outcome = match &result {
                Ok(()) => Outcome::Aborted,
                Err(err) => outcome_of_error(err),
            };
            metrics.op(Op::Put, outcome);
        }
        result
    }
}

/// Unit tests read the series back, so the recorder must exist before the first emission of
/// any store, and a store is always created before it emits. Nothing in a production build.
#[cfg(not(test))]
fn ensure_test_recorder() {}

#[cfg(test)]
fn ensure_test_recorder() {
    capture::capture();
}

#[cfg(test)]
pub(crate) mod capture;
#[cfg(test)]
mod tests;
