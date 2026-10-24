//! First-start data: the bootstrap administrator and the default blob store.
//!
//! Both need a migrated schema, and `rustryx serve` never migrates (a node starting must not race
//! the migrate job, and a rolling update must not apply a migration under running pods). So this
//! work is a task that waits for the schema to be servable, does its idempotent writes once, and
//! then parks until shutdown. Waiting costs one cheap query every few seconds and makes the order
//! "serve first, migrate after" work without a restart, which is what the readiness probe
//! (503 before the migrate job, 200 after) promises.
//!
//! The task parks instead of returning because the supervisor restarts a task that ends before
//! shutdown, and a finished bootstrap must not run again in a loop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use rustryx_auth::passwords::Passwords;
use rustryx_blob::BlobStoreRegistry;
use rustryx_config::BlobConfig;
use rustryx_core::Secret;
use rustryx_core::ports::{BlobStoreRepo, Clock};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::blobs::seed_default_store;
use super::database::schema_is_serving;
use crate::supervisor::BackgroundTask;

/// How long the task waits between looks at the schema and between retries of a failed write.
const RETRY: Duration = Duration::from_secs(5);

/// The first administrator, as the deployment handed it over.
pub struct AdminSeed {
    /// Login name.
    pub username: String,
    /// The password. Hashed here with argon2id; the plain value goes nowhere else.
    pub password: Secret,
}

/// Everything the first-start work needs.
pub struct Bootstrap {
    /// The pool the writes go through.
    pub pool: PgPool,
    /// Password hashing.
    pub passwords: Arc<Passwords>,
    /// The administrator, when the deployment configured one.
    pub admin: Option<AdminSeed>,
    /// The `blob.*` seed.
    pub blob: BlobConfig,
    /// Blob store definitions.
    pub definitions: Arc<dyn BlobStoreRepo>,
    /// The registry, told about the store it should open.
    pub registry: Arc<BlobStoreRegistry>,
    /// Time source.
    pub clock: Arc<dyn Clock>,
}

impl Bootstrap {
    /// The supervised task.
    #[must_use]
    pub fn task(self) -> BackgroundTask {
        let bootstrap = Arc::new(self);
        BackgroundTask::new("bootstrap", move |cancel| {
            let bootstrap = Arc::clone(&bootstrap);
            async move { bootstrap.run(&cancel).await }
        })
    }

    async fn run(&self, cancel: &CancellationToken) {
        loop {
            if schema_is_serving(&self.pool).await {
                match self.once().await {
                    Ok(()) => break,
                    Err(error) => {
                        tracing::warn!(error = %format!("{error:#}"), "first-start setup failed, retrying");
                    }
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(RETRY) => {}
            }
        }
        cancel.cancelled().await;
    }

    /// One idempotent pass. Safe on every node at once: the administrator is created under an
    /// advisory lock inside the database and only when none exists, and the store row has a
    /// unique name.
    ///
    /// # Errors
    /// The database failed; the caller retries.
    pub async fn once(&self) -> anyhow::Result<()> {
        self.create_admin().await?;
        let created = seed_default_store(
            &self.blob,
            self.definitions.as_ref(),
            &self.registry,
            self.clock.as_ref(),
        )
        .await?;
        if created {
            tracing::info!(store = %self.blob.store_name, "default blob store created");
        }
        Ok(())
    }

    async fn create_admin(&self) -> anyhow::Result<()> {
        let Some(admin) = &self.admin else {
            return Ok(());
        };
        let hash = match self.passwords.hash(&admin.password).await {
            Ok(hash) => hash,
            Err(error) => {
                // Retrying cannot help: the same password hashes the same way. The node stays up
                // without the account and says so, instead of looping on it.
                tracing::error!(%error, username = %admin.username, "the bootstrap administrator password could not be hashed, no administrator is created");
                return Ok(());
            }
        };
        let created = rustryx_db::bootstrap_admin(&self.pool, &admin.username, &hash)
            .await
            .context("create the bootstrap administrator")?;
        if created {
            tracing::info!(username = %admin.username, "bootstrap administrator created");
        }
        Ok(())
    }
}
