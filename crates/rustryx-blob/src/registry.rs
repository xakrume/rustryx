//! Builds blob stores from their stored definitions and caches them.
//!
//! One store instance per definition id matters for correctness, not only for speed:
//! the S3 store carries the semaphore that limits concurrent uploads (and with it the
//! memory budget), so two instances of the same bucket would double the limit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rustryx_core::DomainError;
use rustryx_core::StoreError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::{BlobStore, BlobStoreRepo};
use tokio::sync::OnceCell;

use crate::cleanup::CleanupDrain;
use crate::config::{BuildError, LocalConfig, S3Config, parse_config};
use crate::local::LocalBlobStore;
use crate::s3::{S3BlobStore, S3Credentials};
use crate::secrets::SecretResolver;

/// Why a store could not be produced for an id.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// No definition with that id exists.
    #[error("blob store {0} is not defined")]
    Unknown(BlobStoreId),
    /// The definition could not be read from the metadata store.
    #[error("could not load the blob store definition")]
    Repo(#[from] StoreError),
    /// The definition is invalid or a secret could not be resolved.
    #[error(transparent)]
    Build(#[from] BuildError),
}

impl From<RegistryError> for DomainError {
    fn from(err: RegistryError) -> Self {
        match err {
            RegistryError::Unknown(_) => Self::NotFound {
                entity: "blob store",
            },
            RegistryError::Repo(store) => Self::Store(store),
            // The message of a build error names fields and limits, not values, but
            // it is still configuration detail that belongs in logs only.
            RegistryError::Build(build) => Self::Internal(build.to_string()),
        }
    }
}

/// A built store together with the handle that drains its background cleanup.
#[derive(Clone)]
struct Built {
    store: Arc<dyn BlobStore>,
    drain: Arc<dyn CleanupDrain>,
}

/// Builds the store described by `def`, resolving secret references with `secrets`.
///
/// Local stores create their directories here; S3 stores make no network call.
/// Cancel safe: a cancelled call leaves at most created (empty) directories.
pub async fn build_store(
    def: &BlobStoreDef,
    secrets: &dyn SecretResolver,
) -> Result<Arc<dyn BlobStore>, BuildError> {
    Ok(build_parts(def, secrets).await?.store)
}

async fn build_parts(
    def: &BlobStoreDef,
    secrets: &dyn SecretResolver,
) -> Result<Built, BuildError> {
    match def.kind {
        BlobStoreKind::Local => {
            let config: LocalConfig = parse_config("local", &def.config)?;
            config.validate()?;
            let store = LocalBlobStore::open(&config.path)
                .await
                .map_err(BuildError::Open)?;
            Ok(Built {
                store: Arc::new(store.clone()),
                drain: Arc::new(store),
            })
        }
        BlobStoreKind::S3 => {
            let config: S3Config = parse_config("s3", &def.config)?;
            config.validate()?;
            let credentials = resolve_credentials(&config, secrets).await?;
            let store = S3BlobStore::new(&config, credentials.as_ref())?;
            Ok(Built {
                store: Arc::new(store.clone()),
                drain: Arc::new(store),
            })
        }
    }
}

async fn resolve_credentials(
    config: &S3Config,
    secrets: &dyn SecretResolver,
) -> Result<Option<S3Credentials>, BuildError> {
    // `S3Config::validate` guarantees the two references come as a pair.
    let (Some(id_ref), Some(secret_ref)) = (
        config.access_key_id_ref.as_deref(),
        config.secret_access_key_ref.as_deref(),
    ) else {
        return Ok(None);
    };
    let session_token = match config.session_token_ref.as_deref() {
        Some(reference) => Some(secrets.resolve(reference).await?),
        None => None,
    };
    Ok(Some(S3Credentials {
        access_key_id: secrets.resolve(id_ref).await?,
        secret_access_key: secrets.resolve(secret_ref).await?,
        session_token,
    }))
}

type Slot = Arc<OnceCell<Built>>;

/// Caches one `Arc<dyn BlobStore>` per `BlobStoreId`.
///
/// Concurrent first requests for an id build the store once (the other callers wait
/// on the same `OnceCell`). A failed build is not cached, so the next call retries
/// and a fixed configuration or a restored secret takes effect without a restart.
pub struct BlobStoreRegistry {
    repo: Arc<dyn BlobStoreRepo>,
    secrets: Arc<dyn SecretResolver>,
    // Held only for the instant of a map operation, never across an await.
    slots: Mutex<HashMap<BlobStoreId, Slot>>,
}

impl std::fmt::Debug for BlobStoreRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobStoreRegistry").finish_non_exhaustive()
    }
}

impl BlobStoreRegistry {
    /// Creates an empty registry reading definitions from `repo`.
    #[must_use]
    pub fn new(repo: Arc<dyn BlobStoreRepo>, secrets: Arc<dyn SecretResolver>) -> Self {
        Self {
            repo,
            secrets,
            slots: Mutex::new(HashMap::new()),
        }
    }

    fn slot(&self, id: BlobStoreId) -> Slot {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(slots.entry(id).or_default())
    }

    fn forget_if_empty(&self, id: BlobStoreId, slot: &Slot) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        // Remove a slot whose build failed so unknown ids cannot grow the map; keep
        // it if another caller replaced it or filled it in the meantime.
        if slots
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, slot))
            && slot.get().is_none()
        {
            slots.remove(&id);
        }
    }

    /// Returns the store for `id`, building it on first use. Cancel safe: a cancelled
    /// build leaves the slot empty and the next call builds again.
    pub async fn store_for(&self, id: BlobStoreId) -> Result<Arc<dyn BlobStore>, RegistryError> {
        let slot = self.slot(id);
        let built = slot
            .get_or_try_init(|| async {
                let def = self.repo.get(id).await?.ok_or(RegistryError::Unknown(id))?;
                Ok::<_, RegistryError>(build_parts(&def, self.secrets.as_ref()).await?)
            })
            .await;
        match built {
            Ok(built) => Ok(Arc::clone(&built.store)),
            Err(err) => {
                self.forget_if_empty(id, &slot);
                Err(err)
            }
        }
    }

    /// Builds the store for `def` now and makes it the cached one for `def.id`,
    /// replacing any earlier instance. Writers that already hold the old instance
    /// finish on it. Use it when a definition was just created or changed.
    /// Cancel safe: until the build finishes the cache is untouched.
    pub async fn register(&self, def: &BlobStoreDef) -> Result<Arc<dyn BlobStore>, BuildError> {
        let built = build_parts(def, self.secrets.as_ref()).await?;
        let store = Arc::clone(&built.store);
        let slot: Slot = Arc::new(OnceCell::new_with(Some(built)));
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.insert(def.id, slot);
        drop(slots);
        Ok(store)
    }

    /// Drops the cached store for `id` (after the definition was deleted or edited).
    /// In-flight users keep their `Arc`.
    pub fn invalidate(&self, id: BlobStoreId) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.remove(&id);
    }

    /// Waits for the background cleanup of every cached store (abandoned temporary
    /// files, abandoned multipart uploads). Call it once at graceful shutdown, after
    /// in-flight requests were drained. Stores that `invalidate` or `register` already
    /// dropped from the cache are not waited for: their writers finish on their own,
    /// and leftovers fall to the staging GC and the bucket lifecycle rule. Cancel safe.
    pub async fn shutdown(&self) {
        let drains: Vec<Arc<dyn CleanupDrain>> = {
            let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots
                .values()
                .filter_map(|slot| slot.get())
                .map(|b| Arc::clone(&b.drain))
                .collect()
        };
        futures::future::join_all(drains.iter().map(|drain| drain.drain_cleanup())).await;
    }

    /// Number of cached (or being built) stores.
    #[must_use]
    pub fn cached(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests;
