//! Proxy repositories: content is fetched from a remote repository and cached.
//!
//! Flow of a read that is not a fresh cache hit:
//!
//! ```text
//! request -> negative cache? -> singleflight join
//!   leader:   spawn fill task (upstream -> blob store [-> client tee]), wait for the head
//!   follower: wait for the leader's result
//! result -> serve from the blob store (or the tee), else stale copy, else error
//! ```
//!
//! Everything that makes this safe lives in the submodules: [`ssrf`] (what may be
//! requested), [`executor`] (timeouts, retries, breaker, concurrency cap),
//! [`flight`] (exactly one upstream request per key), [`negative`] (404 memory).
//! The upstream's 401 and 403 are never forwarded: they become 502 (the client
//! cannot fix our credentials), or 404 for upstreams that answer 403 for a missing
//! object when the format says so ([`ProxyFetch::forbidden_is_miss`]).

pub mod breaker;
pub mod executor;
mod fill;
pub mod flight;
pub mod negative;
pub mod ssrf;
mod target;
pub mod upstream;

use std::sync::Arc;
use std::time::Duration;

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::model::{Asset, ProxyConfig, RepoType};
use rustryx_core::ports::BlobKey;
use time::OffsetDateTime;
use tokio::sync::oneshot;
use url::Url;

pub use fill::{FillFailure, FillOutcome};
pub use target::UpstreamPath;

use self::executor::{CallClass, UpstreamCall};
use self::fill::{FillIntent, FillJob, FillPlan, TeeHead, tee_body, tee_headers};
use self::flight::{FlightKey, Join, WaitError};
use self::target::build_upstream_url;
use crate::checksum::{ChecksumSource, ExpectedChecksum};
use crate::conditional::{etag_from_checksums, format_http_date};
use crate::context::FormatContext;
use crate::error::{FormatError, UpstreamFailureKind};
use crate::hosted::{CacheStatus, ComponentSpec, ServeOptions, X_RUSTRYX_CACHE, serve_asset};
use crate::metrics;
use crate::request::ArtifactRequest;
use crate::stream::collect_bounded;

/// Attempts at becoming (or following) a leader before giving up. A second one is
/// only needed when the first leader vanished without a result (shutdown).
const JOIN_ATTEMPTS: usize = 2;

/// What a format asks of the proxy pipeline for one request.
#[derive(Clone)]
pub struct ProxyFetch<'a> {
    request: &'a ArtifactRequest,
    upstream_path: Option<UpstreamPath>,
    upstream_query: Option<String>,
    accept: Option<HeaderValue>,
    extra_headers: HeaderMap,
    cache_path: Option<String>,
    content_type: Option<String>,
    expected: Option<ExpectedChecksum>,
    checksum_source: Option<Arc<dyn ChecksumSource>>,
    component: Option<ComponentSpec>,
    forbidden_is_miss: bool,
}

impl<'a> ProxyFetch<'a> {
    /// Fetch of the request's own path from the upstream, cached under the same path.
    #[must_use]
    pub fn new(request: &'a ArtifactRequest) -> Self {
        Self {
            request,
            upstream_path: None,
            upstream_query: None,
            accept: None,
            extra_headers: HeaderMap::new(),
            cache_path: None,
            content_type: None,
            expected: None,
            checksum_source: None,
            component: None,
            forbidden_is_miss: false,
        }
    }

    /// Requests a different upstream path than the request path (default: the same).
    #[must_use]
    pub fn upstream_path(mut self, path: UpstreamPath) -> Self {
        self.upstream_path = Some(path);
        self
    }

    /// Query string for the upstream, already percent-encoded. Nothing from the
    /// client's query is forwarded unless the format passes it here.
    #[must_use]
    pub fn upstream_query(mut self, query: impl Into<String>) -> Self {
        self.upstream_query = Some(query.into());
        self
    }

    /// `Accept` header for the upstream (pub `application/vnd.pub.v2+json`, PEP 691).
    #[must_use]
    pub fn accept(mut self, accept: HeaderValue) -> Self {
        self.accept = Some(accept);
        self
    }

    /// Adds a header for the upstream. Only what the format passes is sent: client
    /// headers are never forwarded wholesale.
    #[must_use]
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        let mut value = value;
        // Anything that carries credentials must not show up in a `Debug` dump of the request.
        if name == header::AUTHORIZATION
            || name == header::COOKIE
            || name == header::PROXY_AUTHORIZATION
        {
            value.set_sensitive(true);
        }
        self.extra_headers.insert(name, value);
        self
    }

    /// Storage path of the cached copy (default: the request path). It must be
    /// derived deterministically from the request, never from upstream data, and
    /// must differ for variants of one URL that have different content (for
    /// example the same path with two `Accept` types).
    #[must_use]
    pub fn cache_path(mut self, path: impl Into<String>) -> Self {
        self.cache_path = Some(path.into());
        self
    }

    /// Content type to store when the upstream sends none.
    #[must_use]
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    /// A checksum known before the fetch (pub `archive_sha256`, npm integrity). The
    /// body is then verified before any byte reaches the client.
    #[must_use]
    pub fn expect(mut self, checksum: ExpectedChecksum) -> Self {
        self.expected = Some(checksum);
        self
    }

    /// Reads a checksum from the upstream's response headers; same verification rule.
    #[must_use]
    pub fn checksum_source(mut self, source: Arc<dyn ChecksumSource>) -> Self {
        self.checksum_source = Some(source);
        self
    }

    /// Component to create or reuse together with the cached asset.
    #[must_use]
    pub fn component(mut self, component: ComponentSpec) -> Self {
        self.component = Some(component);
        self
    }

    /// Treat an upstream 403 as "not found" (object stores answer 403 for missing keys).
    /// A 401 is never a miss.
    #[must_use]
    pub fn forbidden_is_miss(mut self) -> Self {
        self.forbidden_is_miss = true;
        self
    }
}

/// A cached index document, read into memory within the document size limit.
#[derive(Debug, Clone)]
pub struct CachedDocument {
    /// The document bytes as the upstream sent them.
    pub body: Bytes,
    /// Content type recorded at fill time.
    pub content_type: String,
    /// Whether this was a hit, a fresh fetch or a stale fallback.
    pub cache: CacheStatus,
    /// When the cached copy was written.
    pub last_modified: OffsetDateTime,
    /// Strong validator derived from the content hash.
    pub etag: String,
}

impl CachedDocument {
    /// Headers every index response should carry: `ETag`, `Last-Modified`, the cache
    /// marker and, for a stale copy, `Warning: 110`. Content type and length are the
    /// format's to set, because it may rewrite the body.
    #[must_use]
    pub fn response_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(&self.etag) {
            headers.insert(header::ETAG, value);
        }
        if let Ok(value) = HeaderValue::from_str(&format_http_date(self.last_modified)) {
            headers.insert(header::LAST_MODIFIED, value);
        }
        headers.insert(
            X_RUSTRYX_CACHE,
            HeaderValue::from_static(self.cache.as_str()),
        );
        if self.cache == CacheStatus::Stale {
            headers.insert(
                header::WARNING,
                HeaderValue::from_static("110 - \"Response is stale\""),
            );
        }
        headers
    }
}

/// Fetches from the upstream of the context's proxy repository.
pub struct ProxyPipeline<'a> {
    cx: &'a FormatContext,
}

/// Outcome of the shared resolution step.
enum Resolved {
    /// A cached asset to serve.
    Asset(Box<Asset>, CacheStatus),
    /// A live tee of the leader's fill.
    Tee(TeeHead),
}

/// Request-independent facts computed once per fetch.
struct Prepared {
    config: ProxyConfig,
    cache_path: String,
    url: Url,
    headers: HeaderMap,
    ttl: Duration,
}

impl<'a> ProxyPipeline<'a> {
    pub(crate) fn new(cx: &'a FormatContext) -> Self {
        Self { cx }
    }

    /// Serves an artifact: from the cache when fresh, otherwise by filling the cache
    /// from the upstream. The first request of a burst receives the body streamed
    /// while it is written to the blob store (a tee); concurrent requests for the
    /// same key wait for that fill and then read the cached copy, so N concurrent
    /// misses cause one upstream request.
    ///
    /// When the fill needs the bytes verified first (an expected checksum is known),
    /// nothing is streamed until the copy is complete and verified. If the upstream
    /// is unavailable and an expired copy exists, that copy is served with
    /// `X-Rustryx-Cache: STALE` and `Warning: 110`.
    ///
    /// A `HEAD` that misses the cache is answered from an upstream `HEAD` without
    /// filling the cache. A `Range` header on a miss is ignored (the full body is
    /// sent with 200), which RFC 9110 permits.
    ///
    /// Cancel safe: the fill runs in its own tracked task. Dropping this future (the
    /// client left) does not stop the fill, and the singleflight key is released
    /// when the fill ends.
    ///
    /// # Errors
    /// `NotFound`, an upstream failure mapped to 502/504, `Unavailable` (503) when the
    /// concurrency cap is exhausted, store errors. Never 401 or 403 from the upstream.
    pub async fn fetch_artifact(
        &self,
        fetch: ProxyFetch<'_>,
    ) -> Result<Response<Body>, FormatError> {
        let prepared = self.prepare(&fetch, CallClass::Data)?;
        let head = &fetch.request.head;
        if head.method == Method::HEAD {
            return self.head_response(&fetch, &prepared).await;
        }
        match self
            .resolve(&fetch, &prepared, CallClass::Data, true)
            .await?
        {
            Resolved::Asset(asset, status) => {
                serve_asset(
                    self.cx,
                    head,
                    &asset,
                    ServeOptions {
                        cache: Some(status),
                    },
                )
                .await
            }
            Resolved::Tee(tee) => Ok(tee_response(tee)),
        }
    }

    /// Fetches an index document into memory (bounded by `Limits::max_document_bytes`),
    /// through the same cache, singleflight and stale logic as artifacts but with the
    /// metadata TTL and the 30 s total budget. The format typically rewrites URLs in
    /// the result before serving it.
    ///
    /// Cancel safe, see [`ProxyPipeline::fetch_artifact`].
    ///
    /// # Errors
    /// As [`ProxyPipeline::fetch_artifact`], plus `Upstream` when the stored document
    /// exceeds the size limit.
    pub async fn fetch_document(
        &self,
        fetch: ProxyFetch<'_>,
    ) -> Result<CachedDocument, FormatError> {
        let prepared = self.prepare(&fetch, CallClass::Metadata)?;
        match self
            .resolve(&fetch, &prepared, CallClass::Metadata, false)
            .await?
        {
            Resolved::Asset(asset, cache) => self.read_document(&asset, cache).await,
            Resolved::Tee(_) => Err(FormatError::internal(
                "a tee was produced for a document fetch",
            )),
        }
    }

    fn prepare(&self, fetch: &ProxyFetch<'_>, class: CallClass) -> Result<Prepared, FormatError> {
        let repository = self.cx.repository();
        if repository.repo_type != RepoType::Proxy {
            return Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" });
        }
        let config = repository
            .proxy
            .clone()
            .ok_or_else(|| FormatError::internal("proxy repository without proxy settings"))?;
        let base = Url::parse(&config.remote_url).map_err(|_| {
            FormatError::upstream(
                UpstreamFailureKind::Blocked,
                "remote_url is not a valid url",
            )
        })?;
        let path = fetch
            .upstream_path
            .clone()
            .unwrap_or_else(|| UpstreamPath::Decoded(fetch.request.path.clone()));
        let url = build_upstream_url(&base, &path, fetch.upstream_query.as_deref())?;
        let mut headers = fetch.extra_headers.clone();
        if let Some(accept) = &fetch.accept {
            headers.insert(header::ACCEPT, accept.clone());
        }
        let ttl_secs = match class {
            CallClass::Data => config.content_max_age_secs,
            CallClass::Metadata => config.metadata_max_age_secs,
        };
        Ok(Prepared {
            cache_path: fetch
                .cache_path
                .clone()
                .unwrap_or_else(|| fetch.request.path.as_str().to_owned()),
            config,
            url,
            headers,
            ttl: Duration::from_secs(ttl_secs),
        })
    }

    fn is_fresh(&self, asset: &Asset, ttl: Duration) -> bool {
        age(self.cx.now(), asset) < ttl
    }

    fn is_servable_stale(&self, asset: &Asset) -> bool {
        age(self.cx.now(), asset) <= self.cx.limits().stale_max_age
    }

    async fn resolve(
        &self,
        fetch: &ProxyFetch<'_>,
        prepared: &Prepared,
        class: CallClass,
        allow_tee: bool,
    ) -> Result<Resolved, FormatError> {
        let cx = self.cx;
        let repository = cx.repository();
        let format = repository.format;
        let existing = cx
            .ports()
            .assets
            .get_by_path(repository.id, &prepared.cache_path)
            .await?;
        if let Some(asset) = &existing
            && self.is_fresh(asset, prepared.ttl)
        {
            metrics::cache_result(format, "hit");
            return Ok(Resolved::Asset(Box::new(asset.clone()), CacheStatus::Hit));
        }
        if cx
            .shared
            .negative
            .contains(repository.id, &prepared.cache_path)
            .await
        {
            metrics::cache_result(format, "negative");
            return Err(FormatError::NotFound);
        }

        let key = FlightKey {
            repository: repository.id,
            path: prepared.cache_path.clone(),
        };
        for attempt in 1..=JOIN_ATTEMPTS {
            let (follower, tee_rx) = match cx.shared.flights.join(key.clone()) {
                Join::Follower(follower) => (follower, None),
                Join::Leader(guard) => {
                    let follower = guard.follower();
                    let (tee_tx, tee_rx) = if allow_tee && fetch.expected.is_none() {
                        let (tx, rx) = oneshot::channel();
                        (Some(tx), Some(rx))
                    } else {
                        (None, None)
                    };
                    let plan = Self::plan(fetch, prepared, class, existing.as_ref());
                    let job = FillJob {
                        cx: cx.clone(),
                        guard,
                        plan,
                        tee: tee_tx,
                    };
                    // Tracked, detached from this request: see the module docs of `fill`.
                    drop(cx.tasks().spawn(job.run()));
                    (follower, tee_rx)
                }
            };
            if let Some(rx) = tee_rx {
                // The head is sent once the upstream has answered with a usable status.
                // If the fill ends without one, the sender is dropped and we fall
                // through to waiting for the outcome.
                if let Ok(head) = rx.await {
                    metrics::cache_result(format, "miss");
                    return Ok(Resolved::Tee(head));
                }
            }
            match follower.wait(cx.limits().singleflight_max_wait).await {
                Ok(FillOutcome::Cached) => {
                    return self.after_cached_fill(&prepared.cache_path).await;
                }
                Ok(FillOutcome::NotFound) => {
                    metrics::cache_result(format, "negative");
                    return Err(FormatError::NotFound);
                }
                Ok(FillOutcome::Failed(failure)) => {
                    return self.fallback_to_stale(existing, failure);
                }
                Err(WaitError::Aborted) if attempt < JOIN_ATTEMPTS => {}
                Err(WaitError::Aborted) => {
                    return self.fallback_to_stale(existing, FillFailure::Shutdown);
                }
                Err(WaitError::Timeout) => {
                    return Err(FormatError::Unavailable {
                        reason: "the upstream fetch is taking too long, retry",
                        retry_after: Some(Duration::from_secs(5)),
                    });
                }
            }
        }
        Err(FormatError::Unavailable {
            reason: "the upstream fetch did not complete, retry",
            retry_after: Some(Duration::from_secs(2)),
        })
    }

    async fn after_cached_fill(&self, cache_path: &str) -> Result<Resolved, FormatError> {
        let repository = self.cx.repository();
        metrics::cache_result(repository.format, "miss");
        match self
            .cx
            .ports()
            .assets
            .get_by_path(repository.id, cache_path)
            .await?
        {
            Some(asset) => Ok(Resolved::Asset(Box::new(asset), CacheStatus::Miss)),
            // Removed between the fill and now (cleanup, delete). The client may simply retry.
            None => Err(FormatError::Unavailable {
                reason: "the cached copy disappeared, retry",
                retry_after: Some(Duration::from_secs(1)),
            }),
        }
    }

    fn fallback_to_stale(
        &self,
        existing: Option<Asset>,
        failure: FillFailure,
    ) -> Result<Resolved, FormatError> {
        let format = self.cx.repository().format;
        match existing {
            Some(asset) if self.is_servable_stale(&asset) => {
                metrics::cache_result(format, "stale");
                Ok(Resolved::Asset(Box::new(asset), CacheStatus::Stale))
            }
            _ => Err(failure.into_error()),
        }
    }

    fn plan(
        fetch: &ProxyFetch<'_>,
        prepared: &Prepared,
        class: CallClass,
        existing: Option<&Asset>,
    ) -> FillPlan {
        FillPlan {
            cache_path: prepared.cache_path.clone(),
            url: prepared.url.clone(),
            headers: prepared.headers.clone(),
            class,
            max_concurrent: prepared.config.max_concurrent_upstream,
            credentials: prepared.config.auth.clone(),
            content_type_fallback: fetch.content_type.clone(),
            expected: fetch.expected.clone(),
            checksum_source: fetch.checksum_source.clone(),
            component: fetch.component.clone(),
            forbidden_is_miss: fetch.forbidden_is_miss,
            intent: if existing.is_some() {
                FillIntent::Refresh
            } else {
                FillIntent::Fill
            },
            existing_size: existing.map_or(0, |a| a.size_bytes),
            negative_ttl: Duration::from_secs(prepared.config.negative_cache_ttl_secs),
        }
    }

    async fn read_document(
        &self,
        asset: &Asset,
        cache: CacheStatus,
    ) -> Result<CachedDocument, FormatError> {
        let limit = self.cx.limits().max_document_bytes;
        if asset.size_bytes > limit {
            return Err(FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                format!(
                    "cached document of {} bytes exceeds the {limit} byte limit",
                    asset.size_bytes
                ),
            ));
        }
        let record = self
            .cx
            .ports()
            .blobs
            .get(asset.blob_id)
            .await?
            .ok_or_else(|| {
                FormatError::internal("asset references a blob record that does not exist")
            })?;
        let store = self.cx.store()?;
        let read = store.get(&BlobKey::new(record.key), None).await?;
        let body = collect_bounded(read.stream, limit)
            .await
            .map_err(|e| FormatError::internal(format!("reading cached document: {e}")))?;
        Ok(CachedDocument {
            body,
            content_type: asset.content_type.clone(),
            cache,
            last_modified: asset.updated_at,
            etag: etag_from_checksums(&asset.checksums),
        })
    }

    /// Answers `HEAD` on a miss from an upstream `HEAD`, without touching the cache.
    async fn head_response(
        &self,
        fetch: &ProxyFetch<'_>,
        prepared: &Prepared,
    ) -> Result<Response<Body>, FormatError> {
        let cx = self.cx;
        let repository = cx.repository();
        if let Some(asset) = cx
            .ports()
            .assets
            .get_by_path(repository.id, &prepared.cache_path)
            .await?
        {
            let status = if self.is_fresh(&asset, prepared.ttl) {
                CacheStatus::Hit
            } else {
                CacheStatus::Stale
            };
            return serve_asset(
                cx,
                &fetch.request.head,
                &asset,
                ServeOptions {
                    cache: Some(status),
                },
            )
            .await;
        }
        if cx
            .shared
            .negative
            .contains(repository.id, &prepared.cache_path)
            .await
        {
            return Err(FormatError::NotFound);
        }
        let call = UpstreamCall {
            repository: repository.id,
            repo_name: repository.name.to_string(),
            method: Method::HEAD,
            url: prepared.url.clone(),
            headers: prepared.headers.clone(),
            credentials: prepared.config.auth.clone(),
            max_concurrent: prepared.config.max_concurrent_upstream,
            class: CallClass::Metadata,
        };
        let result = cx
            .shared
            .upstream
            .execute(call)
            .await
            .map_err(|error| FormatError::upstream(error.kind(), error.to_string()))?;
        match result.status.as_u16() {
            200 => {
                let mut response = Response::new(Body::empty());
                let headers = response.headers_mut();
                for name in [
                    header::CONTENT_TYPE,
                    header::CONTENT_LENGTH,
                    header::ETAG,
                    header::LAST_MODIFIED,
                ] {
                    if let Some(value) = result.headers.get(&name) {
                        headers.insert(name, value.clone());
                    }
                }
                headers.insert(
                    X_RUSTRYX_CACHE,
                    HeaderValue::from_static(CacheStatus::Miss.as_str()),
                );
                Ok(response)
            }
            404 | 410 => Err(FormatError::NotFound),
            403 if fetch.forbidden_is_miss => Err(FormatError::NotFound),
            401 | 403 => Err(FormatError::upstream(
                UpstreamFailureKind::Rejected,
                format!("upstream answered {}", result.status),
            )),
            _ => Err(FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                format!("upstream answered {}", result.status),
            )),
        }
    }
}

fn age(now: OffsetDateTime, asset: &Asset) -> Duration {
    // A copy "from the future" (clock skew between nodes) counts as brand new, not as expired.
    Duration::try_from(now - asset.updated_at).unwrap_or(Duration::ZERO)
}

fn tee_response(tee: TeeHead) -> Response<Body> {
    let headers = tee_headers(&tee);
    let mut response = Response::new(tee_body(tee.rx));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().extend(headers);
    let headers = response.headers_mut();
    headers.insert(
        X_RUSTRYX_CACHE,
        HeaderValue::from_static(CacheStatus::Miss.as_str()),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    response
}
