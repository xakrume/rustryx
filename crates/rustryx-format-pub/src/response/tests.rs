//! Tests of the JSON answers.

use http::{HeaderMap, Method};
use rustryx_core::identity::{Action, Principal};
use rustryx_format_core::stream::read_bounded;
use rustryx_format_core::{NormalizedPath, RequestHead};
use url::Url;

use super::*;

fn request(method: Method, if_none_match: Option<&str>) -> ArtifactRequest {
    let mut headers = HeaderMap::new();
    if let Some(value) = if_none_match {
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
    }
    let head = RequestHead::new(
        method,
        "api/packages/demo",
        Principal::anonymous(),
        Url::parse("https://rustryx.test").unwrap(),
    )
    .with_headers(headers);
    ArtifactRequest::new(
        &head,
        NormalizedPath::parse("api/packages/demo").unwrap(),
        Action::Read,
    )
}

#[tokio::test]
async fn a_document_has_media_type_length_and_a_strong_etag() {
    let response = document(&request(Method::GET, None), b"{\"a\":1}".to_vec());
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], MEDIA_TYPE);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "7");
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(etag.starts_with('"') && etag.ends_with('"') && etag.len() == 66);
    let body = read_bounded(response.into_body(), 100).await.unwrap();
    assert_eq!(body.as_ref(), b"{\"a\":1}");
}

#[tokio::test]
async fn the_etag_follows_the_bytes_sent() {
    let one = document(&request(Method::GET, None), b"one".to_vec());
    let two = document(&request(Method::GET, None), b"two".to_vec());
    let again = document(&request(Method::GET, None), b"one".to_vec());
    assert_ne!(one.headers()[header::ETAG], two.headers()[header::ETAG]);
    assert_eq!(one.headers()[header::ETAG], again.headers()[header::ETAG]);
}

#[tokio::test]
async fn a_matching_if_none_match_is_a_304_without_a_body() {
    let first = document(&request(Method::GET, None), b"body".to_vec());
    let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
    let second = document(&request(Method::GET, Some(&etag)), b"body".to_vec());
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(second.headers()[header::ETAG], etag.as_str());
    assert!(
        read_bounded(second.into_body(), 10)
            .await
            .unwrap()
            .is_empty()
    );
    let other = document(
        &request(Method::GET, Some("\"different\"")),
        b"body".to_vec(),
    );
    assert_eq!(other.status(), StatusCode::OK);
}

#[tokio::test]
async fn head_has_the_headers_of_get_and_no_body() {
    let response = document(&request(Method::HEAD, None), b"1234".to_vec());
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
    assert!(
        read_bounded(response.into_body(), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_publish_answers_are_not_cacheable() {
    let response = json(StatusCode::OK, b"{}".to_vec());
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::CONTENT_TYPE], MEDIA_TYPE);
}

#[test]
fn the_cache_marker_and_the_stale_warning() {
    let mut response = json(StatusCode::OK, Vec::new());
    mark_cache(&mut response, CacheStatus::Hit);
    assert_eq!(response.headers()[X_RUSTRYX_CACHE], "HIT");
    assert!(!response.headers().contains_key(header::WARNING));
    mark_cache(&mut response, CacheStatus::Stale);
    assert_eq!(response.headers()[X_RUSTRYX_CACHE], "STALE");
    assert!(
        response.headers()[header::WARNING]
            .to_str()
            .unwrap()
            .starts_with("110 ")
    );
}
