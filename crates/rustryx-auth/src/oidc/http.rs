//! Outgoing HTTP to Keycloak: one `reqwest` 0.13 client for discovery, JWKS and the token endpoint.
//!
//! `openidconnect` 4 ships an adapter for `reqwest` 0.12 and would drag a second HTTP
//! stack (with its own TLS configuration) into the process. Its `AsyncHttpClient`
//! trait is public, so the code flow runs through this client instead and the
//! 0.12 adapter is never used. One client means one place that enforces:
//! HTTPS only, certificate verification, no redirects, hard timeouts, capped bodies.

use std::future::Future;
use std::pin::Pin;

use http::{HeaderMap, HeaderValue, header};
use openidconnect::{AsyncHttpClient, HttpRequest, HttpResponse};

use super::config::OidcConfig;

/// Largest accepted discovery document or JWKS.
pub(crate) const MAX_METADATA_BYTES: usize = 1024 * 1024;
/// Largest accepted token endpoint response (ID token, access token and refresh token).
const MAX_TOKEN_RESPONSE_BYTES: usize = 256 * 1024;
const USER_AGENT: &str = concat!("rustryx-oidc/", env!("CARGO_PKG_VERSION"));

/// Failure of one outgoing call.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HttpError {
    /// Transport failure: DNS, connect, TLS (including certificate verification), timeout.
    #[error("request failed: {0}")]
    Request(#[source] reqwest::Error),
    /// The server answered with something other than 200.
    #[error("unexpected status {0}")]
    Status(u16),
    /// The body is larger than the cap.
    #[error("response body exceeds {0} bytes")]
    TooLarge(usize),
    /// The request could not be built.
    #[error("invalid request: {0}")]
    Build(String),
    /// The HTTP client could not be configured.
    #[error("cannot configure http client: {0}")]
    Client(String),
}

/// A successful response with the headers the caches need.
#[derive(Debug)]
pub(crate) struct Fetched {
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

/// HTTP client bound to the OIDC security policy.
#[derive(Clone, Debug)]
pub(crate) struct OidcHttp {
    client: reqwest::Client,
}

impl OidcHttp {
    pub(crate) fn new(config: &OidcConfig) -> Result<Self, HttpError> {
        let mut builder = reqwest::Client::builder()
            // The JWKS decides who may sign tokens: refuse anything but TLS.
            .https_only(true)
            // A redirect could hand the request to a host nobody configured.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .user_agent(USER_AGENT);
        let mut extra = Vec::new();
        for pem in &config.extra_root_certs_pem {
            let bundle = reqwest::Certificate::from_pem_bundle(pem)
                .map_err(|e| HttpError::Client(format!("invalid extra root certificate: {e}")))?;
            extra.extend(bundle);
        }
        if !extra.is_empty() {
            // Merge, not replace: a corporate CA is added to the platform roots.
            builder = builder.tls_certs_merge(extra);
        }
        let client = builder
            .build()
            .map_err(|e| HttpError::Client(e.to_string()))?;
        Ok(Self { client })
    }

    /// GET returning the body of a 200 response, at most `max_bytes` long.
    pub(crate) async fn get(&self, url: &str, max_bytes: usize) -> Result<Fetched, HttpError> {
        let response = self
            .client
            .get(url)
            .header(header::ACCEPT, HeaderValue::from_static("application/json"))
            .send()
            .await
            .map_err(HttpError::Request)?;
        if response.status() != http::StatusCode::OK {
            return Err(HttpError::Status(response.status().as_u16()));
        }
        let headers = response.headers().clone();
        let body = read_capped(response, max_bytes).await?;
        Ok(Fetched { headers, body })
    }
}

/// Reads the body chunk by chunk so an oversized response is cut off instead of buffered.
async fn read_capped(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, HttpError> {
    if response
        .content_length()
        .is_some_and(|len| len > max_bytes as u64)
    {
        return Err(HttpError::TooLarge(max_bytes));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(HttpError::Request)? {
        if body.len() + chunk.len() > max_bytes {
            return Err(HttpError::TooLarge(max_bytes));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl<'c> AsyncHttpClient<'c> for OidcHttp {
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let request =
                reqwest::Request::try_from(request).map_err(|e| HttpError::Build(e.to_string()))?;
            let response = self
                .client
                .execute(request)
                .await
                .map_err(HttpError::Request)?;
            let status = response.status();
            let version = response.version();
            let headers = response.headers().clone();
            // Error bodies (OAuth error JSON) are small too, so the same cap applies to every status.
            let body = read_capped(response, MAX_TOKEN_RESPONSE_BYTES).await?;
            let mut out = http::Response::builder().status(status).version(version);
            if let Some(map) = out.headers_mut() {
                *map = headers;
            }
            out.body(body).map_err(|e| HttpError::Build(e.to_string()))
        })
    }
}
