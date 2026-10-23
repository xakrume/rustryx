//! The log layer: one JSON object per line, or a plain text line for local work.
//!
//! `tracing_subscriber`'s JSON formatter nests event fields under `fields` and
//! names the message `message` and the time `timestamp`. The log contract (SRE-11)
//! wants flat keys: `ts`, `level`, `msg`, `request_id`, `trace_id`, `span_id`,
//! `route`, `status`, `duration_ms`, `repo`. This layer produces exactly that:
//! the fields of every enclosing span (outermost first) are merged into the line,
//! then the fields of the event, then the reserved keys, so an event can never
//! overwrite `ts`, `level` or `target`.
//!
//! Every text value goes through the [`Redactor`] and fields with secret-looking
//! names are replaced wholesale before the line is rendered. The text format is
//! rendered from the same redacted map, so it cannot leak what JSON would not.

use std::fmt;
use std::io::Write;
use std::sync::Arc;

use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::redact::{REDACTED, Redactor};

/// Fields recorded on a span, kept in its extensions.
struct SpanFields(Map<String, Value>);

/// Collects tracing fields into a JSON map, redacting as it goes.
struct FieldCollector<'a> {
    redactor: &'a Redactor,
    map: &'a mut Map<String, Value>,
}

impl FieldCollector<'_> {
    fn insert(&mut self, field: &Field, value: Value) {
        let name = field.name();
        let key = if name == "message" { "msg" } else { name };
        let value = if Redactor::is_sensitive_name(name) {
            Value::String(REDACTED.to_owned())
        } else {
            value
        };
        self.map.insert(key.to_owned(), value);
    }

    fn text(&self, text: &str) -> Value {
        Value::String(self.redactor.scrub(text).into_owned())
    }
}

impl Visit for FieldCollector<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        let value = self.text(value);
        self.insert(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // JSON has no NaN or infinity; `from_f64` yields None for them.
        self.insert(
            field,
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
        );
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        // The whole source chain: the log is where internal detail belongs.
        let mut text = value.to_string();
        let mut source = value.source();
        while let Some(cause) = source {
            text.push_str(": ");
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        let value = self.text(&text);
        self.insert(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let value = self.text(&format!("{value:?}"));
        self.insert(field, value);
    }
}

/// Line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line.
    Json,
    /// `ts LEVEL target msg key=value ...` for local development.
    Text,
}

/// Layer writing flat log lines to a [`MakeWriter`].
pub struct LogLayer<W> {
    make_writer: W,
    redactor: Arc<Redactor>,
    format: LogFormat,
}

impl<W> LogLayer<W> {
    /// Creates the layer.
    pub fn new(make_writer: W, redactor: Arc<Redactor>, format: LogFormat) -> Self {
        Self {
            make_writer,
            redactor,
            format,
        }
    }
}

impl<S, W> Layer<S> for LogLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut map = Map::new();
        attrs.record(&mut FieldCollector {
            redactor: &self.redactor,
            map: &mut map,
        });
        span.extensions_mut().insert(SpanFields(map));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<SpanFields>() {
            values.record(&mut FieldCollector {
                redactor: &self.redactor,
                map: &mut fields.0,
            });
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut line = Map::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<SpanFields>() {
                    for (key, value) in &fields.0 {
                        line.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        event.record(&mut FieldCollector {
            redactor: &self.redactor,
            map: &mut line,
        });
        let meta = event.metadata();
        line.insert("ts".to_owned(), Value::String(now_rfc3339()));
        line.insert(
            "level".to_owned(),
            Value::String(meta.level().as_str().to_ascii_lowercase()),
        );
        line.insert("target".to_owned(), Value::String(meta.target().to_owned()));
        line.entry("msg")
            .or_insert_with(|| Value::String(String::new()));

        let mut rendered = match self.format {
            LogFormat::Json => match serde_json::to_vec(&Value::Object(line)) {
                Ok(bytes) => bytes,
                Err(_) => return,
            },
            LogFormat::Text => render_text(&line).into_bytes(),
        };
        rendered.push(b'\n');
        write_line(&self.make_writer, &rendered);
    }
}

/// `ts LEVEL target: msg key=value ...`; values with spaces or quotes are JSON quoted.
fn render_text(line: &Map<String, Value>) -> String {
    let text = |key: &str| line.get(key).and_then(Value::as_str).unwrap_or_default();
    let mut out = format!(
        "{} {} {}: {}",
        text("ts"),
        text("level").to_ascii_uppercase(),
        text("target"),
        text("msg")
    );
    for (key, value) in line {
        if matches!(key.as_str(), "ts" | "level" | "target" | "msg") {
            continue;
        }
        let rendered = match value {
            Value::String(s) if !s.is_empty() && !s.contains([' ', '"', '\n', '\r', '\t']) => {
                s.clone()
            }
            other => other.to_string(),
        };
        out.push(' ');
        out.push_str(key);
        out.push('=');
        out.push_str(&rendered);
    }
    out
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Writes one rendered line with a single `write_all`, so that concurrent lines do
/// not interleave.
fn write_line<W>(make_writer: &W, rendered: &[u8])
where
    W: for<'w> MakeWriter<'w>,
{
    // A failed write has nowhere to be reported: the log is the error channel. The
    // non-blocking writer drops lines when its queue is full and counts them, which
    // `ObservabilityGuard::dropped_log_lines` exposes.
    let _ = make_writer.make_writer().write_all(rendered);
}

#[cfg(test)]
pub(crate) mod test_support {
    //! In-memory writer shared by the tests of this crate.

    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use tracing_subscriber::fmt::MakeWriter;

    /// Collects everything written to it.
    #[derive(Clone, Default)]
    pub struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        pub fn text(&self) -> String {
            let bytes = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            String::from_utf8_lossy(&bytes).into_owned()
        }

        pub fn lines(&self) -> Vec<serde_json::Value> {
            self.text()
                .lines()
                .map(|l| serde_json::from_str(l).expect("each line is JSON"))
                .collect()
        }
    }

    pub struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CaptureWriter(self.0.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::test_support::Capture;
    use super::*;

    fn run(f: impl FnOnce()) -> Vec<Value> {
        let capture = Capture::default();
        let redactor = Arc::new(Redactor::standard().expect("patterns"));
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(
            capture.clone(),
            redactor,
            LogFormat::Json,
        ));
        tracing::subscriber::with_default(subscriber, f);
        capture.lines()
    }

    #[test]
    fn line_has_the_contract_keys() {
        let lines = run(|| tracing::info!(status = 200_u64, duration_ms = 3_u64, "served"));
        let line = &lines[0];
        assert_eq!(line["level"], "info");
        assert_eq!(line["msg"], "served");
        assert_eq!(line["status"], 200);
        assert_eq!(line["duration_ms"], 3);
        assert!(line["ts"].as_str().expect("ts").ends_with('Z'));
        assert!(
            line["target"]
                .as_str()
                .expect("target")
                .contains("log_layer")
        );
    }

    #[test]
    fn span_fields_are_inherited_by_events() {
        let lines = run(|| {
            let outer = tracing::info_span!("outer", request_id = "req-1", route = "/a");
            let _o = outer.enter();
            let inner = tracing::info_span!("inner", route = "/b", repo = "maven-central");
            let _i = inner.enter();
            tracing::warn!("inside");
        });
        let line = &lines[0];
        assert_eq!(line["request_id"], "req-1");
        assert_eq!(line["route"], "/b", "inner span wins");
        assert_eq!(line["repo"], "maven-central");
        assert_eq!(line["level"], "warn");
    }

    #[test]
    fn late_recorded_span_fields_appear() {
        let lines = run(|| {
            let span = tracing::info_span!("req", status = tracing::field::Empty);
            let _e = span.enter();
            span.record("status", 404_u64);
            tracing::info!("done");
        });
        assert_eq!(lines[0]["status"], 404);
    }

    #[test]
    fn events_cannot_overwrite_reserved_keys() {
        let lines = run(|| tracing::info!(ts = "forged", level = "forged", target = "forged", "x"));
        let line = &lines[0];
        assert_ne!(line["ts"], "forged");
        assert_eq!(line["level"], "info");
        assert_ne!(line["target"], "forged");
    }

    #[test]
    fn must_not_log_secret_named_fields() {
        let lines = run(|| {
            tracing::info!(
                password = "hunter2",
                authorization = "Bearer abc",
                client_secret = "cs",
                api_key = 12345_u64,
                user = "bob",
                "login"
            );
        });
        let text = serde_json::to_string(&lines[0]).expect("json");
        for leaked in ["hunter2", "Bearer abc", "\"cs\"", "12345"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        assert_eq!(lines[0]["user"], "bob");
        assert_eq!(lines[0]["password"], REDACTED);
    }

    #[test]
    fn must_not_log_secrets_inside_messages_and_debug_values() {
        let lines = run(|| {
            let url = "postgres://app:hunter2@db/x";
            tracing::error!(?url, "failed to use token ryx_AbCdEf0123456789");
        });
        let text = serde_json::to_string(&lines[0]).expect("json");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("AbCdEf0123456789"), "{text}");
    }

    #[test]
    fn must_not_log_secrets_in_span_fields() {
        let lines = run(|| {
            let span = tracing::info_span!(
                "req",
                authorization = "Bearer topsecretvalue",
                note = "token=abc12345"
            );
            let _e = span.enter();
            tracing::info!("inside");
        });
        let text = serde_json::to_string(&lines[0]).expect("json");
        assert!(!text.contains("topsecretvalue"), "{text}");
        assert!(!text.contains("abc12345"), "{text}");
    }

    #[test]
    fn error_values_log_the_whole_source_chain() {
        #[derive(Debug, thiserror::Error)]
        #[error("outer failed")]
        struct Outer(#[source] Inner);
        #[derive(Debug, thiserror::Error)]
        #[error("inner cause")]
        struct Inner;
        let lines = run(|| {
            let err = Outer(Inner);
            tracing::error!(error = &err as &dyn std::error::Error, "request failed");
        });
        assert_eq!(lines[0]["error"], "outer failed: inner cause");
    }

    #[test]
    fn newlines_in_values_cannot_forge_log_lines() {
        let capture = Capture::default();
        let redactor = Arc::new(Redactor::standard().expect("patterns"));
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(
            capture.clone(),
            redactor,
            LogFormat::Json,
        ));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                user_agent = "x\n{\"level\":\"error\",\"msg\":\"forged\"}",
                "request"
            );
        });
        assert_eq!(capture.text().lines().count(), 1, "one event, one line");
    }

    #[test]
    fn non_finite_floats_become_null() {
        let lines = run(|| {
            tracing::info!(
                ratio = f64::NAN,
                ok = 0.5_f64,
                bool_field = true,
                neg = -3_i64,
                "x"
            );
        });
        assert_eq!(lines[0]["ratio"], Value::Null);
        assert_eq!(lines[0]["ok"], 0.5);
        assert_eq!(lines[0]["bool_field"], true);
        assert_eq!(lines[0]["neg"], -3);
    }

    #[test]
    fn event_without_message_still_has_msg() {
        let lines = run(|| tracing::info!(status = 1_u64));
        assert_eq!(lines[0]["msg"], "");
    }
}
