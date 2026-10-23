//! Metrics: the series the dashboards and alerts read, with the labels they select.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::duration_suboptimal_units,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::similar_names,
    clippy::match_wild_err_arm,
    clippy::manual_assert,
    clippy::semicolon_if_nothing_returned,
    clippy::match_single_binding,
    clippy::large_futures,
    reason = "test scenarios read best as straight-line code with local helpers"
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum_core::body::Body;
use http::{HeaderValue, Method, Response, header};
use regex::Regex;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_format_core::metrics;
use rustryx_format_core::proxy::breaker::{BreakerConfig, BreakerState, CircuitBreaker};
use rustryx_format_core::proxy::executor::ExecutorConfig;
use rustryx_format_core::proxy::upstream::{TimeoutPhase, UpstreamError};
use rustryx_format_core::testing::Reply;
use rustryx_format_core::{CacheLabel, RequestClass, RequestLabels};
use rustryx_testkit::fixtures;
use support::recorder::{TestRecorder, series_id};
use support::{World, body_bytes, fast_executor, principal};
use tokio::time::Instant;

const CONTENT: &[u8] = b"bytes";
const ALERTS: &str = include_str!("../../../deploy/monitoring/rustryx-alerts.yaml");
const SLO: &str = include_str!("../../../docs/slo.md");

fn labels(response: &Response<Body>) -> (RequestClass, CacheLabel) {
    let labels = RequestLabels::of(response);
    (labels.class, labels.cache)
}

async fn hosted_world() -> (World, Arc<Repository>) {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    (world, repo)
}

async fn proxy_world() -> (World, Arc<Repository>) {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    (world, repo)
}

#[tokio::test]
async fn hosted_requests_are_labelled_by_class_and_cache() {
    use CacheLabel::{Hit, None as NoCache};
    use RequestClass::{Download, Metadata, Upload};
    let (world, repo) = hosted_world().await;
    let alice = principal("alice");

    let put = world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    assert_eq!(labels(&put), (Upload, NoCache));
    world.put(&repo, "pkg/list.index", &alice, b"one").await;

    let get = world.get(&repo, "pkg/a.bin", &alice).await;
    assert_eq!(
        labels(&get),
        (Download, Hit),
        "a hosted read is local: it is the case the TTFB objective measures"
    );
    let index = world.get(&repo, "pkg/list.index", &alice).await;
    assert_eq!(labels(&index), (Metadata, Hit));

    let etag = get.headers().get(header::ETAG).unwrap().clone();
    let mut conditional = world.head(Method::GET, "pkg/a.bin", &alice);
    conditional.headers.insert(header::IF_NONE_MATCH, etag);
    let not_modified = world.send_with(&repo, conditional, Body::empty()).await;
    assert_eq!(not_modified.status(), 304);
    assert_eq!(labels(&not_modified), (Download, Hit));

    let delete = world
        .send(&repo, Method::DELETE, "pkg/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(labels(&delete), (Upload, NoCache));
}

#[tokio::test]
async fn failures_keep_the_class_of_the_request_and_have_no_cache_relation() {
    use CacheLabel::None as NoCache;
    use RequestClass::{Download, Other, Upload};
    let (world, repo) = hosted_world().await;
    let alice = principal("alice");
    // Classified, then failed: the class is known.
    let missing = world.get(&repo, "pkg/missing.bin", &alice).await;
    assert_eq!(missing.status(), 404);
    assert_eq!(labels(&missing), (Download, NoCache));
    let anonymous = world
        .get(
            &repo,
            "pkg/a.bin",
            &rustryx_core::identity::Principal::anonymous(),
        )
        .await;
    assert_eq!(anonymous.status(), 401);
    assert_eq!(labels(&anonymous), (Download, NoCache));
    // Refused after classification: a write to a proxy is a 405 and still an upload.
    let (pworld, proxy) = proxy_world().await;
    let refused = pworld.put(&proxy, "a/b.bin", &alice, CONTENT).await;
    assert_eq!(refused.status(), 405);
    assert_eq!(labels(&refused), (Upload, NoCache));
    // Failed before classification.
    let bad = world.get(&repo, "../etc/passwd", &alice).await;
    assert_eq!(bad.status(), 400);
    assert_eq!(labels(&bad), (Other, NoCache));
    let mut offline = fixtures::repository("off", RepoFormat::Raw, RepoType::Hosted);
    offline.online = false;
    let offline = world.add(offline).await;
    let response = world.get(&offline, "a", &alice).await;
    assert_eq!(response.status(), 503);
    assert_eq!(labels(&response), (Other, NoCache));
    // No handler for the format at all.
    let maven = world
        .add(fixtures::repository(
            "m",
            RepoFormat::Maven2,
            RepoType::Hosted,
        ))
        .await;
    let response = world.get(&maven, "a", &alice).await;
    assert_eq!(response.status(), 500);
    assert_eq!(labels(&response), (Other, NoCache));
}

#[tokio::test]
async fn proxy_requests_say_how_the_cache_was_used() {
    use CacheLabel::{Hit, Miss, Stale};
    use RequestClass::{Download, Metadata};
    let (world, repo) = proxy_world().await;
    let alice = principal("alice");
    world.upstream.route("/r/a.bin", vec![Reply::ok(CONTENT)]);
    world
        .upstream
        .route("/r/list.index", vec![Reply::ok(&b"one"[..])]);

    let miss = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(labels(&miss), (Download, Miss));
    body_bytes(miss).await;
    world.settle().await;
    let hit = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(labels(&hit), (Download, Hit));

    let index_miss = world.get(&repo, "r/list.index", &alice).await;
    assert_eq!(labels(&index_miss), (Metadata, Miss));
    let index_hit = world.get(&repo, "r/list.index", &alice).await;
    assert_eq!(labels(&index_hit), (Metadata, Hit));

    // Expired, upstream down: the copy is served and says so.
    world.clock.advance(Duration::from_secs(2 * 3600));
    world.upstream.route("/r/a.bin", vec![Reply::status(503)]);
    let stale = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(labels(&stale), (Download, Stale));
    world
        .upstream
        .route("/r/list.index", vec![Reply::status(503)]);
    let stale_index = world.get(&repo, "r/list.index", &alice).await;
    assert_eq!(labels(&stale_index), (Metadata, Stale));
}

#[tokio::test]
async fn a_head_that_goes_upstream_is_a_miss() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/h/a.bin", vec![Reply::ok(CONTENT)]);
    let head = world.head(Method::HEAD, "h/a.bin", &principal("alice"));
    let response = world.send_with(&repo, head, Body::empty()).await;
    assert_eq!(
        labels(&response),
        (RequestClass::Download, CacheLabel::Miss)
    );
}

#[tokio::test]
async fn group_answers_are_labelled_by_what_served_them() {
    use CacheLabel::{Bypass, Hit, Miss};
    use RequestClass::{Download, Metadata};
    let world = World::new();
    let local = world.hosted("local").await;
    let mirror = world.proxy("mirror").await;
    let group = world.group("all", &[&local, &mirror]).await;
    world.allow_all("alice", &[&local, &mirror, &group]);
    world.put_as_admin(&local, "own/a.bin", CONTENT).await;
    world.put_as_admin(&local, "own/list.index", b"one").await;
    world.upstream.route("/up/b.bin", vec![Reply::ok(CONTENT)]);
    world
        .upstream
        .route("/own/list.index", vec![Reply::ok(&b"two"[..])]);
    let alice = principal("alice");

    let from_hosted = world.get(&group, "own/a.bin", &alice).await;
    assert_eq!(labels(&from_hosted), (Download, Hit));
    let from_proxy = world.get(&group, "up/b.bin", &alice).await;
    assert_eq!(labels(&from_proxy), (Download, Miss));
    let merged = world.get(&group, "own/list.index", &alice).await;
    assert_eq!(merged.status(), 200);
    assert_eq!(
        labels(&merged),
        (Metadata, Bypass),
        "a merged index is composed from several members, no cache serves it"
    );
}

#[test]
fn labels_have_a_neutral_default_and_survive_attach() {
    let foreign: Response<Body> = Response::new(Body::empty());
    assert_eq!(
        RequestLabels::of(&foreign),
        RequestLabels::new(RequestClass::Other, CacheLabel::None)
    );
    let mut mine: Response<Body> = Response::new(Body::empty());
    RequestLabels::new(RequestClass::Api, CacheLabel::Bypass).attach(&mut mine);
    assert_eq!(labels(&mine), (RequestClass::Api, CacheLabel::Bypass));
    assert_eq!(
        RequestLabels::new(RequestClass::Download, CacheLabel::Hit).pairs(),
        [("class", "download"), ("cache", "hit")]
    );
    // The extension never reaches the wire: it is not a header.
    assert!(mine.headers().is_empty());
    let _ = HeaderValue::from_static("x");
}

#[test]
fn every_label_value_has_a_stable_text() {
    let classes = [
        (RequestClass::Metadata, "metadata"),
        (RequestClass::Download, "download"),
        (RequestClass::Upload, "upload"),
        (RequestClass::Api, "api"),
        (RequestClass::Other, "other"),
    ];
    for (class, text) in classes {
        assert_eq!(class.as_str(), text);
    }
    let caches = [
        (CacheLabel::Hit, "hit"),
        (CacheLabel::Miss, "miss"),
        (CacheLabel::Stale, "stale"),
        (CacheLabel::Bypass, "bypass"),
        (CacheLabel::None, "none"),
    ];
    for (cache, text) in caches {
        assert_eq!(cache.as_str(), text);
    }
}

#[test]
fn the_alerts_select_only_label_values_this_crate_produces() {
    let classes: Vec<&str> = [
        RequestClass::Metadata,
        RequestClass::Download,
        RequestClass::Upload,
        RequestClass::Api,
        RequestClass::Other,
    ]
    .iter()
    .map(|c| c.as_str())
    .collect();
    let caches: Vec<&str> = [
        CacheLabel::Hit,
        CacheLabel::Miss,
        CacheLabel::Stale,
        CacheLabel::Bypass,
        CacheLabel::None,
    ]
    .iter()
    .map(|c| c.as_str())
    .collect();
    let class_selector = Regex::new(r#"\bclass="([a-z_]+)""#).unwrap();
    let cache_selector = Regex::new(r#"\bcache="([a-z_]+)""#).unwrap();
    let selected_classes: Vec<_> = class_selector
        .captures_iter(ALERTS)
        .map(|c| c[1].to_owned())
        .collect();
    let selected_caches: Vec<_> = cache_selector
        .captures_iter(ALERTS)
        .map(|c| c[1].to_owned())
        .collect();
    assert!(
        selected_classes.iter().any(|c| c == "metadata")
            && selected_classes.iter().any(|c| c == "download"),
        "the two latency rules must still select by class: {selected_classes:?}"
    );
    assert!(
        selected_caches.iter().any(|c| c == "hit"),
        "{selected_caches:?}"
    );
    for class in &selected_classes {
        assert!(
            classes.contains(&class.as_str()),
            "alert selects class {class:?}"
        );
    }
    for cache in &selected_caches {
        assert!(
            caches.contains(&cache.as_str()),
            "alert selects cache {cache:?}"
        );
    }
}

#[test]
fn the_names_the_docs_and_alerts_read_are_the_names_the_code_emits() {
    for (constant, name) in [
        (metrics::UPSTREAM_REQUESTS_TOTAL, "upstream_requests_total"),
        (
            metrics::UPSTREAM_REQUEST_DURATION_SECONDS,
            "upstream_request_duration_seconds",
        ),
        (metrics::UPSTREAM_ERRORS_TOTAL, "upstream_errors_total"),
        (metrics::UPSTREAM_CIRCUIT_STATE, "upstream_circuit_state"),
        (
            metrics::HTTP_REQUEST_DURATION_SECONDS,
            "http_request_duration_seconds",
        ),
    ] {
        assert_eq!(constant, name);
        assert!(
            SLO.contains(&format!("`{name}`")),
            "{name} is not in docs/slo.md"
        );
    }
    assert!(ALERTS.contains("upstream_errors_total"));
    assert!(ALERTS.contains("kind=\"circuit_open\""));
    assert!(ALERTS.contains("http_request_duration_seconds_bucket"));
}

#[tokio::test]
async fn upstream_series_carry_exactly_the_labels_of_the_catalogue() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let (world, repo) = proxy_world().await;
    world.upstream.route("/r/a.bin", vec![Reply::ok(CONTENT)]);
    body_bytes(world.get(&repo, "r/a.bin", &principal("alice")).await).await;
    world.settle().await;
    // docs/slo.md: `upstream_requests_total{repo,status}` and `..._duration_seconds{repo}`.
    assert_eq!(
        recorder.counter(
            "upstream_requests_total",
            &[("repo", "mirror"), ("status", "200")]
        ),
        1
    );
    assert_eq!(
        recorder.histogram_count("upstream_request_duration_seconds", &[("repo", "mirror")]),
        1
    );
    let series: Vec<String> = recorder.counters().into_keys().collect();
    assert!(
        series.contains(&series_id(
            "proxy_cache_total",
            &[("format", "raw"), ("result", "miss")]
        )),
        "{series:?}"
    );
    assert!(
        !series
            .iter()
            .any(|id| id.starts_with("proxy_cache_total") && id.contains("repo=")),
        "the repository name is allowed as a label on upstream series only"
    );
}

#[tokio::test]
async fn upstream_errors_use_the_documented_kinds() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    // A breaker that does not open, so that every kind below reaches the upstream.
    let world = World::with_executor(ExecutorConfig {
        breaker: BreakerConfig {
            min_requests: 1_000,
            ..BreakerConfig::default()
        },
        ..fast_executor()
    });
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    world.upstream.route(
        "/e/connect",
        vec![Reply::error(UpstreamError::Connect("refused".into()))],
    );
    world.upstream.route("/e/5xx", vec![Reply::status(500)]);
    world.upstream.route(
        "/e/timeout",
        vec![Reply::error(UpstreamError::Timeout(TimeoutPhase::Idle))],
    );
    // Connect failures and 5xx are retried (3 attempts); an idle timeout is not.
    world.get(&repo, "e/connect", &alice).await;
    let errors = |kind: &str| {
        recorder.counter(
            "upstream_errors_total",
            &[("repo", "mirror"), ("kind", kind)],
        )
    };
    assert_eq!(errors("connect"), 3);
    world.get(&repo, "e/timeout", &alice).await;
    assert_eq!(errors("timeout"), 1);
    world.get(&repo, "e/5xx", &alice).await;
    assert_eq!(errors("5xx"), 3);
    assert_eq!(
        recorder.counter(
            "upstream_requests_total",
            &[("repo", "mirror"), ("status", "error")]
        ),
        4,
        "a failed attempt is counted with status=error"
    );
}

#[tokio::test]
async fn an_open_circuit_is_counted_as_circuit_open_without_reaching_the_upstream() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let (world, repo) = proxy_world().await;
    let alice = principal("alice");
    world.upstream.route("/e/5xx", vec![Reply::status(500)]);
    // min_requests is 4 in the harness: two requests of three attempts open the breaker.
    for _ in 0..2 {
        world.get(&repo, "e/5xx", &alice).await;
    }
    let refused = || {
        recorder.counter(
            "upstream_errors_total",
            &[("repo", "mirror"), ("kind", "circuit_open")],
        )
    };
    // The breaker opened while the second request was retrying: that retry was already refused.
    let before = world.upstream.calls();
    let counted = refused();
    assert!(counted >= 1, "{counted}");
    world.get(&repo, "e/other", &alice).await;
    assert_eq!(
        world.upstream.calls(),
        before,
        "an open circuit sends nothing upstream"
    );
    assert_eq!(
        refused(),
        counted + 1,
        "RustryxUpstreamCircuitOpen selects exactly this kind"
    );
}

#[tokio::test]
async fn the_circuit_gauge_is_exported_closed_and_follows_the_breaker_for_every_repository() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let executor = ExecutorConfig {
        breaker: BreakerConfig {
            min_requests: 4,
            open_for: Duration::from_millis(150),
            ..BreakerConfig::default()
        },
        ..fast_executor()
    };
    let world = World::with_executor(executor);
    let first = world.proxy("mirror").await;
    // Same remote host as `mirror`: one breaker, two repositories that report it.
    let second = world.proxy("mirror2").await;
    world.allow_all("alice", &[&first, &second]);
    let alice = principal("alice");
    let gauge = |repo: &str| recorder.gauge("upstream_circuit_state", &[("repo", repo)]);
    assert_eq!(gauge("mirror"), None, "nothing was called yet");

    world.upstream.route("/ok", vec![Reply::ok(CONTENT)]);
    assert_eq!(world.get(&first, "ok", &alice).await.status(), 200);
    assert_eq!(
        gauge("mirror"),
        Some(0.0),
        "a healthy circuit must be visible: it is closed, not absent"
    );
    assert_eq!(world.get(&second, "ok", &alice).await.status(), 200);
    assert_eq!(
        gauge("mirror2"),
        Some(0.0),
        "every repository of the host reports it"
    );

    world.upstream.route("/bad", vec![Reply::status(500)]);
    for _ in 0..2 {
        world.get(&first, "bad", &alice).await;
    }
    assert_eq!(gauge("mirror"), Some(2.0));
    assert_eq!(
        gauge("mirror2"),
        Some(2.0),
        "the host is open for the repository that did not make the failing calls too"
    );

    // After the pause one probe is admitted; a healthy answer closes the circuit.
    tokio::time::sleep(Duration::from_millis(200)).await;
    world.upstream.route("/ok2", vec![Reply::ok(CONTENT)]);
    assert_eq!(world.get(&second, "ok2", &alice).await.status(), 200);
    assert_eq!(gauge("mirror"), Some(0.0));
    assert_eq!(gauge("mirror2"), Some(0.0));
}

#[tokio::test(start_paused = true)]
async fn a_breaker_exports_closed_at_birth_and_registers_repositories_once() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let config = BreakerConfig {
        min_requests: 2,
        ..BreakerConfig::default()
    };
    let breaker = CircuitBreaker::new(config, "a");
    let gauge = |repo: &str| recorder.gauge("upstream_circuit_state", &[("repo", repo)]);
    assert_eq!(gauge("a"), Some(0.0));
    breaker.register_repository("b");
    breaker.register_repository("b");
    breaker.register_repository("a");
    assert_eq!(gauge("b"), Some(0.0));

    let now = Instant::now();
    for _ in 0..2 {
        breaker.admit(now).unwrap().record(now, false);
    }
    assert_eq!(breaker.state(), BreakerState::Open);
    assert_eq!((gauge("a"), gauge("b")), (Some(2.0), Some(2.0)));
    // A repository that joins while the host is open starts with the open value.
    breaker.register_repository("c");
    assert_eq!(gauge("c"), Some(2.0));

    let probe_at = now + config.open_for;
    let probe = breaker.admit(probe_at).unwrap();
    assert_eq!(gauge("c"), Some(1.0), "half open while the probe is out");
    probe.record(probe_at, true);
    assert_eq!(
        (gauge("a"), gauge("b"), gauge("c")),
        (Some(0.0), Some(0.0), Some(0.0))
    );
}

#[tokio::test]
async fn cache_results_are_counted_by_format() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    // The 503s of the stale step must not open the breaker, or the last step never gets through.
    let world = World::with_executor(ExecutorConfig {
        breaker: BreakerConfig {
            min_requests: 1_000,
            ..BreakerConfig::default()
        },
        ..fast_executor()
    });
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    world
        .upstream
        .route("/c/a.index", vec![Reply::ok(&b"one"[..])]);
    world
        .upstream
        .route("/c/none.index", vec![Reply::status(404)]);
    let cache = |result: &str| {
        recorder.counter(
            "proxy_cache_total",
            &[("format", "raw"), ("result", result)],
        )
    };
    world.get(&repo, "c/a.index", &alice).await;
    assert_eq!(cache("miss"), 1);
    world.get(&repo, "c/a.index", &alice).await;
    assert_eq!(cache("hit"), 1);
    world.get(&repo, "c/none.index", &alice).await;
    world.get(&repo, "c/none.index", &alice).await;
    assert_eq!(
        cache("negative"),
        2,
        "a 404 is counted when the upstream says it and again when the remembered one answers"
    );
    assert_eq!(world.upstream.calls_for("/c/none.index"), 1);
    world.clock.advance(Duration::from_secs(700));
    world.upstream.route("/c/a.index", vec![Reply::status(503)]);
    world.get(&repo, "c/a.index", &alice).await;
    assert_eq!(cache("stale"), 1);
    world.upstream.route(
        "/c/a.index",
        vec![Reply::ok(&b"one"[..]), Reply::status(304)],
    );
    world.clock.advance(Duration::from_secs(700));
    world.get(&repo, "c/a.index", &alice).await;
    assert_eq!(
        cache("revalidated"),
        1,
        "an identical refetch confirms the copy"
    );
}

#[tokio::test]
async fn uploads_are_counted_by_format_and_result_with_their_bytes() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let (world, repo) = hosted_world().await;
    let alice = principal("alice");
    world.put(&repo, "a/b.bin", &alice, CONTENT).await;
    world
        .put(&repo, "a/b.bin", &principal("nobody"), CONTENT)
        .await;
    assert_eq!(
        recorder.counter(
            "hosted_uploads_total",
            &[("format", "raw"), ("result", "ok")]
        ),
        1
    );
    assert_eq!(
        recorder.counter("hosted_upload_bytes_total", &[("format", "raw")]),
        CONTENT.len() as u64
    );
    assert_eq!(
        recorder.counter_total("hosted_uploads_total"),
        1,
        "a refused caller never reaches the pipeline"
    );
}
