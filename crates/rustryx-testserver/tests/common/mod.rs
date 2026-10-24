//! Helpers shared by the integration tests of the harness.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset of the helpers"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]

use reqwest::{Client, Response};
use rustryx_core::model::RepoFormat;
use rustryx_testserver::{NewTestRepo, RawLikeHandler, TestServer, TestServerBuilder};
use std::sync::Arc;

/// An HTTP client that ignores proxy environment variables: the harness is on loopback.
pub fn client() -> Client {
    rustryx_testserver::http_client()
}

/// A builder with the raw-like handler registered.
pub fn raw_builder() -> TestServerBuilder {
    TestServer::builder().register(Arc::new(RawLikeHandler::new()))
}

/// A server with the raw-like handler and one hosted repository `files`.
pub async fn raw_server() -> TestServer {
    let server = raw_builder().start().await;
    server
        .add_repository(NewTestRepo::hosted("files", RepoFormat::Raw))
        .await;
    server
}

/// `GET url` with an optional `Authorization` header value.
pub async fn get(http: &Client, url: &str, authorization: Option<&str>) -> Response {
    let mut request = http.get(url);
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    request.send().await.unwrap()
}

/// `PUT url` with a body and an optional `Authorization` header value.
pub async fn put(
    http: &Client,
    url: &str,
    authorization: Option<&str>,
    body: &'static [u8],
) -> Response {
    let mut request = http.put(url).body(body);
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    request.send().await.unwrap()
}

/// The text of a header, if present.
pub fn header_text(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}
