//! The middleware stack, one function per concern.
//!
//! Order matters and is fixed in `router.rs`; each layer here states what it
//! relies on being outside or inside of it.

use std::any::Any;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::header::{self, HeaderName, HeaderValue};
use http::{HeaderMap, Method};
use http_body::{Frame, SizeHint};
use http_body_util::Limited;
use rustryx_observability::{InFlightGuard, MetricsHandle, UNMATCHED_ROUTE};
use tower::BoxError;
use tower::load_shed::error::Overloaded;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutBody;
use tracing::{Instrument, field};

use crate::client_ip::{ClientIp, peer_ip, resolve_client_ip};
use crate::error::ApiError;
use crate::request_id::{REQUEST_ID_HEADER, RequestId};
use crate::state::{ApiSettings, AppState};

/// Routes whose completion is logged at debug: probes and scrapes arrive every
/// few seconds from every monitor and would drown the access log.
const QUIET_ROUTES: [&str; 3] = ["/healthz", "/readyz", "/metrics"];

/// Static security headers. Each is added only when the handler did not set its
/// own, so a module that serves a legitimate HTML page can override them.
///
/// The CSP is the strictest possible: `sandbox` without `allow-scripts` means
/// that an uploaded HTML or SVG file opened in a browser cannot run script,
/// which is the requirement for user-supplied artifacts (SEC-35).
const SECURITY_HEADERS: [(HeaderName, &str); 5] = [
    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    (header::REFERRER_POLICY, "no-referrer"),
    (
        header::STRICT_TRANSPORT_SECURITY,
        "max-age=31536000; includeSubDomains",
    ),
    (header::X_FRAME_OPTIONS, "DENY"),
    (
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; frame-ancestors 'none'; sandbox",
    ),
];

/// Wraps a response body so that the in-flight gauge stays up until the body is
/// finished or dropped, not just until the headers were produced.
struct GuardedBody {
    inner: Body,
    _guard: InFlightGuard,
}

impl HttpBody for GuardedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        // Forwarded so that hyper still sends `Content-Length` for small bodies.
        self.inner.size_hint()
    }
}

/// Request span, client address, metrics and the completion log line.
///
/// Sits just inside the request-id layer (the span carries the id) and outside
/// everything else (so metrics and the log see the final status of panics,
/// timeouts and shed requests).
///
/// `route` is the matched route template, never the request path: repository
/// names and artifact paths in a label would create unbounded series. It is only
/// available because this layer runs inside the router.
pub(crate) async fn observe(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| UNMATCHED_ROUTE.to_owned(), |m| m.as_str().to_owned());
    let method = request.method().clone();
    let client_ip = resolve_client_ip(
        peer_ip(request.extensions()),
        request.headers(),
        &state.settings().trusted_proxies,
    );
    request.extensions_mut().insert(ClientIp(client_ip));
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map_or_else(String::new, ToString::to_string);
    let client_text = client_ip.map(|ip| ip.to_string());

    // Span fields: `user` is filled by the authentication middleware, `repo` by the
    // format handlers (`record_repo`), the trace ids right here.
    let span = tracing::info_span!(
        "http.request",
        request_id = %request_id,
        method = %method,
        route = %route,
        client_ip = client_text.as_deref(),
        user = field::Empty,
        repo = field::Empty,
        trace_id = field::Empty,
        span_id = field::Empty,
        status = field::Empty,
    );
    rustryx_observability::propagation::adopt_remote_parent(&span, request.headers());
    rustryx_observability::propagation::record_trace_ids(&span);

    let guard = state.metrics().map(MetricsHandle::track_in_flight);
    let started = Instant::now();
    let response = next.run(request).instrument(span.clone()).await;
    let elapsed = started.elapsed();
    let status = response.status();

    span.record("status", status.as_u16());
    if let Some(metrics) = state.metrics() {
        metrics.record_http_request(&route, method.as_str(), status.as_u16(), elapsed);
    }
    let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    span.in_scope(|| {
        if QUIET_ROUTES.contains(&route.as_str()) {
            tracing::debug!(duration_ms, "request completed");
        } else {
            tracing::info!(duration_ms, "request completed");
        }
    });

    match guard {
        Some(guard) => response.map(|body| {
            Body::new(GuardedBody {
                inner: body,
                _guard: guard,
            })
        }),
        None => response,
    }
}

/// Adds the static security headers where the handler did not set them.
pub(crate) async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in &SECURITY_HEADERS {
        headers
            .entry(name.clone())
            .or_insert_with(|| HeaderValue::from_static(value));
    }
    response
}

/// `Cache-Control: no-store` for responses of the API group that do not choose
/// their own caching: API bodies are per-user, mutable data.
pub(crate) async fn no_store_by_default(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert_with(|| HeaderValue::from_static("no-store"));
    response
}

/// 500 for a panicking handler. The payload goes to the log (and through the
/// redactor); the client gets the generic envelope, never the panic message.
// The signature is dictated by `tower_http`'s `ResponseForPanic`, which hands over ownership.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn panic_response(payload: Box<dyn Any + Send + 'static>) -> Response {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned());
    ApiError::internal(format!("handler panicked: {detail}")).into_response()
}

/// Maps the tower load-shed error to a 503 with `Retry-After`.
pub(crate) fn shed_response(
    error: &BoxError,
    metrics: Option<&MetricsHandle>,
    retry_after: Duration,
) -> Response {
    if error.is::<Overloaded>() {
        if let Some(metrics) = metrics {
            metrics.record_shed("overload");
        }
        ApiError::overloaded(retry_after).into_response()
    } else {
        ApiError::internal(format!("unexpected load-shed layer error: {error}")).into_response()
    }
}

/// 504 when the handler did not produce a response head in time.
pub(crate) async fn request_timeout(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(state.settings().request_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => ApiError::request_timeout().into_response(),
    }
}

/// Body limits of one route group.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BodyLimits {
    /// Largest request body.
    pub max_bytes: u64,
    /// Idle time allowed between body chunks.
    pub idle: Duration,
    /// Also bound the idle time of the response body (artifact downloads).
    pub response_idle: bool,
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

/// Request body size and idle limits.
///
/// A declared `Content-Length` over the limit is refused before any byte is read.
/// A chunked or lying body is cut off by `Limited` when it exceeds the limit, and
/// by `TimeoutBody` when it goes quiet; both surface as errors on the body, which
/// handlers map with [`ApiError::from_body_error`].
pub(crate) async fn body_limits(
    State(limits): State<BodyLimits>,
    request: Request,
    next: Next,
) -> Response {
    if content_length(request.headers()).is_some_and(|declared| declared > limits.max_bytes) {
        return ApiError::payload_too_large().into_response();
    }
    let (parts, body) = request.into_parts();
    let max = usize::try_from(limits.max_bytes).unwrap_or(usize::MAX);
    let body = Body::new(TimeoutBody::new(limits.idle, Limited::new(body, max)));
    let response = next.run(Request::from_parts(parts, body)).await;
    if limits.response_idle {
        response.map(|body| Body::new(TimeoutBody::new(limits.idle, body)))
    } else {
        response
    }
}

/// CORS for the API group, or `None` when no origin is configured.
///
/// The origin list is exact. An origin is never reflected and `*` is never used:
/// reflecting `Origin` or answering `*` next to credentials lets any site read
/// authenticated responses. Credentials are not allowed at all: browser clients
/// authenticate with a bearer token in a header.
pub(crate) fn cors(settings: &ApiSettings) -> Option<CorsLayer> {
    let origins: Vec<HeaderValue> = settings
        .cors_allowed_origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();
    if origins.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                REQUEST_ID_HEADER,
            ])
            .expose_headers([REQUEST_ID_HEADER, header::RETRY_AFTER])
            .max_age(Duration::from_secs(600)),
    )
}

/// Fallback for paths no route matched.
pub(crate) async fn not_found() -> ApiError {
    ApiError::route_not_found()
}

/// Fallback for a known path with an unsupported method.
pub(crate) async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

/// Records the repository a request concerns on the request span, so that the
/// completion line and every log line of the request carry `repo`. Format
/// handlers call it as soon as they have resolved the repository name.
pub fn record_repo(name: &str) {
    tracing::Span::current().record("repo", name);
}
