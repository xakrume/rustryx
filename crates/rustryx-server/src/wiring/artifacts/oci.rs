//! `/v2`, `/v2/`, `/v2/token`, `/v2/_catalog` and `/v2/<repository>/...`.
//!
//! Repository traffic is the format engine with `head.path` starting at `v2/`. The ping, the
//! token endpoint and the cross-repository catalog are not engine routes.

use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use rustryx_api::{ApiError, current_request_id};
use rustryx_core::Secret;
use rustryx_core::identity::{AuthContext, Credentials, Principal};
use rustryx_core::names::RepoName;
use rustryx_format_core::{ClientInfo, RequestHead, RequestLabels as FormatLabels};
use rustryx_format_oci::parse::scope::Action;
use rustryx_format_oci::{
    OciError, OciErrorCode, TokenRequest, bearer_challenge, catalog_response, challenge_scope,
    ping_challenge, ping_ok, token_response,
};

use super::{ArtifactState, base_url, metric_labels, sensitive_authorization};
use crate::wiring::formats::REGISTRY_SERVICE;

/// Largest token form accepted. A password grant is a few hundred bytes.
const MAX_TOKEN_FORM_BYTES: usize = 64 * 1024;

pub(super) fn ping(state: &ArtifactState, headers: &HeaderMap, principal: &Principal) -> Response {
    let base = match base_url(state, headers) {
        Ok(url) => url,
        Err(error) => return error.into_response(),
    };
    if bearer(headers).is_some_and(|token| state.tokens.verify(token).is_ok())
        || !principal.is_anonymous()
    {
        return ping_ok();
    }
    ping_challenge(&base, REGISTRY_SERVICE)
}

pub(super) async fn token(
    state: &ArtifactState,
    request: Request,
    principal: Principal,
) -> Response {
    let post = request.method() == Method::POST;
    if !post && request.method() != Method::GET {
        return OciError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            OciErrorCode::Unsupported,
            "the token endpoint answers GET and POST",
        )
        .into_response();
    }
    let (parts, body) = request.into_parts();
    if post {
        return token_post(state, body).await;
    }
    let request = parts
        .uri
        .query()
        .map(|query| TokenRequest::from_urlencoded(query.as_bytes()))
        .unwrap_or_default();
    token_response(&state.tokens, &principal, &request, false).await
}

async fn token_post(state: &ArtifactState, body: Body) -> Response {
    let Ok(bytes) = axum::body::to_bytes(body, MAX_TOKEN_FORM_BYTES).await else {
        return OciError::unsupported(StatusCode::BAD_REQUEST, "unreadable token request")
            .into_response();
    };
    let form = TokenRequest::from_urlencoded(&bytes);
    let fields: Vec<(String, String)> = url::form_urlencoded::parse(&bytes).into_owned().collect();
    let grant = field(&fields, "grant_type");
    if grant != Some("password") {
        return (
            StatusCode::BAD_REQUEST,
            [("content-type", "application/json")],
            r#"{"error":"unsupported_grant_type"}"#,
        )
            .into_response();
    }
    let username = field(&fields, "username").unwrap_or("");
    let password = field(&fields, "password").unwrap_or("");
    let Some(authenticator) = &state.authenticator else {
        return OciError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            OciErrorCode::Unavailable,
            "authentication is not available",
        )
        .with_header(http::header::RETRY_AFTER, HeaderValue::from_static("1"))
        .into_response();
    };
    let credentials = Credentials::Basic {
        username: username.to_owned(),
        password: Secret::new(password),
    };
    let principal = match authenticator
        .authenticate(&credentials, &AuthContext::new())
        .await
    {
        Ok(Some(principal)) => principal,
        Ok(None) => return ApiError::unauthenticated().into_response(),
        Err(error) => return ApiError::from(error).into_response(),
    };
    token_response(&state.tokens, &principal, &form, true).await
}

pub(super) async fn catalog(
    state: &ArtifactState,
    headers: &HeaderMap,
    principal: &Principal,
    query: Option<&str>,
) -> Response {
    if let Some(token) = bearer(headers)
        && let Ok(claims) = state.tokens.verify(token)
        && !claims.permits_catalog()
    {
        return insufficient(state, headers, "registry:catalog:*");
    }
    catalog_response(&state.catalog, principal, query).await
}

pub(super) async fn repository(
    state: &ArtifactState,
    request: Request,
    principal: Principal,
    ip: Option<IpAddr>,
) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let Some(below) = path.strip_prefix("/v2/") else {
        return name_unknown();
    };
    let (raw_name, image_path) = below.split_once('/').unwrap_or((below, ""));
    let Ok(name) = RepoName::parse(raw_name) else {
        return OciError::new(
            StatusCode::BAD_REQUEST,
            OciErrorCode::NameInvalid,
            "repository name is not valid",
        )
        .into_response();
    };
    if let Some(image) = image_name(image_path)
        && let Some(token) = bearer(&parts.headers)
        && let Ok(claims) = state.tokens.verify(token)
    {
        let action = required_action(&parts.method);
        let docker_name = format!("{raw_name}/{image}");
        if !claims.permits(&docker_name, action) {
            let scope = challenge_scope(raw_name, &image, &parts.method);
            return insufficient(state, &parts.headers, &scope);
        }
    }
    let repository = match state.repositories.get_by_name(&name).await {
        Ok(Some(repository)) => repository,
        Ok(None) => return name_unknown(),
        Err(error) => return ApiError::from(error).into_response(),
    };
    let base = match base_url(state, &parts.headers) {
        Ok(url) => url,
        Err(error) => return error.into_response(),
    };
    let scope =
        image_name(image_path).map(|image| challenge_scope(raw_name, &image, &parts.method));
    let user_agent = parts
        .headers
        .get(http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let head_path = if image_path.is_empty() {
        "v2".to_owned()
    } else {
        format!("v2/{image_path}")
    };
    let mut head = RequestHead::new(parts.method.clone(), head_path, principal, base.clone())
        .with_headers(sensitive_authorization(parts.headers))
        .with_client(ClientInfo {
            ip,
            user_agent,
            request_id: current_request_id().map(|id| id.as_str().to_owned()),
            ..ClientInfo::default()
        });
    if let Some(query) = parts.uri.query() {
        head = head.with_query(query);
    }
    let mut response = state.engine.serve(Arc::new(repository), head, body).await;
    if response.status() == StatusCode::UNAUTHORIZED
        && let Some(scope) = scope.as_deref()
        && response.headers().get(WWW_AUTHENTICATE).is_none()
        && let Ok(value) = HeaderValue::from_str(&bearer_challenge(
            &base,
            REGISTRY_SERVICE,
            Some(scope),
            None,
        ))
    {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    let labels = metric_labels(FormatLabels::of(&response));
    response.extensions_mut().insert(labels);
    response
}

fn insufficient(state: &ArtifactState, headers: &HeaderMap, scope: &str) -> Response {
    let Ok(base) = base_url(state, headers) else {
        return name_unknown();
    };
    let mut response = OciError::new(
        StatusCode::UNAUTHORIZED,
        OciErrorCode::Unauthorized,
        "authentication required",
    )
    .into_response();
    if let Ok(value) = HeaderValue::from_str(&bearer_challenge(
        &base,
        REGISTRY_SERVICE,
        Some(scope),
        Some("insufficient_scope"),
    )) {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    response
}

fn name_unknown() -> Response {
    OciError::new(
        StatusCode::NOT_FOUND,
        OciErrorCode::NameUnknown,
        "repository name not known to registry",
    )
    .into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(token.trim())
    } else {
        None
    }
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// The image path in front of the protocol marker, when the request names one.
fn image_name(image_path: &str) -> Option<String> {
    let segments: Vec<&str> = image_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let marker = segments.iter().position(|part| {
        matches!(
            *part,
            "manifests" | "blobs" | "tags" | "referrers" | "uploads"
        )
    })?;
    if marker == 0 {
        return None;
    }
    Some(segments[..marker].join("/"))
}

fn required_action(method: &Method) -> Action {
    if *method == Method::GET || *method == Method::HEAD {
        Action::Pull
    } else if *method == Method::DELETE {
        Action::Delete
    } else {
        Action::Push
    }
}
