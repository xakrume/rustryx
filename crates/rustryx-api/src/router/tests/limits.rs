//! Body limits, timeouts, concurrency limit and load shedding.

use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use http::{Method, StatusCode};
use tower::ServiceExt;

use crate::RouterBuilder;
use crate::test_support::{
    TestModule, call, get_request, get_uri, ready_state, request, state_with_settings, test_router,
};

fn post_with_length(uri: &str, len: usize, declare: bool) -> http::Request<Body> {
    let mut builder = request(Method::POST, uri);
    if declare {
        builder = builder.header("content-length", len.to_string());
    }
    builder.body(Body::from(vec![b'x'; len])).expect("req")
}

fn small_limits() -> axum::Router {
    test_router(state_with_settings(|s| {
        s.max_body_bytes = 100;
        s.max_blob_bytes = 1000;
    }))
}

#[tokio::test]
async fn api_body_over_the_limit_is_refused_from_the_declared_length() {
    let reply = call(&small_limits(), post_with_length("/api/body", 200, true)).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(reply.error_code(), "payload_too_large");
    assert!(reply.json()["error"]["request_id"].is_string());
}

#[tokio::test]
async fn api_body_over_the_limit_is_cut_off_when_no_length_is_declared() {
    let reply = call(&small_limits(), post_with_length("/api/body", 200, false)).await;
    assert_eq!(
        reply.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "the stream itself is limited"
    );
    assert_eq!(reply.error_code(), "payload_too_large");
}

#[tokio::test]
async fn api_body_within_the_limit_is_read() {
    let reply = call(&small_limits(), post_with_length("/api/body", 100, true)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), "100");
}

#[tokio::test]
async fn artifact_routes_use_the_blob_limit_not_the_api_limit() {
    let router = small_limits();
    let ok = call(
        &router,
        post_with_length("/repository/maven/x.jar", 900, true),
    )
    .await;
    assert_eq!((ok.status, ok.text().as_str()), (StatusCode::OK, "900"));
    let too_big = call(
        &router,
        post_with_length("/repository/maven/x.jar", 1500, true),
    )
    .await;
    assert_eq!(too_big.status, StatusCode::PAYLOAD_TOO_LARGE);
    let streamed = call(
        &router,
        post_with_length("/repository/maven/x.jar", 1500, false),
    )
    .await;
    assert_eq!(streamed.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn json_extractor_reports_oversized_bodies_as_413() {
    let body = format!(r#"{{"name":"{}"}}"#, "n".repeat(300));
    let req = request(Method::POST, "/api/echo")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("req");
    let reply = call(&small_limits(), req).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
}

fn stalling_body() -> Body {
    let first = futures::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))]);
    Body::from_stream(first.chain(futures::stream::pending()))
}

#[tokio::test(start_paused = true)]
async fn a_request_body_that_goes_quiet_is_cut_off() {
    let router = test_router(state_with_settings(|s| {
        s.body_idle_timeout = Duration::from_secs(1);
    }));
    let req = request(Method::POST, "/api/body")
        .body(stalling_body())
        .expect("req");
    let reply = call(&router, req).await;
    assert_eq!(reply.status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(reply.error_code(), "request_body_timeout");
    let artifact = call(
        &router,
        request(Method::POST, "/repository/m/x")
            .body(stalling_body())
            .expect("req"),
    )
    .await;
    assert_eq!(artifact.status, StatusCode::REQUEST_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn a_handler_that_never_answers_gets_a_504() {
    let router = test_router(state_with_settings(|s| {
        s.request_timeout = Duration::from_secs(2);
    }));
    let reply = get_uri(&router, "/api/slow").await;
    assert_eq!(reply.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(reply.error_code(), "request_timeout");
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::OK,
        "later requests are fine"
    );
}

// ---- load shedding ----------------------------------------------------------

fn one_slot_router() -> (axum::Router, crate::test_support::Gate) {
    let module = TestModule::new();
    let gate = module.gate.clone();
    let state = state_with_settings(|s| {
        s.max_in_flight = 1;
        s.shed_retry_after = Duration::from_secs(7);
    });
    (RouterBuilder::new(state).module(module).build(), gate)
}

#[tokio::test]
async fn requests_beyond_the_concurrency_limit_are_shed_with_retry_after() {
    let (router, gate) = one_slot_router();
    let first = tokio::spawn(router.clone().oneshot(get_request("/api/gate")));
    gate.wait_until_entered().await;

    let shed = get_uri(&router, "/api/only-get").await;
    assert_eq!(shed.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(shed.error_code(), "overloaded");
    assert_eq!(shed.header("retry-after"), Some("7"));
    assert!(shed.json()["error"]["request_id"].is_string());

    let artifact = get_uri(&router, "/repository/a/b").await;
    assert_eq!(
        artifact.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "api and artifacts share one limit"
    );

    let metrics = get_uri(&router, "/metrics").await.text();
    assert!(
        metrics.contains(r#"http_shed_total{reason="overload"} 2"#),
        "{metrics}"
    );

    gate.open();
    let released = first.await.expect("join").expect("response");
    assert_eq!(released.status(), StatusCode::OK);
    drop(released);
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::OK,
        "capacity returns"
    );
}

#[tokio::test]
async fn must_not_shed_health_probes_or_metrics() {
    let (router, gate) = one_slot_router();
    let first = tokio::spawn(router.clone().oneshot(get_request("/api/gate")));
    gate.wait_until_entered().await;
    assert_eq!(get_uri(&router, "/healthz").await.status, StatusCode::OK);
    assert_eq!(get_uri(&router, "/readyz").await.status, StatusCode::OK);
    assert_eq!(get_uri(&router, "/metrics").await.status, StatusCode::OK);
    gate.open();
    let _ = first.await.expect("join").expect("response");
}

#[tokio::test]
async fn the_limit_is_global_not_per_route() {
    let module = TestModule::new();
    let gate = module.gate.clone();
    let state = state_with_settings(|s| s.max_in_flight = 2);
    let router = RouterBuilder::new(state).module(module).build();
    let first = tokio::spawn(router.clone().oneshot(get_request("/api/gate")));
    gate.wait_until_entered().await;
    // One slot left: a request to a different route uses it.
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::OK
    );
    // With a per-route limit of two the gate route could take a second request; the
    // global limit lets it through only while a slot is free, which it is.
    gate.open();
    let _ = first.await.expect("join").expect("response");
}

#[tokio::test]
async fn default_settings_let_ordinary_traffic_through() {
    let router = test_router(ready_state());
    for _ in 0..50 {
        assert_eq!(
            get_uri(&router, "/api/only-get").await.status,
            StatusCode::OK
        );
    }
}

#[tokio::test(start_paused = true)]
async fn json_extractor_reports_a_stalled_upload_as_408() {
    let router = test_router(state_with_settings(|s| {
        s.body_idle_timeout = Duration::from_secs(1);
    }));
    let req = request(Method::POST, "/api/echo")
        .header("content-type", "application/json")
        .body(stalling_body())
        .expect("req");
    let reply = call(&router, req).await;
    assert_eq!(
        reply.status,
        StatusCode::REQUEST_TIMEOUT,
        "{}",
        reply.text()
    );
    assert_eq!(reply.error_code(), "request_body_timeout");
}
