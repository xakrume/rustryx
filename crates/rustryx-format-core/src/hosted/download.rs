//! Streaming download of a stored asset: headers, validators, ranges.
//!
//! Used by hosted downloads, by proxy cache hits and by followers of a cache fill,
//! so all three answer identically.

use axum_core::body::Body;
use http::{HeaderName, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::error::BlobError;
use rustryx_core::model::Asset;
use rustryx_core::ports::ByteRange;

use crate::conditional::{
    Conditional, RangeDecision, decide_range, etag_from_checksums, evaluate_conditional,
    format_http_date,
};
use crate::context::{ContentEvent, FormatContext};
use crate::error::FormatError;
use crate::request::RequestHead;
use crate::stream::body_from_blob;

/// Header that tells how a proxy response was produced.
pub const X_RUSTRYX_CACHE: HeaderName = HeaderName::from_static("x-rustryx-cache");

/// How a response relates to the cache. Sent as `X-Rustryx-Cache`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// Served from the cache, fresh.
    Hit,
    /// Fetched from the upstream for this request.
    Miss,
    /// Served from the cache although expired, because the upstream is unavailable.
    Stale,
}

impl CacheStatus {
    /// The header value: `HIT`, `MISS` or `STALE`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "HIT",
            Self::Miss => "MISS",
            Self::Stale => "STALE",
        }
    }
}

/// Options for [`serve_asset`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ServeOptions {
    /// Adds `X-Rustryx-Cache` (and `Warning: 110` for stale).
    pub(crate) cache: Option<CacheStatus>,
    /// The read is a download of an artifact and may count (a `GET` that opens the body, by a
    /// client). Index documents never count: a group merge reads one per member, and a
    /// metadata read is not a pull of the component.
    pub(crate) count_download: bool,
}

/// Builds the response for an asset. The body is the blob stream, not a copy.
///
/// Cancel safe: nothing is held between awaits, and dropping the response body
/// drops the blob stream.
pub(crate) async fn serve_asset(
    cx: &FormatContext,
    head: &RequestHead,
    asset: &Asset,
    options: ServeOptions,
) -> Result<Response<Body>, FormatError> {
    let etag = etag_from_checksums(&asset.checksums);
    let last_modified = asset.updated_at;
    match evaluate_conditional(&head.method, &head.headers, &etag, last_modified) {
        Conditional::Proceed => {}
        Conditional::PreconditionFailed => return Err(FormatError::PreconditionFailed),
        Conditional::NotModified => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::NOT_MODIFIED;
            let headers = response.headers_mut();
            insert_text(headers, header::ETAG, &etag);
            insert_text(
                headers,
                header::LAST_MODIFIED,
                &format_http_date(last_modified),
            );
            apply_cache(headers, options.cache);
            return Ok(response);
        }
    }

    let total = asset.size_bytes;
    let decision = decide_range(&head.headers, total, &etag, last_modified);
    let (status, range, length) = match decision {
        RangeDecision::Full => (StatusCode::OK, None, total),
        RangeDecision::Partial(range) => {
            let end = range.end.unwrap_or(total.saturating_sub(1));
            (
                StatusCode::PARTIAL_CONTENT,
                Some(range),
                end - range.start + 1,
            )
        }
        RangeDecision::Unsatisfiable => return Err(FormatError::RangeNotSatisfiable { total }),
    };

    let body = if head.method == Method::HEAD {
        Body::empty()
    } else {
        open_body(cx, asset, range).await?
    };

    if options.count_download && head.method == Method::GET {
        record_download(cx, asset);
    }
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    insert_text(headers, header::CONTENT_TYPE, &asset.content_type);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    insert_text(headers, header::ETAG, &etag);
    insert_text(
        headers,
        header::LAST_MODIFIED,
        &format_http_date(last_modified),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    insert_text(
        headers,
        HeaderName::from_static("x-checksum-sha1"),
        &asset.checksums.sha1,
    );
    insert_text(
        headers,
        HeaderName::from_static("x-checksum-sha256"),
        &asset.checksums.sha256,
    );
    insert_text(
        headers,
        HeaderName::from_static("x-checksum-md5"),
        &asset.checksums.md5,
    );
    // Stored content is user content. nosniff and a sandboxing CSP keep a browser
    // from ever executing an uploaded html or svg file in this origin (SEC-35).
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    if let Some(range) = range {
        let end = range.end.unwrap_or(total.saturating_sub(1));
        insert_text(
            headers,
            header::CONTENT_RANGE,
            &format!("bytes {}-{end}/{total}", range.start),
        );
    }
    apply_cache(headers, options.cache);
    Ok(response)
}

/// Announces a client download. Scanner, replication and internal reads are not announced:
/// they must not bump counters or refresh the pull time, or scanning would keep an unused
/// artifact alive (SCN-21).
fn record_download(cx: &FormatContext, asset: &Asset) {
    let source = cx.download_source();
    if !source.counts_as_download() {
        return;
    }
    cx.ports().events.emit(ContentEvent::AssetDownloaded {
        repository: asset.repository_id,
        asset: asset.id,
        source,
        at: cx.now(),
    });
}

async fn open_body(
    cx: &FormatContext,
    asset: &Asset,
    range: Option<ByteRange>,
) -> Result<Body, FormatError> {
    let record = cx.ports().blobs.get(asset.blob_id).await?.ok_or_else(|| {
        FormatError::internal("asset references a blob record that does not exist")
    })?;
    // The store the blob row names, never the repository's: for a group store that is the member
    // that holds the bytes, so a read never probes members.
    let store = cx.store_for_blob(&record).await?;
    let key = rustryx_core::ports::BlobKey::new(record.key);
    match store.get(&key, range).await {
        Ok(read) => Ok(body_from_blob(read)),
        Err(BlobError::NotFound) => {
            // The database says the blob exists, the store says it does not: data loss
            // or a misconfigured store. That is our fault, not a missing artifact.
            Err(FormatError::internal(
                "blob object missing in the store for an existing asset",
            ))
        }
        Err(BlobError::RangeNotSatisfiable) => Err(FormatError::RangeNotSatisfiable {
            total: asset.size_bytes,
        }),
        Err(other) => Err(other.into()),
    }
}

fn apply_cache(headers: &mut http::HeaderMap, cache: Option<CacheStatus>) {
    let Some(status) = cache else { return };
    headers.insert(X_RUSTRYX_CACHE, HeaderValue::from_static(status.as_str()));
    if status == CacheStatus::Stale {
        headers.insert(
            header::WARNING,
            HeaderValue::from_static("110 - \"Response is stale\""),
        );
    }
}

fn insert_text(headers: &mut http::HeaderMap, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_status_labels() {
        assert_eq!(CacheStatus::Hit.as_str(), "HIT");
        assert_eq!(CacheStatus::Miss.as_str(), "MISS");
        assert_eq!(CacheStatus::Stale.as_str(), "STALE");
    }

    #[test]
    fn stale_adds_the_warning_header_and_hit_does_not() {
        let mut headers = http::HeaderMap::new();
        apply_cache(&mut headers, Some(CacheStatus::Stale));
        assert_eq!(headers.get(X_RUSTRYX_CACHE).unwrap(), "STALE");
        assert!(
            headers
                .get(header::WARNING)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("110 ")
        );
        let mut headers = http::HeaderMap::new();
        apply_cache(&mut headers, Some(CacheStatus::Hit));
        assert!(headers.get(header::WARNING).is_none());
        let mut headers = http::HeaderMap::new();
        apply_cache(&mut headers, None);
        assert!(headers.is_empty());
    }
}
