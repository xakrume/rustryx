//! Responses for documents the format builds itself: generated or merged metadata, derived
//! checksum files, a cached upstream document. Stored files go through the hosted pipeline,
//! which streams them from the blob store; these are small and in memory by design (the
//! only buffered data, under the caps documented in `config`).
//!
//! The headers match the pipeline's download: `Content-Type`, `Content-Length`, strong `ETag`,
//! `Last-Modified`, `Accept-Ranges`, single `Range` with `If-Range`, `If-None-Match`,
//! `If-Match`, `If-Modified-Since`, `If-Unmodified-Since`, `HEAD`, `nosniff` and a sandboxing CSP.

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header};
use rustryx_format_core::conditional::{
    Conditional, RangeDecision, decide_range, evaluate_conditional, format_http_date,
    if_none_match_hits,
};
use rustryx_format_core::{FormatError, RequestHead};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// A document held in memory, ready to be answered.
pub(crate) struct Document {
    pub(crate) body: Bytes,
    pub(crate) content_type: String,
    /// `None` for a document with no meaningful modification time (a merged index): then
    /// only entity tags decide conditional requests, so `If-Modified-Since` cannot produce
    /// a wrong 304.
    pub(crate) last_modified: Option<OffsetDateTime>,
    /// Headers to add as they are (cache marker, stale warning, group skip marker).
    pub(crate) extra: HeaderMap,
    /// Entity tag; `None` derives a strong tag from the SHA-256 of the body.
    pub(crate) etag: Option<String>,
}

impl Document {
    pub(crate) fn new(body: impl Into<Bytes>, content_type: &str) -> Self {
        Self {
            body: body.into(),
            content_type: content_type.to_owned(),
            last_modified: None,
            extra: HeaderMap::new(),
            etag: None,
        }
    }

    pub(crate) fn modified(mut self, at: Option<OffsetDateTime>) -> Self {
        self.last_modified = at;
        self
    }

    pub(crate) fn etag(mut self, etag: String) -> Self {
        self.etag = Some(etag);
        self
    }

    pub(crate) fn extra(mut self, headers: HeaderMap) -> Self {
        self.extra = headers;
        self
    }
}

fn strong_etag(body: &[u8]) -> String {
    format!("\"{}\"", hex::encode(Sha256::digest(body)))
}

fn insert(headers: &mut HeaderMap, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

/// Answers `head` with `document`.
///
/// # Errors
/// `PreconditionFailed` and `RangeNotSatisfiable`, which the engine renders.
pub(crate) fn respond(
    head: &RequestHead,
    document: Document,
) -> Result<Response<Body>, FormatError> {
    let etag = document
        .etag
        .clone()
        .unwrap_or_else(|| strong_etag(&document.body));
    let total = document.body.len() as u64;

    let condition = match document.last_modified {
        Some(at) => evaluate_conditional(&head.method, &head.headers, &etag, at),
        None if head.is_read_method() && if_none_match_hits(&head.headers, &etag) => {
            Conditional::NotModified
        }
        None => Conditional::Proceed,
    };
    match condition {
        Conditional::Proceed => {}
        Conditional::PreconditionFailed => return Err(FormatError::PreconditionFailed),
        Conditional::NotModified => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::NOT_MODIFIED;
            let headers = response.headers_mut();
            insert(headers, header::ETAG, &etag);
            if let Some(at) = document.last_modified {
                insert(headers, header::LAST_MODIFIED, &format_http_date(at));
            }
            headers.extend(document.extra);
            return Ok(response);
        }
    }

    let decision = decide_range(
        &head.headers,
        total,
        &etag,
        document.last_modified.unwrap_or(OffsetDateTime::UNIX_EPOCH),
    );
    let (status, slice, content_range) = match decision {
        RangeDecision::Full => (StatusCode::OK, document.body.clone(), None),
        RangeDecision::Partial(range) => {
            let end = range.end.unwrap_or(total.saturating_sub(1));
            let start = usize::try_from(range.start).unwrap_or(usize::MAX);
            let stop = usize::try_from(end.saturating_add(1)).unwrap_or(usize::MAX);
            let part = document
                .body
                .get(start..stop.min(document.body.len()))
                .map_or_else(Bytes::new, |slice| document.body.slice_ref(slice));
            (
                StatusCode::PARTIAL_CONTENT,
                part,
                Some(format!("bytes {}-{end}/{total}", range.start)),
            )
        }
        RangeDecision::Unsatisfiable => return Err(FormatError::RangeNotSatisfiable { total }),
    };

    let length = slice.len() as u64;
    let body = if head.method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(slice)
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    insert(headers, header::CONTENT_TYPE, &document.content_type);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    insert(headers, header::ETAG, &etag);
    if let Some(at) = document.last_modified {
        insert(headers, header::LAST_MODIFIED, &format_http_date(at));
    }
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    if let Some(range) = content_range {
        insert(headers, header::CONTENT_RANGE, &range);
    }
    headers.extend(document.extra);
    Ok(response)
}

/// A checksum file body: the lowercase hex digest, no newline (the form Maven Central serves).
pub(crate) fn checksum_document(digest: String, last_modified: Option<OffsetDateTime>) -> Document {
    Document::new(digest, "text/plain; charset=utf-8").modified(last_modified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::identity::Principal;
    use url::Url;

    fn head(method: Method, pairs: &[(&str, &str)]) -> RequestHead {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        RequestHead::new(
            method,
            "x",
            Principal::anonymous(),
            Url::parse("https://x.test").unwrap(),
        )
        .with_headers(headers)
    }

    fn at(unix: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(unix).unwrap()
    }

    fn doc() -> Document {
        Document::new("0123456789", "application/xml").modified(Some(at(1_700_000_000)))
    }

    #[test]
    fn a_get_carries_the_full_set_of_headers() {
        let response = respond(&head(Method::GET, &[]), doc()).unwrap();
        assert_eq!(response.status(), 200);
        let h = response.headers();
        assert_eq!(h[header::CONTENT_TYPE], "application/xml");
        assert_eq!(h[header::CONTENT_LENGTH], "10");
        assert!(h[header::ETAG].to_str().unwrap().starts_with('"'));
        assert_eq!(h[header::LAST_MODIFIED], "Tue, 14 Nov 2023 22:13:20 GMT");
        assert_eq!(h[header::ACCEPT_RANGES], "bytes");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(h.contains_key(header::CONTENT_SECURITY_POLICY));
    }

    #[test]
    fn head_has_the_length_and_no_body() {
        let response = respond(&head(Method::HEAD, &[]), doc()).unwrap();
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
        assert_eq!(response.status(), 200);
    }

    #[test]
    fn a_matching_etag_is_a_304_with_validators() {
        let etag = strong_etag(b"0123456789");
        let response = respond(&head(Method::GET, &[("if-none-match", &etag)]), doc()).unwrap();
        assert_eq!(response.status(), 304);
        assert_eq!(response.headers()[header::ETAG], etag.as_str());
    }

    #[test]
    fn if_modified_since_gives_304_only_with_a_known_modification_time() {
        let since = &[("if-modified-since", "Wed, 15 Nov 2023 00:00:00 GMT")];
        let with_time = respond(&head(Method::GET, since), doc()).unwrap();
        assert_eq!(with_time.status(), 304);
        let without = respond(
            &head(Method::GET, since),
            Document::new("0123456789", "application/xml"),
        )
        .unwrap();
        assert_eq!(without.status(), 200);
    }

    #[test]
    fn if_match_with_another_tag_fails_the_precondition() {
        let result = respond(&head(Method::GET, &[("if-match", "\"other\"")]), doc());
        assert!(matches!(result, Err(FormatError::PreconditionFailed)));
    }

    #[test]
    fn a_range_is_a_206_with_content_range() {
        let response = respond(&head(Method::GET, &[("range", "bytes=2-4")]), doc()).unwrap();
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-4/10");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "3");
    }

    #[test]
    fn an_unsatisfiable_range_is_refused_with_the_size() {
        let result = respond(&head(Method::GET, &[("range", "bytes=50-60")]), doc());
        assert!(matches!(
            result,
            Err(FormatError::RangeNotSatisfiable { total: 10 })
        ));
    }

    #[test]
    fn extra_headers_are_added_and_the_checksum_document_is_plain_text() {
        let mut extra = HeaderMap::new();
        extra.insert("x-rustryx-cache", HeaderValue::from_static("HIT"));
        let response = respond(
            &head(Method::GET, &[]),
            checksum_document("abc".into(), None).extra(extra),
        )
        .unwrap();
        assert_eq!(response.headers()["x-rustryx-cache"], "HIT");
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
        assert!(!response.headers().contains_key(header::LAST_MODIFIED));
    }

    #[test]
    fn an_explicit_etag_wins() {
        let response = respond(&head(Method::GET, &[]), doc().etag("\"fixed\"".into())).unwrap();
        assert_eq!(response.headers()[header::ETAG], "\"fixed\"");
    }
}
