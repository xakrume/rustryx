//! The cache fill: one upstream response written to the blob store and, when a
//! client is waiting and nothing needs verifying first, tee'd to that client.
//!
//! A fill runs in its own tracked task. It is **not** part of any request future:
//!
//! * if the leader's client disconnects, the fill goes on (README 8.1) and the
//!   artifact still lands in the cache for the next request;
//! * the singleflight key is held by the task, so it disappears exactly when the
//!   fill is over, whatever the way it ended;
//! * shutdown cancels the streaming phase (the staged blob is aborted); the commit
//!   and publish phase is never cut in half.
//!
//! Backpressure follows the slowest consumer: a chunk is written to the blob store
//! and handed to the client through a bounded channel before the next chunk is read
//! from the upstream. A client that stalls for longer than the stall timeout, or has
//! left, is dropped from the tee and the fill continues alone.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use rustryx_core::ids::BlobId;
use rustryx_core::model::{Asset, BlobRecord, NewAsset, NewComponent};
use rustryx_core::ports::BlobKey;
use tokio::sync::{mpsc, oneshot};
use url::Url;

use super::executor::{CallClass, UpstreamCall};
use super::flight::LeaderGuard;
use super::upstream::UpstreamError;
use crate::blobio::{BlobSink, SinkError, delete_orphan};
use crate::checksum::{ChecksumSource, ExpectedChecksum};
use crate::context::{ContentEvent, FormatContext, OrphanCause};
use crate::error::{FormatError, UpstreamFailureKind};
use crate::hosted::ComponentSpec;
use crate::hosted::quota;
use crate::metrics;
use crate::mime::{guess_content_type, resolve_content_type, well_formed};

/// Capacity of the tee channel. Two chunks: enough to overlap the blob write of one
/// chunk with the client write of the previous, small enough that memory stays at
/// a couple of chunks per fill.
const TEE_CAPACITY: usize = 2;

/// Why a fill produced no cached copy.
#[derive(Debug, Clone)]
pub enum FillFailure {
    /// The upstream failed or answered something unusable.
    Upstream {
        /// Coarse class.
        kind: UpstreamFailureKind,
        /// Log-only detail.
        detail: String,
    },
    /// The per-repository concurrency cap did not free up in time.
    Overloaded,
    /// The content is larger than the allowed size.
    TooLarge {
        /// The limit that was crossed.
        limit: u64,
    },
    /// The repository or blob store quota has no room.
    Quota,
    /// The blob store or the metadata store failed.
    Storage(String),
    /// The node is shutting down.
    Shutdown,
    /// The client got the data but it could not be cached (cache degraded).
    NotCached,
}

impl FillFailure {
    /// The error a request gets when no stale copy can be served instead.
    #[must_use]
    pub fn into_error(self) -> FormatError {
        match self {
            Self::Upstream { kind, detail } => FormatError::Upstream { kind, detail },
            Self::Overloaded => FormatError::Unavailable {
                reason: "too many concurrent requests to the upstream repository",
                retry_after: Some(Duration::from_secs(5)),
            },
            Self::TooLarge { limit } => FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                format!("upstream content exceeds {limit} bytes"),
            ),
            Self::Quota => FormatError::QuotaExceeded,
            Self::Storage(detail) => FormatError::Internal(detail),
            Self::Shutdown | Self::NotCached => FormatError::Unavailable {
                reason: "the content could not be cached, retry",
                retry_after: Some(Duration::from_secs(2)),
            },
        }
    }
}

/// Result of a fill, shared with every waiter.
#[derive(Debug, Clone)]
pub enum FillOutcome {
    /// The content is cached and can be read from the store.
    Cached,
    /// The upstream answered 404 (or 410, or a 403 treated as a miss).
    NotFound,
    /// No cached copy was produced.
    Failed(FillFailure),
}

/// Whether the fill creates a new cache entry or refreshes an expired one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FillIntent {
    /// No asset existed. The fill must not replace one that appears meanwhile.
    Fill,
    /// An expired asset exists and is meant to be replaced.
    Refresh,
}

/// Everything the fill needs, decided before the task is spawned.
pub(crate) struct FillPlan {
    pub(crate) cache_path: String,
    pub(crate) url: Url,
    pub(crate) headers: HeaderMap,
    pub(crate) class: CallClass,
    pub(crate) max_concurrent: u32,
    pub(crate) credentials: Option<rustryx_core::model::UpstreamAuth>,
    pub(crate) content_type_fallback: Option<String>,
    pub(crate) expected: Option<ExpectedChecksum>,
    pub(crate) checksum_source: Option<Arc<dyn ChecksumSource>>,
    pub(crate) component: Option<ComponentSpec>,
    pub(crate) forbidden_is_miss: bool,
    pub(crate) intent: FillIntent,
    pub(crate) existing_size: u64,
    pub(crate) negative_ttl: Duration,
}

/// Message on the tee channel.
pub(crate) enum TeeMsg {
    Data(Bytes),
    /// The upstream body ended and had the declared length.
    Eof,
}

/// What the leader's request gets to build its streaming response from.
pub(crate) struct TeeHead {
    pub(crate) content_type: String,
    pub(crate) content_length: Option<u64>,
    pub(crate) rx: mpsc::Receiver<TeeMsg>,
}

/// The task body of one fill.
pub(crate) struct FillJob {
    pub(crate) cx: FormatContext,
    pub(crate) guard: LeaderGuard<FillOutcome>,
    pub(crate) plan: FillPlan,
    pub(crate) tee: Option<oneshot::Sender<TeeHead>>,
}

impl FillJob {
    pub(crate) async fn run(self) {
        let Self {
            cx,
            guard,
            plan,
            tee,
        } = self;
        let outcome = fill(&cx, &plan, tee).await;
        match &outcome {
            FillOutcome::Cached => {
                cx.shared
                    .negative
                    .remove(cx.repository().id, &plan.cache_path)
                    .await;
            }
            FillOutcome::NotFound => {
                cx.shared
                    .negative
                    .insert(cx.repository().id, &plan.cache_path, plan.negative_ttl)
                    .await;
            }
            FillOutcome::Failed(_) => {}
        }
        guard.complete(outcome);
    }
}

/// The tee's view of the client: bounded channel plus the stall timeout.
struct Client {
    tx: mpsc::Sender<TeeMsg>,
    stall: Duration,
}

impl Client {
    /// Delivers a message. False means the client is gone or stalled.
    async fn send(&self, msg: TeeMsg) -> bool {
        matches!(
            tokio::time::timeout(self.stall, self.tx.send(msg)).await,
            Ok(Ok(()))
        )
    }
}

fn failure_from_upstream(error: &UpstreamError) -> FillFailure {
    match error {
        UpstreamError::Overloaded => FillFailure::Overloaded,
        other => FillFailure::Upstream {
            kind: other.kind(),
            detail: other.to_string(),
        },
    }
}

enum StatusClass {
    Success,
    Missing,
    Failure(FillFailure),
}

fn classify_status(status: StatusCode, forbidden_is_miss: bool) -> StatusClass {
    match status.as_u16() {
        200 => StatusClass::Success,
        404 | 410 => StatusClass::Missing,
        403 if forbidden_is_miss => StatusClass::Missing,
        401 | 403 => StatusClass::Failure(FillFailure::Upstream {
            kind: UpstreamFailureKind::Rejected,
            detail: format!("upstream answered {status}"),
        }),
        _ => StatusClass::Failure(FillFailure::Upstream {
            kind: UpstreamFailureKind::BadStatus,
            detail: format!("upstream answered {status}"),
        }),
    }
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

fn stored_content_type(plan: &FillPlan, headers: &HeaderMap) -> String {
    let upstream = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let fallback = plan.content_type_fallback.as_deref();
    match plan.class {
        // Index documents are served with a type the format chooses; keep what the
        // upstream said, as long as it is syntactically sound.
        CallClass::Metadata => upstream
            .and_then(well_formed)
            .or_else(|| fallback.map(str::to_owned))
            .unwrap_or_else(|| guess_content_type(&plan.cache_path).to_owned()),
        // Artifacts are served as stored, so an upstream claim of html or svg is not trusted.
        CallClass::Data => resolve_content_type(upstream.or(fallback), &plan.cache_path),
    }
}

/// Runs one fill to its outcome. Never panics, never leaves a staged blob behind.
#[allow(
    clippy::too_many_lines,
    reason = "one linear pipeline; splitting it would scatter the abort and tee invariants"
)]
async fn fill(
    cx: &FormatContext,
    plan: &FillPlan,
    tee: Option<oneshot::Sender<TeeHead>>,
) -> FillOutcome {
    let shutdown = cx.shutdown_token();
    let repository = cx.repository();

    let budget = match quota::budget(cx, plan.existing_size).await {
        Ok(budget) => budget,
        Err(error) => return FillOutcome::Failed(FillFailure::Storage(error.to_string())),
    };
    let limit = match plan.class {
        CallClass::Metadata => budget.bytes.min(cx.limits().max_document_bytes),
        CallClass::Data => budget.bytes,
    };

    let call = UpstreamCall {
        repository: repository.id,
        repo_name: repository.name.to_string(),
        method: http::Method::GET,
        url: plan.url.clone(),
        headers: plan.headers.clone(),
        credentials: plan.credentials.clone(),
        max_concurrent: plan.max_concurrent,
        class: plan.class,
    };
    let response = tokio::select! {
        biased;
        () = shutdown.cancelled() => return FillOutcome::Failed(FillFailure::Shutdown),
        result = cx.shared.upstream.execute(call) => match result {
            Ok(response) => response,
            Err(error) => {
                if matches!(error, UpstreamError::Blocked(_)) {
                    audit_blocked(cx, plan, &error).await;
                }
                return FillOutcome::Failed(failure_from_upstream(&error));
            }
        },
    };

    match classify_status(response.status, plan.forbidden_is_miss) {
        StatusClass::Success => {}
        StatusClass::Missing => return FillOutcome::NotFound,
        StatusClass::Failure(failure) => return FillOutcome::Failed(failure),
    }

    let declared = declared_length(&response.headers);
    let expected = plan.expected.clone().or_else(|| {
        plan.checksum_source
            .as_ref()
            .and_then(|source| source.expected_from_headers(&response.headers))
    });
    let content_type = stored_content_type(plan, &response.headers);

    // Tee only when nobody needs to see the bytes verified first, and only while
    // the leader is still there to receive them.
    let mut client = match (tee, expected.is_none()) {
        (Some(head_tx), true) => open_client(cx, head_tx, &content_type, declared),
        _ => None,
    };

    // The cache is optional while a client is being served (a cache that cannot take
    // the data must not break the download) and mandatory otherwise.
    let too_large = declared.is_some_and(|d| d > limit);
    let mut sink = if too_large {
        None
    } else {
        open_sink(cx, declared, limit).await.ok()
    };
    if sink.is_none() && client.is_none() {
        return FillOutcome::Failed(if too_large {
            large_failure(limit, &budget)
        } else {
            FillFailure::Storage("could not open the blob store".into())
        });
    }

    let mut body = response.body;
    let mut received: u64 = 0;
    loop {
        let item = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                abort_sink(sink.take()).await;
                return FillOutcome::Failed(FillFailure::Shutdown);
            }
            item = body.next() => item,
        };
        let Some(item) = item else { break };
        let chunk = match item {
            Ok(chunk) => chunk,
            Err(error) => {
                abort_sink(sink.take()).await;
                // No Eof on purpose: the client sees its stream break instead of a
                // clean, truncated end.
                drop(client.take());
                return FillOutcome::Failed(failure_from_upstream(&error));
            }
        };
        received = received.saturating_add(chunk.len() as u64);
        if let Some(active) = sink.as_mut()
            && let Err(error) = active.write(chunk.clone()).await
        {
            abort_sink(sink.take()).await;
            if client.is_none() {
                return FillOutcome::Failed(sink_failure(error, &budget));
            }
        }
        if let Some(active) = client.as_ref()
            && !active.send(TeeMsg::Data(chunk)).await
        {
            metrics::count(metrics::PROXY_CLIENT_DETACHED_TOTAL);
            client = None;
        }
        if sink.is_none() && client.is_none() {
            // Nobody left to serve and nowhere to store: stop pulling from the upstream.
            return FillOutcome::Failed(FillFailure::NotCached);
        }
    }

    if declared.is_some_and(|d| d != received) {
        abort_sink(sink.take()).await;
        drop(client.take());
        return FillOutcome::Failed(FillFailure::Upstream {
            kind: UpstreamFailureKind::Integrity,
            detail: "body length differs from Content-Length".into(),
        });
    }
    if let Some(active) = client.take() {
        // A client that stalls right at the end is simply dropped; the fill is done anyway.
        let _ = active.send(TeeMsg::Eof).await;
    }

    match sink {
        Some(sink) => finish(cx, plan, sink, expected, &content_type).await,
        None => FillOutcome::Failed(FillFailure::NotCached),
    }
}

fn large_failure(limit: u64, budget: &quota::Budget) -> FillFailure {
    match budget.limited_by {
        quota::LimitedBy::Quota => FillFailure::Quota,
        quota::LimitedBy::MaxBlob => FillFailure::TooLarge { limit },
    }
}

fn sink_failure(error: SinkError, budget: &quota::Budget) -> FillFailure {
    match error {
        SinkError::LimitExceeded { limit } => large_failure(limit, budget),
        SinkError::Blob(e) => FillFailure::Storage(e.to_string()),
    }
}

async fn open_sink(
    cx: &FormatContext,
    declared: Option<u64>,
    limit: u64,
) -> Result<BlobSink, FormatError> {
    let store = cx.store()?;
    let key = BlobKey::new(cx.ports().ids.next_uuid().to_string());
    Ok(BlobSink::begin(store.as_ref(), key, declared, limit).await?)
}

async fn abort_sink(sink: Option<BlobSink>) {
    if let Some(sink) = sink {
        sink.abort().await;
    }
}

/// Sends the head to the leader and returns the tee side, or `None` when the leader is gone.
fn open_client(
    cx: &FormatContext,
    head_tx: oneshot::Sender<TeeHead>,
    content_type: &str,
    declared: Option<u64>,
) -> Option<Client> {
    let (tx, rx) = mpsc::channel(TEE_CAPACITY);
    let head = TeeHead {
        content_type: content_type.to_owned(),
        content_length: declared,
        rx,
    };
    head_tx.send(head).ok()?;
    Some(Client {
        tx,
        stall: cx.limits().client_stall_timeout,
    })
}

/// Commit, verify, publish. Not cancelled by shutdown: half of it would orphan a blob.
async fn finish(
    cx: &FormatContext,
    plan: &FillPlan,
    sink: BlobSink,
    expected: Option<ExpectedChecksum>,
    content_type: &str,
) -> FillOutcome {
    let repository = cx.repository();
    let key = sink.key().clone();
    let store = match cx.store() {
        Ok(store) => store,
        Err(error) => {
            sink.abort().await;
            return FillOutcome::Failed(FillFailure::Storage(error.to_string()));
        }
    };
    let commit = match sink.commit().await {
        Ok(commit) => commit,
        Err(error) => return FillOutcome::Failed(FillFailure::Storage(error.to_string())),
    };
    if let Some(expected) = &expected
        && !expected.matches(&commit.checksums)
    {
        delete_orphan(store.as_ref(), &key).await;
        cx.audit_content(
            "proxy.checksum_mismatch",
            format!("{}:{}", repository.name, plan.cache_path),
            "failure",
            serde_json::json!({ "algorithm": format!("{:?}", expected.algo()) }),
        )
        .await;
        return FillOutcome::Failed(FillFailure::Upstream {
            kind: UpstreamFailureKind::Integrity,
            detail: "checksum of the upstream content does not match".into(),
        });
    }

    // A fill must never replace an artifact someone else published while it ran.
    if plan.intent == FillIntent::Fill {
        match cx
            .ports()
            .assets
            .get_by_path(repository.id, &plan.cache_path)
            .await
        {
            Ok(Some(_already_there)) => {
                delete_orphan(store.as_ref(), &key).await;
                return FillOutcome::Cached;
            }
            Ok(None) => {}
            Err(error) => {
                delete_orphan(store.as_ref(), &key).await;
                return FillOutcome::Failed(FillFailure::Storage(error.to_string()));
            }
        }
    }

    match publish(cx, plan, &commit, &key, content_type).await {
        Ok(asset) => {
            cx.ports().events.emit(ContentEvent::AssetPublished {
                repository: repository.id,
                asset: asset.0,
                replaced: asset.1.is_some(),
            });
            if let Some(blob) = asset.1 {
                cx.ports().events.emit(ContentEvent::BlobOrphaned {
                    repository: repository.id,
                    blob,
                    cause: OrphanCause::Replaced,
                });
            }
            FillOutcome::Cached
        }
        Err(error) => {
            delete_orphan(store.as_ref(), &key).await;
            FillOutcome::Failed(FillFailure::Storage(error.to_string()))
        }
    }
}

async fn publish(
    cx: &FormatContext,
    plan: &FillPlan,
    commit: &rustryx_core::ports::BlobCommit,
    key: &BlobKey,
    content_type: &str,
) -> Result<(Asset, Option<BlobRecord>), FormatError> {
    let repository = cx.repository();
    let component_id = match &plan.component {
        Some(component) => Some(
            cx.ports()
                .components
                .find_or_create(&NewComponent {
                    repository_id: repository.id,
                    format: repository.format,
                    group_id: component.group.clone(),
                    name: component.name.clone(),
                    version: component.version.clone(),
                })
                .await?
                .id,
        ),
        None => None,
    };
    let id = BlobId::from_uuid(uuid_from_key(key).unwrap_or_else(|| cx.ports().ids.next_uuid()));
    let new = NewAsset {
        repository_id: repository.id,
        component_id,
        path: plan.cache_path.clone(),
        content_type: content_type.to_owned(),
        blob: BlobRecord {
            id,
            blob_store_id: repository.blob_store_id,
            key: key.as_str().to_owned(),
            size_bytes: commit.size_bytes,
            checksums: commit.checksums.clone(),
            created_at: cx.now(),
        },
    };
    let outcome = cx.ports().assets.publish(&new).await?;
    Ok((outcome.asset, outcome.replaced))
}

fn uuid_from_key(key: &BlobKey) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(key.as_str()).ok()
}

async fn audit_blocked(cx: &FormatContext, plan: &FillPlan, error: &UpstreamError) {
    // One record per minute and repository: a misconfigured remote_url is hit by
    // every request, and an audit flood would hide real events.
    let repository = cx.repository();
    if cx
        .shared
        .negative
        .contains(repository.id, "\u{0}ssrf-audit")
        .await
    {
        return;
    }
    cx.shared
        .negative
        .insert(repository.id, "\u{0}ssrf-audit", Duration::from_secs(60))
        .await;
    tracing::warn!(repository = %repository.name, %error, "upstream request refused by the url policy");
    cx.audit_content(
        "proxy.ssrf_blocked",
        format!("{}:{}", repository.name, plan.cache_path),
        "failure",
        serde_json::json!({ "reason": error.to_string() }),
    )
    .await;
}

/// Turns the tee receiver into a response body. Dropping the receiver (client gone)
/// is what tells the fill to stop feeding it.
pub(crate) fn tee_body(rx: mpsc::Receiver<TeeMsg>) -> axum_core::body::Body {
    let stream = futures::stream::unfold(Some(rx), |state| async move {
        let mut rx = state?;
        match rx.recv().await {
            Some(TeeMsg::Data(bytes)) => Some((Ok::<_, io::Error>(bytes), Some(rx))),
            Some(TeeMsg::Eof) => None,
            // The fill ended without Eof: truncated or aborted. Fail the stream loudly.
            None => Some((Err(io::Error::other("cache fill aborted")), None)),
        }
    });
    axum_core::body::Body::from_stream(stream)
}

/// Headers of a tee'd response.
pub(crate) fn tee_headers(head: &TeeHead) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&head.content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Some(length) = head.content_length {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification() {
        assert!(matches!(
            classify_status(StatusCode::OK, false),
            StatusClass::Success
        ));
        for code in [404u16, 410] {
            assert!(matches!(
                classify_status(StatusCode::from_u16(code).unwrap(), false),
                StatusClass::Missing
            ));
        }
        assert!(matches!(
            classify_status(StatusCode::FORBIDDEN, true),
            StatusClass::Missing
        ));
        for (code, kind) in [
            (401u16, UpstreamFailureKind::Rejected),
            (403, UpstreamFailureKind::Rejected),
            (500, UpstreamFailureKind::BadStatus),
            (503, UpstreamFailureKind::BadStatus),
            (206, UpstreamFailureKind::BadStatus),
            (304, UpstreamFailureKind::BadStatus),
            (400, UpstreamFailureKind::BadStatus),
            (204, UpstreamFailureKind::BadStatus),
        ] {
            match classify_status(StatusCode::from_u16(code).unwrap(), false) {
                StatusClass::Failure(FillFailure::Upstream { kind: got, .. }) => {
                    assert_eq!(got, kind, "{code}");
                }
                _ => panic!("{code} must be a failure"),
            }
        }
        // 401 stays a failure even when 403 is configured as a miss.
        assert!(matches!(
            classify_status(StatusCode::UNAUTHORIZED, true),
            StatusClass::Failure(_)
        ));
    }

    #[test]
    fn must_not_turn_upstream_auth_failures_into_client_auth_errors() {
        let error = FillFailure::Upstream {
            kind: UpstreamFailureKind::Rejected,
            detail: "401".into(),
        }
        .into_error();
        assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn failures_map_to_errors() {
        assert_eq!(
            FillFailure::Overloaded.into_error().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(FillFailure::Overloaded.into_error().retry_after().is_some());
        assert_eq!(
            FillFailure::TooLarge { limit: 1 }.into_error().status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            FillFailure::Quota.into_error().status(),
            StatusCode::INSUFFICIENT_STORAGE
        );
        assert_eq!(
            FillFailure::Storage("x".into()).into_error().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            FillFailure::Shutdown.into_error().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            FillFailure::NotCached.into_error().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn declared_length_parsing() {
        let mut h = HeaderMap::new();
        assert_eq!(declared_length(&h), None);
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("123"));
        assert_eq!(declared_length(&h), Some(123));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("x"));
        assert_eq!(declared_length(&h), None);
    }

    #[test]
    fn upstream_errors_map_to_failures() {
        assert!(matches!(
            failure_from_upstream(&UpstreamError::Overloaded),
            FillFailure::Overloaded
        ));
        assert!(matches!(
            failure_from_upstream(&UpstreamError::CircuitOpen),
            FillFailure::Upstream {
                kind: UpstreamFailureKind::CircuitOpen,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn tee_body_ends_cleanly_only_after_eof() {
        use crate::stream::read_bounded;
        let (tx, rx) = mpsc::channel(4);
        tx.send(TeeMsg::Data(Bytes::from_static(b"ab")))
            .await
            .unwrap();
        tx.send(TeeMsg::Data(Bytes::from_static(b"cd")))
            .await
            .unwrap();
        tx.send(TeeMsg::Eof).await.unwrap();
        let bytes = read_bounded(tee_body(rx), 100).await.unwrap();
        assert_eq!(&bytes[..], b"abcd");
    }

    #[tokio::test]
    async fn must_not_end_a_tee_stream_cleanly_when_the_fill_aborts() {
        use crate::stream::read_bounded;
        let (tx, rx) = mpsc::channel(4);
        tx.send(TeeMsg::Data(Bytes::from_static(b"ab")))
            .await
            .unwrap();
        drop(tx); // the fill died without Eof
        assert!(read_bounded(tee_body(rx), 100).await.is_err());
    }
}
