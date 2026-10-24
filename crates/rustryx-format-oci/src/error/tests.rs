use std::time::Duration;

use rustryx_core::error::{BlobError, PolicyViolation, StoreError};
use rustryx_format_core::stream::read_bounded;
use serde_json::Value;

use super::*;

async fn body_of(response: Response<Body>) -> Value {
    let bytes = read_bounded(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn renders_the_errors_array_with_headers() {
    let response = OciError::manifest_blob_unknown("sha256:abc").into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(response.headers()[DISTRIBUTION_API_VERSION], "registry/2.0");
    let body = body_of(response).await;
    let error = &body["errors"][0];
    assert_eq!(error["code"], "MANIFEST_BLOB_UNKNOWN");
    assert_eq!(error["detail"]["digest"], "sha256:abc");
    assert!(error["message"].as_str().unwrap().contains("unknown"));
    assert_eq!(body["errors"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn detail_is_omitted_when_there_is_none() {
    let body = body_of(OciError::blob_unknown().into_response()).await;
    assert!(body["errors"][0].get("detail").is_none());
    assert_eq!(body["errors"][0]["code"], "BLOB_UNKNOWN");
}

#[test]
fn every_code_has_a_unique_wire_name_that_parses_back() {
    let mut names = std::collections::BTreeSet::new();
    for name in [
        "BLOB_UNKNOWN",
        "BLOB_UPLOAD_INVALID",
        "BLOB_UPLOAD_UNKNOWN",
        "DIGEST_INVALID",
        "MANIFEST_BLOB_UNKNOWN",
        "MANIFEST_INVALID",
        "MANIFEST_UNKNOWN",
        "NAME_INVALID",
        "NAME_UNKNOWN",
        "SIZE_INVALID",
        "UNAUTHORIZED",
        "DENIED",
        "UNSUPPORTED",
        "TOOMANYREQUESTS",
        "UNAVAILABLE",
        "UNKNOWN",
    ] {
        let code = OciErrorCode::from_name(name).unwrap();
        assert_eq!(code.as_str(), name);
        assert_eq!(code.to_string(), name);
        assert!(names.insert(name));
    }
    assert_eq!(OciErrorCode::from_name("blob_unknown"), None);
    assert_eq!(OciErrorCode::from_name("NOPE"), None);
}

#[test]
fn an_embedded_code_is_used_and_stripped() {
    let error =
        FormatError::bad_request(coded(OciErrorCode::NameInvalid, "invalid repository name"));
    let described = OciError::describe(&error);
    assert_eq!(described.status(), StatusCode::BAD_REQUEST);
    assert_eq!(described.code(), OciErrorCode::NameInvalid);
    assert_eq!(described.message(), "invalid repository name");
}

#[test]
fn a_stray_bracket_is_not_a_code() {
    for text in ["[NOPE] x", "[BLOB_UNKNOWN]x", "[ x", "plain", "]["] {
        let described = OciError::describe(&FormatError::bad_request(text));
        assert_eq!(described.code(), OciErrorCode::Unsupported, "{text}");
        assert_eq!(described.message(), text);
    }
}

#[test]
fn engine_errors_map_by_status() {
    let cases: Vec<(FormatError, u16, OciErrorCode)> = vec![
        (
            FormatError::Unauthenticated,
            401,
            OciErrorCode::Unauthorized,
        ),
        (FormatError::Forbidden, 403, OciErrorCode::Denied),
        (FormatError::NotFound, 404, OciErrorCode::NameUnknown),
        (
            FormatError::MethodNotAllowed { allow: "GET, HEAD" },
            405,
            OciErrorCode::Unsupported,
        ),
        (FormatError::conflict("taken"), 409, OciErrorCode::Denied),
        (
            FormatError::DigestMismatch,
            400,
            OciErrorCode::DigestInvalid,
        ),
        (
            FormatError::unprocessable("x"),
            422,
            OciErrorCode::ManifestInvalid,
        ),
        (
            FormatError::PayloadTooLarge { limit: 1 },
            413,
            OciErrorCode::SizeInvalid,
        ),
        (
            FormatError::TooManyRequests {
                retry_after: Duration::from_secs(3),
            },
            429,
            OciErrorCode::TooManyRequests,
        ),
        (FormatError::QuotaExceeded, 507, OciErrorCode::Denied),
        (
            FormatError::Policy(PolicyViolation::ReadOnly),
            503,
            OciErrorCode::Unavailable,
        ),
        (
            FormatError::Unavailable {
                reason: "busy",
                retry_after: None,
            },
            503,
            OciErrorCode::Unavailable,
        ),
        (
            FormatError::upstream(rustryx_format_core::UpstreamFailureKind::Timeout, "t"),
            504,
            OciErrorCode::Unavailable,
        ),
        (
            FormatError::upstream(rustryx_format_core::UpstreamFailureKind::Rejected, "r"),
            502,
            OciErrorCode::Unavailable,
        ),
        (FormatError::internal("boom"), 500, OciErrorCode::Unknown),
        (
            FormatError::Store(StoreError::Unavailable("x".into())),
            503,
            OciErrorCode::Unavailable,
        ),
        (
            FormatError::Blob(BlobError::InsufficientStorage),
            507,
            OciErrorCode::Denied,
        ),
        (
            FormatError::RangeNotSatisfiable { total: 5 },
            416,
            OciErrorCode::Unsupported,
        ),
    ];
    for (error, status, code) in cases {
        let described = OciError::describe(&error);
        assert_eq!(described.status().as_u16(), status, "{error:?}");
        assert_eq!(described.code(), code, "{error:?}");
    }
}

#[test]
fn protocol_headers_of_the_engine_error_are_kept() {
    let allow = OciError::describe(&FormatError::MethodNotAllowed { allow: "GET, HEAD" });
    assert_eq!(allow.into_response().headers()[header::ALLOW], "GET, HEAD");
    let retry = OciError::describe(&FormatError::TooManyRequests {
        retry_after: Duration::from_secs(7),
    });
    assert_eq!(retry.into_response().headers()[header::RETRY_AFTER], "7");
    let range = OciError::describe(&FormatError::RangeNotSatisfiable { total: 9 });
    assert_eq!(
        range.into_response().headers()[header::CONTENT_RANGE],
        "bytes */9"
    );
}

#[tokio::test]
async fn must_not_leak_internal_text_in_a_body() {
    let secret = "/var/lib/rustryx/blobs/secret-path postgres://user:hunter2@db/x";
    for error in [
        FormatError::internal(secret),
        FormatError::Store(StoreError::Unavailable(secret.into())),
        FormatError::upstream(
            rustryx_format_core::UpstreamFailureKind::Unreachable,
            secret,
        ),
    ] {
        let response = OciError::from_format_error(error).into_response();
        let text = body_of(response).await.to_string();
        assert!(
            !text.contains("hunter2") && !text.contains("secret-path"),
            "{text}"
        );
    }
}

#[test]
fn server_errors_keep_their_cause_for_the_engine_log() {
    let error = OciError::from_format_error(FormatError::internal("the real cause"));
    assert!(error.is_server_error());
    assert!(
        error
            .into_format_error()
            .to_string()
            .contains("the real cause")
    );
    let client = OciError::blob_unknown();
    assert!(!client.is_server_error());
    assert!(matches!(
        client.into_format_error(),
        FormatError::Internal(_)
    ));
    assert!(OciError::internal("x").is_server_error());
}

#[test]
fn relabelling_only_touches_the_matching_status() {
    let relabelled = OciError::describe(&FormatError::NotFound)
        .not_found_as(OciErrorCode::BlobUnknown, "blob unknown to registry");
    assert_eq!(relabelled.code(), OciErrorCode::BlobUnknown);
    let untouched =
        OciError::describe(&FormatError::Forbidden).not_found_as(OciErrorCode::BlobUnknown, "x");
    assert_eq!(untouched.code(), OciErrorCode::Denied);
    let upload = OciError::describe(&FormatError::bad_request("body length does not match"))
        .bad_request_as(OciErrorCode::BlobUploadInvalid);
    assert_eq!(upload.code(), OciErrorCode::BlobUploadInvalid);
    let digest = OciError::digest_invalid("x").bad_request_as(OciErrorCode::BlobUploadInvalid);
    assert_eq!(digest.code(), OciErrorCode::DigestInvalid);
}
