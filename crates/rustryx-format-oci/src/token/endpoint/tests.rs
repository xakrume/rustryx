use std::sync::Arc;

use rustryx_core::identity::{AuthMethod, UserSource};
use rustryx_core::ports::Clock;
use rustryx_format_core::stream::read_bounded;
use rustryx_testkit::{FixedClock, MemStore};
use serde_json::Value;
use time::OffsetDateTime;

use super::*;
use crate::token::{HmacTokenCodec, TokenServiceConfig};

fn base() -> Url {
    Url::parse("https://registry.example.com/").unwrap()
}

async fn json_of(response: Response<Body>) -> Value {
    let bytes = read_bounded(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn service() -> OciTokenService {
    let store = Arc::new(MemStore::with_builtin_roles().with_default_blob_store());
    let rbac = rustryx_rbac::Rbac::new(
        store.clone(),
        store.clone(),
        &rustryx_rbac::RbacConfig::default(),
    );
    let codec = Arc::new(HmacTokenCodec::new(&[7u8; 32], "rustryx", "rustryx").unwrap());
    OciTokenService::new(
        store,
        Arc::new(rbac),
        codec.clone(),
        codec,
        Arc::new(FixedClock::new(
            OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
        )) as Arc<dyn Clock>,
        TokenServiceConfig::default(),
    )
}

#[test]
fn the_challenge_points_at_the_token_endpoint() {
    assert_eq!(
        bearer_challenge(&base(), "rustryx", None, None),
        "Bearer realm=\"https://registry.example.com/v2/token\",service=\"rustryx\""
    );
    assert_eq!(
        bearer_challenge(
            &base(),
            "rustryx",
            Some("repository:r/a:pull,push"),
            Some("insufficient_scope")
        ),
        "Bearer realm=\"https://registry.example.com/v2/token\",service=\"rustryx\",scope=\"repository:r/a:pull,push\",error=\"insufficient_scope\""
    );
}

#[test]
fn a_quote_in_a_parameter_cannot_end_the_challenge_early() {
    let challenge = bearer_challenge(&base(), "a\"b", Some("x\\y"), None);
    assert!(challenge.contains(r#"service="a\"b""#), "{challenge}");
    assert!(challenge.contains(r#"scope="x\\y""#), "{challenge}");
}

#[test]
fn the_challenge_scope_follows_the_method() {
    assert_eq!(
        challenge_scope("r", "a/b", &Method::GET),
        "repository:r/a/b:pull"
    );
    assert_eq!(
        challenge_scope("r", "a", &Method::HEAD),
        "repository:r/a:pull"
    );
    assert_eq!(
        challenge_scope("r", "a", &Method::POST),
        "repository:r/a:pull,push"
    );
    assert_eq!(
        challenge_scope("r", "a", &Method::PUT),
        "repository:r/a:pull,push"
    );
    assert_eq!(
        challenge_scope("r", "a", &Method::PATCH),
        "repository:r/a:pull,push"
    );
    assert_eq!(
        challenge_scope("r", "a", &Method::DELETE),
        "repository:r/a:pull,delete"
    );
}

#[tokio::test]
async fn the_ping_always_challenges() {
    let response = ping_challenge(&base(), "rustryx");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response.headers()[header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(challenge.starts_with("Bearer realm=\"https://registry.example.com/v2/token\""));
    assert_eq!(
        response.headers()["docker-distribution-api-version"],
        "registry/2.0"
    );
    assert_eq!(json_of(response).await["errors"][0]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn the_ok_ping_is_an_empty_object() {
    let response = ping_ok();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["docker-distribution-api-version"],
        "registry/2.0"
    );
    assert_eq!(json_of(response).await, serde_json::json!({}));
}

#[tokio::test]
async fn a_token_response_has_the_docker_and_oauth_fields() {
    let principal = Principal::new("alice", UserSource::Local, AuthMethod::BasicLocal);
    let request = TokenRequest::from_urlencoded(b"service=rustryx&scope=registry:catalog:*");
    let response = token_response(&service(), &principal, &request, false).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = json_of(response).await;
    assert_eq!(body["token"], body["access_token"]);
    assert_eq!(body["expires_in"], 300);
    assert!(body["issued_at"].as_str().unwrap().starts_with("2027-"));
    assert!(body.get("scope").is_none());

    let oauth = token_response(&service(), &principal, &request, true).await;
    assert_eq!(json_of(oauth).await["scope"], "registry:catalog:*");
}

#[tokio::test]
async fn refusals_have_the_registry_error_shape() {
    let principal = Principal::anonymous();
    let bad_scope = TokenRequest::from_urlencoded(b"scope=repository:UPPER/a:pull");
    let response = token_response(&service(), &principal, &bad_scope, false).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(response).await["errors"][0]["code"], "UNSUPPORTED");

    let wrong_service = TokenRequest::from_urlencoded(b"service=other");
    let response = token_response(&service(), &principal, &wrong_service, false).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn the_request_reads_repeated_scopes_and_the_first_service() {
    let request = TokenRequest::from_urlencoded(
        b"service=a&scope=repository%3Ar%2Fx%3Apull&scope=repository:r/y:push&service=b&other=1",
    );
    assert_eq!(request.service.as_deref(), Some("a"));
    assert_eq!(
        request.scopes,
        ["repository:r/x:pull", "repository:r/y:push"]
    );
    assert_eq!(TokenRequest::from_urlencoded(b""), TokenRequest::default());
}
