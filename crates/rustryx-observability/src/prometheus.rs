//! Prometheus metrics: a recorder you can hold, render and, once, install globally.
//!
//! Emitting through the `metrics` macros needs a *global* recorder, which can only
//! be installed once per process and makes tests that read metrics back awkward.
//! [`MetricsHandle`] therefore also exposes typed helpers that register series on
//! this handle's own recorder, so the HTTP layer records through the handle it was
//! given and an in-process test sees exactly its own series. Crates that use the
//! macros (database pool, upstream client) emit to the same recorder once the
//! server calls [`MetricsHandle::install_global`].

use std::sync::Arc;
use std::time::Duration;

use metrics::{
    Counter, Gauge, Histogram, Key, KeyName, Label, Level, Metadata, Recorder, SharedString, Unit,
};
use metrics_exporter_prometheus::{
    Matcher, PrometheusBuilder, PrometheusHandle, PrometheusRecorder,
};
use tokio_util::sync::CancellationToken;

use crate::ObservabilityError;
use crate::names::{self, MetricKind};

/// Histogram buckets for every `*_seconds` metric: 5 ms to 60 s (SRE-08).
pub const DURATION_BUCKETS: [f64; 13] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// Request methods that get their own label value. Anything else is `OTHER`: the
/// method is attacker-controlled text, and a label value per distinct string would
/// let one client create unbounded series.
const KNOWN_METHODS: [&str; 9] = [
    "GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "CONNECT", "TRACE",
];

/// Route label for requests that matched no route (404, 405 fallbacks).
pub const UNMATCHED_ROUTE: &str = "unmatched";

struct Shared {
    recorder: PrometheusRecorder,
    handle: PrometheusHandle,
}

/// Cheap-to-clone access to one Prometheus recorder.
#[derive(Clone)]
pub struct MetricsHandle {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for MetricsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MetricsHandle")
    }
}

/// Database pool occupancy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// Connections open.
    pub size: u32,
    /// Connections idle.
    pub idle: u32,
}

/// Decrements `http_requests_in_flight` when dropped.
#[derive(Debug)]
pub struct InFlightGuard(Gauge);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.decrement(1.0);
    }
}

impl MetricsHandle {
    /// Builds a recorder with the standard buckets and descriptions. Nothing is
    /// installed globally.
    ///
    /// # Errors
    /// [`ObservabilityError::Metrics`] if the exporter rejects the bucket setup.
    pub fn new() -> Result<Self, ObservabilityError> {
        let recorder = PrometheusBuilder::new()
            .set_buckets_for_metric(Matcher::Suffix("_seconds".to_owned()), &DURATION_BUCKETS)
            .map_err(|e| ObservabilityError::Metrics(e.to_string()))?
            .build_recorder();
        let handle = recorder.handle();
        let this = Self {
            shared: Arc::new(Shared { recorder, handle }),
        };
        this.describe_standard();
        Ok(this)
    }

    fn describe_standard(&self) {
        let recorder = &self.shared.recorder;
        for (name, kind, help) in names::STANDARD {
            let unit = name.ends_with("_seconds").then_some(Unit::Seconds);
            let key = KeyName::from(*name);
            let help = SharedString::from(*help);
            match kind {
                MetricKind::Counter => recorder.describe_counter(key, unit, help),
                MetricKind::Gauge => recorder.describe_gauge(key, unit, help),
                MetricKind::Histogram => recorder.describe_histogram(key, unit, help),
            }
        }
    }

    /// Installs this recorder as the process-wide one, so that `metrics::counter!`
    /// and friends in any crate end up here.
    ///
    /// # Errors
    /// [`ObservabilityError::Metrics`] if a global recorder is already installed.
    pub fn install_global(&self) -> Result<(), ObservabilityError> {
        metrics::set_global_recorder(GlobalRecorder(self.shared.clone()))
            .map_err(|e| ObservabilityError::Metrics(e.to_string()))
    }

    /// The exposition text served at `/metrics`.
    #[must_use]
    pub fn render(&self) -> String {
        self.shared.handle.render()
    }

    /// Drains and ages the recorder's internal state. Call periodically.
    pub fn run_upkeep_once(&self) {
        self.shared.handle.run_upkeep();
    }

    /// Runs [`run_upkeep_once`](Self::run_upkeep_once) every `interval` until `cancel` fires.
    pub async fn run_upkeep(self, interval: Duration, cancel: CancellationToken) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => self.run_upkeep_once(),
            }
        }
    }

    fn metadata() -> Metadata<'static> {
        Metadata::new(module_path!(), Level::INFO, Some(module_path!()))
    }

    fn key(name: &'static str, labels: &[(&'static str, &str)]) -> Key {
        let labels: Vec<Label> = labels
            .iter()
            .map(|(k, v)| Label::new(*k, (*v).to_owned()))
            .collect();
        Key::from_parts(name, labels)
    }

    /// A counter series on this handle's recorder.
    pub fn counter(&self, name: &'static str, labels: &[(&'static str, &str)]) -> Counter {
        self.shared
            .recorder
            .register_counter(&Self::key(name, labels), &Self::metadata())
    }

    /// A gauge series on this handle's recorder.
    pub fn gauge(&self, name: &'static str, labels: &[(&'static str, &str)]) -> Gauge {
        self.shared
            .recorder
            .register_gauge(&Self::key(name, labels), &Self::metadata())
    }

    /// A histogram series on this handle's recorder.
    pub fn histogram(&self, name: &'static str, labels: &[(&'static str, &str)]) -> Histogram {
        self.shared
            .recorder
            .register_histogram(&Self::key(name, labels), &Self::metadata())
    }

    /// Records one finished request. `route` must be a route template, never a path.
    pub fn record_http_request(&self, route: &str, method: &str, status: u16, elapsed: Duration) {
        let method = normalize_method(method);
        let status = status.to_string();
        self.counter(
            names::HTTP_REQUESTS_TOTAL,
            &[("route", route), ("method", method), ("status", &status)],
        )
        .increment(1);
        self.histogram(
            names::HTTP_REQUEST_DURATION_SECONDS,
            &[("route", route), ("method", method)],
        )
        .record(elapsed.as_secs_f64());
    }

    /// Counts a request as in flight until the guard is dropped.
    #[must_use]
    pub fn track_in_flight(&self) -> InFlightGuard {
        let gauge = self.gauge(names::HTTP_REQUESTS_IN_FLIGHT, &[]);
        gauge.increment(1.0);
        InFlightGuard(gauge)
    }

    /// Counts a request refused by the service's own backpressure.
    pub fn record_shed(&self, reason: &'static str) {
        self.counter(names::HTTP_SHED_TOTAL, &[("reason", reason)])
            .increment(1);
    }

    /// Counts an authentication attempt. `method` and `result` must come from a
    /// fixed vocabulary.
    pub fn record_auth_attempt(&self, method: &'static str, result: &'static str) {
        self.counter(
            names::AUTH_ATTEMPTS_TOTAL,
            &[("method", method), ("result", result)],
        )
        .increment(1);
    }

    /// Publishes the pool gauges. Called by the database crate's sampler.
    pub fn set_db_pool(&self, stats: PoolStats) {
        let in_use = stats.size.saturating_sub(stats.idle);
        self.gauge(names::DB_POOL_SIZE, &[])
            .set(f64::from(stats.size));
        self.gauge(names::DB_POOL_IDLE, &[])
            .set(f64::from(stats.idle));
        self.gauge(names::DB_POOL_IN_USE, &[])
            .set(f64::from(in_use));
    }

    /// Records an acquisition that had to wait for a pool connection.
    pub fn record_db_pool_wait(&self, waited: Duration) {
        self.counter(names::DB_POOL_WAIT_TOTAL, &[]).increment(1);
        self.histogram(names::DB_POOL_WAIT_SECONDS, &[])
            .record(waited.as_secs_f64());
    }

    /// Records one upstream response. `repo` is the repository name (allowed on this series).
    pub fn record_upstream(&self, repo: &str, status: u16, elapsed: Duration) {
        let status = status.to_string();
        self.counter(
            names::UPSTREAM_REQUESTS_TOTAL,
            &[("repo", repo), ("status", &status)],
        )
        .increment(1);
        self.histogram(names::UPSTREAM_REQUEST_DURATION_SECONDS, &[("repo", repo)])
            .record(elapsed.as_secs_f64());
    }

    /// Records an upstream failure. `kind` is one of `timeout`, `connect`, `5xx`, `circuit_open`.
    pub fn record_upstream_error(&self, repo: &str, kind: &'static str) {
        self.counter(
            names::UPSTREAM_ERRORS_TOTAL,
            &[("repo", repo), ("kind", kind)],
        )
        .increment(1);
    }

    /// Sets the dropped-log-lines counter to an absolute value reported by the log writer.
    pub fn set_dropped_log_lines(&self, total: u64) {
        let counter = self.counter(names::LOG_LINES_DROPPED_TOTAL, &[]);
        counter.absolute(total);
    }
}

/// Maps a request method to a bounded label value.
#[must_use]
pub fn normalize_method(method: &str) -> &'static str {
    KNOWN_METHODS
        .iter()
        .find(|known| **known == method)
        .copied()
        .unwrap_or("OTHER")
}

/// Forwards the `metrics` facade to a handle's recorder.
struct GlobalRecorder(Arc<Shared>);

impl Recorder for GlobalRecorder {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.0.recorder.describe_counter(key, unit, description);
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.0.recorder.describe_gauge(key, unit, description);
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.0.recorder.describe_histogram(key, unit, description);
    }

    fn register_counter(&self, key: &Key, metadata: &Metadata<'_>) -> Counter {
        self.0.recorder.register_counter(key, metadata)
    }

    fn register_gauge(&self, key: &Key, metadata: &Metadata<'_>) -> Gauge {
        self.0.recorder.register_gauge(key, metadata)
    }

    fn register_histogram(&self, key: &Key, metadata: &Metadata<'_>) -> Histogram {
        self.0.recorder.register_histogram(key, metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> MetricsHandle {
        MetricsHandle::new().expect("recorder")
    }

    #[test]
    fn http_request_series_have_route_method_and_status() {
        let m = handle();
        m.record_http_request("/healthz", "GET", 200, Duration::from_millis(3));
        let text = m.render();
        assert!(
            text.contains(r#"http_requests_total{route="/healthz",method="GET",status="200"} 1"#),
            "{text}"
        );
        assert!(text.contains("http_request_duration_seconds_bucket{route=\"/healthz\",method=\"GET\",le=\"0.005\"}"), "{text}");
        assert!(
            text.contains("# HELP http_requests_total"),
            "descriptions are registered: {text}"
        );
    }

    #[test]
    fn duration_buckets_span_five_ms_to_sixty_seconds() {
        let m = handle();
        m.record_http_request("/x", "GET", 200, Duration::from_secs(1));
        let text = m.render();
        assert!(text.contains("le=\"0.005\""));
        assert!(text.contains("le=\"60\""));
        assert!(text.contains("le=\"+Inf\""));
    }

    #[test]
    fn must_not_create_unbounded_method_label_values() {
        let m = handle();
        for method in ["FOO1", "FOO2", "BREW", "get", "PROPFIND"] {
            m.record_http_request("/x", method, 200, Duration::ZERO);
        }
        let text = m.render();
        assert_eq!(text.matches("http_requests_total{").count(), 1, "{text}");
        assert!(text.contains(r#"method="OTHER""#));
        assert_eq!(normalize_method("DELETE"), "DELETE");
        assert_eq!(normalize_method("delete"), "OTHER");
    }

    #[test]
    fn in_flight_gauge_follows_the_guard() {
        let m = handle();
        let first = m.track_in_flight();
        let second = m.track_in_flight();
        assert!(m.render().contains("http_requests_in_flight 2"));
        drop(first);
        assert!(m.render().contains("http_requests_in_flight 1"));
        drop(second);
        assert!(m.render().contains("http_requests_in_flight 0"));
    }

    #[test]
    fn pool_and_upstream_helpers_publish_series() {
        let m = handle();
        m.set_db_pool(PoolStats { size: 20, idle: 5 });
        m.record_db_pool_wait(Duration::from_millis(40));
        m.record_upstream("maven-central", 200, Duration::from_millis(80));
        m.record_upstream_error("maven-central", "timeout");
        m.record_shed("overload");
        m.record_auth_attempt("token", "ok");
        m.set_dropped_log_lines(7);
        let text = m.render();
        for expected in [
            "db_pool_size 20",
            "db_pool_idle 5",
            "db_pool_in_use 15",
            "db_pool_wait_total 1",
            r#"upstream_requests_total{repo="maven-central",status="200"} 1"#,
            r#"upstream_errors_total{repo="maven-central",kind="timeout"} 1"#,
            r#"http_shed_total{reason="overload"} 1"#,
            r#"auth_attempts_total{method="token",result="ok"} 1"#,
            "log_lines_dropped_total 7",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
    }

    #[test]
    fn handles_are_independent_recorders() {
        let a = handle();
        let b = handle();
        a.record_shed("overload");
        assert!(a.render().contains("http_shed_total"));
        assert!(!b.render().contains("http_shed_total"));
    }

    #[tokio::test]
    async fn upkeep_loop_stops_on_cancel() {
        let m = handle();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(
            m.clone()
                .run_upkeep(Duration::from_millis(5), cancel.clone()),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("stops in time")
            .expect("no panic");
    }

    #[test]
    fn global_install_forwards_the_metrics_macros() {
        // The only test in this crate that installs the process-wide recorder.
        let m = handle();
        m.install_global().expect("first install succeeds");
        metrics::counter!("global_probe_total", "k" => "v").increment(3);
        assert!(m.render().contains(r#"global_probe_total{k="v"} 3"#));
        assert!(
            handle().install_global().is_err(),
            "second install is refused"
        );
    }
}
