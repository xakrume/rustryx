//! A PostgreSQL container shared by the integration tests of this crate.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    missing_docs
)]

use std::time::{Duration, Instant};

use sqlx::{Connection, PgConnection};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// How long the readiness wait lasts.
pub const WAIT: Duration = Duration::from_secs(60);

pub const PG_PASSWORD: &str = "postgres";
/// A container that outlives a crashed test ends itself after this many seconds.
pub const CONTAINER_TTL_SECS: u64 = 600;

pub struct Postgres {
    container: ContainerAsync<GenericImage>,
    port: u16,
}

impl Postgres {
    pub async fn start() -> Self {
        // `(sleep N; kill 1) &` then `exec`: PID 1 is postgres after the entrypoint chain, and
        // SIGTERM shuts it down cleanly. testcontainers has no reaper process, so a container
        // whose test was killed would otherwise stay.
        let script = format!(
            "(sleep {CONTAINER_TTL_SECS}; kill 1) & exec docker-entrypoint.sh postgres \
             -c fsync=off -c synchronous_commit=off -c full_page_writes=off"
        );
        let container = GenericImage::new("postgres", "16-alpine")
            .with_entrypoint("sh")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", PG_PASSWORD)
            .with_label("rustryx.test", "rustryx-server")
            .with_cmd(["-c".to_owned(), script])
            .with_host_config_modifier(|host| host.auto_remove = Some(true))
            .start()
            .await
            .expect("start postgres (is Docker running?)");
        let port = container
            .get_host_port_ipv4(5432.tcp())
            .await
            .expect("mapped port");
        let postgres = Self { container, port };
        postgres.wait_until_ready().await;
        postgres
    }

    pub fn url(&self) -> String {
        format!(
            "postgres://postgres:{PG_PASSWORD}@127.0.0.1:{}/postgres",
            self.port
        )
    }

    /// The image logs "ready" twice (the init server and the real one): connecting is the proof.
    async fn wait_until_ready(&self) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Ok(mut conn) = PgConnection::connect(&self.url()).await
                && sqlx::query("SELECT 1").execute(&mut conn).await.is_ok()
            {
                let _ = conn.close().await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("postgres did not become ready");
    }

    pub async fn stop(self) {
        let _ = self.container.rm().await;
    }
}
