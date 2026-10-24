//! An HTTP client for tests that talk to the server directly (the "curl-like" calls).

/// A `reqwest` client for the harness: it ignores `HTTP_PROXY` and friends, because the server
/// is on loopback and a developer machine with a corporate or Docker proxy in its environment
/// would otherwise send these requests to the proxy.
///
/// # Panics
/// When the TLS backend cannot be initialised, which no test can recover from.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "test harness: a client that cannot be built must fail the calling test at once"
)]
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("the test HTTP client could not be built")
}
