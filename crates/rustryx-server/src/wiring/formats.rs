//! The format engine and what it is configured with.
//!
//! The engine is the one entry point from the HTTP layer into the package formats. It owns the
//! upstream client, so the SSRF policy of `upstream.*` is applied here once, to every URL and
//! every redirect hop of every proxy repository.
//!
//! Every format crate is registered in [`registry`]. One OCI handler serves both `docker` and
//! `oci`. A request for a repository whose format is not in that list is still the engine's
//! own 500.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use rustryx_blob::BlobStoreRegistry;
use rustryx_config::{AppConfig, UpstreamConfig};
use rustryx_core::ports::{Clock, ComponentRepo, RbacChecker, RepositoryRepo, UuidV7IdGen};
use rustryx_format_core::context::{ContentEvents, Limits, NoopEvents, Ports};
use rustryx_format_core::handler::FormatHandler;
use rustryx_format_core::policy::NoopContentPolicy;
use rustryx_format_core::proxy::executor::{ExecutorConfig, RetryPolicy};
use rustryx_format_core::proxy::ssrf::{HostAllowlist, HostPattern, UpstreamUrlPolicy};
use rustryx_format_core::proxy::upstream::ReqwestClientConfig;
use rustryx_format_core::{DownloadBatcher, FormatEngine, FormatRegistry, RuleRoutingGate};
use rustryx_format_helm::HelmHandler;
use rustryx_format_maven::MavenHandler;
use rustryx_format_npm::NpmHandler;
use rustryx_format_oci::{
    CatalogService, HmacTokenCodec, OciConfig, OciHandler, OciTokenService, TokenServiceConfig,
};
use rustryx_format_pub::{PubConfig, PubHandler};
use rustryx_format_pypi::PypiHandler;
use rustryx_format_raw::RawHandler;
use tokio_util::sync::CancellationToken;

use super::blobs::FormatStores;
use super::store::Repos;

/// The engine with the handles its owner needs at shutdown.
pub struct Formats {
    /// The engine the artifact routes serve through.
    pub engine: FormatEngine,
    /// Cancelled at shutdown: cache fills stop streaming, commit phases still finish.
    pub shutdown: CancellationToken,
}

/// Issuer of registry bearers. Distinct from the internal session JWT issuer, so a registry
/// token is not mistaken for a session and a session token is not mistaken for a registry token.
pub const REGISTRY_ISSUER: &str = "rustryx-registry";
/// Audience and `service` parameter of the distribution token endpoint.
pub const REGISTRY_SERVICE: &str = "rustryx";

/// The registered format handlers, one per [`RepoFormat`].
///
/// # Errors
/// Two handlers claim the same format. That is a programming error in this function.
pub fn registry() -> anyhow::Result<FormatRegistry> {
    let mut registry = FormatRegistry::new();
    for handler in handlers() {
        registry
            .register(handler)
            .map_err(|error| anyhow!("format registry: {error}"))?;
    }
    Ok(registry)
}

fn handlers() -> Vec<Arc<dyn FormatHandler>> {
    vec![
        Arc::new(MavenHandler::new()),
        Arc::new(NpmHandler::new()),
        Arc::new(PypiHandler::new()),
        Arc::new(OciHandler::new(OciConfig::default())),
        Arc::new(HelmHandler::new()),
        Arc::new(RawHandler::new()),
        Arc::new(PubHandler::new(PubConfig::default())),
    ]
}

/// The token service and the cross-repository catalog, sharing one RBAC handle.
///
/// # Errors
/// The registry signing key cannot be built.
pub fn open_distribution(
    repositories: Arc<dyn RepositoryRepo>,
    components: Arc<dyn ComponentRepo>,
    rbac: Arc<dyn RbacChecker>,
    clock: Arc<dyn Clock>,
    cookie_key: &rustryx_core::Secret,
) -> anyhow::Result<(OciTokenService, CatalogService)> {
    let tokens = token_service(
        Arc::clone(&repositories),
        Arc::clone(&rbac),
        clock,
        cookie_key,
    )?;
    let catalog = CatalogService::new(repositories, components, rbac, OciConfig::default());
    Ok((tokens, catalog))
}

/// The token service `/v2/token` signs with. The key is a hash of the cookie key, so it
/// follows `auth.cookie_key` across restarts and is not the cookie key itself.
///
/// # Errors
/// The derived key is shorter than the codec accepts. A SHA-256 digest is 32 bytes, so this
/// is a bug in the derivation, not a configuration problem.
pub fn token_service(
    repositories: Arc<dyn RepositoryRepo>,
    rbac: Arc<dyn RbacChecker>,
    clock: Arc<dyn Clock>,
    cookie_key: &rustryx_core::Secret,
) -> anyhow::Result<OciTokenService> {
    let config = TokenServiceConfig {
        service: REGISTRY_SERVICE.to_owned(),
        issuer: REGISTRY_ISSUER.to_owned(),
        ..TokenServiceConfig::default()
    };
    let codec = Arc::new(
        HmacTokenCodec::new(&registry_key(cookie_key), &config.issuer, &config.service)
            .map_err(|error| anyhow!("registry token key: {error}"))?,
    );
    Ok(OciTokenService::new(
        repositories,
        rbac,
        Arc::clone(&codec) as Arc<dyn rustryx_format_oci::TokenIssuer>,
        codec,
        clock,
        config,
    ))
}

fn registry_key(cookie_key: &rustryx_core::Secret) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(cookie_key.expose().as_bytes());
    let mut key = [0_u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// The SSRF policy of `upstream.*`.
///
/// An empty `allowed_hosts` means any public host (the address filter still rejects private,
/// loopback, link-local and metadata addresses); a list narrows it to those hosts. Plain `http`
/// is possible only when `http` is among the allowed schemes AND the host is in
/// `allow_insecure_http_hosts`: the policy accepts it for those hosts only.
///
/// # Errors
/// A host pattern is not a valid host or `*.suffix` (validation reports it first).
pub fn url_policy(upstream: &UpstreamConfig) -> anyhow::Result<UpstreamUrlPolicy> {
    let hosts = if upstream.allowed_hosts.is_empty() {
        HostAllowlist::AnyPublic
    } else {
        HostAllowlist::Only(patterns(&upstream.allowed_hosts, "upstream.allowed_hosts")?)
    };
    let http_allowed = upstream
        .allowed_schemes
        .iter()
        .any(|scheme| scheme.eq_ignore_ascii_case("http"));
    let http_hosts = if http_allowed {
        patterns(
            &upstream.allow_insecure_http_hosts,
            "upstream.allow_insecure_http_hosts",
        )?
    } else {
        Vec::new()
    };
    Ok(UpstreamUrlPolicy {
        hosts,
        http_hosts,
        allow_cidrs: upstream.allow_private_cidrs.clone(),
        max_redirects: u8::try_from(upstream.max_redirects).unwrap_or(u8::MAX),
        ..UpstreamUrlPolicy::default()
    })
}

fn patterns(raw: &[String], key: &str) -> anyhow::Result<Vec<HostPattern>> {
    raw.iter()
        .map(|pattern| {
            HostPattern::parse(pattern)
                .map_err(|error| anyhow!("{key}: `{pattern}` is not a valid host pattern: {error}"))
        })
        .collect()
}

fn limits(config: &AppConfig) -> Limits {
    Limits {
        max_blob_bytes: config.limits.max_blob_bytes,
        body_idle_timeout: config.limits.body_idle_timeout(),
        blocking_permits: config.limits.effective_blocking_permits(),
        blocking_permit_wait: config.limits.permit_wait(),
        ..Limits::default()
    }
}

fn executor(upstream: &UpstreamConfig) -> ExecutorConfig {
    ExecutorConfig {
        metadata_timeout: upstream.metadata_timeout(),
        idle_timeout: upstream.idle_timeout(),
        retry: RetryPolicy {
            max_attempts: upstream.max_attempts,
            ..RetryPolicy::default()
        },
        ..ExecutorConfig::default()
    }
}

/// Builds the engine over the repositories and the open blob stores.
///
/// Download counters are batched: every client read is announced to a [`DownloadBatcher`],
/// flushed into `AssetRepo::record_downloads` every `downloads.flush_interval_secs` and once
/// more at shutdown, inside the engine's own task tracker (so `FormatEngine::drain` waits for it).
///
/// # Errors
/// The SSRF policy is invalid or the upstream HTTP client cannot be built.
pub fn build(
    config: &AppConfig,
    repos: &Repos,
    rbac: Arc<dyn RbacChecker>,
    blobs: Arc<BlobStoreRegistry>,
) -> anyhow::Result<Formats> {
    let batcher = DownloadBatcher::new(
        Arc::new(NoopEvents) as Arc<dyn ContentEvents>,
        DownloadBatcher::DEFAULT_MAX_PENDING,
    );
    let ports = Ports {
        repositories: Arc::clone(&repos.repositories),
        blob_store_defs: Arc::clone(&repos.blob_stores),
        assets: Arc::clone(&repos.assets),
        components: Arc::clone(&repos.components),
        blobs: Arc::clone(&repos.blobs),
        rbac,
        audit: Arc::clone(&repos.audit),
        clock: Arc::clone(&repos.clock),
        ids: Arc::new(UuidV7IdGen),
        blob_stores: Arc::new(FormatStores::new(blobs)),
        routing: Arc::new(RuleRoutingGate::new(Arc::clone(&repos.routing_rules))),
        events: Arc::clone(&batcher) as Arc<dyn ContentEvents>,
        // The port guards enforce immutability, holds and quota inside their transactions; this
        // hook only refuses earlier, and nothing refuses early until a policy is wired.
        policy: Arc::new(NoopContentPolicy),
    };
    let shutdown = CancellationToken::new();
    let upstream = &config.upstream;
    let engine = FormatEngine::builder(ports, registry()?)
        .limits(limits(config))
        .url_policy(url_policy(upstream)?)
        .client_config(ReqwestClientConfig {
            connect_timeout: upstream.connect_timeout(),
            ..ReqwestClientConfig::default()
        })
        .executor_config(executor(upstream))
        .shutdown_token(shutdown.clone())
        .build()
        .map_err(|error| anyhow!("upstream HTTP client: {error}"))
        .context("cannot build the format engine")?;
    batcher.spawn_flusher(
        Arc::clone(&repos.assets),
        config
            .downloads
            .flush_interval()
            .max(Duration::from_secs(1)),
        shutdown.clone(),
        engine.tasks(),
    );
    Ok(Formats { engine, shutdown })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream() -> UpstreamConfig {
        UpstreamConfig::default()
    }

    #[test]
    fn no_host_list_means_any_public_host_over_https_only() {
        let policy = url_policy(&upstream()).unwrap();
        assert_eq!(policy.hosts, HostAllowlist::AnyPublic);
        assert!(policy.http_hosts.is_empty());
        assert!(policy.allow_cidrs.is_empty());
        assert_eq!(policy.max_redirects, 3);
    }

    #[test]
    fn a_host_list_narrows_the_policy_to_those_hosts() {
        let mut config = upstream();
        config.allowed_hosts = vec!["repo.example.com".into(), "*.mirror.example".into()];
        let policy = url_policy(&config).unwrap();
        assert_eq!(
            policy.hosts,
            HostAllowlist::Only(vec![
                HostPattern::Exact("repo.example.com".into()),
                HostPattern::Subdomains("mirror.example".into()),
            ])
        );
    }

    #[test]
    fn must_not_accept_plain_http_hosts_unless_http_is_an_allowed_scheme() {
        let mut config = upstream();
        config.allow_insecure_http_hosts = vec!["internal.example".into()];
        assert!(
            url_policy(&config).unwrap().http_hosts.is_empty(),
            "listing a host does not enable the scheme"
        );
        config.allowed_schemes = vec!["https".into(), "http".into()];
        assert_eq!(url_policy(&config).unwrap().http_hosts.len(), 1);
    }

    #[test]
    fn internal_ranges_and_the_redirect_limit_reach_the_policy() {
        let mut config = upstream();
        config.allow_private_cidrs = vec!["10.20.0.0/16".parse().unwrap()];
        config.max_redirects = 1;
        let policy = url_policy(&config).unwrap();
        assert_eq!(policy.allow_cidrs.len(), 1);
        assert_eq!(policy.max_redirects, 1);
    }

    #[test]
    fn a_wildcard_in_the_middle_of_a_host_is_refused() {
        let mut config = upstream();
        config.allowed_hosts = vec!["a.*.example".into()];
        let error = url_policy(&config).expect_err("not a pattern");
        assert!(format!("{error}").contains("upstream.allowed_hosts"));
    }

    #[test]
    fn upstream_timeouts_and_attempts_reach_the_executor() {
        let mut config = upstream();
        config.metadata_timeout_secs = 7;
        config.idle_timeout_secs = 9;
        config.max_attempts = 2;
        let executor = executor(&config);
        assert_eq!(executor.metadata_timeout, Duration::from_secs(7));
        assert_eq!(executor.idle_timeout, Duration::from_secs(9));
        assert_eq!(executor.retry.max_attempts, 2);
    }

    #[test]
    fn every_format_has_one_handler() {
        use rustryx_core::model::RepoFormat;
        let registry = registry().expect("handlers do not overlap");
        let mut got = registry.formats();
        let mut expect = RepoFormat::ALL.to_vec();
        got.sort();
        expect.sort();
        assert_eq!(got, expect);
    }

    #[test]
    fn limits_follow_the_limits_section() {
        let mut config = AppConfig::default();
        config.limits.max_blob_bytes = 1234;
        config.limits.blocking_permits = 3;
        config.limits.permit_wait_ms = 250;
        let limits = limits(&config);
        assert_eq!(limits.max_blob_bytes, 1234);
        assert_eq!(limits.blocking_permits, 3);
        assert_eq!(limits.blocking_permit_wait, Duration::from_millis(250));
    }
}
