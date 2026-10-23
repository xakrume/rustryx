//! Error envelope through the whole stack, and the envelope extractors.

use axum::body::Body;
use http::{Method, StatusCode};

use crate::test_support::{call, get_uri, ready_state, request, test_router};

#[tokio::test]
async fn must_not_leak_backend_details_through_the_router() {
    let router = test_router(ready_state());
    let leaked = [
        "violates unique constraint",
        "repositories_name_key",
        "10.1.2.3",
        "/var/lib/rustryx",
        "/srv/app/src",
        "ldaps://ldap.corp.internal",
        "ou=groups",
        "redis at",
    ];
    for kind in [
        "sql",
        "blob-path",
        "internal",
        "unavailable",
        "ldap",
        "groups",
    ] {
        let reply = get_uri(&router, &format!("/api/error/{kind}")).await;
        let everything = format!("{}{:?}", reply.text(), reply.headers);
        for needle in leaked {
            assert!(
                !everything.contains(needle),
                "{kind}: {needle} leaked: {everything}"
            );
        }
        assert!(
            reply.json()["error"]["request_id"].is_string(),
            "{kind} carries a request id"
        );
    }
}

#[tokio::test]
async fn upstream_class_errors_use_the_documented_statuses() {
    let router = test_router(ready_state());
    let sql = get_uri(&router, "/api/error/sql").await;
    assert_eq!(
        (sql.status, sql.error_code().as_str()),
        (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    );
    let unavailable = get_uri(&router, "/api/error/unavailable").await;
    assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(unavailable.header("retry-after").is_some());
    let ldap = get_uri(&router, "/api/error/ldap").await;
    assert_eq!(
        (ldap.status, ldap.error_code().as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_provider_unavailable"
        )
    );
    let groups = get_uri(&router, "/api/error/groups").await;
    assert_eq!(
        (groups.status, groups.error_code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    let unknown = get_uri(&router, "/api/error/zzz").await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
}

fn json_post(body: &'static str, content_type: Option<&'static str>) -> http::Request<Body> {
    let mut builder = request(Method::POST, "/api/echo");
    if let Some(ct) = content_type {
        builder = builder.header("content-type", ct);
    }
    builder.body(Body::from(body)).expect("req")
}

#[tokio::test]
async fn json_extractor_accepts_valid_bodies() {
    let reply = call(
        &test_router(ready_state()),
        json_post(r#"{"name":"maven"}"#, Some("application/json")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["name"], "maven");
}

#[tokio::test]
async fn json_extractor_failures_are_envelopes_not_plain_text() {
    let router = test_router(ready_state());
    let syntax = call(&router, json_post("{not json", Some("application/json"))).await;
    assert_eq!(
        (syntax.status, syntax.error_code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_json")
    );
    let shape = call(
        &router,
        json_post(r#"{"other":1}"#, Some("application/json")),
    )
    .await;
    assert_eq!(shape.status, StatusCode::BAD_REQUEST);
    assert!(
        shape.json()["error"]["message"]
            .as_str()
            .expect("message")
            .contains("name")
    );
    let media = call(&router, json_post(r#"{"name":"x"}"#, Some("text/plain"))).await;
    assert_eq!(
        (media.status, media.error_code().as_str()),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
    );
    let missing = call(&router, json_post(r#"{"name":"x"}"#, None)).await;
    assert_eq!(missing.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    for reply in [syntax, shape, media, missing] {
        assert_eq!(reply.header("content-type"), Some("application/json"));
    }
}

#[tokio::test]
async fn query_and_path_extractor_failures_are_envelopes() {
    let router = test_router(ready_state());
    assert_eq!(get_uri(&router, "/api/search?limit=5").await.text(), "5");
    let missing = get_uri(&router, "/api/search").await;
    assert_eq!(
        (missing.status, missing.error_code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_query")
    );
    let bad = get_uri(&router, "/api/search?limit=lots").await;
    assert_eq!(bad.error_code(), "invalid_query");
    assert_eq!(get_uri(&router, "/api/items/7").await.text(), "7");
    let path = get_uri(&router, "/api/items/seven").await;
    assert_eq!(
        (path.status, path.error_code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_path")
    );
}
