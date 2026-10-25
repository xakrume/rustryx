//! The embedded web UI.
//!
//! The composition root passes the files built in `ui/dist`. This module only
//! decides which request gets which file. The rules are the ones in
//! `docs/ui-architecture.md` section 10.2: an existing file is that file, a
//! missing hashed asset is 404, a client route falls back to `index.html`, and
//! the API and package protocols never receive that fallback.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use http::header::{self, HeaderValue};
use http::{HeaderMap, Method, StatusCode};
use sha2::{Digest, Sha256};
use tower_http::compression::CompressionLayer;

use crate::error::ApiError;

/// Policy from `ui/scripts/csp.mjs`. The default API policy (`default-src 'none'; sandbox`)
/// would block the application, so every UI response sets this value itself.
const UI_CSP: &str = "\
default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; \
font-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'; \
object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Prefixes that belong to the API, the package protocols or the probes.
/// A missing path under one of them stays a 404 from this layer.
const RESERVED: [&str; 7] = [
    "api",
    "repository",
    "v2",
    "service",
    "healthz",
    "readyz",
    "metrics",
];

/// One file from the embedded UI build.
///
/// The bytes live for the process: the build output is compiled into the binary.
pub trait WebUi: Send + Sync {
    /// Returns the file at `path` (`assets/index-abc.js`), or nothing when it is absent.
    fn asset(&self, path: &str) -> Option<&'static [u8]>;
}

/// Router used as the fallback of the main router.
///
/// Compression matches the API group (gzip). Hashed assets and `index.html` still
/// set their own `Cache-Control`, which this layer does not replace.
pub(crate) fn ui_service(ui: Arc<dyn WebUi>) -> Router {
    Router::new()
        .fallback(serve)
        .layer(CompressionLayer::new())
        .with_state(ui)
}

async fn serve(State(ui): State<Arc<dyn WebUi>>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    dispatch(ui.as_ref(), &parts.method, parts.uri.path(), &parts.headers)
}

fn dispatch(ui: &dyn WebUi, method: &Method, path: &str, headers: &HeaderMap) -> Response {
    let relative = match classify(path) {
        Class::Reserved | Class::Reject => return ApiError::route_not_found().into_response(),
        Class::Root => return spa(ui, method, headers),
        Class::File(relative) => relative,
    };
    if let Some(bytes) = ui.asset(relative) {
        return if is_read(method) {
            file_response(method, headers, relative, bytes)
        } else {
            ApiError::method_not_allowed().into_response()
        };
    }
    if relative == "assets" || relative.starts_with("assets/") {
        return ApiError::route_not_found().into_response();
    }
    spa(ui, method, headers)
}

enum Class<'a> {
    Reserved,
    Reject,
    Root,
    File(&'a str),
}

fn classify(path: &str) -> Class<'_> {
    if is_reserved(path) {
        return Class::Reserved;
    }
    if path == "/" {
        return Class::Root;
    }
    let Some(relative) = path.strip_prefix('/') else {
        return Class::Reject;
    };
    if relative
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Class::Reject;
    }
    Class::File(relative)
}

fn is_reserved(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    RESERVED.iter().any(|prefix| {
        rest == *prefix
            || (rest.starts_with(prefix) && rest.as_bytes().get(prefix.len()) == Some(&b'/'))
    })
}

fn spa(ui: &dyn WebUi, method: &Method, headers: &HeaderMap) -> Response {
    if !is_read(method) {
        return ApiError::method_not_allowed().into_response();
    }
    if !accepts_html(headers) {
        return ApiError::route_not_found().into_response();
    }
    ui.asset("index.html").map_or_else(
        || ApiError::route_not_found().into_response(),
        |bytes| file_response(method, headers, "index.html", bytes),
    )
}

fn is_read(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

fn accepts_html(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::ACCEPT) else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let mut html: Option<f32> = None;
    let mut star: Option<f32> = None;
    for part in value.split(',') {
        let mut pieces = part.split(';');
        let mime = pieces.next().unwrap_or("").trim();
        let mut quality = 1.0_f32;
        for param in pieces {
            if let Some(raw) = param.trim().strip_prefix("q=") {
                quality = raw.parse().unwrap_or(0.0);
            }
        }
        if mime == "text/html" || mime == "text/*" {
            html = Some(quality);
        } else if mime == "*/*" {
            star = Some(quality);
        }
    }
    html.or(star).unwrap_or(0.0) > 0.0
}

fn file_response(
    method: &Method,
    headers: &HeaderMap,
    path: &str,
    bytes: &'static [u8],
) -> Response {
    let etag = entity_tag(bytes);
    let cache = cache_control(path);
    if not_modified(headers, &etag) {
        return tagged(StatusCode::NOT_MODIFIED, &etag, cache, Body::empty());
    }
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    let mut response = tagged(StatusCode::OK, &etag, cache, body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    if *method == Method::HEAD {
        insert_length(headers, bytes.len());
    }
    response
}

fn tagged(status: StatusCode, etag: &str, cache: &'static str, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(UI_CSP),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    response
}

fn insert_length(headers: &mut HeaderMap, len: usize) {
    if let Ok(value) = HeaderValue::from_str(&len.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
}

fn cache_control(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        IMMUTABLE
    } else {
        REVALIDATE
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

fn entity_tag(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut tag = String::with_capacity(66);
    tag.push('"');
    for byte in digest {
        tag.push(char::from(HEX[usize::from(byte >> 4)]));
        tag.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    tag.push('"');
    tag
}

fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    if value.trim() == "*" {
        return true;
    }
    value
        .split(',')
        .any(|part| part.trim().trim_start_matches("W/") == etag)
}
