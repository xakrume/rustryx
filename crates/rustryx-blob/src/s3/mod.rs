//! S3-compatible blob store (AWS S3, `MinIO`, Ceph RGW) on top of `object_store`.
//!
//! Why `object_store` and not the AWS SDK: it is one small trait with multipart,
//! streaming and retry built in, it speaks to every S3 dialect we need, and its TLS
//! is rustls (no `openssl-sys`). The SDK would only be needed for SSE-KMS and
//! presigned URLs, neither of which is in scope.
//!
//! Key layout in the bucket: `<prefix>/<b0>/<b1>/<key>` (see [`crate::key`]).
//!
//! **A bucket lifecycle rule for incomplete multipart uploads is required.** A
//! process that is killed in the middle of an upload cannot abort it, and
//! `object_store` 0.14 offers no way to list open multipart uploads, so
//! [`BlobStore::list_stale_staging`] cannot find them. Without the rule the parts
//! stay in the bucket (and on the bill) forever. See `docs/blob.md`.

mod sink;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use object_store::aws::AmazonS3Builder;
use object_store::multipart::MultipartStore;
use object_store::path::Path as ObjectPath;
use object_store::{
    BackoffConfig, ClientOptions, GetOptions, GetRange, ObjectStore, ObjectStoreExt, RetryConfig,
};
use rustryx_core::model::BlobStoreKind;
use rustryx_core::ports::{BlobKey, BlobMeta, BlobRead, BlobStore, BlobWriter, ByteRange};
use rustryx_core::{BlobError, Secret};
use tokio::sync::Semaphore;
use tokio_util::task::TaskTracker;

use crate::cleanup::CleanupDrain;
use crate::config::{BuildError, MAX_PARTS, S3Config, S3Settings};
use crate::error::{backend, invalid_key, with_deadline};
use crate::hashing::HashingWriter;
use crate::key::{relative_location, validate_key, validate_prefix};
use crate::read::{READ_CHUNK_BYTES, cap_chunks, resolve_range};
use sink::S3Sink;

/// Connect timeout of the HTTP client (README 8.2: connect 3 s).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Idle timeout of the HTTP client: a transfer may take as long as it likes as long
/// as bytes keep moving (README 8.2: streams are bounded by idle time only).
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Retries inside the client. The overall deadline of every call is enforced by the
/// store, so retries cannot extend a request past it.
const MAX_RETRIES: usize = 3;
const RETRY_BUDGET: Duration = Duration::from_secs(30);

/// Key probed by `health`. A missing object proves reachability and credentials.
const HEALTH_PROBE: &str = "_rustryx/health-probe";

/// Static S3 credentials, already resolved from the secret store.
#[derive(Debug, Clone)]
pub struct S3Credentials {
    /// Access key id.
    pub access_key_id: Secret,
    /// Secret access key.
    pub secret_access_key: Secret,
    /// Session token for temporary credentials.
    pub session_token: Option<Secret>,
}

/// Shared state of a store and of every writer it hands out.
pub(crate) struct Backend {
    pub(crate) store: Arc<dyn ObjectStore>,
    pub(crate) multipart: Arc<dyn MultipartStore>,
    /// Empty or `name/` (with the trailing slash).
    prefix: String,
    pub(crate) settings: S3Settings,
    uploads: Arc<Semaphore>,
    /// Aborts of dropped writers run here so that shutdown can wait for them.
    pub(crate) cleanup: TaskTracker,
}

/// A blob store backed by an S3-compatible bucket.
///
/// Cheap to clone. Concurrent uploads are limited by a semaphore whose permit is
/// taken before `begin` returns and released at commit, abort or drop.
#[derive(Clone)]
pub struct S3BlobStore {
    backend: Arc<Backend>,
}

impl std::fmt::Debug for S3BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3BlobStore")
            .field("prefix", &self.backend.prefix)
            .field("settings", &self.backend.settings)
            .finish_non_exhaustive()
    }
}

impl S3BlobStore {
    /// Builds a store for an S3-compatible endpoint. No network traffic happens here.
    ///
    /// With `credentials == None` the client falls back to the ambient chain (web
    /// identity, container credentials, instance metadata). `object_store` is never
    /// asked to read `AWS_*` variables for static keys.
    pub fn new(config: &S3Config, credentials: Option<&S3Credentials>) -> Result<Self, BuildError> {
        config.validate()?;
        let path_style = config.path_style.unwrap_or(config.endpoint.is_some());
        let client_options = ClientOptions::new()
            .with_allow_http(config.allow_http)
            .with_connect_timeout(CONNECT_TIMEOUT)
            // The total-request timeout would also cut off long streaming bodies;
            // the idle timeout below plus the store's own deadlines replace it.
            .with_timeout_disabled()
            .with_read_timeout(IDLE_TIMEOUT);
        let retry = RetryConfig {
            backoff: BackoffConfig::default(),
            max_retries: MAX_RETRIES,
            retry_timeout: RETRY_BUDGET,
        };
        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(&config.bucket)
            .with_region(&config.region)
            .with_virtual_hosted_style_request(!path_style)
            .with_allow_http(config.allow_http)
            .with_client_options(client_options)
            .with_retry(retry);
        if let Some(endpoint) = &config.endpoint {
            builder = builder.with_endpoint(endpoint);
        }
        if let Some(creds) = credentials {
            builder = builder
                .with_access_key_id(creds.access_key_id.expose())
                .with_secret_access_key(creds.secret_access_key.expose());
            if let Some(token) = &creds.session_token {
                builder = builder.with_token(token.expose());
            }
        }
        let s3 = Arc::new(builder.build().map_err(BuildError::Client)?);
        Self::from_backends(s3.clone(), s3, &config.prefix, config.settings())
    }

    /// Builds a store over arbitrary `object_store` backends. `AmazonS3` implements
    /// both traits; tests pass an in-memory store and a fake multipart store.
    /// `prefix` is validated like a key (no leading or trailing slash).
    pub fn from_backends(
        store: Arc<dyn ObjectStore>,
        multipart: Arc<dyn MultipartStore>,
        prefix: &str,
        settings: S3Settings,
    ) -> Result<Self, BuildError> {
        validate_prefix(prefix)?;
        settings.validate()?;
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        let uploads = Arc::new(Semaphore::new(settings.max_concurrent_uploads));
        Ok(Self {
            backend: Arc::new(Backend {
                store,
                multipart,
                prefix,
                settings,
                uploads,
                cleanup: TaskTracker::new(),
            }),
        })
    }

    /// Waits until every abort spawned by dropped writers has finished. Call it at
    /// graceful shutdown (architecture 1.4: unfinished multipart uploads are aborted
    /// before exit). Cancel safe.
    pub async fn drain_cleanup(&self) {
        // Left closed on purpose: see `LocalBlobStore::drain_cleanup`.
        self.backend.cleanup.close();
        self.backend.cleanup.wait().await;
    }

    /// Number of uploads that could start right now without waiting.
    #[must_use]
    pub fn available_upload_slots(&self) -> usize {
        self.backend.uploads.available_permits()
    }

    fn object_path(&self, key: &BlobKey) -> Result<ObjectPath, BlobError> {
        let text = validate_key(key).map_err(invalid_key)?;
        let name = format!("{}{}", self.backend.prefix, relative_location(text));
        ObjectPath::parse(name).map_err(|e| backend("invalid blob key", e))
    }

    fn deadline(&self) -> Duration {
        self.backend.settings.metadata_timeout
    }

    /// Part size for an upload: the configured one, enlarged only when the size hint
    /// would otherwise need more than [`MAX_PARTS`] parts. Enlarging breaks the
    /// one-part memory bound, which is why it only happens for objects above
    /// 10000 parts (160 GB at the default) and never silently below that.
    fn part_size_for(&self, size_hint: Option<u64>) -> usize {
        let configured = self.backend.settings.part_size;
        let Some(hint) = size_hint else {
            return configured;
        };
        let needed = hint.div_ceil(MAX_PARTS as u64);
        usize::try_from(needed).map_or(configured, |needed| configured.max(needed))
    }
}

#[async_trait]
impl CleanupDrain for S3BlobStore {
    async fn drain_cleanup(&self) {
        Self::drain_cleanup(self).await;
    }
}

/// Maps `object_store` errors onto the port's error contract.
///
/// `Generic` is what remains after the client exhausted its retries on 5xx and
/// connection errors, so it is reported as `Unavailable` (503 for the caller).
/// Permission and authentication failures are configuration problems that retrying
/// the request cannot fix; they stay `Backend` so they do not look transient.
pub(crate) fn store_error(context: &'static str, err: object_store::Error) -> BlobError {
    match err {
        object_store::Error::NotFound { .. } => BlobError::NotFound,
        object_store::Error::Generic { .. } => BlobError::Unavailable(err.to_string()),
        other => backend(context, other),
    }
}

#[async_trait]
impl BlobStore for S3BlobStore {
    fn kind(&self) -> BlobStoreKind {
        BlobStoreKind::S3
    }

    /// Takes an upload slot, then returns a writer whose objects are invisible until
    /// `commit`. Waits at most `permit_timeout` for a slot and then fails with
    /// `Unavailable` (503), so the queue of waiting uploads cannot grow without
    /// bound. Cancel safe: waiting for the slot holds nothing, and dropping the
    /// writer aborts any multipart upload it started.
    async fn begin(
        &self,
        key: &BlobKey,
        size_hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        let path = self.object_path(key)?;
        let wait = Arc::clone(&self.backend.uploads).acquire_owned();
        let permit = match tokio::time::timeout(self.backend.settings.permit_timeout, wait).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_closed)) => return Err(BlobError::Unavailable("upload limiter closed".into())),
            Err(_elapsed) => {
                return Err(BlobError::Unavailable("too many concurrent uploads".into()));
            }
        };
        let sink = S3Sink::new(
            Arc::clone(&self.backend),
            path,
            self.part_size_for(size_hint),
            size_hint,
            permit,
        );
        Ok(Box::new(HashingWriter::new(sink)))
    }

    /// Streams the object in chunks of at most 64 KiB. A ranged read costs one extra
    /// `HEAD` because the object store reports an unsatisfiable range as an opaque
    /// error; checking against the size first keeps `RangeNotSatisfiable` exact.
    /// Cancel safe.
    async fn get(&self, key: &BlobKey, range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
        let path = self.object_path(key)?;
        let mut options = GetOptions::default();
        if range.is_some() {
            let size = self.stat(&path).await?.ok_or(BlobError::NotFound)?;
            let (start, len) = resolve_range(size, range)?;
            options = options.with_range(Some(GetRange::Bounded(start..start + len)));
        }
        let backend_handle = Arc::clone(&self.backend);
        let result = with_deadline("s3 get", self.deadline(), async {
            backend_handle
                .store
                .get_opts(&path, options)
                .await
                .map_err(|e| store_error("get object", e))
        })
        .await?;
        let total_size = result.meta.size;
        let stream = result
            .into_stream()
            .map_err(|e| store_error("read object", e))
            .boxed();
        Ok(BlobRead {
            total_size,
            stream: cap_chunks(stream, READ_CHUNK_BYTES),
        })
    }

    /// Cancel safe.
    async fn head(&self, key: &BlobKey) -> Result<Option<BlobMeta>, BlobError> {
        let path = self.object_path(key)?;
        Ok(self
            .stat(&path)
            .await?
            .map(|size_bytes| BlobMeta { size_bytes }))
    }

    /// Deleting a missing key succeeds. Cancel safe.
    async fn delete(&self, key: &BlobKey) -> Result<(), BlobError> {
        let path = self.object_path(key)?;
        let backend_handle = Arc::clone(&self.backend);
        let outcome = with_deadline("s3 delete", self.deadline(), async {
            backend_handle
                .store
                .delete(&path)
                .await
                .map_err(|e| store_error("delete object", e))
        })
        .await;
        match outcome {
            Err(BlobError::NotFound) => Ok(()),
            other => other,
        }
    }

    /// Always empty. Incomplete multipart uploads are the S3 equivalent of staging,
    /// but `object_store` cannot list them, so they are reaped by the bucket
    /// lifecycle rule `AbortIncompleteMultipartUpload`, which is mandatory (see the
    /// module docs). Cancel safe.
    async fn list_stale_staging(&self, _min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        Ok(Vec::new())
    }

    /// Probes a key that never exists: `NotFound` means the bucket answered and the
    /// credentials were accepted. Needs `s3:ListBucket` on the bucket, otherwise S3
    /// answers 403 instead of 404 and the store reports unhealthy. Cancel safe.
    async fn health(&self) -> Result<(), BlobError> {
        let path = ObjectPath::parse(format!("{}{HEALTH_PROBE}", self.backend.prefix))
            .map_err(|e| backend("health probe", e))?;
        match self.stat(&path).await {
            Ok(_) => Ok(()),
            Err(BlobError::Backend { source, .. }) => Err(BlobError::Unavailable(format!(
                "bucket probe was rejected: {source}"
            ))),
            Err(other) => Err(other),
        }
    }
}

impl S3BlobStore {
    /// `HEAD` returning the size, `None` when the object does not exist.
    async fn stat(&self, path: &ObjectPath) -> Result<Option<u64>, BlobError> {
        let backend_handle = Arc::clone(&self.backend);
        let outcome = with_deadline("s3 head", self.deadline(), async {
            backend_handle
                .store
                .head(path)
                .await
                .map_err(|e| store_error("head object", e))
        })
        .await;
        match outcome {
            Ok(meta) => Ok(Some(meta.size)),
            Err(BlobError::NotFound) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests;
