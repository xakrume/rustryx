//! Helpers of the tests that go through a real HTTP listener (`rustryx-testserver`).
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

use std::sync::Arc;

use reqwest::{Client, Response};
use rustryx_core::model::RepoFormat;
use rustryx_format_raw::RawHandler;
use rustryx_testserver::{NewTestRepo, TestServer, TestServerBuilder};

/// An HTTP client that ignores proxy environment variables: the harness is on loopback.
pub fn client() -> Client {
    Client::builder().no_proxy().build().unwrap()
}

/// A builder with the real raw handler registered.
pub fn raw_builder() -> TestServerBuilder {
    TestServer::builder().register(Arc::new(RawHandler::new()))
}

/// A server with the raw handler and one hosted repository `files`.
pub async fn raw_server() -> TestServer {
    let server = raw_builder().start().await;
    server
        .add_repository(NewTestRepo::hosted("files", RepoFormat::Raw))
        .await;
    server
}

pub async fn get(http: &Client, url: &str, authorization: Option<&str>) -> Response {
    let mut request = http.get(url);
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    request.send().await.unwrap()
}

pub async fn put(http: &Client, url: &str, authorization: Option<&str>, body: Vec<u8>) -> Response {
    let mut request = http.put(url).body(body);
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    request.send().await.unwrap()
}

pub fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

/// Sends a request line as written, without the client normalizing the path (`reqwest` and
/// `url` collapse `..`, which would hide exactly the requests that matter), and returns the
/// status code.
pub async fn raw_status(server: &TestServer, request_target: &str, authorization: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    let request = format!(
        "GET {request_target} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {authorization}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response);
    text.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}
