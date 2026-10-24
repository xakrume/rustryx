//! A blob store that keeps a digest and a size, never the bytes, for bodies that must not be
//! held in memory (the streaming self test pushes 256 MiB through the server).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use rustryx_core::error::BlobError;
use rustryx_core::model::{BlobStoreKind, Checksums};
use rustryx_core::ports::{
    BlobCommit, BlobKey, BlobMeta, BlobRead, BlobStore, BlobWriter, ByteRange,
};
use rustryx_format_core::testing::synthetic_range;
use sha2::{Digest, Sha256};

/// Chunk size of the regenerated body: the pipelines slice at 64 KiB as well.
const CHUNK: usize = 64 * 1024;

/// A [`BlobStore`] that accepts any bytes and, on read, regenerates the deterministic body of
/// [`synthetic_stream`](crate::synthetic_stream) of the stored size.
///
/// It is only correct for content that WAS that synthetic body: reading back anything else
/// returns the synthetic bytes, not what was written. That is the point: memory growth of the
/// process can then only come from the code under test, not from the store.
#[derive(Default)]
pub struct SyntheticBlobStore {
    objects: Mutex<HashMap<String, (u64, String)>>,
}

impl SyntheticBlobStore {
    /// An empty store, ready to be passed to `TestServerBuilder::blob_store`.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Number of committed objects.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.lock().len()
    }

    /// `(size, sha256)` of a committed object by key.
    #[must_use]
    pub fn object(&self, key: &str) -> Option<(u64, String)> {
        self.lock().get(key).cloned()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, (u64, String)>> {
        self.objects.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Writer {
    store: Arc<SyntheticBlobStore>,
    key: String,
    hasher: Sha256,
    size: u64,
}

#[async_trait]
impl BlobWriter for Writer {
    async fn write(&mut self, chunk: Bytes) -> Result<(), BlobError> {
        self.hasher.update(&chunk);
        self.size += chunk.len() as u64;
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<BlobCommit, BlobError> {
        let sha256 = hex::encode(self.hasher.finalize());
        self.store
            .lock()
            .insert(self.key.clone(), (self.size, sha256.clone()));
        // Only SHA-256 is computed; the other digests are placeholders of the right length,
        // which is enough for a store that exists to measure memory.
        Ok(BlobCommit::new(
            self.size,
            Checksums {
                sha1: "0".repeat(40),
                sha256,
                md5: "0".repeat(32),
            },
        ))
    }

    async fn abort(self: Box<Self>) -> Result<(), BlobError> {
        Ok(())
    }
}

/// The `BlobStore` face of a shared [`SyntheticBlobStore`].
struct Handle(Arc<SyntheticBlobStore>);

#[async_trait]
impl BlobStore for Handle {
    fn kind(&self) -> BlobStoreKind {
        BlobStoreKind::Local
    }

    async fn begin(
        &self,
        key: &BlobKey,
        _size_hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        Ok(Box::new(Writer {
            store: Arc::clone(&self.0),
            key: key.as_str().to_owned(),
            hasher: Sha256::new(),
            size: 0,
        }))
    }

    async fn get(&self, key: &BlobKey, range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
        let size = self
            .0
            .lock()
            .get(key.as_str())
            .map(|(size, _)| *size)
            .ok_or(BlobError::NotFound)?;
        let (start, end) = match range {
            None => (0, size),
            Some(r) => (
                r.start,
                r.end.map_or(size, |e| e.saturating_add(1).min(size)),
            ),
        };
        Ok(BlobRead {
            total_size: size,
            stream: synthetic_range(start, end, CHUNK)
                .map(|item| item.map_err(|e| BlobError::Unavailable(e.to_string())))
                .boxed(),
        })
    }

    async fn head(&self, key: &BlobKey) -> Result<Option<BlobMeta>, BlobError> {
        Ok(self
            .0
            .lock()
            .get(key.as_str())
            .map(|(size, _)| BlobMeta::new(*size)))
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), BlobError> {
        self.0.lock().remove(key.as_str());
        Ok(())
    }

    async fn list_stale_staging(&self, _min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        Ok(Vec::new())
    }

    async fn health(&self) -> Result<(), BlobError> {
        Ok(())
    }
}

impl SyntheticBlobStore {
    /// The store as the `Arc<dyn BlobStore>` the builder takes.
    #[must_use]
    pub fn as_blob_store(self: &Arc<Self>) -> Arc<dyn BlobStore> {
        Arc::new(Handle(Arc::clone(self)))
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use rustryx_core::ports::BlobStore;

    use super::*;

    async fn put(store: &Arc<dyn BlobStore>, key: &str, body: &'static [u8]) {
        let mut writer = store.begin(&BlobKey::new(key), Some(3)).await.unwrap();
        writer.write(Bytes::from_static(body)).await.unwrap();
        let commit = writer.commit().await.unwrap();
        assert_eq!(commit.size_bytes, body.len() as u64);
    }

    async fn read(store: &Arc<dyn BlobStore>, key: &str, range: Option<ByteRange>) -> Vec<u8> {
        let mut read = store.get(&BlobKey::new(key), range).await.unwrap();
        let mut out = Vec::new();
        while let Some(chunk) = read.stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn keeps_a_digest_and_regenerates_the_synthetic_body() {
        let store = SyntheticBlobStore::new();
        let blobs = store.as_blob_store();
        assert_eq!(blobs.kind(), BlobStoreKind::Local);
        put(&blobs, "k", b"abc").await;
        assert_eq!(store.object_count(), 1);
        let (size, sha256) = store.object("k").unwrap();
        assert_eq!(size, 3);
        assert_eq!(sha256, hex::encode(Sha256::digest(b"abc")));

        // The bytes are the synthetic pattern of the stored size, not what was written.
        assert_eq!(read(&blobs, "k", None).await, [0, 1, 2]);
        assert_eq!(
            read(
                &blobs,
                "k",
                Some(ByteRange {
                    start: 1,
                    end: Some(1)
                })
            )
            .await,
            [1]
        );
        assert_eq!(
            read(
                &blobs,
                "k",
                Some(ByteRange {
                    start: 1,
                    end: None
                })
            )
            .await,
            [1, 2]
        );
        assert_eq!(
            blobs
                .head(&BlobKey::new("k"))
                .await
                .unwrap()
                .unwrap()
                .size_bytes,
            3
        );
        assert!(
            blobs
                .head(&BlobKey::new("missing"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            blobs.get(&BlobKey::new("missing"), None).await,
            Err(BlobError::NotFound)
        ));
        let stale = blobs.list_stale_staging(Duration::ZERO).await.unwrap();
        assert!(matches!(stale.as_slice(), []), "this store never stages");
        blobs.health().await.unwrap();

        blobs.delete(&BlobKey::new("k")).await.unwrap();
        assert_eq!(store.object_count(), 0);
        assert!(store.object("k").is_none());
    }

    #[tokio::test]
    async fn an_aborted_write_stores_nothing() {
        let store = SyntheticBlobStore::new();
        let blobs = store.as_blob_store();
        let mut writer = blobs.begin(&BlobKey::new("gone"), None).await.unwrap();
        writer.write(Bytes::from_static(b"x")).await.unwrap();
        writer.abort().await.unwrap();
        assert_eq!(store.object_count(), 0);
    }
}
