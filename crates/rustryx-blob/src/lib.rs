//! Blob stores of Rustryx: a local directory and an S3-compatible bucket behind the
//! `rustryx_core::ports::BlobStore` port.
//!
//! What is in here:
//!
//! - [`LocalBlobStore`]: temp file in `_staging/` on the same file system, fsync,
//!   atomic rename; streamed reads with byte ranges.
//! - [`S3BlobStore`]: `object_store` based, 16 MiB multipart parts, memory per upload
//!   bounded by one part, concurrent uploads bounded by a semaphore.
//! - [`HashingWriter`] / [`BlobHasher`]: SHA-1, SHA-256 and MD5 computed
//!   incrementally in the calling task over slices of at most 64 KiB.
//! - [`BlobStoreRegistry`]: builds and caches stores from `BlobStoreDef`, with secrets
//!   coming from a [`SecretResolver`] and never from the stored JSON.
//! - [`check_quota`] and [`check_quota_after_write`].
//!
//! Design notes, layout, the required S3 lifecycle rule and the list of decisions are
//! in `docs/blob.md`.

// Names such as `LocalBlobStore` or `S3BlobStore` are the public vocabulary of the
// crate and are re-exported at the root; the module names repeat them on purpose.
#![allow(clippy::module_name_repetitions)]
// Errors of the port are documented once on `rustryx_core::BlobError`; repeating
// `# Errors` on every method of the implementations adds noise without information.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod cleanup;
mod config;
mod error;
mod hashing;
mod key;
mod local;
mod quota;
mod read;
mod registry;
mod s3;
mod secrets;

pub use cleanup::CleanupDrain;
pub use config::{
    BuildError, DEFAULT_PART_BYTES, LocalConfig, MAX_PARTS, MIN_PART_BYTES, S3Config, S3Settings,
};
pub use hashing::{BlobHasher, BlobSink, HASH_SLICE_BYTES, HashingWriter, YIELD_EVERY_BYTES};
pub use key::{KeyError, MAX_COMPONENT_BYTES, MAX_KEY_BYTES, STAGING_DIR, shard_of, validate_key};
pub use local::LocalBlobStore;
pub use quota::{QuotaExceeded, check_quota, check_quota_after_write};
pub use read::READ_CHUNK_BYTES;
pub use registry::{BlobStoreRegistry, RegistryError, build_store};
pub use s3::{S3BlobStore, S3Credentials};
pub use secrets::{EnvSecretResolver, MapSecretResolver, SecretError, SecretResolver};
