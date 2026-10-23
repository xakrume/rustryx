//! Observability for Rustryx: structured logs, Prometheus metrics, optional OTLP traces.
//!
//! - [`init`] installs the global log subscriber (flat JSON lines on stdout through
//!   a non-blocking writer) and, only when an endpoint is configured, the OTLP
//!   exporter.
//! - [`MetricsHandle`] is the Prometheus recorder; the HTTP layer renders it at `/metrics`.
//! - [`names`] holds the standard metric names.
//! - [`redact`] is the secret scrubber every log value passes through.
//! - [`propagation`] adopts and injects W3C trace context.

// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod init;
mod log_filter;
mod log_layer;
pub mod names;
mod otlp;
mod prometheus;
pub mod propagation;
pub mod redact;

pub use init::{DroppedLines, ObservabilityGuard, ObservabilityOptions, build_subscriber, init};
pub use log_filter::LogFilter;
pub use log_layer::{LogFormat, LogLayer};
pub use otlp::{OtlpOptions, OtlpProtocol};
pub use prometheus::{
    DURATION_BUCKETS, InFlightGuard, MetricsHandle, PoolStats, UNMATCHED_ROUTE, normalize_method,
};
pub use redact::Redactor;

/// Failure to set up observability.
#[derive(Debug, thiserror::Error)]
pub enum ObservabilityError {
    /// The log filter text is not valid.
    #[error("invalid log filter: {0}")]
    LogFilter(String),
    /// The redaction patterns failed to compile (a programming error).
    #[error("redaction patterns do not compile: {0}")]
    Redactor(#[source] regex::Error),
    /// The metrics recorder could not be built or installed.
    #[error("metrics recorder: {0}")]
    Metrics(String),
    /// The OTLP exporter could not be built or flushed.
    #[error("trace exporter: {0}")]
    Otlp(String),
    /// A global tracing subscriber is already installed.
    #[error("cannot install the tracing subscriber: {0}")]
    Subscriber(String),
}
