//! Authentication towards an upstream registry: the token dance, with cached tokens.
//!
//! What a registry client does (<https://distribution.github.io/distribution/spec/auth/token/>):
//!
//! 1. ask `GET <remote>/v2/`. `200` means the upstream needs nothing more from us (an open
//!    registry, or one that takes the Basic credentials of the repository directly);
//!    `401` with `WWW-Authenticate: Bearer realm=...,service=...` names the token service;
//! 2. ask the realm for a token for `repository:<name>:pull`, with the repository's Basic
//!    credentials when it has any;
//! 3. send `Authorization: Bearer <token>` to the registry until the token expires.
//!
//! Both results are cached: the outcome of the probe per repository (so a hot repository does
//! not ping the upstream before every pull) and the tokens per `(repository, scope)` for as
//! long as the upstream says they live, less a safety margin.
//!
//! Where credentials go, and where they must never go: the Basic credentials are sent to the
//! probe URL and the realm only, by the executor, which sends credentials to the origin of
//! the call and strips them on a redirect to another origin. A token is a [`HeaderValue`] marked
//! sensitive, so `Debug` never prints it; nothing here logs a token, a realm query or a
//! credential, and no error text of this module contains one. An upstream `401` or `403` is
//! never turned into a client status: it is an `Upstream` failure, which the engine renders as
//! 502.

use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, header};
use moka::Expiry;
use moka::future::Cache;
use rustryx_core::error::UpstreamError as CoreUpstreamError;
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::ProxyConfig;
use rustryx_format_core::proxy::executor::{CallClass, UpstreamCall};
use rustryx_format_core::stream::collect_bounded;
use rustryx_format_core::{FormatContext, FormatError, UpstreamFailureKind};
use serde::Deserialize;
use url::Url;

use super::challenge::{BearerChallenge, find_bearer};
use crate::config::OciConfig;

/// How requests to the upstream are authorized.
#[derive(Clone)]
pub(crate) enum UpstreamAuth {
    /// Nothing for us to add: the upstream is open, or takes the Basic credentials of the
    /// repository, which the proxy pipeline sends by itself.
    Direct,
    /// A registry token to send as `Authorization`.
    Bearer(HeaderValue),
}

impl std::fmt::Debug for UpstreamAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => f.write_str("Direct"),
            Self::Bearer(_) => f.write_str("Bearer([REDACTED])"),
        }
    }
}

/// The result of probing an upstream.
#[derive(Debug, Clone)]
enum Probe {
    Direct,
    Bearer(BearerChallenge),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TokenKey {
    repository: RepositoryId,
    scope: String,
}

#[derive(Clone)]
struct CachedToken {
    header: HeaderValue,
    lifetime: Duration,
}

/// Each token lives for the lifetime its own response announced.
struct TokenExpiry;

impl Expiry<TokenKey, CachedToken> for TokenExpiry {
    fn expire_after_create(
        &self,
        _key: &TokenKey,
        value: &CachedToken,
        _created_at: Instant,
    ) -> Option<Duration> {
        Some(value.lifetime)
    }
}

/// The caches of probe results and upstream tokens, shared by every proxy repository.
pub(crate) struct UpstreamTokens {
    probes: Cache<RepositoryId, Probe>,
    tokens: Cache<TokenKey, CachedToken>,
    max_token_ttl: Duration,
    token_response_cap: u64,
}

/// Safety margin taken off a token's announced lifetime.
const TOKEN_SKEW: Duration = Duration::from_secs(15);
/// Lifetime assumed when the token response names none (the specification's default).
const DEFAULT_TOKEN_LIFETIME: Duration = Duration::from_secs(60);
/// No token is cached for less than this: shorter would be a request per request.
const MIN_TOKEN_TTL: Duration = Duration::from_secs(5);

impl UpstreamTokens {
    pub(crate) fn new(config: &OciConfig) -> Self {
        Self {
            probes: Cache::builder()
                .max_capacity(config.upstream_token_capacity)
                .time_to_live(config.upstream_probe_ttl)
                .build(),
            tokens: Cache::builder()
                .max_capacity(config.upstream_token_capacity)
                .expire_after(TokenExpiry)
                .build(),
            max_token_ttl: config.upstream_token_max_ttl,
            token_response_cap: config.upstream_token_response_max_bytes,
        }
    }

    /// How to authorize a request for `upstream_name` on the context's proxy repository.
    /// `refresh` ignores a cached token (the upstream refused it).
    pub(crate) async fn authorization(
        &self,
        cx: &FormatContext,
        upstream_name: &str,
        refresh: bool,
    ) -> Result<UpstreamAuth, FormatError> {
        let repository = cx.repository();
        let config = proxy_config(cx)?;
        let probe = match self.probes.get(&repository.id).await {
            Some(probe) if !refresh => probe,
            _ => {
                let probe = probe(cx, config).await?;
                self.probes.insert(repository.id, probe.clone()).await;
                probe
            }
        };
        let Probe::Bearer(challenge) = probe else {
            return Ok(UpstreamAuth::Direct);
        };
        let scope = format!("repository:{upstream_name}:pull");
        let key = TokenKey {
            repository: repository.id,
            scope: scope.clone(),
        };
        if !refresh && let Some(cached) = self.tokens.get(&key).await {
            return Ok(UpstreamAuth::Bearer(cached.header));
        }
        let token = self.fetch_token(cx, config, &challenge, &scope).await?;
        self.tokens.insert(key, token.clone()).await;
        Ok(UpstreamAuth::Bearer(token.header))
    }

    /// Forgets what is cached for a repository and name, after the upstream refused a token.
    pub(crate) async fn forget(&self, repository: RepositoryId, upstream_name: &str) {
        self.probes.invalidate(&repository).await;
        self.tokens
            .invalidate(&TokenKey {
                repository,
                scope: format!("repository:{upstream_name}:pull"),
            })
            .await;
    }

    async fn fetch_token(
        &self,
        cx: &FormatContext,
        config: &ProxyConfig,
        challenge: &BearerChallenge,
        scope: &str,
    ) -> Result<CachedToken, FormatError> {
        let mut url = challenge.realm.clone();
        {
            let mut query = url.query_pairs_mut();
            if let Some(service) = &challenge.service {
                query.append_pair("service", service);
            }
            query.append_pair("scope", scope);
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        let result = run(cx, config, url, headers, config.auth.clone(), Method::GET).await?;
        match result.status.as_u16() {
            200 => {}
            401 | 403 => {
                return Err(FormatError::upstream(
                    UpstreamFailureKind::Rejected,
                    "the upstream token service refused the configured credentials",
                ));
            }
            other => {
                return Err(FormatError::upstream(
                    UpstreamFailureKind::BadStatus,
                    format!("the upstream token service answered {other}"),
                ));
            }
        }
        let body: Bytes = collect_bounded(result.body, self.token_response_cap)
            .await
            .map_err(|_| {
                FormatError::upstream(
                    UpstreamFailureKind::BadStatus,
                    "the upstream token response could not be read within its limit",
                )
            })?;
        let parsed: TokenResponse = serde_json::from_slice(&body).map_err(|_| {
            FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                "the upstream token response is not valid JSON",
            )
        })?;
        let token = parsed
            .token
            .filter(|token| !token.is_empty())
            .or(parsed.access_token.filter(|token| !token.is_empty()))
            .ok_or_else(|| {
                FormatError::upstream(
                    UpstreamFailureKind::BadStatus,
                    "the upstream token response carries no token",
                )
            })?;
        let mut header = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                "the upstream token is not a valid header value",
            )
        })?;
        header.set_sensitive(true);
        let announced = parsed
            .expires_in
            .map_or(DEFAULT_TOKEN_LIFETIME, Duration::from_secs);
        let lifetime = announced
            .saturating_sub(TOKEN_SKEW)
            .max(MIN_TOKEN_TTL)
            .min(self.max_token_ttl);
        Ok(CachedToken { header, lifetime })
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    token: Option<String>,
    access_token: Option<String>,
    expires_in: Option<u64>,
}

/// The proxy settings of the context's repository.
fn proxy_config(cx: &FormatContext) -> Result<&ProxyConfig, FormatError> {
    cx.repository()
        .proxy
        .as_ref()
        .ok_or_else(|| FormatError::internal("proxy repository without proxy settings"))
}

/// `GET <remote>/v2/`.
async fn probe(cx: &FormatContext, config: &ProxyConfig) -> Result<Probe, FormatError> {
    let mut url = Url::parse(&config.remote_url).map_err(|_| {
        FormatError::upstream(
            UpstreamFailureKind::Blocked,
            "remote_url is not a valid url",
        )
    })?;
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut segments = url.path_segments_mut().map_err(|()| {
            FormatError::upstream(UpstreamFailureKind::Blocked, "remote_url cannot be a base")
        })?;
        segments.pop_if_empty().push("v2").push("");
    }
    let result = run(
        cx,
        config,
        url,
        HeaderMap::new(),
        config.auth.clone(),
        Method::GET,
    )
    .await?;
    let status = result.status.as_u16();
    match status {
        // Open, or accepting the Basic credentials of the repository directly; and an
        // upstream without the version endpoint, where the data requests will say what is wrong.
        200..=299 | 404 | 405 | 410 => Ok(Probe::Direct),
        401 => {
            let challenges: Vec<&str> = result
                .headers
                .get_all(header::WWW_AUTHENTICATE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .collect();
            match find_bearer(challenges) {
                Some(challenge) => Ok(Probe::Bearer(challenge)),
                None => Err(FormatError::upstream(
                    UpstreamFailureKind::Rejected,
                    if config.auth.is_some() {
                        "the upstream rejected the configured credentials"
                    } else {
                        "the upstream requires credentials and none are configured"
                    },
                )),
            }
        }
        403 => Err(FormatError::upstream(
            UpstreamFailureKind::Rejected,
            "the upstream refused the version check",
        )),
        other => Err(FormatError::from(CoreUpstreamError::Status(other))),
    }
}

/// One call through the executor: SSRF policy, breaker, concurrency cap, timeouts, retries.
async fn run(
    cx: &FormatContext,
    config: &ProxyConfig,
    url: Url,
    headers: HeaderMap,
    credentials: Option<rustryx_core::model::UpstreamAuth>,
    method: Method,
) -> Result<rustryx_format_core::proxy::executor::UpstreamResult, FormatError> {
    let repository = cx.repository();
    let call = UpstreamCall {
        repository: repository.id,
        repo_name: repository.name.to_string(),
        method,
        url,
        headers,
        credentials,
        max_concurrent: config.max_concurrent_upstream,
        class: CallClass::Metadata,
    };
    cx.upstream()
        .execute(call)
        .await
        .map_err(|error| FormatError::upstream(error.kind(), error.to_string()))
}

#[cfg(test)]
mod tests;
