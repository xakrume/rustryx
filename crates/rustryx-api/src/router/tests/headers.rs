//! Security headers, CORS, compression, request ids.

use axum::body::Body;
use http::{Method, StatusCode};

use crate::test_support::{
    StubAuth, call, get_uri, principal, ready_state, request, state_with_settings, test_router,
};
use rustryx_core::AuthError;
use rustryx_core::identity::AuthMethod;

const EXPECTED: [(&str, &str); 5] = [
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    (
        "strict-transport-security",
        "max-age=31536000; includeSubDomains",
    ),
    ("x-frame-options", "DENY"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'; sandbox",
    ),
];

#[track_caller]
fn assert_secure(reply: &crate::test_support::Reply, what: &str) {
    for (name, value) in EXPECTED {
        assert_eq!(reply.header(name), Some(value), "{what}: {name}");
    }
}

#[tokio::test]
async fn security_headers_are_on_every_kind_of_response() {
    let router = test_router(
        ready_state().with_authenticator(StubAuth::failing(|| AuthError::InvalidCredentials)),
    );
    assert_secure(&get_uri(&router, "/healthz").await, "probe");
    assert_secure(&get_uri(&router, "/metrics").await, "metrics");
    assert_secure(&get_uri(&router, "/api/only-get").await, "api 200");
    assert_secure(&get_uri(&router, "/repository/a/b").await, "artifact 200");
    assert_secure(&get_uri(&router, "/nothing").await, "404");
    assert_secure(&get_uri(&router, "/api/panic").await, "500");
    let req = request(Method::GET, "/api/whoami")
        .header("authorization", "Bearer x")
        .body(Body::empty())
        .expect("req");
    assert_secure(&call(&router, req).await, "401");
}

#[tokio::test]
async fn a_handler_may_override_the_default_content_security_policy() {
    let reply = get_uri(&test_router(ready_state()), "/api/csp").await;
    assert_eq!(
        reply.header("content-security-policy"),
        Some("default-src 'self'")
    );
    assert_eq!(
        reply.header("x-frame-options"),
        Some("DENY"),
        "other headers still apply"
    );
}

#[tokio::test]
async fn api_responses_are_not_cacheable_but_artifacts_are_left_alone() {
    let router = test_router(ready_state());
    assert_eq!(
        get_uri(&router, "/api/only-get")
            .await
            .header("cache-control"),
        Some("no-store")
    );
    let artifact = get_uri(&router, "/repository/a/b").await;
    assert_eq!(
        artifact.header("cache-control"),
        None,
        "must not force no-store on cacheable artifacts"
    );
}

// ---- request id -----------------------------------------------------------

#[tokio::test]
async fn request_ids_are_generated_and_distinct() {
    let router = test_router(ready_state());
    let a = get_uri(&router, "/healthz")
        .await
        .header("x-request-id")
        .expect("id")
        .to_owned();
    let b = get_uri(&router, "/healthz")
        .await
        .header("x-request-id")
        .expect("id")
        .to_owned();
    assert_ne!(a, b);
    assert!(a.len() >= 32);
}

#[tokio::test]
async fn a_well_formed_incoming_request_id_is_propagated() {
    let router = test_router(ready_state());
    let req = request(Method::GET, "/api/only-get")
        .header("x-request-id", "client-id.42")
        .body(Body::empty())
        .expect("req");
    assert_eq!(
        call(&router, req).await.header("x-request-id"),
        Some("client-id.42")
    );
}

#[tokio::test]
async fn must_not_propagate_a_malformed_request_id() {
    let router = test_router(ready_state());
    for bad in [
        "has space",
        "semi;colon",
        "{\"level\":\"error\"}",
        &"a".repeat(100),
    ] {
        let req = request(Method::GET, "/api/only-get")
            .header("x-request-id", bad)
            .body(Body::empty())
            .expect("req");
        let reply = call(&router, req).await;
        assert_ne!(reply.header("x-request-id"), Some(bad), "{bad}");
        assert!(reply.header("x-request-id").expect("replacement").len() >= 32);
    }
}

#[tokio::test]
async fn error_bodies_carry_the_same_request_id_as_the_header() {
    let router = test_router(ready_state());
    let req = request(Method::GET, "/api/error/sql")
        .header("x-request-id", "trace-me-1")
        .body(Body::empty())
        .expect("req");
    let reply = call(&router, req).await;
    assert_eq!(reply.header("x-request-id"), Some("trace-me-1"));
    assert_eq!(reply.json()["error"]["request_id"], "trace-me-1");
}

// ---- CORS -----------------------------------------------------------------

fn cors_router(origins: &[&str]) -> axum::Router {
    let origins: Vec<String> = origins.iter().map(|o| (*o).to_owned()).collect();
    test_router(state_with_settings(move |s| {
        s.cors_allowed_origins = origins;
    }))
}

fn with_origin(method: Method, uri: &str, origin: &str) -> http::Request<Body> {
    request(method, uri)
        .header("origin", origin)
        .body(Body::empty())
        .expect("req")
}

#[tokio::test]
async fn listed_origins_get_cors_headers_on_simple_and_preflight_requests() {
    let router = cors_router(&["https://ui.example", "https://other.example:8443"]);
    let simple = call(
        &router,
        with_origin(Method::GET, "/api/only-get", "https://ui.example"),
    )
    .await;
    assert_eq!(
        simple.header("access-control-allow-origin"),
        Some("https://ui.example")
    );
    assert!(
        simple
            .header("vary")
            .expect("vary")
            .to_ascii_lowercase()
            .contains("origin")
    );
    assert_eq!(
        simple.header("access-control-allow-credentials"),
        None,
        "credentials are not allowed"
    );

    let preflight = call(
        &router,
        request(Method::OPTIONS, "/api/only-get")
            .header("origin", "https://other.example:8443")
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization,content-type",
            )
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert!(preflight.status.is_success(), "{}", preflight.status);
    assert_eq!(
        preflight.header("access-control-allow-origin"),
        Some("https://other.example:8443")
    );
    assert!(
        preflight
            .header("access-control-allow-methods")
            .expect("methods")
            .contains("POST")
    );
    assert!(
        preflight
            .header("access-control-allow-headers")
            .expect("headers")
            .to_ascii_lowercase()
            .contains("authorization")
    );
}

#[tokio::test]
async fn must_not_reflect_an_unlisted_origin() {
    let router = cors_router(&["https://ui.example"]);
    for origin in [
        "https://evil.example",
        "https://ui.example.evil.example",
        "http://ui.example",
        "null",
    ] {
        let reply = call(&router, with_origin(Method::GET, "/api/only-get", origin)).await;
        assert_eq!(
            reply.header("access-control-allow-origin"),
            None,
            "{origin}"
        );
    }
}

#[tokio::test]
async fn must_not_send_cors_headers_when_no_origin_is_configured() {
    let router = cors_router(&[]);
    let reply = call(
        &router,
        with_origin(Method::GET, "/api/only-get", "https://ui.example"),
    )
    .await;
    assert_eq!(reply.header("access-control-allow-origin"), None);
    let preflight = call(
        &router,
        request(Method::OPTIONS, "/api/only-get")
            .header("origin", "https://ui.example")
            .header("access-control-request-method", "GET")
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(preflight.header("access-control-allow-origin"), None);
}

#[tokio::test]
async fn a_browser_can_read_a_401_from_an_allowed_origin() {
    let state =
        state_with_settings(|s| s.cors_allowed_origins = vec!["https://ui.example".to_owned()])
            .with_authenticator(StubAuth::failing(|| AuthError::InvalidCredentials));
    let router = test_router(state);
    let req = request(Method::GET, "/api/whoami")
        .header("origin", "https://ui.example")
        .header("authorization", "Bearer bad")
        .body(Body::empty())
        .expect("req");
    let reply = call(&router, req).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        reply.header("access-control-allow-origin"),
        Some("https://ui.example")
    );
}

#[tokio::test]
async fn artifact_routes_have_no_cors() {
    let router = cors_router(&["https://ui.example"]);
    let reply = call(
        &router,
        with_origin(Method::GET, "/repository/a/b", "https://ui.example"),
    )
    .await;
    assert_eq!(reply.header("access-control-allow-origin"), None);
}

// ---- compression ----------------------------------------------------------

fn accepting_gzip(uri: &str) -> http::Request<Body> {
    request(Method::GET, uri)
        .header("accept-encoding", "gzip")
        .body(Body::empty())
        .expect("req")
}

#[tokio::test]
async fn api_responses_are_compressed_when_the_client_accepts_gzip() {
    let router = test_router(ready_state());
    let plain = get_uri(&router, "/api/large").await;
    let gzipped = call(&router, accepting_gzip("/api/large")).await;
    assert_eq!(gzipped.header("content-encoding"), Some("gzip"));
    assert!(
        gzipped.body.len() < plain.body.len() / 2,
        "{} vs {}",
        gzipped.body.len(),
        plain.body.len()
    );
    assert_eq!(plain.header("content-encoding"), None);
}

#[tokio::test]
async fn must_not_compress_artifacts() {
    let router = test_router(ready_state());
    let reply = call(&router, accepting_gzip("/repository-big/x")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-encoding"), None);
    assert_eq!(reply.body.len(), 8192, "bytes are served as stored");
}

// ---- panic recovery -------------------------------------------------------

#[tokio::test]
async fn a_panicking_handler_returns_500_and_the_server_keeps_serving() {
    let router = test_router(ready_state());
    let reply = get_uri(&router, "/api/panic").await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.error_code(), "internal_error");
    let id = reply.header("x-request-id").expect("request id").to_owned();
    assert_eq!(reply.json()["error"]["request_id"], id.as_str());

    for _ in 0..3 {
        assert_eq!(
            get_uri(&router, "/api/only-get").await.status,
            StatusCode::OK
        );
    }
    assert_eq!(get_uri(&router, "/healthz").await.status, StatusCode::OK);
    let metrics = get_uri(&router, "/metrics").await.text();
    assert!(
        metrics.contains(r#"route="/api/panic",method="GET",status="500"} 1"#),
        "{metrics}"
    );
}

#[tokio::test]
async fn must_not_leak_the_panic_message() {
    let reply = get_uri(&test_router(ready_state()), "/api/panic").await;
    assert!(!reply.text().contains("secret-in-panic-message"));
    assert!(!reply.text().contains("boom"));
}

#[tokio::test]
async fn authenticated_principal_name_is_not_needed_for_panic_recovery() {
    let auth = StubAuth::always(principal("alice", AuthMethod::BasicLocal, "r"));
    let router = test_router(ready_state().with_authenticator(auth));
    let req = request(Method::GET, "/api/panic")
        .header("authorization", "Bearer abc")
        .body(Body::empty())
        .expect("req");
    assert_eq!(
        call(&router, req).await.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}
