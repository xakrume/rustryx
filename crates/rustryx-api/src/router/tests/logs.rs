//! What the stack writes to the log, and what it must never write.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::Method;
use rustryx_core::AuthError;
use rustryx_core::identity::AuthMethod;

use crate::test_support::{
    StubAuth, call, capture_logs, get_uri, principal, ready_state, request, test_router,
};

#[tokio::test]
async fn completion_line_carries_the_contract_fields() {
    let (logs, _guard) = capture_logs("info");
    let auth = StubAuth::always(principal("alice", AuthMethod::BasicLocal, "r"));
    let router = test_router(ready_state().with_authenticator(auth));
    let mut req = request(Method::GET, "/repository/maven-central/org/x/lib.jar")
        .header("authorization", "Bearer ryx_sometoken123")
        .header("x-request-id", "req-77")
        .body(Body::empty())
        .expect("req");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 44], 4000))));
    let _ = call(&router, req).await;

    let lines = logs.with_msg("request completed");
    assert_eq!(lines.len(), 1, "{:?}", logs.text());
    let line = &lines[0];
    assert_eq!(line["level"], "info");
    assert_eq!(line["request_id"], "req-77");
    assert_eq!(line["route"], "/repository/{name}/{*path}");
    assert_eq!(line["method"], "GET");
    assert_eq!(line["status"], 200);
    assert_eq!(line["repo"], "maven-central");
    assert_eq!(line["user"], "alice");
    assert_eq!(line["client_ip"], "192.0.2.44");
    assert!(line["duration_ms"].is_u64());
    assert!(line["ts"].is_string());
}

#[tokio::test]
async fn route_label_in_logs_is_the_template_not_the_path() {
    let (logs, _guard) = capture_logs("info");
    let router = test_router(ready_state());
    let _ = get_uri(&router, "/api/items/12345").await;
    let _ = get_uri(&router, "/no/such/path/with-secret-name").await;
    let text = logs.text();
    assert!(text.contains(r#""route":"/api/items/{id}""#), "{text}");
    assert!(text.contains(r#""route":"unmatched""#), "{text}");
    assert!(
        !text.contains("12345") && !text.contains("with-secret-name"),
        "{text}"
    );
}

#[tokio::test]
async fn probes_are_logged_at_debug_only() {
    let router = test_router(ready_state());
    {
        let (logs, _guard) = capture_logs("info");
        let _ = get_uri(&router, "/healthz").await;
        let _ = get_uri(&router, "/readyz").await;
        let _ = get_uri(&router, "/metrics").await;
        assert!(
            logs.lines().is_empty(),
            "probe traffic must not flood the info log: {}",
            logs.text()
        );
    }
    let (logs, _guard) = capture_logs("debug");
    let _ = get_uri(&router, "/healthz").await;
    assert_eq!(logs.with_msg("request completed").len(), 1);
}

#[tokio::test]
async fn an_error_is_logged_once_at_the_boundary_at_the_right_level() {
    let (logs, _guard) = capture_logs("info");
    let auth = StubAuth::failing(|| AuthError::InvalidCredentials);
    let router = test_router(ready_state().with_authenticator(auth));
    let req = request(Method::GET, "/api/whoami")
        .header("authorization", "Bearer sometoken")
        .body(Body::empty())
        .expect("req");
    let _ = call(&router, req).await;
    let rejected = logs.with_msg("request rejected");
    assert_eq!(rejected.len(), 1, "{}", logs.text());
    assert_eq!(
        rejected[0]["level"], "info",
        "an expected 401 is not an incident"
    );
    assert_eq!(rejected[0]["error.kind"], "invalid_credentials");
    assert_eq!(rejected[0]["status"], 401);
    assert!(rejected[0]["request_id"].is_string());
    assert_eq!(
        logs.with_msg("request failed"),
        Vec::<serde_json::Value>::new()
    );
}

#[tokio::test]
async fn server_faults_are_logged_as_errors_with_the_cause_for_operators() {
    let (logs, _guard) = capture_logs("info");
    let router = test_router(ready_state());
    let reply = get_uri(&router, "/api/error/sql").await;
    let failed = logs.with_msg("request failed");
    assert_eq!(failed.len(), 1, "{}", logs.text());
    assert_eq!(failed[0]["level"], "error");
    let cause = failed[0]["error"].as_str().expect("error chain");
    assert!(
        cause.contains("insert repository"),
        "the log has what the client must not: {cause}"
    );
    assert!(cause.contains("repositories_name_key"));
    assert!(!reply.text().contains("repositories_name_key"));
}

#[tokio::test]
async fn a_panic_is_logged_with_its_message_but_not_returned() {
    let (logs, _guard) = capture_logs("info");
    let router = test_router(ready_state());
    let reply = get_uri(&router, "/api/panic").await;
    let failed = logs.with_msg("request failed");
    assert_eq!(failed.len(), 1, "{}", logs.text());
    assert!(
        failed[0]["error"]
            .as_str()
            .expect("error")
            .contains("handler panicked: boom")
    );
    assert!(!reply.text().contains("boom"));
}

#[tokio::test]
async fn must_not_log_credentials_or_tokens_from_requests() {
    let (logs, _guard) = capture_logs("debug");
    let auth = StubAuth::failing(|| AuthError::InvalidCredentials);
    let router = test_router(ready_state().with_authenticator(auth));
    let basic = format!("Basic {}", STANDARD.encode("alice:PASSWORD-CANARY-7f3a"));
    for header in [
        basic.as_str(),
        "Bearer ryx_TOKENCANARY0123456789",
        "Bearer eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl",
    ] {
        let req = request(
            Method::GET,
            "/api/whoami?access_token=QUERYCANARY123&password=QUERYPASS",
        )
        .header("authorization", header)
        .header("cookie", "session=COOKIECANARY")
        .body(Body::empty())
        .expect("req");
        let _ = call(&router, req).await;
    }
    let metrics = get_uri(&router, "/metrics").await.text();
    let everything = format!("{}{}", logs.text(), metrics);
    for canary in [
        "PASSWORD-CANARY-7f3a",
        "alice:",
        "TOKENCANARY",
        "eyJhbGci",
        "QUERYCANARY",
        "QUERYPASS",
        "COOKIECANARY",
        &STANDARD.encode("alice:PASSWORD-CANARY-7f3a"),
    ] {
        assert!(
            !everything.contains(canary),
            "{canary} reached the logs or metrics"
        );
    }
}

#[tokio::test]
async fn must_not_let_a_forged_request_id_forge_log_lines() {
    let (logs, _guard) = capture_logs("info");
    let router = test_router(ready_state());
    let req = request(Method::GET, "/api/only-get")
        .header("x-request-id", "x\",\"level\":\"error\",\"msg\":\"forged")
        .body(Body::empty())
        .expect("req");
    let _ = call(&router, req).await;
    for line in logs.lines() {
        assert_eq!(line["level"], "info");
        assert_ne!(line["msg"], "forged");
    }
}

#[tokio::test]
async fn trace_context_is_adopted_from_the_incoming_request() {
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt;

    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_sampler(opentelemetry_sdk::trace::Sampler::AlwaysOn)
        .build();
    let capture = crate::test_support::LogCapture::default();
    let redactor =
        std::sync::Arc::new(rustryx_observability::Redactor::standard().expect("patterns"));
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("t")))
        .with(rustryx_observability::LogLayer::new(
            capture.clone(),
            redactor,
            rustryx_observability::LogFormat::Json,
        ));
    let _guard = tracing::subscriber::set_default(subscriber);
    let router = test_router(ready_state());
    let req = request(Method::GET, "/api/only-get")
        .header(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        )
        .body(Body::empty())
        .expect("req");
    let _ = call(&router, req).await;
    let lines = capture.with_msg("request completed");
    assert_eq!(lines[0]["trace_id"], "0af7651916cd43dd8448eb211c80319c");
    assert_eq!(lines[0]["span_id"].as_str().map(str::len), Some(16));
}
