//! Pool construction and the readiness ping.

use std::str::FromStr;
use std::time::Duration;

use rustryx_core::{Secret, StoreError};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::config::PgConfig;
use crate::error::{backend, map_err};

/// Upper bound of the readiness probe query (README 8.2: `SELECT 1` within one second).
pub const PING_TIMEOUT: Duration = Duration::from_secs(1);

/// Parses a connection URL.
///
/// A failed parse reports a fixed message. The parser's own text is not forwarded: a
/// malformed URL still contains the password the operator typed, and configuration errors
/// end up in startup logs.
pub(crate) fn parse_options(url: &Secret) -> Result<PgConnectOptions, StoreError> {
    PgConnectOptions::from_str(url.expose())
        .map_err(|_| backend("parse database url", "invalid database url"))
}

fn pool_options(config: &PgConfig) -> PgPoolOptions {
    let statement_timeout_ms = config.statement_timeout.map(|d| d.as_millis().to_string());
    PgPoolOptions::new()
        .max_connections(config.max_connections.max(1))
        .acquire_timeout(config.acquire_timeout)
        .after_connect(move |conn, _meta| {
            let statement_timeout_ms = statement_timeout_ms.clone();
            Box::pin(async move {
                if let Some(ms) = statement_timeout_ms {
                    // set_config with a bind parameter instead of `SET statement_timeout`,
                    // which cannot be parameterised.
                    sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                        .bind(ms)
                        .execute(&mut *conn)
                        .await?;
                }
                Ok(())
            })
        })
}

/// Opens the pool and verifies that the server answers.
pub async fn connect(config: &PgConfig) -> Result<PgPool, StoreError> {
    let options = parse_options(&config.url)?.application_name(&config.application_name);
    pool_options(config)
        .connect_with(options)
        .await
        .map_err(|e| map_err("connect to database", e))
}

/// Builds the pool without touching the network. Connections are opened on first use, so
/// a node can start while `PostgreSQL` is down and report not-ready instead of crashing.
pub fn connect_lazy(config: &PgConfig) -> Result<PgPool, StoreError> {
    let options = parse_options(&config.url)?.application_name(&config.application_name);
    Ok(pool_options(config).connect_lazy_with(options))
}

/// `SELECT 1` with a one second timeout, for `/readyz`.
pub async fn ping(pool: &PgPool) -> Result<(), StoreError> {
    match tokio::time::timeout(PING_TIMEOUT, sqlx::query("SELECT 1").execute(pool)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(map_err("ping database", e)),
        Err(_) => Err(StoreError::Unavailable(
            "database ping timed out".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn must_not_leak_password_when_url_is_invalid() {
        let url = Secret::new("postgres://app:hunter2-pw@db:notaport/rustryx");
        let err = parse_options(&url).expect_err("must fail");
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("hunter2-pw"), "{shown}");
    }

    #[test]
    fn must_not_print_password_in_config_debug() {
        let config = PgConfig::new(Secret::new("postgres://app:hunter2-pw@db/rustryx"));
        assert!(!format!("{config:?}").contains("hunter2-pw"));
    }

    #[test]
    fn defaults_follow_the_sre_requirements() {
        let config = PgConfig::new(Secret::new("postgres://x@y/z"));
        assert_eq!(config.max_connections, 20);
        assert_eq!(config.acquire_timeout, Duration::from_secs(5));
        assert_eq!(config.statement_timeout, Some(Duration::from_secs(30)));
    }
}
