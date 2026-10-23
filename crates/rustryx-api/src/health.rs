//! `/healthz`, `/readyz` and `/metrics`.
//!
//! These routes sit outside the concurrency limit, the load shedder and the
//! authentication middleware: a node that is saturated with uploads must still
//! answer its liveness probe, or Kubernetes would restart a healthy pod, and a
//! scrape must not need an account.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http::header::{self, HeaderValue};
use http::{HeaderMap, StatusCode};
use rustryx_core::Secret;
use serde::Serialize;
use subtle::ConstantTimeEq;

use crate::error::{ApiError, Challenge};
use crate::readiness::{Outcome, SchemaStatus};
use crate::state::{AppState, MetricsAccess};

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The three operational routes.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
}

/// Liveness: the process is up and the runtime schedules tasks.
///
/// The body is a constant and the handler touches no dependency (no database,
/// Redis, blob store, Keycloak, LDAP or upstream): a dependency outage must never
/// make Kubernetes restart a healthy pod.
async fn healthz() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        r#"{"status":"ok"}"#,
    )
        .into_response()
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
}

#[derive(Serialize)]
struct AdvisoryBody {
    name: &'static str,
    status: &'static str,
}

#[derive(Serialize)]
struct ReadyBody {
    status: &'static str,
    checks: Vec<Check>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    advisories: Vec<AdvisoryBody>,
}

fn check(name: &'static str, ok: bool) -> Check {
    Check {
        name,
        status: if ok { "ok" } else { "fail" },
        note: None,
    }
}

/// Readiness. See `readiness.rs` for why PostgreSQL decides and nothing else does.
async fn readyz(State(state): State<AppState>) -> Response {
    let (probe, cache) = state.readiness();
    let report = cache.report(probe).await;
    let lifecycle = state.lifecycle();

    let mut schema = check("schema", report.schema.is_servable());
    if report.schema == SchemaStatus::Ahead {
        schema.note = Some("newer than this binary, serving in compatibility mode");
    }
    let checks = vec![
        check("listener", lifecycle.is_listening()),
        check("draining", !lifecycle.is_draining()),
        schema,
        check("database", report.database == Outcome::Ok),
    ];
    let ready = checks.iter().all(|c| c.status == "ok");
    let body = ReadyBody {
        status: if ready { "ready" } else { "not_ready" },
        checks,
        // Reported, never counted: Redis, Keycloak, LDAP, S3 and upstreams are
        // not allowed to change the status code.
        advisories: report
            .advisories
            .iter()
            .map(|a| AdvisoryBody {
                name: a.name,
                status: if a.healthy { "ok" } else { "degraded" },
            })
            .collect(),
    };
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        axum::Json(body),
    )
        .into_response()
}

/// Prometheus exposition. Disabled means 404, so that a scanner learns nothing.
async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(handle) = state.metrics() else {
        return ApiError::route_not_found().into_response();
    };
    match &state.settings().metrics_access {
        MetricsAccess::Disabled => ApiError::route_not_found().into_response(),
        MetricsAccess::Bearer(expected) if !bearer_matches(&headers, expected) => {
            ApiError::unauthenticated()
                .with_challenge(Challenge::bearer("metrics"))
                .into_response()
        }
        MetricsAccess::Public | MetricsAccess::Bearer(_) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(PROMETHEUS_CONTENT_TYPE),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            handle.render(),
        )
            .into_response(),
    }
}

/// Constant-time comparison, so response timing does not reveal a prefix of the token.
fn bearer_matches(headers: &HeaderMap, expected: &Secret) -> bool {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim());
    match presented {
        Some(token) => bool::from(token.as_bytes().ct_eq(expected.expose().as_bytes())),
        None => false,
    }
}
