//! W3C Trace Context at the edges: adopt an incoming `traceparent`, stamp log
//! lines with trace ids, and inject the current context into outgoing requests.
//!
//! All three are cheap no-ops when OTLP export is not configured: without the
//! OpenTelemetry layer a span has no context and the global propagator does nothing.

use http::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::TraceContextExt;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}

struct HeaderInjector<'a>(&'a mut HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            self.0.insert(name, value);
        }
    }
}

/// Makes `span` a child of the remote span named by `traceparent`/`tracestate`.
/// Returns true when a valid remote parent was applied. Call right after the span
/// is created, before anything is recorded on it.
// The span is changed as a side effect; callers that only want that may ignore the flag.
#[allow(clippy::must_use_candidate)]
pub fn adopt_remote_parent(span: &Span, headers: &HeaderMap) -> bool {
    if !headers.contains_key("traceparent") {
        return false;
    }
    let context = global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(headers)));
    if !context.span().span_context().is_valid() {
        return false;
    }
    span.set_parent(context).is_ok()
}

/// Records `trace_id` and `span_id` on `span` when it belongs to a trace. The span
/// must declare both fields (as `tracing::field::Empty`), which is how log lines
/// emitted inside it get them.
pub fn record_trace_ids(span: &Span) {
    let context = span.context();
    let span_context = context.span().span_context().clone();
    if span_context.is_valid() {
        span.record("trace_id", span_context.trace_id().to_string());
        span.record("span_id", span_context.span_id().to_string());
    }
}

/// Writes `traceparent` (and `tracestate`) for the current span into `headers`.
/// Outgoing upstream, replication and webhook requests call this.
pub fn inject_current_context(headers: &mut HeaderMap) {
    let context = Span::current().context();
    global::get_text_map_propagator(|p| p.inject_context(&context, &mut HeaderInjector(headers)));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::log_layer::test_support::Capture;
    use crate::log_layer::{LogFormat, LogLayer};
    use crate::redact::Redactor;

    const TRACE_ID: &str = "0af7651916cd43dd8448eb211c80319c";
    const PARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

    fn with_tracing<R>(f: impl FnOnce() -> R) -> (R, Capture) {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .build();
        let tracer = provider.tracer("test");
        let capture = Capture::default();
        let redactor = Arc::new(Redactor::standard().expect("patterns"));
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .with(LogLayer::new(capture.clone(), redactor, LogFormat::Json));
        let result = tracing::subscriber::with_default(subscriber, f);
        (result, capture)
    }

    fn headers(traceparent: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "traceparent",
            HeaderValue::from_str(traceparent).expect("header"),
        );
        h
    }

    #[test]
    fn incoming_traceparent_becomes_the_parent_and_logs_carry_its_trace_id() {
        let (adopted, capture) = with_tracing(|| {
            let span = tracing::info_span!(
                "request",
                trace_id = tracing::field::Empty,
                span_id = tracing::field::Empty
            );
            let adopted = adopt_remote_parent(&span, &headers(PARENT));
            record_trace_ids(&span);
            let _e = span.enter();
            tracing::info!("handled");
            adopted
        });
        assert!(adopted);
        let line = &capture.lines()[0];
        assert_eq!(line["trace_id"], TRACE_ID);
        let span_id = line["span_id"].as_str().expect("span id");
        assert_eq!(span_id.len(), 16);
        assert_ne!(
            span_id, "b7ad6b7169203331",
            "the request span is a child, not the parent"
        );
    }

    #[test]
    fn without_traceparent_a_new_trace_still_gets_ids() {
        let (adopted, capture) = with_tracing(|| {
            let span = tracing::info_span!(
                "request",
                trace_id = tracing::field::Empty,
                span_id = tracing::field::Empty
            );
            let adopted = adopt_remote_parent(&span, &HeaderMap::new());
            record_trace_ids(&span);
            let _e = span.enter();
            tracing::info!("handled");
            adopted
        });
        assert!(!adopted);
        assert_eq!(
            capture.lines()[0]["trace_id"].as_str().map(str::len),
            Some(32)
        );
    }

    #[test]
    fn must_not_adopt_a_malformed_traceparent() {
        for bad in [
            "garbage",
            "00-zz-yy-01",
            "00-00000000000000000000000000000000-0000000000000000-01",
        ] {
            let (adopted, _) = with_tracing(|| {
                let span = tracing::info_span!("request");
                adopt_remote_parent(&span, &headers(bad))
            });
            assert!(!adopted, "{bad}");
        }
    }

    #[test]
    fn without_the_otel_layer_everything_is_a_noop() {
        let capture = Capture::default();
        let redactor = Arc::new(Redactor::standard().expect("patterns"));
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(
            capture.clone(),
            redactor,
            LogFormat::Json,
        ));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", trace_id = tracing::field::Empty);
            assert!(!adopt_remote_parent(&span, &headers(PARENT)));
            record_trace_ids(&span);
            let _e = span.enter();
            tracing::info!("handled");
        });
        assert!(capture.lines()[0].get("trace_id").is_none());
    }

    #[test]
    fn current_context_is_injected_into_outgoing_headers() {
        let (injected, _) = with_tracing(|| {
            let span = tracing::info_span!("outgoing");
            let _e = span.enter();
            let mut out = HeaderMap::new();
            inject_current_context(&mut out);
            out
        });
        let value = injected.get("traceparent").expect("traceparent injected");
        assert!(value.to_str().expect("ascii").starts_with("00-"));
    }

    #[test]
    fn non_ascii_header_values_are_ignored_by_the_extractor() {
        let mut h = HeaderMap::new();
        h.insert(
            "traceparent",
            HeaderValue::from_bytes(&[0xff, 0xfe]).expect("opaque bytes"),
        );
        let extractor = HeaderExtractor(&h);
        assert!(extractor.get("traceparent").is_none());
        assert_eq!(extractor.keys(), vec!["traceparent"]);
    }
}
