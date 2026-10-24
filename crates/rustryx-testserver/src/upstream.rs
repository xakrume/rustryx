//! Upstream fakes for proxy tests, over wiremock: a real HTTP server on loopback that serves
//! static files, counts requests and can answer 404, 5xx, slowly or with a body that ends
//! early.
//!
//! [`FakeUpstream`](crate::FakeUpstream) (re-exported) replaces the HTTP client altogether and
//! is faster; this one is real HTTP, so it exercises the SSRF guard, the resolver, redirects
//! and the executor's timeouts as production would.

use std::time::Duration;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A request the stub received.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    /// HTTP method.
    pub method: String,
    /// Path, as sent.
    pub path: String,
    /// Raw query string, if any.
    pub query: Option<String>,
    /// Headers, lowercase names. Includes `authorization` when the proxy sent one.
    pub headers: Vec<(String, String)>,
}

impl RecordedCall {
    /// The first value of a header (name in lowercase).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A local HTTP upstream. Dropping it stops the server.
pub struct UpstreamStub {
    server: MockServer,
}

impl UpstreamStub {
    /// Starts a stub on an ephemeral loopback port. Every path answers 404 until scripted.
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    /// `http://127.0.0.1:PORT`, without a trailing slash.
    #[must_use]
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// `http://127.0.0.1:PORT/`: the usual shape of a proxy repository's `remote_url`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("{}/", self.server.uri())
    }

    /// The underlying wiremock server, for scripting this wrapper does not cover.
    #[must_use]
    pub fn server(&self) -> &MockServer {
        &self.server
    }

    /// Serves `body` at `path` for `GET` and `HEAD` with a matching `Content-Length`.
    pub async fn serve_file(&self, path: &str, body: impl Into<Vec<u8>>, content_type: &str) {
        self.serve_file_with_headers(path, body, content_type, &[])
            .await;
    }

    /// [`UpstreamStub::serve_file`] plus extra response headers (`ETag`, `Last-Modified`).
    pub async fn serve_file_with_headers(
        &self,
        path_text: &str,
        body: impl Into<Vec<u8>>,
        content_type: &str,
        headers: &[(&str, &str)],
    ) {
        let body: Vec<u8> = body.into();
        for verb in ["GET", "HEAD"] {
            let mut template = ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("content-type", content_type);
            for (name, value) in headers {
                template = template.insert_header(*name, *value);
            }
            Mock::given(method(verb))
                .and(path(path_text))
                .respond_with(template)
                .mount(&self.server)
                .await;
        }
    }

    /// Answers 404 at `path` (the default for unscripted paths, but explicit when a test
    /// replaces a file that existed).
    pub async fn not_found(&self, path_text: &str) {
        self.status(path_text, 404).await;
    }

    /// Answers `status` at `path` for `GET` and `HEAD`, for example 500, 503, 401 or 403.
    pub async fn status(&self, path_text: &str, status: u16) {
        for verb in ["GET", "HEAD"] {
            Mock::given(method(verb))
                .and(path(path_text))
                .respond_with(ResponseTemplate::new(status))
                .mount(&self.server)
                .await;
        }
    }

    /// Fails `failures` times with `status`, then serves `body`: a flaky upstream, for retry
    /// tests. The failing mock has the higher priority and expires after `failures` hits.
    pub async fn flaky(
        &self,
        path_text: &str,
        failures: u64,
        status: u16,
        body: impl Into<Vec<u8>>,
    ) {
        Mock::given(method("GET"))
            .and(path(path_text))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(failures)
            .with_priority(1)
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(path_text))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.into()))
            .with_priority(2)
            .mount(&self.server)
            .await;
    }

    /// Serves `body` at `path` after waiting `delay` before the response head.
    pub async fn slow(&self, path_text: &str, body: impl Into<Vec<u8>>, delay: Duration) {
        Mock::given(method("GET"))
            .and(path(path_text))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.into())
                    .set_delay(delay),
            )
            .mount(&self.server)
            .await;
    }

    /// Serves a body that ends early: the response announces `declared_len` bytes in
    /// `Content-Length` and sends only `body`, so the connection ends mid-body and the client
    /// sees a broken stream. A proxy must neither serve it as complete nor cache it.
    pub async fn truncated(&self, path_text: &str, body: impl Into<Vec<u8>>, declared_len: usize) {
        Mock::given(method("GET"))
            .and(path(path_text))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.into())
                    .insert_header("content-length", declared_len.to_string().as_str()),
            )
            .mount(&self.server)
            .await;
    }

    /// Redirects `path` to `location` with `status` (301, 302, 307).
    pub async fn redirect(&self, path_text: &str, status: u16, location: &str) {
        Mock::given(method("GET"))
            .and(path(path_text))
            .respond_with(ResponseTemplate::new(status).insert_header("location", location))
            .mount(&self.server)
            .await;
    }

    /// Every request received so far, in order.
    pub async fn requests(&self) -> Vec<RecordedCall> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .map(|request| RecordedCall {
                method: request.method.to_string(),
                path: request.url.path().to_owned(),
                query: request.url.query().map(str::to_owned),
                headers: request
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_ascii_lowercase(),
                            value.to_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect(),
            })
            .collect()
    }

    /// Number of requests of any method.
    pub async fn request_count(&self) -> usize {
        self.requests().await.len()
    }

    /// Number of requests of `method` (`"GET"`, `"HEAD"`) for `path`.
    pub async fn count(&self, verb: &str, path_text: &str) -> usize {
        self.requests()
            .await
            .iter()
            .filter(|call| call.method == verb && call.path == path_text)
            .count()
    }

    /// Forgets scripted answers and the request log.
    pub async fn reset(&self) {
        self.server.reset().await;
    }
}
