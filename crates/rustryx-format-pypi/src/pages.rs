//! Responses that carry an index document: representation, size cap, validators, headers.
//!
//! Index documents are generated per request (hosted), rewritten from a cached upstream
//! document (proxy) or merged (group), so their validators are computed here from the bytes that
//! are sent: an `ETag` that is the SHA-256 of the body, honoured for `If-None-Match`. A client
//! that revalidates (pip does) gets a 304 and no body.

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header};
use rustryx_format_core::conditional::if_none_match_hits;
use rustryx_format_core::{CachedDocument, RequestHead};
use sha2::{Digest, Sha256};

use crate::negotiate::{Representation, negotiate};

/// The representation the `Accept` header of the request asks for, `None` when nothing offered
/// is acceptable (the caller answers with [`not_acceptable`]).
pub(crate) fn negotiated(head: &RequestHead) -> Option<Representation> {
    negotiate(head.header(&header::ACCEPT))
}

/// The 406 answer of PEP 691 for a client that accepts none of the offered media types. It is a
/// response of its own because `FormatError` has no 406.
pub(crate) fn not_acceptable() -> Response<Body> {
    let text = "none of the requested media types is available; \
                supported: application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html, text/html\n";
    let mut response = Response::new(Body::from(text));
    *response.status_mut() = StatusCode::NOT_ACCEPTABLE;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Accept"));
    response
}

/// Adds `Vary: Accept` (the representation depends on it).
pub(crate) fn add_vary(response: &mut Response<Body>) {
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("Accept"));
}

/// Cache headers of a proxied document that stay true after the body was rewritten: the marker
/// the engine reads for its `cache` label, the `Warning` of a stale copy, `Last-Modified`.
const KEPT_FROM_DOCUMENT: [HeaderName; 3] = [
    HeaderName::from_static("x-rustryx-cache"),
    header::WARNING,
    header::LAST_MODIFIED,
];

/// Builds the response for an index document of `body`. `negotiated` is true when the
/// representation was chosen from `Accept`, which makes the response vary on it.
pub(crate) fn page_response(
    head: &RequestHead,
    content_type: &'static str,
    negotiated: bool,
    body: &Bytes,
    document: Option<&CachedDocument>,
    complete: bool,
) -> Response<Body> {
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(body)));
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if negotiated {
        headers.insert(header::VARY, HeaderValue::from_static("Accept"));
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // Revalidate on every use: a new upload must be visible at once, and the 304 keeps it cheap.
    // A page that is known to be incomplete must not be kept at all.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if complete { "no-cache" } else { "no-store" }),
    );
    if let Some(document) = document {
        let kept = document.response_headers();
        for name in &KEPT_FROM_DOCUMENT {
            if let Some(value) = kept.get(name) {
                headers.insert(name.clone(), value.clone());
            }
        }
    }
    let mut response = if if_none_match_hits(&head.headers, &etag) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response
    } else {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len() as u64));
        let payload = if head.method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(Bytes::clone(body))
        };
        Response::new(payload)
    };
    response.headers_mut().extend(headers);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::identity::Principal;
    use url::Url;

    fn head(method: Method, accept: Option<&str>, if_none_match: Option<&str>) -> RequestHead {
        let mut head = RequestHead::new(
            method,
            "simple/foo",
            Principal::anonymous(),
            Url::parse("https://r.test").unwrap(),
        );
        if let Some(accept) = accept {
            head.headers
                .insert(header::ACCEPT, HeaderValue::from_str(accept).unwrap());
        }
        if let Some(tag) = if_none_match {
            head.headers
                .insert(header::IF_NONE_MATCH, HeaderValue::from_str(tag).unwrap());
        }
        head
    }

    #[test]
    fn negotiation_failure_is_406() {
        assert_eq!(
            negotiated(&head(Method::GET, Some("image/png"), None)),
            None
        );
        assert_eq!(
            negotiated(&head(Method::GET, None, None)),
            Some(Representation::TextHtml)
        );
        let response = not_acceptable();
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
        assert_eq!(response.headers().get(header::VARY).unwrap(), "Accept");
    }

    #[test]
    fn a_page_carries_its_validators_and_vary() {
        let body = Bytes::from_static(b"<html></html>");
        let response = page_response(
            &head(Method::GET, None, None),
            Representation::TextHtml.content_type(),
            true,
            &body,
            None,
            true,
        );
        assert_eq!(response.status(), StatusCode::OK);
        let etag = response
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(etag.starts_with('"') && etag.len() == 66);
        assert_eq!(response.headers().get(header::VARY).unwrap(), "Accept");
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "13"
        );
        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .unwrap(),
            "nosniff"
        );
        // The same body revalidates.
        let again = page_response(
            &head(Method::GET, None, Some(&etag)),
            Representation::TextHtml.content_type(),
            true,
            &body,
            None,
            true,
        );
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        assert!(again.headers().get(header::ETAG).is_some());
        assert!(again.headers().get(header::CONTENT_LENGTH).is_none());
    }

    #[test]
    fn head_has_no_body_but_the_length_and_an_incomplete_page_is_not_cached() {
        let body = Bytes::from_static(b"{}");
        let response = page_response(
            &head(Method::HEAD, None, None),
            Representation::Json.content_type(),
            true,
            &body,
            None,
            false,
        );
        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "2");
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/vnd.pypi.simple.v1+json"
        );
    }

    #[test]
    fn a_document_that_was_not_negotiated_does_not_vary() {
        let body = Bytes::from_static(b"{}");
        let response = page_response(
            &head(Method::GET, None, None),
            "application/json",
            false,
            &body,
            None,
            true,
        );
        assert!(response.headers().get(header::VARY).is_none());
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[test]
    fn add_vary_appends() {
        let mut response = Response::new(Body::empty());
        add_vary(&mut response);
        assert_eq!(response.headers().get(header::VARY).unwrap(), "Accept");
    }
}
