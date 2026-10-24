//! Tests of the error rendering: the shape the Dart client reads, and the 401 and 403 rules.

use std::time::Duration;

use rustryx_core::error::{BlobError, PolicyViolation, StoreError};
use rustryx_format_core::UpstreamFailureKind;
use rustryx_format_core::stream::read_bounded;
use serde_json::Value;

use super::*;

async fn body_json(response: Response<Body>) -> Value {
    let bytes = read_bounded(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn every_error() -> Vec<FormatError> {
    vec![
        FormatError::bad_request("bad"),
        FormatError::Unauthenticated,
        FormatError::Forbidden,
        FormatError::NotFound,
        FormatError::MethodNotAllowed { allow: "GET, HEAD" },
        FormatError::conflict("exists"),
        FormatError::PreconditionFailed,
        FormatError::RequestTimeout,
        FormatError::PayloadTooLarge { limit: 1 },
        FormatError::RangeNotSatisfiable { total: 1 },
        FormatError::unprocessable("bad archive"),
        FormatError::TooManyRequests {
            retry_after: Duration::from_secs(3),
        },
        FormatError::QuotaExceeded,
        FormatError::Policy(PolicyViolation::ReadOnly),
        FormatError::DigestMismatch,
        FormatError::upstream(UpstreamFailureKind::Timeout, "https://10.1.2.3:8443/secret"),
        FormatError::upstream(
            UpstreamFailureKind::Rejected,
            "401 from https://corp.internal",
        ),
        FormatError::IndexTooLarge { limit: 1 },
        FormatError::Unavailable {
            reason: "busy",
            retry_after: Some(Duration::from_secs(2)),
        },
        FormatError::Store(StoreError::Unavailable("db at 10.0.0.5".into())),
        FormatError::Blob(BlobError::NotFound),
        FormatError::internal("panic at /srv/rustryx/src/x.rs:1"),
    ]
}

#[tokio::test]
async fn every_error_has_the_pub_body_and_media_type() {
    for failure in every_error() {
        for anonymous in [true, false] {
            let response = render(&failure, anonymous);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                MEDIA_TYPE,
                "{failure:?}"
            );
            let status = response.status();
            let body = body_json(response).await;
            let object = body.as_object().unwrap();
            // Exactly one key: the generic API envelope (with `request_id`) is not used here.
            assert_eq!(object.len(), 1, "{failure:?}");
            let error = object["error"].as_object().unwrap();
            let mut keys: Vec<&str> = error.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["code", "message"], "{failure:?}");
            assert!(error["code"].as_str().is_some_and(|c| !c.is_empty()));
            assert!(error["message"].as_str().is_some_and(|m| !m.is_empty()));
            assert!(status.is_client_error() || status.is_server_error());
        }
    }
}

#[tokio::test]
async fn must_not_put_internal_text_into_the_body_or_the_challenge() {
    for failure in every_error() {
        let response = render(&failure, true);
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .map(|v| v.to_str().unwrap().to_owned());
        let text = body_json(response).await.to_string() + &challenge.unwrap_or_default();
        for secret in [
            "10.1.2.3",
            "8443",
            "corp.internal",
            "10.0.0.5",
            "/srv/rustryx",
            "x.rs",
        ] {
            assert!(
                !text.contains(secret),
                "{failure:?} leaked {secret}: {text}"
            );
        }
    }
}

#[tokio::test]
async fn an_anonymous_caller_gets_401_with_the_pub_challenge() {
    let response = render(&FormatError::Unauthenticated, true);
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response.headers()[header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap();
    assert!(
        challenge.starts_with("Bearer realm=\"pub\", message=\""),
        "{challenge}"
    );
    assert!(challenge.ends_with('"'));
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "unauthenticated");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("dart pub token add")
    );
}

#[tokio::test]
async fn a_forbidden_caller_gets_403_with_the_same_challenge() {
    for anonymous in [true, false] {
        let response = render(&FormatError::Forbidden, anonymous);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let challenge = response.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap();
        assert!(challenge.starts_with("Bearer realm=\"pub\", message=\""));
        assert_eq!(body_json(response).await["error"]["code"], "forbidden");
    }
}

#[tokio::test]
async fn must_not_tell_a_caller_with_a_valid_token_to_authenticate_again() {
    // dart pub erases its stored token on 401. A caller that did present credentials is never
    // answered 401, whichever code path produced the error.
    let response = render(&FormatError::Unauthenticated, false);
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));
    assert_eq!(body_json(response).await["error"]["code"], "forbidden");
    for failure in every_error() {
        assert_ne!(
            render(&failure, false).status(),
            StatusCode::UNAUTHORIZED,
            "{failure:?}"
        );
    }
}

#[test]
fn only_401_and_403_carry_a_challenge() {
    for failure in every_error() {
        let response = render(&failure, true);
        let has = response.headers().contains_key(header::WWW_AUTHENTICATE);
        let auth = matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        );
        assert_eq!(has, auth, "{failure:?}");
    }
}

#[test]
fn protocol_headers_of_the_error_are_kept() {
    let response = render(
        &FormatError::Unavailable {
            reason: "busy",
            retry_after: Some(Duration::from_secs(2)),
        },
        true,
    );
    assert_eq!(response.headers()[header::RETRY_AFTER], "2");
    let response = render(&FormatError::MethodNotAllowed { allow: "GET, HEAD" }, true);
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()[header::ALLOW], "GET, HEAD");
}

#[test]
fn the_challenge_survives_any_message() {
    for message in [
        "plain text",
        "has \"quotes\" and \\ backslash",
        "line one\r\nSet-Cookie: x=1",
        "unicode \u{e9}\u{4e2d}\u{1f600}",
        "",
    ] {
        let value = challenge(message);
        let text = value.to_str().unwrap();
        assert!(
            text.starts_with("Bearer realm=\"pub\", message=\""),
            "{text}"
        );
        assert!(text.ends_with('"'));
        // One quoted string: the only quotes are the two that delimit it, and the realm's two.
        assert_eq!(text.matches('"').count(), 4, "{text}");
        assert!(!text.contains('\\') && !text.contains('\r') && !text.contains('\n'));
    }
}

#[test]
fn respond_sets_the_media_type_and_nosniff() {
    let response = respond(StatusCode::NOT_ACCEPTABLE, "not_acceptable", "only v2");
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(response.headers()[header::CONTENT_TYPE], MEDIA_TYPE);
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
}

#[test]
fn name_and_version_errors_become_bad_requests() {
    let name = FormatError::from(NameError::BadChar);
    assert!(matches!(&name, FormatError::BadRequest(m) if m.starts_with("package name")));
    let version = FormatError::from(VersionError::Invalid);
    assert!(matches!(&version, FormatError::BadRequest(m) if m.starts_with("version")));
}
