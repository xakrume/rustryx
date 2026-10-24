//! Blob storage: the registry of open stores and its two seams.
//!
//! `rustryx-blob` owns the open stores and `rustryx-format-core` consumes a trait of its own
//! (`BlobStoreRegistry`), so the composition root adapts one to the other. The registry builds a
//! store when it is first asked for and drops it when the definition changes (the invalidation
//! bus delivers that as `Reset`).
//!
//! The `blob.*` section of the configuration is a SEED, not the source of truth: it describes the
//! default store and is written to the database once, when no store of that name exists. After
//! that the row rules, which is what lets an administrator add stores and change a quota without
//! a redeploy. Secrets never go into the row: it holds the names of the environment variables the
//! secret resolver reads (`RUSTRYX_BLOB_S3_ACCESS_KEY` and its `_FILE` form).

use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use futures::StreamExt;
use rustryx_blob::{BlobStoreRegistry, EnvSecretResolver};
use rustryx_config::{BlobConfig, BlobKind};
use rustryx_core::error::BlobError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind, Repository};
use rustryx_core::ports::{BlobStore, BlobStoreRepo, Clock, InvalidationBus, InvalidationEvent};
use rustryx_format_core::context::BlobStoreRegistry as FormatBlobStores;
use serde_json::{Value, json};

use crate::supervisor::BackgroundTask;

/// The environment variable the seeded S3 definition reads the access key id from.
pub const S3_ACCESS_KEY_REF: &str = "RUSTRYX_BLOB_S3_ACCESS_KEY";
/// The environment variable the seeded S3 definition reads the secret access key from.
pub const S3_SECRET_KEY_REF: &str = "RUSTRYX_BLOB_S3_SECRET_KEY";

/// The registry over the definitions in the database.
#[must_use]
pub fn registry(definitions: Arc<dyn BlobStoreRepo>) -> Arc<BlobStoreRegistry> {
    Arc::new(BlobStoreRegistry::new(
        definitions,
        Arc::new(EnvSecretResolver),
    ))
}

/// What the format engine sees of the registry.
///
/// A new write goes to the store of the repository (`store_for`), an existing blob is read from
/// the store its row names (`store_for_blob`, the provided method of the trait), and a lookup by
/// id serves group members.
pub struct FormatStores {
    registry: Arc<BlobStoreRegistry>,
}

impl FormatStores {
    /// The adapter over `registry`.
    #[must_use]
    pub fn new(registry: Arc<BlobStoreRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl FormatBlobStores for FormatStores {
    async fn store_by_id(&self, id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError> {
        self.registry.store_by_id(id).await.map_err(BlobError::from)
    }

    async fn store_for(&self, repository: &Repository) -> Result<Arc<dyn BlobStore>, BlobError> {
        self.registry
            .store_for_repository(repository)
            .await
            .map_err(BlobError::from)
    }
}

/// The definition the `blob.*` section seeds.
///
/// # Errors
/// An S3 seed lacks a bucket or region (validation refuses that first).
pub fn seed_definition(
    blob: &BlobConfig,
    id: BlobStoreId,
    now: time::OffsetDateTime,
) -> anyhow::Result<BlobStoreDef> {
    let (kind, config) = match blob.kind {
        BlobKind::Local => (
            BlobStoreKind::Local,
            json!({
                "path": blob.local_path,
                "min_free_percent": blob.local_min_free_percent,
            }),
        ),
        BlobKind::S3 => (BlobStoreKind::S3, s3_config(blob)?),
    };
    Ok(BlobStoreDef::new(
        id,
        blob.store_name.clone(),
        kind,
        config,
        now,
    ))
}

fn s3_config(blob: &BlobConfig) -> anyhow::Result<Value> {
    let bucket = blob
        .s3_bucket
        .clone()
        .context("blob.s3_bucket is not set")?;
    let region = blob
        .s3_region
        .clone()
        .context("blob.s3_region is not set")?;
    let mut config = json!({
        "bucket": bucket,
        "region": region,
        "path_style": blob.s3_path_style,
        "part_size_mib": blob.s3_part_size_mb,
        "access_key_id_ref": S3_ACCESS_KEY_REF,
        "secret_access_key_ref": S3_SECRET_KEY_REF,
    });
    if let Some(endpoint) = blob.s3_endpoint.as_deref() {
        config["endpoint"] = json!(endpoint);
        // The operator typed a plain http endpoint (a MinIO next to the node); the store refuses
        // it unless that is said explicitly, and writing the URL is saying it.
        config["allow_http"] = json!(endpoint.starts_with("http://"));
    }
    Ok(config)
}

/// Creates the default store when no store of that name exists. Idempotent and safe on every
/// node at once: a lost race is a `Conflict` on the unique name, which means "already there".
///
/// Returns true when this call created the row.
///
/// # Errors
/// The database failed, or the seed is incomplete.
pub async fn seed_default_store(
    blob: &BlobConfig,
    definitions: &dyn BlobStoreRepo,
    registry: &BlobStoreRegistry,
    clock: &dyn Clock,
) -> anyhow::Result<bool> {
    if definitions
        .get_by_name(&blob.store_name)
        .await
        .map_err(|error| anyhow!("read blob stores: {error}"))?
        .is_some()
    {
        return Ok(false);
    }
    let definition = seed_definition(blob, BlobStoreId::generate(), clock.now())?;
    match definitions.create(&definition).await {
        Ok(()) => {}
        Err(rustryx_core::StoreError::Conflict(_)) => return Ok(false),
        Err(error) => return Err(anyhow!("create the default blob store: {error}")),
    }
    // Building the store now creates a local store's directories and surfaces a bad path at
    // startup instead of at the first upload. A failure is reported but does not undo the row:
    // the registry retries on the next request.
    if let Err(error) = registry.register(&definition).await {
        tracing::error!(%error, store = %definition.name, "the default blob store could not be opened");
    }
    Ok(true)
}

/// Drops the cached stores when a definition changed anywhere in the cluster.
///
/// `BlobStoreRepo::update` and `delete` announce `Reset`; the registry cannot tell which store
/// changed from it, so it forgets all of them and rebuilds on demand (cheap: a local store opens
/// a directory, an S3 store builds a client and calls nothing).
#[must_use]
pub fn invalidation_task(
    registry: Arc<BlobStoreRegistry>,
    bus: Arc<dyn InvalidationBus>,
) -> BackgroundTask {
    BackgroundTask::new("blob_registry_invalidation", move |cancel| {
        let registry = Arc::clone(&registry);
        let mut events = bus.subscribe();
        async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    event = events.next() => match event {
                        Some(InvalidationEvent::Reset) => registry.invalidate_all(),
                        // RBAC, user, project and settings events do not touch store definitions.
                        Some(_) => {}
                        None => return,
                    },
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use rustryx_config::ConfigSecret;

    use super::*;

    fn now() -> time::OffsetDateTime {
        time::OffsetDateTime::UNIX_EPOCH
    }

    #[test]
    fn a_local_seed_carries_the_path_and_the_free_space_floor() {
        let mut blob = BlobConfig::default();
        blob.local_path = "/data/blobs".into();
        blob.local_min_free_percent = 15;
        let definition = seed_definition(&blob, BlobStoreId::generate(), now()).unwrap();
        assert_eq!(definition.kind, BlobStoreKind::Local);
        assert_eq!(definition.name, "default");
        assert_eq!(definition.config["path"], "/data/blobs");
        assert_eq!(definition.config["min_free_percent"], 15);
    }

    #[test]
    fn an_s3_seed_names_the_secret_variables_and_never_holds_their_values() {
        let mut blob = BlobConfig::default();
        blob.kind = BlobKind::S3;
        blob.s3_bucket = Some("artifacts".into());
        blob.s3_region = Some("eu-1".into());
        blob.s3_endpoint = Some("https://s3.example".into());
        blob.s3_access_key = Some(ConfigSecret::new("canary-access"));
        blob.s3_secret_key = Some(ConfigSecret::new("canary-secret"));
        let definition = seed_definition(&blob, BlobStoreId::generate(), now()).unwrap();
        assert_eq!(definition.kind, BlobStoreKind::S3);
        let text = definition.config.to_string();
        assert!(text.contains(S3_ACCESS_KEY_REF) && text.contains(S3_SECRET_KEY_REF));
        assert!(
            !text.contains("canary-access") && !text.contains("canary-secret"),
            "{text}"
        );
        assert_eq!(definition.config["allow_http"], false);
    }

    #[test]
    fn a_plain_http_endpoint_is_allowed_only_because_the_operator_wrote_it() {
        let mut blob = BlobConfig::default();
        blob.kind = BlobKind::S3;
        blob.s3_bucket = Some("b".into());
        blob.s3_region = Some("r".into());
        blob.s3_endpoint = Some("http://minio:9000".into());
        let definition = seed_definition(&blob, BlobStoreId::generate(), now()).unwrap();
        assert_eq!(definition.config["allow_http"], true);
    }

    #[test]
    fn an_s3_seed_without_bucket_or_region_is_refused() {
        let mut blob = BlobConfig::default();
        blob.kind = BlobKind::S3;
        assert!(seed_definition(&blob, BlobStoreId::generate(), now()).is_err());
        blob.s3_bucket = Some("b".into());
        assert!(seed_definition(&blob, BlobStoreId::generate(), now()).is_err());
    }
}
