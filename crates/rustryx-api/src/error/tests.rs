//! Mapping and envelope tests.

use std::time::Duration;

use axum::body::to_bytes;
use axum::response::IntoResponse;
use http::StatusCode;
use rustryx_core::AuthError;
use rustryx_core::error::{BlobError, DomainError, StoreError};

use super::*;
use crate::test_support::Reply;

async fn render(error: impl Into<ApiError>) -> Reply {
    let response = error.into().into_response();
    let (parts, body) = response.into_parts();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: to_bytes(body, usize::MAX).await.expect("body"),
    }
}

#[tokio::test]
async fn envelope_has_code_message_and_no_request_id_outside_a_request() {
    let reply = render(ApiError::forbidden()).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.header("content-type"), Some("application/json"));
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    let json = reply.json();
    assert_eq!(json["error"]["code"], "forbidden");
    assert_eq!(json["error"]["message"], "Access denied");
    assert!(json["error"].get("request_id").is_none());
}

#[tokio::test]
async fn domain_errors_map_to_stable_statuses_and_codes() {
    let cases: Vec<(DomainError, StatusCode, &str)> = vec![
        (
            DomainError::NotFound {
                entity: "repository",
            },
            StatusCode::NOT_FOUND,
            "repository_not_found",
        ),
        (
            DomainError::AlreadyExists("repository 'a' already exists".into()),
            StatusCode::CONFLICT,
            "already_exists",
        ),
        (
            DomainError::Conflict("state".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            DomainError::invalid("name is empty"),
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            DomainError::Unauthenticated,
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
        ),
        (DomainError::Forbidden, StatusCode::FORBIDDEN, "forbidden"),
        (
            DomainError::QuotaExceeded,
            StatusCode::INSUFFICIENT_STORAGE,
            "quota_exceeded",
        ),
        (
            DomainError::Unavailable("x".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            "dependency_unavailable",
        ),
        (
            DomainError::Internal("x".into()),
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
        ),
    ];
    for (error, status, code) in cases {
        let reply = render(error).await;
        assert_eq!(reply.status, status, "{code}");
        assert_eq!(reply.error_code(), code);
    }
}

#[tokio::test]
async fn domain_error_wrapping_other_errors_maps_like_the_inner_error() {
    let reply = render(DomainError::Store(StoreError::NotFound("row".into()))).await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    let reply = render(DomainError::Blob(BlobError::RangeNotSatisfiable)).await;
    assert_eq!(reply.status, StatusCode::RANGE_NOT_SATISFIABLE);
    let reply = render(DomainError::Auth(AuthError::TokenExpired)).await;
    assert_eq!(reply.error_code(), "token_expired");
}

#[tokio::test]
async fn store_and_blob_errors_map_to_client_dependency_and_internal_classes() {
    let reply = render(StoreError::Conflict("duplicate key".into())).await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (StatusCode::CONFLICT, "conflict")
    );
    let reply = render(StoreError::Unavailable("pool timed out".into())).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(reply.header("retry-after").is_some());
    let reply = render(BlobError::NotFound).await;
    assert_eq!(reply.error_code(), "blob_not_found");
    let reply = render(BlobError::Unavailable("s3 down".into())).await;
    assert_eq!(reply.error_code(), "blob_store_unavailable");
    assert!(reply.header("retry-after").is_some());
    let reply = render(BlobError::WriterClosed).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn auth_errors_map_and_unauthorized_ones_carry_a_challenge() {
    for (error, status, code) in [
        (
            AuthError::InvalidCredentials,
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
        ),
        (
            AuthError::InvalidToken("bad sig"),
            StatusCode::UNAUTHORIZED,
            "invalid_token",
        ),
        (
            AuthError::TokenExpired,
            StatusCode::UNAUTHORIZED,
            "token_expired",
        ),
        (
            AuthError::TokenRevoked,
            StatusCode::UNAUTHORIZED,
            "token_revoked",
        ),
        (
            AuthError::UsernameConflict,
            StatusCode::CONFLICT,
            "username_conflict",
        ),
        (
            AuthError::GroupLookupFailed("x".into()),
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        (
            AuthError::ProviderUnavailable("x".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_provider_unavailable",
        ),
        (
            AuthError::Overloaded,
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
        ),
        (
            AuthError::GuardUnavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "login_unavailable",
        ),
    ] {
        let reply = render(error).await;
        assert_eq!((reply.status, reply.error_code().as_str()), (status, code));
        if status == StatusCode::UNAUTHORIZED {
            assert!(
                reply.header("www-authenticate").is_some(),
                "{code} needs a challenge"
            );
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            assert!(
                reply.header("retry-after").is_some(),
                "{code} needs Retry-After"
            );
        }
    }
}

#[tokio::test]
async fn must_not_reveal_a_disabled_account_differently_from_a_wrong_password() {
    let wrong = render(AuthError::InvalidCredentials).await;
    let disabled = render(AuthError::AccountDisabled).await;
    assert_eq!(wrong.status, disabled.status);
    assert_eq!(wrong.body, disabled.body, "bodies must be identical");
}

#[tokio::test]
async fn lockout_is_429_with_retry_after_rounded_up() {
    let reply = render(AuthError::LockedOut {
        retry_after: Duration::from_millis(61_500),
    })
    .await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(reply.header("retry-after"), Some("62"));
    let reply = render(AuthError::LockedOut {
        retry_after: Duration::ZERO,
    })
    .await;
    assert_eq!(reply.header("retry-after"), Some("1"), "never zero");
}

#[tokio::test]
async fn must_not_leak_internal_details_of_backend_errors() {
    let secrets = [
        "violates unique constraint",
        "repositories_name_key",
        "10.1.2.3",
        "/var/lib/rustryx",
        "/srv/app/src",
        "ldaps://ldap.corp.internal",
        "ou=groups",
        "redis at",
        "stack:",
    ];
    let errors: Vec<ApiError> = vec![
        StoreError::Backend {
            context: "insert",
            source: "duplicate key value violates unique constraint \"repositories_name_key\" host=10.1.2.3".into(),
        }
        .into(),
        BlobError::Backend {
            context: "write",
            source: "ENOENT /var/lib/rustryx/blobs/ab".into(),
        }
        .into(),
        DomainError::Internal("stack: /srv/app/src/service.rs:42".into()).into(),
        DomainError::Unavailable("redis at 10.9.8.7 refused".into()).into(),
        AuthError::ProviderUnavailable("ldaps://ldap.corp.internal:636".into()).into(),
        AuthError::GroupLookupFailed("ou=groups,dc=corp".into()).into(),
        StoreError::Conflict("repositories_name_key".into()).into(),
        StoreError::Unavailable("host=10.1.2.3 refused".into()).into(),
        BlobError::Unavailable("s3.internal:9000".into()).into(),
        DomainError::Store(StoreError::NotFound("row 42 in table repositories".into())).into(),
        AuthError::Backend {
            context: "ldap bind",
            source: "ldaps://ldap.corp.internal:636 reset".into(),
        }
        .into(),
    ];
    for error in errors {
        let reply = render(error).await;
        let text = reply.text();
        for secret in secrets {
            assert!(!text.contains(secret), "{secret} leaked in {text}");
        }
        for header in reply.headers.values() {
            let value = header.to_str().unwrap_or_default();
            for secret in secrets {
                assert!(!value.contains(secret), "{secret} leaked in a header");
            }
        }
    }
}

#[tokio::test]
async fn client_messages_are_cleaned_and_capped() {
    let long = "x".repeat(1000);
    let reply = render(DomainError::invalid(format!("bad\nvalue\u{7}{long}"))).await;
    let message = reply.json()["error"]["message"]
        .as_str()
        .expect("message")
        .to_owned();
    assert!(!message.contains('\n') && !message.contains('\u{7}'));
    assert!(message.chars().count() <= MAX_CLIENT_MESSAGE_CHARS + 3);
    assert!(message.ends_with("..."));
}

#[tokio::test]
async fn entity_codes_are_slugged_and_messages_capitalised() {
    let reply = render(DomainError::NotFound {
        entity: "content selector",
    })
    .await;
    assert_eq!(reply.error_code(), "content_selector_not_found");
    assert_eq!(
        reply.json()["error"]["message"],
        "Content selector not found"
    );
}

#[tokio::test]
async fn challenges_render_as_headers() {
    assert_eq!(
        Challenge::basic("r")
            .header_value()
            .to_str()
            .expect("ascii"),
        "Basic realm=\"r\", charset=\"UTF-8\""
    );
    assert_eq!(
        Challenge::bearer("pub")
            .header_value()
            .to_str()
            .expect("ascii"),
        "Bearer realm=\"pub\""
    );
    let with_error = Challenge::Bearer {
        realm: "pub".into(),
        error: Some("invalid_token"),
    };
    assert_eq!(
        with_error.header_value().to_str().expect("ascii"),
        "Bearer realm=\"pub\", error=\"invalid_token\""
    );
    let hostile = Challenge::basic("evil\", realm=\"x");
    assert!(
        hostile
            .header_value()
            .to_str()
            .expect("ascii")
            .contains("realm=\"rustryx\"")
    );
}

#[tokio::test]
async fn explicit_challenge_wins_and_forbidden_may_carry_one() {
    let reply = render(ApiError::forbidden().with_challenge(Challenge::bearer("pub"))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        reply.header("www-authenticate"),
        Some("Bearer realm=\"pub\"")
    );
    let reply = render(ApiError::unauthenticated()).await;
    assert!(
        reply
            .header("www-authenticate")
            .expect("default challenge")
            .starts_with("Basic")
    );
}

#[tokio::test]
async fn named_constructors_have_the_documented_statuses() {
    for (error, status, code) in [
        (
            ApiError::route_not_found(),
            StatusCode::NOT_FOUND,
            "route_not_found",
        ),
        (
            ApiError::method_not_allowed(),
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
        (
            ApiError::payload_too_large(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
        ),
        (
            ApiError::request_body_timeout(),
            StatusCode::REQUEST_TIMEOUT,
            "request_body_timeout",
        ),
        (
            ApiError::request_timeout(),
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
        ),
        (
            ApiError::upstream_error(),
            StatusCode::BAD_GATEWAY,
            "upstream_error",
        ),
        (
            ApiError::upstream_timeout(),
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
        ),
        (
            ApiError::bad_request("c", "m"),
            StatusCode::BAD_REQUEST,
            "c",
        ),
        (
            ApiError::internal("detail"),
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
        ),
    ] {
        assert_eq!((error.status(), error.code()), (status, code));
    }
    let overloaded = ApiError::overloaded(Duration::from_secs(9));
    assert_eq!(overloaded.retry_after(), Some(Duration::from_secs(9)));
}

#[tokio::test]
async fn must_not_expose_the_detail_given_to_internal() {
    let reply = render(ApiError::internal("password=hunter2 at /etc/rustryx.yaml")).await;
    assert!(!reply.text().contains("hunter2"));
    assert!(!reply.text().contains("/etc/rustryx.yaml"));
    assert_eq!(reply.json()["error"]["message"], "Internal server error");
}

#[test]
fn body_errors_are_classified_by_their_cause() {
    use http_body_util::{BodyExt, Limited};

    let limited = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(async {
            let body = Limited::new(axum::body::Body::from(vec![0_u8; 10]), 4);
            body.collect().await.expect_err("over the limit")
        });
    let boxed: Box<dyn std::error::Error + Send + Sync> = limited;
    assert_eq!(
        ApiError::from_body_error(boxed.as_ref()).status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let wrapped = axum::Error::new(boxed);
    assert_eq!(
        ApiError::from_body_error(&wrapped).status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let other = std::io::Error::other("connection reset");
    assert_eq!(
        ApiError::from_body_error(&other).status(),
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn api_error_is_an_error_with_a_source() {
    let error = ApiError::from(StoreError::Unavailable("x".into()));
    assert_eq!(error.to_string(), "503 store_unavailable");
    assert!(std::error::Error::source(&error).is_some());
    assert!(std::error::Error::source(&ApiError::forbidden()).is_none());
}
