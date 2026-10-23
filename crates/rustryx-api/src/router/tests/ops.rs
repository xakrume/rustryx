//! `/healthz`, `/readyz`, `/metrics`, fallbacks and the module extension point.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use http::{Method, StatusCode};
use http_body::Body as _;
use tower::ServiceExt;

use crate::test_support::{
    StubProbe, call, get_request, get_uri, ready_state, request, state_with, state_with_settings,
    test_router,
};
use crate::{
    Advisory, MetricsAccess, Outcome, ProbeReport, RouteModule, RouterBuilder, SchemaStatus,
    build_router,
};

fn ready_report() -> ProbeReport {
    ProbeReport::healthy()
}

#[tokio::test]
async fn healthz_answers_the_constant_body() {
    let router = test_router(ready_state());
    let reply = get_uri(&router, "/healthz").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), r#"{"status":"ok"}"#);
    assert_eq!(reply.header("content-type"), Some("application/json"));
}

#[tokio::test]
async fn healthz_response_keeps_its_exact_size_hint() {
    let router = test_router(ready_state());
    let response = router
        .clone()
        .oneshot(get_request("/healthz"))
        .await
        .expect("response");
    assert_eq!(
        response.body().size_hint().exact(),
        Some(15),
        "hyper can send Content-Length"
    );
}

#[tokio::test]
async fn healthz_supports_head() {
    let router = test_router(ready_state());
    let reply = call(
        &router,
        request(Method::HEAD, "/healthz")
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.is_empty());
}

#[tokio::test]
async fn must_not_touch_dependencies_in_healthz() {
    let probe = StubProbe::new(ProbeReport {
        database: Outcome::Failed,
        schema: SchemaStatus::Behind,
        advisories: vec![Advisory {
            name: "redis",
            healthy: false,
        }],
    });
    let state = state_with(probe.clone());
    state.lifecycle().start_draining();
    let router = test_router(state);
    let reply = get_uri(&router, "/healthz").await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "liveness never depends on readiness inputs"
    );
    assert_eq!(probe.calls(), 0, "healthz must not run the readiness probe");
}

#[tokio::test]
async fn readyz_is_200_when_everything_that_matters_is_fine() {
    let router = test_router(ready_state());
    let reply = get_uri(&router, "/readyz").await;
    assert_eq!(reply.status, StatusCode::OK);
    let json = reply.json();
    assert_eq!(json["status"], "ready");
    let names: Vec<&str> = json["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["listener", "draining", "schema", "database"]);
    assert_eq!(reply.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn readyz_is_503_until_the_listener_is_up() {
    let state = crate::AppState::new(StubProbe::new(ready_report()));
    let router = test_router(state.clone());
    let reply = get_uri(&router, "/readyz").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.json()["checks"][0]["status"], "fail");
    state.lifecycle().mark_listening();
    assert_eq!(get_uri(&router, "/readyz").await.status, StatusCode::OK);
}

#[tokio::test]
async fn readyz_flips_to_503_as_soon_as_draining_starts_while_healthz_stays_up() {
    let state = ready_state();
    let router = test_router(state.clone());
    assert_eq!(get_uri(&router, "/readyz").await.status, StatusCode::OK);
    state.lifecycle().start_draining();
    let reply = get_uri(&router, "/readyz").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.json()["checks"][1]["name"], "draining");
    assert_eq!(reply.json()["checks"][1]["status"], "fail");
    assert_eq!(get_uri(&router, "/healthz").await.status, StatusCode::OK);
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::OK,
        "in-flight work is still served"
    );
}

#[tokio::test]
async fn readyz_is_503_when_the_database_does_not_answer() {
    let probe = StubProbe::new(ProbeReport {
        database: Outcome::Failed,
        ..ready_report()
    });
    let router = test_router(state_with(probe));
    let reply = get_uri(&router, "/readyz").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.json()["checks"][3]["status"], "fail");
}

#[tokio::test]
async fn readyz_is_503_when_the_schema_is_older_than_the_binary() {
    for schema in [SchemaStatus::Behind, SchemaStatus::Unknown] {
        let probe = StubProbe::new(ProbeReport {
            schema,
            ..ready_report()
        });
        let reply = get_uri(&test_router(state_with(probe)), "/readyz").await;
        assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{schema:?}");
    }
}

#[tokio::test]
async fn a_newer_schema_is_served_in_compatibility_mode() {
    let probe = StubProbe::new(ProbeReport {
        schema: SchemaStatus::Ahead,
        ..ready_report()
    });
    let reply = get_uri(&test_router(state_with(probe)), "/readyz").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.json()["checks"][2]["note"]
            .as_str()
            .expect("note")
            .contains("compatibility")
    );
}

#[tokio::test]
async fn must_not_depend_on_redis_keycloak_ldap_s3_or_upstreams() {
    let advisories = ["redis", "keycloak", "ldap", "s3", "upstream"]
        .into_iter()
        .map(|name| Advisory {
            name,
            healthy: false,
        })
        .collect();
    let probe = StubProbe::new(ProbeReport {
        advisories,
        ..ready_report()
    });
    let reply = get_uri(&test_router(state_with(probe)), "/readyz").await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "optional dependencies never flip readiness"
    );
    let json = reply.json();
    assert_eq!(json["status"], "ready");
    let reported = json["advisories"].as_array().expect("advisories");
    assert_eq!(reported.len(), 5);
    assert!(reported.iter().all(|a| a["status"] == "degraded"));
}

#[tokio::test]
async fn must_not_leak_connection_details_in_readyz() {
    let probe = StubProbe::new(ProbeReport {
        database: Outcome::Failed,
        ..ready_report()
    });
    let body = get_uri(&test_router(state_with(probe)), "/readyz")
        .await
        .text();
    for word in [
        "postgres://",
        "password",
        "host=",
        "5432",
        "sqlx",
        "Connection refused",
    ] {
        assert!(!body.contains(word), "{word} in {body}");
    }
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    for check in json["checks"].as_array().expect("checks") {
        let keys: Vec<&String> = check.as_object().expect("object").keys().collect();
        assert!(
            keys.iter()
                .all(|k| matches!(k.as_str(), "name" | "status" | "note")),
            "{keys:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn readyz_gives_up_on_a_stuck_database_after_one_second() {
    let probe = StubProbe::new(ready_report());
    probe.set_delay(Duration::from_secs(30));
    let router = test_router(state_with(probe));
    let started = tokio::time::Instant::now();
    let reply = get_uri(&router, "/readyz").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        started.elapsed() <= Duration::from_millis(1100),
        "answered after {:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn readyz_runs_the_probe_at_most_once_per_cache_window() {
    let probe = StubProbe::new(ready_report());
    let router = test_router(state_with(probe.clone()));
    for _ in 0..10 {
        assert_eq!(get_uri(&router, "/readyz").await.status, StatusCode::OK);
    }
    assert_eq!(probe.calls(), 1);
    probe.set(ProbeReport {
        database: Outcome::Failed,
        ..ready_report()
    });
    assert_eq!(
        get_uri(&router, "/readyz").await.status,
        StatusCode::OK,
        "still cached"
    );
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(
        get_uri(&router, "/readyz").await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(probe.calls(), 2);
}

#[tokio::test]
async fn unwired_state_reports_not_ready() {
    let state = crate::AppState::unwired();
    state.lifecycle().mark_listening();
    let reply = get_uri(&build_router(state), "/readyz").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
}

// ---- metrics --------------------------------------------------------------

#[tokio::test]
async fn metrics_are_served_in_prometheus_text_format_when_public() {
    let router = test_router(ready_state());
    let _ = get_uri(&router, "/healthz").await;
    let reply = get_uri(&router, "/metrics").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply
            .header("content-type")
            .expect("type")
            .starts_with("text/plain; version=0.0.4")
    );
    assert!(
        reply
            .text()
            .contains(r#"http_requests_total{route="/healthz",method="GET",status="200"} 1"#),
        "{}",
        reply.text()
    );
}

#[tokio::test]
async fn metrics_with_a_token_need_the_bearer_header() {
    let state = state_with_settings(|s| {
        s.metrics_access =
            MetricsAccess::Bearer(rustryx_core::Secret::new("scrape-token-0123456789"));
    });
    let router = test_router(state);
    let missing = get_uri(&router, "/metrics").await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        missing.header("www-authenticate"),
        Some("Bearer realm=\"metrics\"")
    );
    for wrong in [
        "Bearer nope",
        "Bearer scrape-token-012345678",
        "Basic c2NyYXBl",
        "scrape-token-0123456789",
    ] {
        let req = request(Method::GET, "/metrics")
            .header("authorization", wrong)
            .body(Body::empty())
            .expect("req");
        assert_eq!(
            call(&router, req).await.status,
            StatusCode::UNAUTHORIZED,
            "{wrong}"
        );
    }
    let ok = request(Method::GET, "/metrics")
        .header("authorization", "Bearer scrape-token-0123456789")
        .body(Body::empty())
        .expect("req");
    assert_eq!(call(&router, ok).await.status, StatusCode::OK);
    let lower = request(Method::GET, "/metrics")
        .header("authorization", "bearer scrape-token-0123456789")
        .body(Body::empty())
        .expect("req");
    assert_eq!(call(&router, lower).await.status, StatusCode::OK);
}

#[tokio::test]
async fn disabled_metrics_answer_404_like_any_unknown_path() {
    let state = state_with_settings(|s| s.metrics_access = MetricsAccess::Disabled);
    let reply = get_uri(&test_router(state), "/metrics").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.error_code(), "route_not_found");
}

#[tokio::test]
async fn metrics_without_a_handle_answer_404() {
    let state = crate::AppState::new(StubProbe::new(ready_report()));
    let reply = get_uri(&test_router(state), "/metrics").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn must_label_metrics_with_route_templates_never_paths() {
    let router = test_router(ready_state());
    for id in 1..=5 {
        let _ = get_uri(&router, &format!("/api/items/{id}")).await;
    }
    for path in ["/nope/1", "/nope/2", "/another/unknown/path"] {
        let _ = get_uri(&router, path).await;
    }
    let _ = get_uri(&router, "/repository/maven-central/org/example/lib.jar").await;
    let _ = get_uri(&router, "/repository/npm-proxy/left-pad").await;
    let text = get_uri(&router, "/metrics").await.text();
    assert!(
        text.contains(r#"route="/api/items/{id}",method="GET",status="200"} 5"#),
        "{text}"
    );
    assert!(
        text.contains(r#"route="unmatched",method="GET",status="404"} 3"#),
        "{text}"
    );
    assert!(
        text.contains(r#"route="/repository/{name}/{*path}""#),
        "{text}"
    );
    for leaked in ["maven-central", "left-pad", "/nope", "lib.jar"] {
        assert!(
            !text.contains(leaked),
            "{leaked} became a label value:\n{text}"
        );
    }
}

#[tokio::test]
async fn must_not_create_a_series_per_request_method() {
    let router = test_router(ready_state());
    for method in ["BREW", "PROPFIND", "FOO1", "FOO2"] {
        let req = request(
            Method::from_bytes(method.as_bytes()).expect("method"),
            "/api/whoami",
        )
        .body(Body::empty())
        .expect("req");
        let _ = call(&router, req).await;
    }
    let text = get_uri(&router, "/metrics").await.text();
    assert!(text.contains(r#"method="OTHER""#), "{text}");
    for method in ["BREW", "PROPFIND", "FOO1"] {
        assert!(!text.contains(method), "{method} leaked into labels");
    }
}

#[tokio::test]
async fn request_duration_histogram_is_recorded_per_route() {
    let router = test_router(ready_state());
    let _ = get_uri(&router, "/api/only-get").await;
    let text = get_uri(&router, "/metrics").await.text();
    assert!(
        text.contains(
            r#"http_request_duration_seconds_count{route="/api/only-get",method="GET"} 1"#
        ),
        "{text}"
    );
}

// ---- fallbacks and modules -------------------------------------------------

#[tokio::test]
async fn unknown_paths_get_the_envelope_with_request_id_and_security_headers() {
    let router = test_router(ready_state());
    let reply = get_uri(&router, "/definitely/not/here").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.error_code(), "route_not_found");
    let id = reply.header("x-request-id").expect("request id").to_owned();
    assert_eq!(reply.json()["error"]["request_id"], id.as_str());
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
}

#[tokio::test]
async fn wrong_method_is_a_405_envelope_with_an_allow_header() {
    let router = test_router(ready_state());
    let req = request(Method::POST, "/api/only-get")
        .body(Body::empty())
        .expect("req");
    let reply = call(&router, req).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(reply.error_code(), "method_not_allowed");
    assert!(reply.header("allow").expect("Allow header").contains("GET"));
    let ops = call(
        &router,
        request(Method::POST, "/healthz")
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(ops.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(ops.error_code(), "method_not_allowed");
}

struct SecondModule;

impl RouteModule for SecondModule {
    fn name(&self) -> &'static str {
        "second"
    }

    fn mount_api(&self, router: axum::Router<crate::AppState>) -> axum::Router<crate::AppState> {
        router.route("/api/second", axum::routing::get(|| async { "second" }))
    }
}

#[tokio::test]
async fn several_modules_mount_side_by_side_and_inherit_the_stack() {
    let router = RouterBuilder::new(ready_state())
        .module(crate::test_support::TestModule::new())
        .module(Arc::new(SecondModule))
        .build();
    let second = get_uri(&router, "/api/second").await;
    assert_eq!(second.text(), "second");
    assert!(
        second
            .header("x-request-id")
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(
        second.header("cache-control"),
        Some("no-store"),
        "api group default"
    );
    let artifact = get_uri(&router, "/repository/maven/org/x").await;
    assert_eq!(artifact.text(), "maven:org/x");
    assert!(
        artifact
            .header("content-security-policy")
            .expect("csp")
            .contains("sandbox")
    );
}

#[tokio::test]
async fn router_without_modules_serves_probes_and_the_envelope() {
    let router = build_router(ready_state());
    assert_eq!(get_uri(&router, "/healthz").await.status, StatusCode::OK);
    assert_eq!(
        get_uri(&router, "/api/anything").await.error_code(),
        "route_not_found"
    );
}

#[tokio::test]
async fn in_flight_gauge_counts_only_running_requests() {
    let module = crate::test_support::TestModule::new();
    let gate = module.gate.clone();
    let router = RouterBuilder::new(ready_state()).module(module).build();
    let blocked = tokio::spawn(router.clone().oneshot(get_request("/api/gate")));
    gate.wait_until_entered().await;
    let during = get_uri(&router, "/metrics").await.text();
    assert!(
        during.contains("http_requests_in_flight 2"),
        "gate request plus the scrape: {during}"
    );
    gate.open();
    let finished = blocked.await.expect("join").expect("response");
    let _ = axum::body::to_bytes(finished.into_body(), usize::MAX)
        .await
        .expect("body");
    let after = get_uri(&router, "/metrics").await.text();
    assert!(
        after.contains("http_requests_in_flight 1"),
        "only the scrape itself: {after}"
    );
}
