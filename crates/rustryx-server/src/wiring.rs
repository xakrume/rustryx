//! The composition root: the configuration becomes settings and components.
//!
//! [`assemble`] builds every component from the configuration and returns them as
//! [`Components`]; `serve::run_server` runs them. The pieces live in the submodules:
//!
//! | module | what it wires |
//! |---|---|
//! | `database` | the lazily connected pool, the sealer for stored secrets, readiness, `rustryx migrate` |
//! | `store` | the repositories as trait objects |
//! | `bus` | one `LISTEN` connection shared by every in-process subscriber |
//! | `identity` | passwords, the token service, login guard, group mapping, JIT, Keycloak, LDAP, the chain |
//! | `sso` | the adapters between the HTTP layer's session ports and `rustryx-auth` |
//! | `blobs` | the registry of open stores, its adapter for the format engine, the default store seed |
//! | `formats` | the format engine and the SSRF policy of `upstream.*` |
//! | `artifacts` | `/repository/{name}/...` and `/v2/...` |
//! | `bootstrap` | the bootstrap administrator and the default store, once the schema is servable |
//! | `maintenance` | audit partitions, revoked token and login counter purges |
//!
//! Constraints from the requirements that hold for everything here: the node starts without
//! PostgreSQL, Keycloak, LDAP or S3 being reachable (the pool connects lazily, discovery runs in
//! the background, LDAP binds on the first login), and it never migrates: `rustryx migrate` is a
//! separate command and the readiness probe says when the schema is there.

mod artifacts;
mod blobs;
pub mod bootstrap;
mod bus;
mod database;
mod formats;
mod identity;
pub mod maintenance;
mod registry_login;
mod sso;
mod store;

#[cfg(test)]
mod chain_tests;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::future::BoxFuture;
use rustryx_api::{
    ApiSettings, AppState, AuthApiDeps, AuthApiSettings, CsrfKey, Lifecycle, MetricsAccess,
    NativeAuthModule, NotWiredProbe, OidcLoginPort, RateLimitSettings, ReadinessProbe, RouteModule,
    SessionManager, SessionSettings,
};
use rustryx_config::{AppConfig, LockBackendKind, LogFormat, ObservabilityConfig, OtlpProtocol};
use rustryx_core::ports::{Authenticator, InvalidationBus, LockBackend, RbacChecker};
use rustryx_db::{PgLockBackend, PgStore};
use rustryx_observability::LogFormat as ObsLogFormat;
use rustryx_observability::{
    MetricsHandle, ObservabilityOptions, OtlpOptions, OtlpProtocol as ExportProtocol,
    run_runtime_sampler,
};
use rustryx_rbac::{Rbac, RbacConfig};
use url::Url;

use self::artifacts::ArtifactModule;
use self::bootstrap::{AdminSeed, Bootstrap};
use self::bus::SharedBus;
use self::database::{PgMigrator, PgReadiness};
use self::maintenance::Maintenance;
use self::sso::JwtSessionIssuer;
use self::store::Repos;
use crate::listener::ConnectionOptions;
use crate::migrate::Migrator;
use crate::shutdown::Finalizer;
use crate::supervisor::BackgroundTask;

/// Interval of the runtime and process sampler.
const RUNTIME_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
/// How long the shutdown waits for the session revocations that were waiting out their grace.
const SESSION_DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Everything the server runs with, produced by [`assemble`].
pub struct Components {
    /// Readiness probe (database ping and schema version).
    pub readiness: Arc<dyn ReadinessProbe>,
    /// Authenticator chain (local, token, internal JWT, OIDC bearer, LDAP). `None` means every
    /// request that presents credentials is answered 503.
    pub authenticator: Option<Arc<dyn Authenticator>>,
    /// Authorization checker. `None` means [`AppState::authorize`] answers 503.
    pub rbac: Option<Arc<dyn RbacChecker>>,
    /// The cookie session of the web UI. `None` turns the cookie path off.
    pub sessions: Option<Arc<SessionManager>>,
    /// Registry bearers on `/v2`. `None` leaves an unrecognised bearer as a 401.
    pub registry_bearer: Option<Arc<dyn rustryx_api::RegistryBearerTokens>>,
    /// Route modules: the native API under `/api/v1`, `/repository/{name}/...`, `/v2/...`.
    pub modules: Vec<Arc<dyn RouteModule>>,
    /// Long-lived background tasks (invalidation listeners, identity provider discovery,
    /// maintenance, samplers), run under the supervisor.
    pub background: Vec<BackgroundTask>,
    /// Steps of the final shutdown, in order (see `shutdown`).
    pub finalizers: Vec<Finalizer>,
}

impl Components {
    /// A node with nothing wired but a probe that reports "not ready".
    #[must_use]
    pub fn not_wired() -> Self {
        Self {
            readiness: Arc::new(NotWiredProbe),
            authenticator: None,
            rbac: None,
            sessions: None,
            registry_bearer: None,
            modules: Vec::new(),
            background: Vec::new(),
            finalizers: Vec::new(),
        }
    }
}

/// The lock backend of `lock.backend`. Only PostgreSQL advisory locks exist: a Redis backend is
/// refused instead of quietly replaced, because the port promises there is no automatic switch
/// between backends (a node on another backend would not exclude its peers).
fn lock_backend(config: &AppConfig) -> anyhow::Result<Arc<dyn LockBackend>> {
    if config.lock.backend == LockBackendKind::Redis {
        anyhow::bail!(
            "lock.backend is redis, but this build has no Redis lock backend: set lock.backend \
             to postgres"
        );
    }
    let url = database::direct_url(config)?;
    let backend = PgLockBackend::new(&url)
        .map_err(|error| anyhow::anyhow!("cannot set up the lock backend: {error}"))?;
    Ok(Arc::new(backend))
}

fn session_manager(
    config: &AppConfig,
    identity: &identity::Identity,
    repos: &Repos,
    cookie_key: &rustryx_core::Secret,
    bus: Arc<dyn InvalidationBus>,
) -> anyhow::Result<Arc<SessionManager>> {
    let session = &config.auth.session;
    let csrf = CsrfKey::from_cookie_key(cookie_key)
        .map_err(|_| anyhow::anyhow!("auth.cookie_key cannot derive the CSRF key"))?;
    Ok(Arc::new(SessionManager::new(
        SessionSettings::new(
            Duration::from_secs(session.idle_timeout_secs),
            Duration::from_secs(session.absolute_timeout_secs),
        ),
        Arc::new(JwtSessionIssuer::new(
            Arc::clone(&identity.jwt),
            Arc::clone(&identity.service),
            bus,
        )),
        Arc::clone(&repos.clock),
        csrf,
        // No SSO refresher exists: see the module docs of `sso`.
        None,
    )))
}

fn native_auth_module(
    config: &AppConfig,
    identity: &identity::Identity,
    repos: &Repos,
) -> Arc<dyn RouteModule> {
    let mut deps = AuthApiDeps::new(
        Arc::clone(&repos.users),
        Arc::clone(&repos.roles),
        Arc::clone(&repos.tokens),
        Arc::clone(&repos.audit),
        Arc::clone(&repos.clock),
    );
    deps.oidc = identity
        .keycloak
        .as_ref()
        .map(|login| Arc::clone(login) as Arc<dyn OidcLoginPort>);
    let mut settings = AuthApiSettings::default();
    settings.ldap_enabled = identity.ldap_enabled;
    settings.base_url_configured = config.server.base_url.is_some();
    deps.settings = settings;
    Arc::new(NativeAuthModule::new(deps))
}

fn admin_seed(config: &AppConfig) -> Option<AdminSeed> {
    let admin = &config.auth.bootstrap_admin;
    admin.password.as_ref().map(|password| AdminSeed {
        username: admin.username.clone(),
        password: password.to_secret(),
    })
}

/// Builds the components from the configuration. Runs inside the async runtime, before the
/// listener opens.
///
/// Nothing here waits for a dependency: the pool is lazy, Keycloak discovery and the LDAP
/// connection happen later, and no migration runs. What can fail is configuration that cannot
/// work (a malformed key, an invalid provider setting, an unsupported lock backend).
///
/// # Errors
/// A component that is mandatory for serving could not be built.
pub async fn assemble(config: &AppConfig, metrics: &MetricsHandle) -> anyhow::Result<Components> {
    let pool = database::connect_lazy(config)?;
    let store = PgStore::new(pool.clone(), database::sealer(config)?);
    let repos = Repos::from_store(&store);
    let locks = lock_backend(config)?;
    let bus = SharedBus::new(Arc::new(
        store.invalidation_bus(database::direct_url(config)?),
    ));

    let cookie_key = identity::cookie_key(config);
    let identity = identity::build(config, &repos, &cookie_key).await?;
    let rbac = Arc::new(Rbac::new(
        Arc::clone(&repos.roles),
        Arc::clone(&repos.selectors),
        &RbacConfig {
            anonymous_enabled: config.auth.anonymous_enabled,
            ..RbacConfig::default()
        },
    ));

    let blob_registry = blobs::registry(Arc::clone(&repos.blob_stores));
    let (tokens, catalog) = formats::open_distribution(
        Arc::clone(&repos.repositories),
        Arc::clone(&repos.components),
        Arc::clone(&rbac) as Arc<dyn RbacChecker>,
        Arc::clone(&repos.clock),
        &cookie_key,
    )?;
    let registry_bearer = Arc::new(registry_login::RegistryLogin::new(
        tokens.clone(),
        Arc::clone(&repos.users),
    ));
    let formats = formats::build(
        config,
        &repos,
        Arc::clone(&rbac) as Arc<dyn RbacChecker>,
        Arc::clone(&blob_registry),
    )?;
    let base_url = config
        .server
        .base_url
        .as_deref()
        .map(Url::parse)
        .transpose()
        .context("server.base_url is not a valid URL")?;

    let mut background = vec![
        bus.pump_task(),
        rbac_listener(&rbac, &bus),
        auth_invalidation(&identity, &bus),
        blobs::invalidation_task(Arc::clone(&blob_registry), Arc::new(bus.clone())),
        database::pool_sampler(pool.clone(), metrics.clone()),
        runtime_sampler(metrics),
        Maintenance {
            pool: pool.clone(),
            revoked: Arc::clone(&repos.revoked_tokens),
            locks,
            lock_ttl: config.lock.ttl(),
            metrics: metrics.clone(),
        }
        .task(),
        Bootstrap {
            pool: pool.clone(),
            passwords: Arc::clone(&identity.passwords),
            admin: admin_seed(config),
            blob: config.blob.clone(),
            definitions: Arc::clone(&repos.blob_stores),
            registry: Arc::clone(&blob_registry),
            clock: Arc::clone(&repos.clock),
        }
        .task(),
    ];
    background.extend(identity.background.iter().cloned());

    let modules = route_modules(
        config, &identity, &repos, &formats, base_url, tokens, catalog,
    );

    let sessions = session_manager(config, &identity, &repos, &cookie_key, Arc::new(bus))?;
    let finalizers = shutdown_finalizers(
        Arc::clone(&sessions),
        formats.engine.clone(),
        formats.shutdown.clone(),
        Arc::clone(&blob_registry),
        pool.clone(),
    );

    Ok(Components {
        readiness: Arc::new(PgReadiness::new(pool, metrics.clone())),
        authenticator: Some(Arc::clone(&identity.service) as Arc<dyn Authenticator>),
        rbac: Some(rbac),
        sessions: Some(sessions),
        registry_bearer: Some(registry_bearer),
        modules,
        background,
        finalizers,
    })
}

fn route_modules(
    config: &AppConfig,
    identity: &identity::Identity,
    repos: &Repos,
    formats: &formats::Formats,
    base_url: Option<url::Url>,
    tokens: rustryx_format_oci::OciTokenService,
    catalog: rustryx_format_oci::CatalogService,
) -> Vec<Arc<dyn RouteModule>> {
    vec![
        native_auth_module(config, identity, repos),
        Arc::new(ArtifactModule::new(
            formats.engine.clone(),
            Arc::clone(&repos.repositories),
            base_url,
            config.server.require_https,
            tokens,
            catalog,
            Some(Arc::clone(&identity.service) as Arc<dyn Authenticator>),
        )),
    ]
}

/// Shutdown steps, in order. Session revocations go first, while the pool is still open.
fn shutdown_finalizers(
    sessions: Arc<SessionManager>,
    engine: rustryx_format_core::FormatEngine,
    engine_stop: tokio_util::sync::CancellationToken,
    registry: Arc<rustryx_blob::BlobStoreRegistry>,
    pool: sqlx::PgPool,
) -> Vec<Finalizer> {
    vec![
        Finalizer::new("session_revocations", async move {
            // The manager logs what it could not finish.
            let _unfinished = sessions.drain(SESSION_DRAIN_DEADLINE).await;
        }),
        Finalizer::new("format_engine", async move {
            engine_stop.cancel();
            engine.drain().await;
        }),
        Finalizer::new("blob_stores", async move { registry.shutdown().await }),
        Finalizer::new("database_pool", async move { pool.close().await }),
    ]
}

/// The RBAC caches follow the invalidation bus. The listener is started inside the task, so a
/// restart by the supervisor starts a fresh one.
fn rbac_listener(rbac: &Arc<Rbac>, bus: &SharedBus) -> BackgroundTask {
    let rbac = Arc::clone(rbac);
    let bus = bus.clone();
    BackgroundTask::new("rbac_invalidation", move |cancel| {
        let listener = rbac.spawn_invalidation_listener(&bus, cancel);
        async move {
            if let Err(error) = listener.join().await {
                tracing::error!(%error, "the rbac invalidation listener failed");
            }
        }
    })
}

fn auth_invalidation(identity: &identity::Identity, bus: &SharedBus) -> BackgroundTask {
    let service = Arc::clone(&identity.service);
    let bus = bus.clone();
    BackgroundTask::new("auth_invalidation", move |cancel| {
        // The task is `'static` by its signature but its type still names the lifetimes of the
        // borrows it was made from; boxing erases them, which a closure result cannot do alone.
        let task: BoxFuture<'static, ()> = Box::pin(service.invalidation_task(&bus, cancel));
        task
    })
}

fn runtime_sampler(metrics: &MetricsHandle) -> BackgroundTask {
    let metrics = metrics.clone();
    BackgroundTask::new("runtime_sampler", move |cancel| {
        run_runtime_sampler(metrics.clone(), RUNTIME_SAMPLE_INTERVAL, cancel)
    })
}

/// The implementation behind `rustryx migrate`.
#[must_use]
pub fn migrator() -> Box<dyn Migrator> {
    Box::new(PgMigrator)
}

/// Builds the shared state of the HTTP layer.
#[must_use]
pub fn app_state(
    config: &AppConfig,
    components: &Components,
    lifecycle: Lifecycle,
    metrics: MetricsHandle,
) -> AppState {
    let mut state = AppState::new(components.readiness.clone())
        .with_settings(api_settings(config))
        .with_lifecycle(lifecycle);
    if config.observability.metrics_enabled {
        state = state.with_metrics(metrics);
    }
    if let Some(authenticator) = &components.authenticator {
        state = state.with_authenticator(authenticator.clone());
    }
    if let Some(rbac) = &components.rbac {
        state = state.with_rbac(rbac.clone());
    }
    if let Some(sessions) = &components.sessions {
        state = state.with_sessions(sessions.clone());
    }
    if let Some(bearer) = &components.registry_bearer {
        state = state.with_registry_bearer(bearer.clone());
    }
    state
}

/// Maps the configuration onto the HTTP layer's settings.
#[must_use]
pub fn api_settings(config: &AppConfig) -> ApiSettings {
    let mut settings = ApiSettings::default();
    let limits = &config.limits;
    settings.request_timeout = limits.request_timeout();
    settings.body_idle_timeout = limits.body_idle_timeout();
    settings.max_body_bytes = limits.max_body_bytes();
    settings.max_blob_bytes = limits.max_blob_bytes;
    settings.max_in_flight = limits.max_in_flight_requests as usize;
    settings
        .cors_allowed_origins
        .clone_from(&config.server.cors_allowed_origins);
    settings
        .trusted_proxies
        .clone_from(&config.server.trusted_proxies);
    settings.anonymous_enabled = config.auth.anonymous_enabled;
    settings.require_https = config.server.require_https;
    settings.service_origin.clone_from(&config.server.base_url);
    settings.rate_limit = limits.rate_limit.enabled.then_some(RateLimitSettings {
        requests_per_second: limits.rate_limit.requests_per_second,
        burst: limits.rate_limit.burst,
    });
    settings.metrics_access = metrics_access(&config.observability);
    settings
}

fn metrics_access(observability: &ObservabilityConfig) -> MetricsAccess {
    if !observability.metrics_enabled {
        MetricsAccess::Disabled
    } else if observability.metrics_public {
        MetricsAccess::Public
    } else if let Some(token) = &observability.metrics_token {
        MetricsAccess::Bearer(token.to_secret())
    } else {
        // Only reachable in development: validation refuses this in production.
        MetricsAccess::Public
    }
}

/// Maps the configuration onto the observability options.
#[must_use]
pub fn observability_options(config: &AppConfig) -> ObservabilityOptions {
    let obs = &config.observability;
    ObservabilityOptions {
        log_filter: obs.log_level.clone(),
        log_format: match obs.log_format {
            LogFormat::Json => ObsLogFormat::Json,
            LogFormat::Text => ObsLogFormat::Text,
        },
        service_name: obs.service_name.clone(),
        otlp: obs.otlp_endpoint.as_ref().map(|endpoint| OtlpOptions {
            endpoint: endpoint.clone(),
            protocol: match obs.otlp_protocol {
                OtlpProtocol::Grpc => ExportProtocol::Grpc,
                OtlpProtocol::Http => ExportProtocol::Http,
            },
            sample_ratio: obs.otlp_sample_ratio,
        }),
    }
}

/// Maps the configuration onto the connection loop's options.
#[must_use]
pub fn connection_options(config: &AppConfig) -> ConnectionOptions {
    ConnectionOptions {
        header_read_timeout: config.limits.header_read_timeout(),
        max_connections: config.limits.max_connections as usize,
    }
}

#[cfg(test)]
mod tests {
    use rustryx_config::ConfigSecret;

    use super::*;

    #[test]
    fn limits_and_edge_settings_reach_the_http_layer() {
        let mut config = AppConfig::default();
        config.limits.max_body_mb = 3;
        config.limits.max_blob_bytes = 12_345;
        config.limits.max_in_flight_requests = 77;
        config.limits.request_timeout_secs = 90;
        config.limits.body_idle_timeout_secs = 15;
        config.server.cors_allowed_origins = vec!["https://ui.example".into()];
        config.server.trusted_proxies = vec!["10.0.0.0/8".parse().expect("cidr")];
        config.auth.anonymous_enabled = true;
        let settings = api_settings(&config);
        assert_eq!(
            settings.rate_limit,
            Some(RateLimitSettings {
                requests_per_second: 50,
                burst: 100
            }),
            "enabled by default with the specified 50 rps and burst 100"
        );
        assert_eq!(settings.max_body_bytes, 3 * 1024 * 1024);
        assert_eq!(settings.max_blob_bytes, 12_345);
        assert_eq!(settings.max_in_flight, 77);
        assert_eq!(settings.request_timeout.as_secs(), 90);
        assert_eq!(settings.body_idle_timeout.as_secs(), 15);
        assert_eq!(settings.cors_allowed_origins, ["https://ui.example"]);
        assert_eq!(settings.trusted_proxies.len(), 1);
        assert!(settings.anonymous_enabled);
    }

    #[test]
    fn rate_limit_can_be_switched_off() {
        let mut config = AppConfig::default();
        config.limits.rate_limit.enabled = false;
        assert_eq!(api_settings(&config).rate_limit, None);
    }

    #[test]
    fn metrics_access_follows_the_three_switches() {
        let mut obs = ObservabilityConfig::default();
        obs.metrics_enabled = false;
        assert!(matches!(metrics_access(&obs), MetricsAccess::Disabled));
        obs.metrics_enabled = true;
        obs.metrics_public = true;
        assert!(matches!(metrics_access(&obs), MetricsAccess::Public));
        obs.metrics_public = false;
        obs.metrics_token = Some(ConfigSecret::new("scrape-token"));
        assert!(matches!(metrics_access(&obs), MetricsAccess::Bearer(_)));
        obs.metrics_token = None;
        assert!(
            matches!(metrics_access(&obs), MetricsAccess::Public),
            "development fallback"
        );
    }

    #[test]
    fn must_not_print_the_metrics_token_from_the_derived_settings() {
        let mut config = AppConfig::default();
        config.observability.metrics_token = Some(ConfigSecret::new("canary-metrics-token"));
        let settings = api_settings(&config);
        assert!(!format!("{settings:?}").contains("canary-metrics-token"));
    }

    #[test]
    fn observability_options_carry_the_otlp_choice_only_when_an_endpoint_is_set() {
        let mut config = AppConfig::default();
        assert!(observability_options(&config).otlp.is_none());
        config.observability.otlp_endpoint = Some("http://collector:4318".into());
        config.observability.otlp_protocol = OtlpProtocol::Http;
        config.observability.otlp_sample_ratio = 0.25;
        config.observability.log_format = LogFormat::Text;
        config.observability.log_level = "debug".into();
        let options = observability_options(&config);
        let otlp = options.otlp.expect("otlp");
        assert_eq!(otlp.protocol, ExportProtocol::Http);
        assert!((otlp.sample_ratio - 0.25).abs() < f64::EPSILON);
        assert_eq!(options.log_format, ObsLogFormat::Text);
        assert_eq!(options.log_filter, "debug");
    }

    #[test]
    fn connection_options_come_from_limits() {
        let mut config = AppConfig::default();
        config.limits.header_read_timeout_secs = 7;
        config.limits.max_connections = 123;
        let options = connection_options(&config);
        assert_eq!(options.header_read_timeout.as_secs(), 7);
        assert_eq!(options.max_connections, 123);
    }

    /// A configuration that assembles without any service behind it: the database URL points at
    /// a port nobody listens on, and nothing connects at assembly.
    fn offline_config() -> AppConfig {
        let mut config = AppConfig::default();
        config.environment = rustryx_config::Environment::Development;
        config.database.url = Some(ConfigSecret::new("postgres://u:p@127.0.0.1:1/rustryx"));
        config.database.acquire_timeout_secs = 1;
        config.server.base_url = Some("https://repo.example".into());
        config
    }

    #[tokio::test]
    async fn assembly_needs_no_database_and_wires_the_whole_node() {
        let metrics = MetricsHandle::new().expect("metrics");
        let components = assemble(&offline_config(), &metrics)
            .await
            .expect("assembles without reaching any service");
        assert!(components.authenticator.is_some());
        assert!(components.rbac.is_some());
        assert!(components.sessions.is_some());
        assert_eq!(components.modules.len(), 2, "native auth and artifacts");
        let names: Vec<_> = components.background.iter().map(|task| task.name).collect();
        for expected in [
            "invalidation_bus",
            "rbac_invalidation",
            "auth_invalidation",
            "blob_registry_invalidation",
            "db_pool_sampler",
            "runtime_sampler",
            "db_maintenance",
            "bootstrap",
        ] {
            assert!(
                names.contains(&expected),
                "missing task {expected}: {names:?}"
            );
        }
        assert!(
            !names.contains(&"oidc_discovery"),
            "no discovery task while OIDC is off"
        );
        let finalizers: Vec<_> = components.finalizers.iter().map(Finalizer::name).collect();
        assert_eq!(
            finalizers,
            [
                "session_revocations",
                "format_engine",
                "blob_stores",
                "database_pool"
            ],
            "the session revocations run first and the pool closes last"
        );
        let report = components.readiness.check().await;
        assert_eq!(report.database, rustryx_api::Outcome::Failed);
    }

    #[tokio::test]
    async fn the_final_steps_run_cleanly_on_an_idle_node() {
        let metrics = MetricsHandle::new().expect("metrics");
        let components = assemble(&offline_config(), &metrics).await.unwrap();
        let report =
            crate::shutdown::run_all(components.finalizers, std::time::Duration::from_secs(10))
                .await;
        assert_eq!(report.abandoned, 0);
        assert_eq!(report.finished, 4);
    }

    #[tokio::test]
    async fn a_redis_lock_backend_is_refused_instead_of_replaced() {
        let mut config = offline_config();
        config.lock.backend = rustryx_config::LockBackendKind::Redis;
        let metrics = MetricsHandle::new().expect("metrics");
        let error = assemble(&config, &metrics).await.err().expect("refused");
        assert!(format!("{error:#}").contains("lock.backend"), "{error:#}");
    }

    #[tokio::test]
    async fn oidc_adds_a_discovery_task_and_the_browser_login() {
        let mut config = offline_config();
        config.auth.oidc.enabled = true;
        config.auth.oidc.issuer = Some("https://kc.example/realms/r".into());
        config.auth.oidc.client_id = Some("rustryx-ui".into());
        config.auth.oidc.client_secret = Some(ConfigSecret::new("s3cret"));
        let metrics = MetricsHandle::new().expect("metrics");
        let components = assemble(&config, &metrics).await.expect("assembles");
        assert!(
            components
                .background
                .iter()
                .any(|task| task.name == "oidc_discovery")
        );
    }

    #[tokio::test]
    async fn a_malformed_key_stops_assembly_with_a_message_that_names_the_key() {
        let mut config = offline_config();
        config.database.sealing_key = Some(ConfigSecret::new("not-hex"));
        let metrics = MetricsHandle::new().expect("metrics");
        let error = assemble(&config, &metrics).await.err().expect("refused");
        let text = format!("{error:#}");
        assert!(text.contains("sealing"), "{text}");
        assert!(
            !text.contains("not-hex"),
            "the value is never echoed: {text}"
        );
    }

    #[test]
    fn the_session_and_https_settings_reach_the_http_layer() {
        let mut config = AppConfig::default();
        config.server.require_https = false;
        config.server.base_url = Some("https://repo.example".into());
        let settings = api_settings(&config);
        assert!(!settings.require_https);
        assert_eq!(
            settings.service_origin.as_deref(),
            Some("https://repo.example")
        );
        config.server.require_https = true;
        assert!(
            api_settings(&config).require_https,
            "on unless switched off"
        );
    }

    #[test]
    fn app_state_omits_metrics_when_disabled() {
        let mut config = AppConfig::default();
        config.observability.metrics_enabled = false;
        let state = app_state(
            &config,
            &Components::not_wired(),
            Lifecycle::new(),
            MetricsHandle::new().expect("metrics"),
        );
        assert!(state.metrics().is_none());
        config.observability.metrics_enabled = true;
        let state = app_state(
            &config,
            &Components::not_wired(),
            Lifecycle::new(),
            MetricsHandle::new().expect("metrics"),
        );
        assert!(state.metrics().is_some());
    }
}
