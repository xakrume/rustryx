//! Wiring: log layer, optional OTLP layer, global subscriber and shutdown.

use std::sync::Arc;

use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use tracing::{Level, Subscriber};
use tracing_appender::non_blocking::{ErrorCounter, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::filter::{LevelFilter, filter_fn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

use crate::ObservabilityError;
use crate::log_filter::LogFilter;
use crate::log_layer::{LogFormat, LogLayer};
use crate::otlp::{self, OtlpOptions};
use crate::redact::Redactor;

/// Log lines buffered before the writer starts dropping. At roughly 300 bytes a
/// line this is a few tens of megabytes at most, and only fills if stdout stalls.
const LOG_QUEUE_LINES: usize = 65_536;

/// Everything `init` needs. Built by the server from the configuration, so this
/// crate does not depend on `rustryx-config`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservabilityOptions {
    /// Log filter, see [`LogFilter`].
    pub log_filter: String,
    /// Line format.
    pub log_format: LogFormat,
    /// `service.name` of exported traces.
    pub service_name: String,
    /// Trace export. `None` builds no exporter at all.
    pub otlp: Option<OtlpOptions>,
}

impl Default for ObservabilityOptions {
    fn default() -> Self {
        Self {
            log_filter: "info".to_owned(),
            log_format: LogFormat::Json,
            service_name: "rustryx".to_owned(),
            otlp: None,
        }
    }
}

/// Reads the number of log lines dropped so far. Cloneable, so a metrics task can
/// keep it after the guard has moved elsewhere.
#[derive(Debug, Clone)]
pub struct DroppedLines(ErrorCounter);

impl DroppedLines {
    /// Lines dropped since start.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.dropped_lines() as u64
    }
}

/// Keeps logging and trace export alive; flush and stop them with [`shutdown`](Self::shutdown).
pub struct ObservabilityGuard {
    worker: Option<WorkerGuard>,
    provider: Option<SdkTracerProvider>,
    dropped: Option<ErrorCounter>,
}

impl std::fmt::Debug for ObservabilityGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservabilityGuard")
            .field("otlp", &self.provider.is_some())
            .finish_non_exhaustive()
    }
}

impl ObservabilityGuard {
    /// True when a trace exporter is running.
    #[must_use]
    pub fn tracing_enabled(&self) -> bool {
        self.provider.is_some()
    }

    /// Log lines dropped so far because the output queue was full.
    #[must_use]
    pub fn dropped_log_lines(&self) -> u64 {
        self.dropped
            .as_ref()
            .map_or(0, |c| c.dropped_lines() as u64)
    }

    /// A handle that keeps reading the dropped-line count after the guard moved.
    #[must_use]
    pub fn dropped_lines(&self) -> Option<DroppedLines> {
        self.dropped.clone().map(DroppedLines)
    }

    /// Flushes buffered spans and log lines and stops the exporter.
    ///
    /// Blocks while the batch thread exports, so call it from a plain thread (after
    /// the runtime has finished), not from async code.
    ///
    /// # Errors
    /// [`ObservabilityError::Otlp`] if the exporter failed to flush.
    pub fn shutdown(mut self) -> Result<(), ObservabilityError> {
        let result = match self.provider.take() {
            Some(provider) => provider
                .shutdown()
                .map_err(|e| ObservabilityError::Otlp(e.to_string())),
            None => Ok(()),
        };
        // Dropping the worker guard flushes the queued log lines.
        drop(self.worker.take());
        result
    }
}

/// Installs the global subscriber that writes to stdout.
///
/// When `options.otlp` uses gRPC this must run inside a tokio runtime context
/// (`Runtime::enter`), because the exporter's channel is created lazily on it.
///
/// # Errors
/// Invalid filter, a failed exporter build, or a subscriber already installed.
pub fn init(options: &ObservabilityOptions) -> Result<ObservabilityGuard, ObservabilityError> {
    let (writer, worker) = NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(LOG_QUEUE_LINES)
        .thread_name("rustryx-log")
        .finish(std::io::stdout());
    let dropped = writer.error_counter();
    let exporter = options
        .otlp
        .as_ref()
        .map(|otlp| otlp::build(&options.service_name, otlp))
        .transpose()?;
    let provider = exporter.as_ref().map(|e| e.provider.clone());
    let tracer = exporter.map(|e| e.tracer);

    let subscriber = build_subscriber(options, writer, tracer)?;
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| ObservabilityError::Subscriber(e.to_string()))?;
    Ok(ObservabilityGuard {
        worker: Some(worker),
        provider,
        dropped: Some(dropped),
    })
}

/// Builds the subscriber without installing it (tests install it per thread).
///
/// # Errors
/// Invalid filter or redaction patterns.
pub fn build_subscriber<W>(
    options: &ObservabilityOptions,
    writer: W,
    tracer: Option<SdkTracer>,
) -> Result<Box<dyn Subscriber + Send + Sync>, ObservabilityError>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let filter = LogFilter::parse(&options.log_filter)?;
    let redactor = Arc::new(Redactor::standard().map_err(ObservabilityError::Redactor)?);
    // Request spans are INFO; they must stay enabled even when the log level is
    // WARN or ERROR, or those log lines lose their request id. See `LogFilter::enabled`.
    let hint = filter.max_level().max(LevelFilter::INFO);

    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();
    if let Some(tracer) = tracer {
        // Only this workspace's spans at INFO or above are exported; dependency
        // spans (sqlx, hyper) would carry details that do not belong in traces.
        let only_ours =
            filter_fn(|meta| meta.target().starts_with("rustryx") && *meta.level() <= Level::INFO)
                .with_max_level_hint(LevelFilter::INFO);
        layers.push(Box::new(
            tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(only_ours),
        ));
    }
    let log_filter = filter_fn(move |meta| filter.enabled(meta)).with_max_level_hint(hint);
    layers.push(Box::new(
        LogLayer::new(writer, redactor, options.log_format).with_filter(log_filter),
    ));
    Ok(Box::new(tracing_subscriber::registry().with(layers)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_layer::test_support::Capture;

    fn run(options: &ObservabilityOptions, f: impl FnOnce()) -> Capture {
        let capture = Capture::default();
        let subscriber = build_subscriber(options, capture.clone(), None).expect("builds");
        tracing::subscriber::with_default(subscriber, f);
        capture
    }

    fn options(filter: &str) -> ObservabilityOptions {
        ObservabilityOptions {
            log_filter: filter.to_owned(),
            ..ObservabilityOptions::default()
        }
    }

    #[test]
    fn filter_applies_to_events() {
        let capture = run(&options("warn"), || {
            tracing::info!("hidden");
            tracing::warn!("shown");
        });
        let lines = capture.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["msg"], "shown");
    }

    #[test]
    fn target_directives_override_the_default() {
        let capture = run(&options("error,rustryx_observability=debug"), || {
            tracing::debug!("ours");
        });
        assert_eq!(capture.lines().len(), 1);
    }

    #[test]
    fn must_not_drop_request_context_when_the_level_hides_info_spans() {
        let capture = run(&options("error"), || {
            let span = tracing::info_span!("http.request", request_id = "req-42");
            let _e = span.enter();
            tracing::error!("boom");
        });
        let lines = capture.lines();
        assert_eq!(lines.len(), 1, "the info span itself is not logged");
        assert_eq!(lines[0]["request_id"], "req-42");
    }

    #[test]
    fn text_format_is_redacted_and_one_line_per_event() {
        let mut o = options("info");
        o.log_format = LogFormat::Text;
        let capture = run(&o, || {
            tracing::info!(
                user = "bob",
                password = "hunter2",
                note = "two words",
                "login ok"
            );
        });
        let text = capture.text();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains(" INFO "), "{text}");
        assert!(text.contains("login ok"), "{text}");
        assert!(text.contains("user=bob"), "{text}");
        assert!(text.contains(r#"note="two words""#), "{text}");
        assert!(
            !text.contains("hunter2"),
            "text format must redact too: {text}"
        );
    }

    #[test]
    fn invalid_filter_is_an_error_before_anything_is_installed() {
        let result = build_subscriber(&options("loud"), Capture::default(), None);
        assert!(matches!(result, Err(ObservabilityError::LogFilter(_))));
    }

    #[test]
    fn must_not_build_an_exporter_without_an_endpoint() {
        // No runtime is entered here: building any gRPC exporter would fail, so a
        // successful default `init`-style assembly proves nothing OTLP was created.
        let subscriber =
            build_subscriber(&ObservabilityOptions::default(), Capture::default(), None);
        assert!(subscriber.is_ok());
        assert!(ObservabilityOptions::default().otlp.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn otlp_layer_is_added_when_a_tracer_is_given() {
        use opentelemetry::trace::TracerProvider as _;
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let tracer = provider.tracer("t");
        let capture = Capture::default();
        let subscriber =
            build_subscriber(&options("info"), capture.clone(), Some(tracer)).expect("builds");
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", trace_id = tracing::field::Empty);
            crate::propagation::record_trace_ids(&span);
            let _e = span.enter();
            tracing::info!("traced");
        });
        assert_eq!(
            capture.lines()[0]["trace_id"].as_str().map(str::len),
            Some(32)
        );
    }

    #[test]
    fn guard_without_exporter_shuts_down_cleanly() {
        let guard = ObservabilityGuard {
            worker: None,
            provider: None,
            dropped: None,
        };
        assert!(!guard.tracing_enabled());
        assert_eq!(guard.dropped_log_lines(), 0);
        assert!(format!("{guard:?}").contains("otlp"));
        assert!(guard.shutdown().is_ok());
    }
}
