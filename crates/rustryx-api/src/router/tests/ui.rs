//! Embedded UI fallback: files, SPA routes, and the prefixes that stay on the API.

use std::sync::Arc;

use axum::body::Body;
use http::{HeaderValue, Method, StatusCode};

use crate::router::RouterBuilder;
use crate::test_support::{call, get_uri, ready_state, request};
use crate::ui::WebUi;

const INDEX: &[u8] = b"<!doctype html><title>rustryx-shell</title>";
const THEME: &[u8] = b"document.documentElement.dataset.theme='dark';";
const ICON: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg'></svg>";
const SCRIPT: &[u8] =
    b"/* rustryx-asset */\nconst started = true;\nexport const shell = started;\n";
const STYLE: &[u8] =
    b"body{color:#112233}main{display:block}nav{display:flex}footer{display:block}";

const UI_CSP: &str = "\
default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; \
font-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'; \
object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

struct Fixture;

impl WebUi for Fixture {
    fn asset(&self, path: &str) -> Option<&'static [u8]> {
        match path {
            "index.html" => Some(INDEX),
            "theme-init.js" => Some(THEME),
            "favicon.svg" => Some(ICON),
            "assets/app.js" => Some(SCRIPT),
            "assets/app.css" => Some(STYLE),
            _ => None,
        }
    }
}

fn router() -> axum::Router {
    RouterBuilder::new(ready_state())
        .web_ui(Arc::new(Fixture))
        .build()
}

#[tokio::test]
async fn root_and_a_client_route_serve_index_html() {
    let router = router();
    for path in ["/", "/repositories", "/login"] {
        let reply = get_uri(&router, path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.body.as_ref(), INDEX, "{path}");
        assert_eq!(
            reply.header("content-type"),
            Some("text/html; charset=utf-8"),
            "{path}"
        );
        assert_eq!(reply.header("cache-control"), Some("no-cache"), "{path}");
        assert_eq!(
            reply.header("content-security-policy"),
            Some(UI_CSP),
            "{path}"
        );
        assert_eq!(reply.header("x-frame-options"), Some("DENY"), "{path}");
        assert!(reply.header("etag").is_some(), "{path}");
        assert!(reply.header("x-request-id").is_some(), "{path}");
    }
}

#[tokio::test]
async fn a_hashed_asset_is_immutable_and_a_root_file_is_revalidated() {
    let router = router();
    let script = get_uri(&router, "/assets/app.js").await;
    assert_eq!(script.status, StatusCode::OK);
    assert_eq!(script.body.as_ref(), SCRIPT);
    assert_eq!(
        script.header("content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        script.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let theme = get_uri(&router, "/theme-init.js").await;
    assert_eq!(theme.status, StatusCode::OK);
    assert_eq!(theme.header("cache-control"), Some("no-cache"));
    assert_eq!(
        theme.header("content-type"),
        Some("text/javascript; charset=utf-8")
    );
    let icon = get_uri(&router, "/favicon.svg").await;
    assert_eq!(icon.header("content-type"), Some("image/svg+xml"));
    assert_eq!(icon.header("cache-control"), Some("no-cache"));
}

#[tokio::test]
async fn must_not_serve_index_html_for_a_missing_asset() {
    let reply = get_uri(&router(), "/assets/missing-hash.js").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.error_code(), "route_not_found");
    assert!(!reply.text().contains("rustryx-shell"));
}

#[tokio::test]
async fn must_not_serve_the_ui_for_an_api_or_protocol_path() {
    let router = router();
    for path in [
        "/api/v1/no-such",
        "/repository/demo/missing",
        "/v2/demo/manifests/latest",
        "/service/rest/no-such",
        "/healthz/extra",
        "/metrics/extra",
    ] {
        let reply = get_uri(&router, path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert!(!reply.text().contains("rustryx-shell"), "{path}");
        assert_eq!(
            reply.header("content-security-policy"),
            Some("default-src 'none'; frame-ancestors 'none'; sandbox"),
            "{path}"
        );
    }
    let health = get_uri(&router, "/healthz").await;
    assert_eq!(health.status, StatusCode::OK);
    assert!(health.text().contains("ok"));
}

#[tokio::test]
async fn must_not_fall_back_to_index_when_the_client_does_not_accept_html() {
    let req = request(Method::GET, "/repositories")
        .header("accept", "application/json")
        .body(Body::empty())
        .expect("request");
    let reply = call(&router(), req).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert!(!reply.text().contains("rustryx-shell"));
}

#[tokio::test]
async fn other_methods_on_a_page_are_405_and_a_missing_asset_stays_404() {
    let router = router();
    let post = request(Method::POST, "/repositories")
        .body(Body::empty())
        .expect("request");
    let page = call(&router, post).await;
    assert_eq!(page.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(page.error_code(), "method_not_allowed");
    let put = request(Method::PUT, "/assets/missing.js")
        .body(Body::empty())
        .expect("request");
    assert_eq!(call(&router, put).await.status, StatusCode::NOT_FOUND);
    let write = request(Method::POST, "/assets/app.js")
        .body(Body::empty())
        .expect("request");
    assert_eq!(
        call(&router, write).await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn head_returns_the_headers_without_a_body() {
    let req = request(Method::HEAD, "/assets/app.js")
        .body(Body::empty())
        .expect("request");
    let reply = call(&router(), req).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.is_empty());
    let expected = SCRIPT.len().to_string();
    assert_eq!(reply.header("content-length"), Some(expected.as_str()));
}

#[tokio::test]
async fn if_none_match_returns_304() {
    let first = get_uri(&router(), "/").await;
    let etag = first.header("etag").expect("etag").to_owned();
    let req = request(Method::GET, "/repositories")
        .header("if-none-match", HeaderValue::from_str(&etag).expect("etag"))
        .body(Body::empty())
        .expect("request");
    let reply = call(&router(), req).await;
    assert_eq!(reply.status, StatusCode::NOT_MODIFIED);
    assert!(reply.body.is_empty());
    assert_eq!(reply.header("etag"), Some(etag.as_str()));
}

#[tokio::test]
async fn must_not_serve_a_path_that_climbs_out_of_the_ui() {
    let reply = get_uri(&router(), "/assets/../index.html").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert!(!reply.text().contains("rustryx-shell"));
}

#[tokio::test]
async fn a_text_asset_is_gzip_compressed_when_the_client_asks() {
    let req = request(Method::GET, "/assets/app.css")
        .header("accept-encoding", "gzip")
        .body(Body::empty())
        .expect("request");
    let reply = call(&router(), req).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-encoding"), Some("gzip"));
    assert!(reply.body.starts_with(&[0x1f, 0x8b]));
}
