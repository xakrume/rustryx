//! The composition root: configuration to settings, and the list of components.
//!
//! This is the one file the lead edits to integrate the other crates. [`assemble`]
//! builds [`Components`] from the configuration; today it wires nothing that does
//! not exist, so the node starts, answers `/healthz`, reports `/readyz` 503 (no
//! database) and serves no repository routes. The checklist of what replaces
//! what is in `docs/server.md`.

use std::sync::Arc;

use rustryx_api::{
    ApiSettings, AppState, Lifecycle, MetricsAccess, NotWiredProbe, RateLimitSettings,
    ReadinessProbe, RouteModule,
};
use rustryx_config::{AppConfig, LogFormat, ObservabilityConfig, OtlpProtocol};
use rustryx_core::ports::{Authenticator, RbacChecker};
use rustryx_observability::LogFormat as ObsLogFormat;
use rustryx_observability::{
    MetricsHandle, ObservabilityOptions, OtlpOptions, OtlpProtocol as ExportProtocol,
};

use crate::listener::ConnectionOptions;
use crate::migrate::{Migrator, NotWiredMigrator};
use crate::supervisor::BackgroundTask;

/// Everything the server runs with, produced by [`assemble`].
pub struct Components {
    /// Readiness probe (database ping and schema version).
    pub readiness: Arc<dyn ReadinessProbe>,
    /// Authenticator chain (local, token, OIDC bearer, LDAP). `None` means every
    /// request that presents credentials is answered 503.
    pub authenticator: Option<Arc<dyn Authenticator>>,
    /// Authorization checker. `None` means [`AppState::authorize`] answers 503.
    pub rbac: Option<Arc<dyn RbacChecker>>,
    /// Route modules: `/service/rest/v1`, `/repository/{name}/...`, `/v2/...`.
    pub modules: Vec<Arc<dyn RouteModule>>,
    /// Long-lived background tasks (schedulers, garbage collection, cache
    /// invalidation listener), run under the supervisor.
    pub background: Vec<BackgroundTask>,
}

impl Components {
    /// What exists today: nothing but a probe that reports "not ready".
    #[must_use]
    pub fn not_wired() -> Self {
        Self {
            readiness: Arc::new(NotWiredProbe),
            authenticator: None,
            rbac: None,
            modules: Vec::new(),
            background: Vec::new(),
        }
    }
}

/// Builds the components from the configuration. Runs inside the async runtime,
/// before the listener opens.
///
/// Replace the body as crates become available. Constraints from the requirements:
/// start without Keycloak, LDAP, Redis or S3 being reachable (retry in the
/// background, never fail here), and never run migrations.
///
/// # Errors
/// A component that is mandatory for serving could not be built.
#[allow(clippy::unused_async)] // The real assembly connects to PostgreSQL; the signature is the contract.
pub async fn assemble(_config: &AppConfig, _metrics: &MetricsHandle) -> anyhow::Result<Components> {
    Ok(Components::not_wired())
}

/// The implementation behind `rustryx migrate`.
#[must_use]
pub fn migrator() -> Box<dyn Migrator> {
    Box::new(NotWiredMigrator)
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

    #[tokio::test]
    async fn nothing_is_wired_yet_and_the_node_says_so() {
        let config = AppConfig::default();
        let metrics = MetricsHandle::new().expect("metrics");
        let components = assemble(&config, &metrics).await.expect("assembles");
        assert!(components.authenticator.is_none());
        assert!(components.rbac.is_none());
        assert!(components.modules.is_empty());
        assert!(components.background.is_empty());
        let report = components.readiness.check().await;
        assert_eq!(report.database, rustryx_api::Outcome::Failed);
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
