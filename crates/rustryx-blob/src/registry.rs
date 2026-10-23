//! Builds blob stores from their stored definitions and caches them.
//!
//! One store instance per definition id matters for correctness, not only for speed:
//! the S3 store carries the semaphore that limits concurrent uploads (and with it the
//! memory budget), so two instances of the same bucket would double the limit.
//!
//! The registry is what `rustryx-format-core`'s `BlobStoreRegistry` adapter wraps: it
//! answers [`BlobStoreRegistry::store_by_id`] (a member, a plain store) and
//! [`BlobStoreRegistry::store_for_repository`] (the store that takes a new write for a
//! repository), and it keeps the definition it built each store from, so a caller that
//! needs `quota_bytes` does not read the database on every write.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rustryx_core::BlobError;
use rustryx_core::DomainError;
use rustryx_core::StoreError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind, Repository};
use rustryx_core::ports::{BlobStore, BlobStoreRepo};
use tokio::sync::OnceCell;

use crate::config::{BuildError, LocalConfig, S3Config, parse_config};
use crate::disk::{DiskProbe, StatvfsProbe};
use crate::error::backend;
use crate::local::{LocalBlobStore, LocalOptions};
use crate::s3::{S3BlobStore, S3Credentials};
use crate::secrets::{SecretError, SecretResolver};

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

impl From<RegistryError> for BlobError {
    /// Maps onto the port's contract for "no usable store": `Unavailable` (503) when a retry
    /// can help (the definition is missing for now, the database or the secret store is
    /// down), the failure of opening the root as it is, and `Backend` (500 class, detail in
    /// the logs) for a definition that is wrong and stays wrong until somebody fixes it.
    fn from(err: RegistryError) -> Self {
        match err {
            RegistryError::Unknown(_) => Self::Unavailable("blob store is not defined".into()),
            RegistryError::Repo(_) => {
                Self::Unavailable("blob store definition could not be loaded".into())
            }
            RegistryError::Build(BuildError::Open(inner)) => inner,
            RegistryError::Build(BuildError::Secret(SecretError::Unavailable { .. })) => {
                Self::Unavailable("blob store secrets could not be read".into())
            }
            RegistryError::Build(other) => backend("build blob store", other),
        }
    }
}

/// A built store with the definition it was built from.
#[derive(Clone)]
struct Built {
    store: Arc<dyn BlobStore>,
    def: Arc<BlobStoreDef>,
}

/// Builds the store described by `def`, resolving secret references with `secrets`.
/// A local store gets the statvfs disk probe (see [`BlobStoreRegistry::with_disk_probe`]
/// to substitute it).
///
/// Local stores create their directories here; S3 stores make no network call.
/// Cancel safe: a cancelled call leaves at most created (empty) directories.
pub async fn build_store(
    def: &BlobStoreDef,
    secrets: &dyn SecretResolver,
) -> Result<Arc<dyn BlobStore>, BuildError> {
    let probe: Arc<dyn DiskProbe> = Arc::new(StatvfsProbe);
    Ok(build_parts(def, secrets, &probe).await?.store)
}

async fn build_parts(
    def: &BlobStoreDef,
    secrets: &dyn SecretResolver,
    probe: &Arc<dyn DiskProbe>,
) -> Result<Built, BuildError> {
    let store: Arc<dyn BlobStore> = match def.kind {
        BlobStoreKind::Local => {
            let config: LocalConfig = parse_config("local", &def.config)?;
            config.validate()?;
            let options = LocalOptions::new(def.name.clone())
                .with_disk_floor(Arc::clone(probe), config.min_free_percent);
            let store = LocalBlobStore::open_with(&config.path, options)
                .await
                .map_err(BuildError::Open)?;
            Arc::new(store)
        }
        BlobStoreKind::S3 => {
            let config: S3Config = parse_config("s3", &def.config)?;
            config.validate()?;
            let credentials = resolve_credentials(&config, secrets).await?;
            Arc::new(S3BlobStore::new(&def.name, &config, credentials.as_ref())?)
        }
        // A group is built by the group adapter and Azure by its own store, both stream F.
        // Refusing here keeps a definition written by a newer node from being served as
        // something it is not.
        BlobStoreKind::Azure => return Err(BuildError::UnsupportedKind("azure")),
        BlobStoreKind::Group => return Err(BuildError::UnsupportedKind("group")),
        _ => return Err(BuildError::UnsupportedKind("unknown")),
    };
    Ok(Built {
        store,
        def: Arc::new(def.clone()),
    })
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
    probe: Arc<dyn DiskProbe>,
    // Held only for the instant of a map operation, never across an await.
    slots: Mutex<HashMap<BlobStoreId, Slot>>,
}

impl std::fmt::Debug for BlobStoreRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobStoreRegistry").finish_non_exhaustive()
    }
}

impl BlobStoreRegistry {
    /// Creates an empty registry reading definitions from `repo`. Local stores sample the
    /// disk with `statvfs`.
    #[must_use]
    pub fn new(repo: Arc<dyn BlobStoreRepo>, secrets: Arc<dyn SecretResolver>) -> Self {
        Self {
            repo,
            secrets,
            probe: Arc::new(StatvfsProbe),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Replaces the disk probe that local stores built from now on use. For tests and for a
    /// deployment that reads capacity from somewhere else.
    #[must_use]
    pub fn with_disk_probe(self, probe: Arc<dyn DiskProbe>) -> Self {
        Self { probe, ..self }
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

    async fn built(&self, id: BlobStoreId) -> Result<Built, RegistryError> {
        let slot = self.slot(id);
        let built = slot
            .get_or_try_init(|| async {
                let def = self.repo.get(id).await?.ok_or(RegistryError::Unknown(id))?;
                Ok::<_, RegistryError>(build_parts(&def, self.secrets.as_ref(), &self.probe).await?)
            })
            .await;
        match built {
            Ok(built) => Ok(built.clone()),
            Err(err) => {
                self.forget_if_empty(id, &slot);
                Err(err)
            }
        }
    }

    /// Returns the store with this id (a plain store or a member), building it on first
    /// use. Cancel safe: a cancelled build leaves the slot empty and the next call builds
    /// again.
    pub async fn store_by_id(&self, id: BlobStoreId) -> Result<Arc<dyn BlobStore>, RegistryError> {
        Ok(self.built(id).await?.store)
    }

    /// The store that takes a new write for `repository`: the one its `blob_store_id` names.
    /// Cancel safe.
    pub async fn store_for_repository(
        &self,
        repository: &Repository,
    ) -> Result<Arc<dyn BlobStore>, RegistryError> {
        self.store_by_id(repository.blob_store_id).await
    }

    /// The definition the cached store was built from, read through `BlobStoreRepo` once and
    /// kept until `invalidate`. The write path needs `quota_bytes` on every upload, and this
    /// answers from memory. Cancel safe.
    pub async fn definition(&self, id: BlobStoreId) -> Result<Arc<BlobStoreDef>, RegistryError> {
        Ok(self.built(id).await?.def)
    }

    /// Builds the store for `def` now and makes it the cached one for `def.id`,
    /// replacing any earlier instance. Writers that already hold the old instance
    /// finish on it. Use it when a definition was just created or changed.
    /// Cancel safe: until the build finishes the cache is untouched.
    pub async fn register(&self, def: &BlobStoreDef) -> Result<Arc<dyn BlobStore>, BuildError> {
        let built = build_parts(def, self.secrets.as_ref(), &self.probe).await?;
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

    /// Drops every cached store (an invalidation `Reset`: the node missed events and cannot
    /// tell which definition changed).
    pub fn invalidate_all(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.clear();
    }

    /// Shuts down every cached store: waits for the background cleanup of dropped writers
    /// (abandoned temporary files, abandoned multipart uploads) and stops their monitors.
    /// Call it once at graceful shutdown, after in-flight requests were drained. Stores that
    /// `invalidate` or `register` already dropped from the cache are not waited for: their
    /// writers finish on their own, and leftovers fall to the staging GC and the bucket
    /// lifecycle rule. Cancel safe.
    pub async fn shutdown(&self) {
        let stores: Vec<Arc<dyn BlobStore>> = {
            let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots
                .values()
                .filter_map(|slot| slot.get())
                .map(|built| Arc::clone(&built.store))
                .collect()
        };
        futures::future::join_all(stores.iter().map(|store| store.shutdown())).await;
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
