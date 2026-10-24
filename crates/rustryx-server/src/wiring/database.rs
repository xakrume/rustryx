//! PostgreSQL: the pool, the sealer for stored secrets, the readiness probe and the migrator.
//!
//! The pool is connected lazily. A node that cannot reach the database yet still starts, answers
//! `/healthz` and reports `/readyz` 503, and serves as soon as the database is there: nothing in
//! startup depends on a round trip (README 8.2, and the cascade argument in the readiness docs).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use rustryx_api::{Advisory, Outcome, ProbeReport, ReadinessProbe, SchemaStatus};
use rustryx_config::{AppConfig, DatabaseConfig};
use rustryx_core::Secret;
use rustryx_db::{AeadSealer, KeyEntry, PgConfig, SecretSealer};
use rustryx_observability::MetricsHandle;
use rustryx_observability::PoolStats;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::migrate::Migrator;
use crate::supervisor::BackgroundTask;

/// How often the pool gauges are refreshed.
const POOL_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// Pool settings from the configuration. A statement timeout of zero means "none", which is the
/// value an operator who wants PostgreSQL's own setting to rule writes.
#[must_use]
pub fn pg_config(database: &DatabaseConfig, url: Secret) -> PgConfig {
    let mut config = PgConfig::new(url);
    config.max_connections = database.pool_size;
    config.acquire_timeout = database.acquire_timeout();
    config.statement_timeout =
        (database.statement_timeout_secs > 0).then(|| database.statement_timeout());
    config
}

/// The pooled connection URL, or an error that names the key.
///
/// # Errors
/// `database.url` is not set (validation refuses that already; the check keeps this function
/// honest for callers that skip validation).
pub fn pooled_url(config: &AppConfig) -> anyhow::Result<Secret> {
    config
        .database
        .url
        .as_ref()
        .map(rustryx_config::ConfigSecret::to_secret)
        .context("database.url is not set")
}

/// The direct (not pooled) URL for migrations, advisory locks and `LISTEN`.
///
/// # Errors
/// Neither `database.direct_url` nor `database.url` is set.
pub fn direct_url(config: &AppConfig) -> anyhow::Result<Secret> {
    config
        .database
        .effective_direct_url()
        .map(rustryx_config::ConfigSecret::to_secret)
        .context("neither database.direct_url nor database.url is set")
}

/// Builds the lazily connected pool.
///
/// # Errors
/// The URL cannot be parsed. The message never contains the URL (it carries the password).
pub fn connect_lazy(config: &AppConfig) -> anyhow::Result<PgPool> {
    let pg = pg_config(&config.database, pooled_url(config)?);
    rustryx_db::connect_lazy(&pg)
        .map_err(|error| anyhow!("cannot set up the database pool: {error}"))
}

/// The sealer that protects stored upstream and system secrets.
///
/// Production validation requires `database.sealing_key`. Without one (development only) the
/// key is generated for this process: secrets sealed by it cannot be read after a restart, which
/// is loud enough in development and never silently weak, unlike a key baked into the binary.
///
/// # Errors
/// The configured ring is malformed.
pub fn sealer(config: &AppConfig) -> anyhow::Result<Arc<dyn SecretSealer>> {
    let ring = config
        .database
        .sealing_ring()
        .map_err(|error| anyhow!("database sealing key ring: {error}"))?;
    let Some(ring) = ring else {
        tracing::warn!(
            "database.sealing_key is not set: stored secrets are sealed with a key that exists \
             only in this process and cannot be read after a restart (development only)"
        );
        let mut bytes = [0_u8; 32];
        rand::fill(&mut bytes);
        let key = KeyEntry::from_bytes("ephemeral", bytes)
            .map_err(|error| anyhow!("ephemeral sealing key: {error}"))?;
        let sealer = AeadSealer::new(key, Vec::new())
            .map_err(|error| anyhow!("ephemeral sealer: {error}"))?;
        return Ok(Arc::new(sealer));
    };
    let current = KeyEntry::from_hex(&ring.current_id, &ring.current)
        .map_err(|error| anyhow!("database.sealing_key: {error}"))?;
    let previous = ring
        .previous
        .iter()
        .map(|(id, key)| {
            KeyEntry::from_hex(id, key)
                .map_err(|error| anyhow!("database.sealing_previous_keys `{id}`: {error}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let sealer = AeadSealer::new(current, previous)
        .map_err(|error| anyhow!("database sealing key ring: {error}"))?;
    Ok(Arc::new(sealer))
}

/// Readiness: the database answers a `SELECT 1` and its schema is one this binary serves.
///
/// Redis, Keycloak, LDAP and S3 are deliberately not here (see `rustryx_api::readiness`). The
/// probe also feeds the `db_up` gauge, so the alert and the probe agree by construction.
pub struct PgReadiness {
    pool: PgPool,
    metrics: MetricsHandle,
}

impl PgReadiness {
    /// A probe over `pool`.
    #[must_use]
    pub fn new(pool: PgPool, metrics: MetricsHandle) -> Self {
        Self { pool, metrics }
    }
}

/// Maps the database crate's verdict onto the HTTP layer's four states.
///
/// `OlderCompatible` is the rolling update case (the schema is newer by expand migrations only,
/// this binary is release N-1): served, as `Ahead`. The database's own `Ahead` means a contract
/// migration removed something this binary needs: not servable, and the HTTP layer has no word
/// for it, so it is `Unknown` there while the log names the versions.
const fn schema_state(status: rustryx_db::SchemaStatus) -> SchemaStatus {
    match status {
        rustryx_db::SchemaStatus::Current => SchemaStatus::UpToDate,
        rustryx_db::SchemaStatus::OlderCompatible { .. } => SchemaStatus::Ahead,
        rustryx_db::SchemaStatus::Behind { .. } => SchemaStatus::Behind,
        rustryx_db::SchemaStatus::Ahead { .. } => SchemaStatus::Unknown,
    }
}

#[async_trait]
impl ReadinessProbe for PgReadiness {
    async fn check(&self) -> ProbeReport {
        let reachable = match rustryx_db::ping(&self.pool).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "readiness: the database did not answer");
                false
            }
        };
        self.metrics.set_db_up(reachable);
        let schema = if reachable {
            match rustryx_db::schema_status(&self.pool).await {
                Ok(status) => {
                    if !status.is_serving() {
                        tracing::warn!(status = %status, ?status, "readiness: the schema cannot be served by this binary");
                    }
                    schema_state(status)
                }
                Err(error) => {
                    tracing::warn!(%error, "readiness: the schema version could not be read");
                    SchemaStatus::Unknown
                }
            }
        } else {
            SchemaStatus::Unknown
        };
        ProbeReport {
            database: if reachable {
                Outcome::Ok
            } else {
                Outcome::Failed
            },
            schema,
            advisories: Vec::<Advisory>::new(),
        }
    }
}

/// True when the schema can be served: the gate of everything that writes at startup (the
/// bootstrap administrator, the default blob store) and of the maintenance jobs.
pub async fn schema_is_serving(pool: &PgPool) -> bool {
    match rustryx_db::schema_status(pool).await {
        Ok(status) => status.is_serving(),
        Err(_) => false,
    }
}

/// The task that publishes the pool gauges.
#[must_use]
pub fn pool_sampler(pool: PgPool, metrics: MetricsHandle) -> BackgroundTask {
    BackgroundTask::new("db_pool_sampler", move |cancel| {
        let pool = pool.clone();
        let metrics = metrics.clone();
        async move { sample_pool(&pool, &metrics, &cancel).await }
    })
}

async fn sample_pool(pool: &PgPool, metrics: &MetricsHandle, cancel: &CancellationToken) {
    let mut ticker = tokio::time::interval(POOL_SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX);
        metrics.set_db_pool(PoolStats {
            size: pool.size(),
            idle,
        });
    }
}

/// `rustryx migrate`: applies the schema on the direct URL.
///
/// The database crate holds a PostgreSQL advisory lock for the run, so two migrators at once
/// cannot interleave, and the second finds nothing to do.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgMigrator;

#[async_trait]
impl Migrator for PgMigrator {
    async fn migrate(&self, config: &AppConfig) -> anyhow::Result<String> {
        let url = direct_url(config)?;
        let report = rustryx_db::migrate(&url)
            .await
            .map_err(|error| anyhow!("migration failed: {error}"))?;
        Ok(summary(&report))
    }
}

fn summary(report: &rustryx_db::MigrateReport) -> String {
    if report.applied.is_empty() {
        return format!("schema is current at version {}", report.schema_version);
    }
    let applied = report
        .applied
        .iter()
        .map(|m| format!("{} {}", m.version, m.description))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "applied {} migration(s): {applied}; schema is at version {}",
        report.applied.len(),
        report.schema_version
    )
}

#[cfg(test)]
mod tests {
    use rustryx_config::ConfigSecret;
    use rustryx_db::migrate::AppliedMigration;
    use rustryx_db::{MigrateReport, SchemaStatus as Db};

    use super::*;

    #[test]
    fn schema_verdicts_map_onto_the_four_states() {
        assert_eq!(schema_state(Db::Current), SchemaStatus::UpToDate);
        assert_eq!(
            schema_state(Db::OlderCompatible {
                applied: 9,
                expected: 8
            }),
            SchemaStatus::Ahead
        );
        assert_eq!(
            schema_state(Db::Behind {
                applied: 0,
                expected: 8
            }),
            SchemaStatus::Behind
        );
        assert!(
            !schema_state(Db::Ahead {
                applied: 9,
                expected: 8
            })
            .is_servable(),
            "a contract migration that this binary does not know must not be served"
        );
    }

    #[test]
    fn the_statement_timeout_of_zero_means_none() {
        let mut database = DatabaseConfig::default();
        database.statement_timeout_secs = 0;
        database.pool_size = 7;
        let pg = pg_config(&database, Secret::new("postgres://u:p@h/db"));
        assert_eq!(pg.statement_timeout, None);
        assert_eq!(pg.max_connections, 7);
        database.statement_timeout_secs = 12;
        let pg = pg_config(&database, Secret::new("postgres://u:p@h/db"));
        assert_eq!(pg.statement_timeout, Some(Duration::from_secs(12)));
    }

    #[test]
    fn the_direct_url_falls_back_to_the_pooled_one() {
        let mut config = AppConfig::default();
        assert!(direct_url(&config).is_err());
        assert!(pooled_url(&config).is_err());
        config.database.url = Some(ConfigSecret::new("postgres://pooled/db"));
        assert_eq!(
            direct_url(&config).unwrap().expose(),
            "postgres://pooled/db"
        );
        config.database.direct_url = Some(ConfigSecret::new("postgres://direct/db"));
        assert_eq!(
            direct_url(&config).unwrap().expose(),
            "postgres://direct/db"
        );
        assert_eq!(
            pooled_url(&config).unwrap().expose(),
            "postgres://pooled/db"
        );
    }

    #[test]
    fn must_not_leak_the_password_when_the_url_is_invalid() {
        let mut config = AppConfig::default();
        config.database.url = Some(ConfigSecret::new("postgres://app:canary-pw@db:notaport/x"));
        let error = connect_lazy(&config).expect_err("invalid url");
        assert!(!format!("{error:#} {error:?}").contains("canary-pw"));
    }

    #[test]
    fn a_configured_sealing_ring_builds_and_a_malformed_one_is_refused() {
        let mut config = AppConfig::default();
        config.database.sealing_key = Some(ConfigSecret::new("ab".repeat(32)));
        assert!(sealer(&config).is_ok());
        config.database.sealing_key = Some(ConfigSecret::new("not hex"));
        let error = sealer(&config).expect_err("malformed key");
        assert!(
            !format!("{error:#}").contains("not hex"),
            "the key is never echoed"
        );
    }

    #[test]
    fn development_without_a_sealing_key_gets_a_process_local_one() {
        let config = AppConfig::default();
        assert!(sealer(&config).is_ok());
    }

    #[test]
    fn the_migration_summary_names_what_was_applied() {
        let none = MigrateReport {
            applied: Vec::new(),
            schema_version: 8,
        };
        assert_eq!(summary(&none), "schema is current at version 8");
        let some = MigrateReport {
            applied: vec![AppliedMigration {
                version: 1,
                description: "core".into(),
            }],
            schema_version: 1,
        };
        let text = summary(&some);
        assert!(
            text.contains("1 core") && text.contains("version 1"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_database_makes_the_node_not_ready_and_db_up_zero() {
        let mut config = AppConfig::default();
        config.database.url = Some(ConfigSecret::new("postgres://u:p@127.0.0.1:1/db"));
        config.database.acquire_timeout_secs = 1;
        let pool = connect_lazy(&config).unwrap();
        let metrics = MetricsHandle::new().unwrap();
        let report = PgReadiness::new(pool, metrics.clone()).check().await;
        assert_eq!(report.database, Outcome::Failed);
        assert!(!report.schema.is_servable());
        assert!(
            metrics.render().contains("db_up 0"),
            "the gauge follows the probe"
        );
    }
}
