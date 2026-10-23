//! Incremental hashing shared by every blob store.
//!
//! [`BlobHasher`] feeds SHA-1, SHA-256 and MD5 from the same slices.
//! [`HashingWriter`] wraps a backend specific [`BlobSink`] and implements the port
//! trait [`BlobWriter`], so the local and the S3 store share one state machine,
//! one slicing rule and one digest path instead of two copies.
//!
//! Why the hashing stays in the calling task (README 8.1): a 64 KiB slice takes a
//! fraction of a millisecond, far less than a hop to the blocking pool, and moving
//! it there would let a slow disk or a storm of uploads starve unrelated blocking
//! work such as password hashing. The cost of a very large caller chunk is bounded
//! by yielding to the scheduler every [`YIELD_EVERY_BYTES`] instead.

use std::future::Future;

use async_trait::async_trait;
use bytes::Bytes;
use md5::Md5;
use rustryx_core::BlobError;
use rustryx_core::model::Checksums;
use rustryx_core::ports::{BlobCommit, BlobWriter};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// Largest slice handed to the digest functions (and to the backend) at once.
pub const HASH_SLICE_BYTES: usize = 64 * 1024;

/// After this many hashed bytes without a natural suspension point the writer
/// yields to the scheduler. Keeps a single `write` of a huge chunk from holding a
/// worker for more than a few milliseconds (REQ-01: no task blocks a worker for
/// more than 10 ms).
pub const YIELD_EVERY_BYTES: usize = 1024 * 1024;

/// SHA-1, SHA-256 and MD5 of a byte stream, computed incrementally.
pub struct BlobHasher {
    sha1: Sha1,
    sha256: Sha256,
    md5: Md5,
    size: u64,
}

impl BlobHasher {
    /// Starts an empty hash.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sha1: Sha1::new(),
            sha256: Sha256::new(),
            md5: Md5::new(),
            size: 0,
        }
    }

    /// Feeds bytes. Synchronous: callers on an async task pass slices of at most
    /// [`HASH_SLICE_BYTES`] (the internal loop splits larger inputs as a safety net,
    /// but it cannot yield, so async callers must slice and yield themselves).
    pub fn update(&mut self, data: &[u8]) {
        for slice in data.chunks(HASH_SLICE_BYTES) {
            self.sha1.update(slice);
            self.sha256.update(slice);
            self.md5.update(slice);
        }
        self.size += data.len() as u64;
    }

    /// Bytes fed so far.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Finishes the hash and returns size and lowercase hex digests.
    #[must_use]
    pub fn finish(self) -> BlobCommit {
        BlobCommit {
            size_bytes: self.size,
            checksums: Checksums {
                sha1: hex::encode(self.sha1.finalize()),
                sha256: hex::encode(self.sha256.finalize()),
                md5: hex::encode(self.md5.finalize()),
            },
        }
    }
}

impl Default for BlobHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Backend half of a write: where the bytes go. Implemented by the local file sink
/// and the S3 multipart sink; consumed by [`HashingWriter`].
///
/// Contract: after `finish` or `discard` (successful or not) the sink has released
/// everything it owned (temporary file, multipart upload, concurrency permit).
/// Dropping a sink without either must also release it, on a best effort basis.
pub trait BlobSink: Send + 'static {
    /// Appends a slice of at most [`HASH_SLICE_BYTES`] bytes. Not cancel safe.
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), BlobError>> + Send;

    /// Makes the object visible atomically. Not cancel safe. On error the sink has
    /// already cleaned up after itself.
    fn finish(self) -> impl Future<Output = Result<(), BlobError>> + Send;

    /// Throws everything away. Safe to call after a failed `write`.
    fn discard(self) -> impl Future<Output = Result<(), BlobError>> + Send;
}

/// A [`BlobWriter`] that hashes every byte and forwards it to a [`BlobSink`].
///
/// After a failed `write` the writer is poisoned: later `write` and `commit` calls
/// return [`BlobError::WriterClosed`] (and `commit` discards the sink), because the
/// digests would no longer describe what the backend holds.
pub struct HashingWriter<S: BlobSink> {
    sink: S,
    hasher: BlobHasher,
    poisoned: bool,
    since_yield: usize,
}

impl<S: BlobSink> HashingWriter<S> {
    /// Wraps a sink.
    #[must_use]
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            hasher: BlobHasher::new(),
            poisoned: false,
            since_yield: 0,
        }
    }

    async fn write_slice(&mut self, slice: &[u8]) -> Result<(), BlobError> {
        self.hasher.update(slice);
        self.sink.write(slice).await?;
        self.since_yield += slice.len();
        if self.since_yield >= YIELD_EVERY_BYTES {
            self.since_yield = 0;
            tokio::task::yield_now().await;
        }
        Ok(())
    }
}

#[async_trait]
impl<S: BlobSink> BlobWriter for HashingWriter<S> {
    /// Hashes and forwards `chunk` in slices of at most 64 KiB. Not cancel safe: a
    /// cancelled call may have hashed or stored a prefix of the chunk, so the writer
    /// must be aborted or dropped, never reused.
    async fn write(&mut self, chunk: Bytes) -> Result<(), BlobError> {
        if self.poisoned {
            return Err(BlobError::WriterClosed);
        }
        for slice in chunk.chunks(HASH_SLICE_BYTES) {
            if let Err(err) = self.write_slice(slice).await {
                self.poisoned = true;
                return Err(err);
            }
        }
        Ok(())
    }

    /// Not cancel safe: the object may or may not be visible afterwards.
    async fn commit(self: Box<Self>) -> Result<BlobCommit, BlobError> {
        let Self {
            sink,
            hasher,
            poisoned,
            ..
        } = *self;
        if poisoned {
            // The discard result is secondary to the reason we are here.
            let _ = sink.discard().await;
            return Err(BlobError::WriterClosed);
        }
        sink.finish().await?;
        Ok(hasher.finish())
    }

    async fn abort(self: Box<Self>) -> Result<(), BlobError> {
        self.sink.discard().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::{Arc, Mutex};

    struct Vector {
        input: &'static [u8],
        sha1: &'static str,
        sha256: &'static str,
        md5: &'static str,
    }

    const VECTORS: &[Vector] = &[
        Vector {
            input: b"",
            sha1: "da39a3ee5e6b4b0d3255bfef95601890afd80709",
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            md5: "d41d8cd98f00b204e9800998ecf8427e",
        },
        Vector {
            input: b"abc",
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            md5: "900150983cd24fb0d6963f7d28e17f72",
        },
        Vector {
            input: b"The quick brown fox jumps over the lazy dog",
            sha1: "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12",
            sha256: "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592",
            md5: "9e107d9d372bb6826bd81d3542a419d6",
        },
    ];

    fn one_shot(data: &[u8]) -> BlobCommit {
        let mut hasher = BlobHasher::new();
        hasher.update(data);
        hasher.finish()
    }

    #[test]
    fn matches_published_digest_vectors() {
        for vector in VECTORS {
            let commit = one_shot(vector.input);
            assert_eq!(commit.size_bytes, vector.input.len() as u64);
            assert_eq!(commit.checksums.sha1, vector.sha1);
            assert_eq!(commit.checksums.sha256, vector.sha256);
            assert_eq!(commit.checksums.md5, vector.md5);
        }
    }

    #[test]
    fn matches_the_one_million_a_vector() {
        let data = vec![b'a'; 1_000_000];
        let commit = one_shot(&data);
        assert_eq!(
            commit.checksums.sha1,
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
        assert_eq!(
            commit.checksums.sha256,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(commit.checksums.md5, "7707d6ae4e027c70eea2a935c2296f21");
    }

    #[test]
    fn digests_are_lowercase_hex_of_the_right_length() {
        let checksums = one_shot(b"x").checksums;
        for (value, len) in [
            (&checksums.sha1, 40),
            (&checksums.sha256, 64),
            (&checksums.md5, 32),
        ] {
            assert_eq!(value.len(), len);
            assert!(
                value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
        }
    }

    /// What a sink saw, shared with the test through an `Arc`.
    #[derive(Default)]
    struct Recorded {
        data: Vec<u8>,
        largest_slice: usize,
        finished: bool,
        discarded: bool,
    }

    struct RecordingSink {
        seen: Arc<Mutex<Recorded>>,
        fail_after_bytes: Option<usize>,
    }

    impl BlobSink for RecordingSink {
        fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), BlobError>> + Send {
            let mut seen = self.seen.lock().unwrap();
            let over_limit = self
                .fail_after_bytes
                .is_some_and(|limit| seen.data.len() + data.len() > limit);
            let outcome = if over_limit {
                Err(BlobError::Unavailable("injected".into()))
            } else {
                seen.largest_slice = seen.largest_slice.max(data.len());
                seen.data.extend_from_slice(data);
                Ok(())
            };
            std::future::ready(outcome)
        }

        fn finish(self) -> impl Future<Output = Result<(), BlobError>> + Send {
            self.seen.lock().unwrap().finished = true;
            std::future::ready(Ok(()))
        }

        fn discard(self) -> impl Future<Output = Result<(), BlobError>> + Send {
            self.seen.lock().unwrap().discarded = true;
            std::future::ready(Ok(()))
        }
    }

    fn recording_writer(
        fail_after_bytes: Option<usize>,
    ) -> (Box<HashingWriter<RecordingSink>>, Arc<Mutex<Recorded>>) {
        let seen = Arc::new(Mutex::new(Recorded::default()));
        let sink = RecordingSink {
            seen: Arc::clone(&seen),
            fail_after_bytes,
        };
        (Box::new(HashingWriter::new(sink)), seen)
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
    }

    #[tokio::test]
    async fn chunk_boundaries_do_not_change_digests() {
        let sizes = [
            0,
            1,
            HASH_SLICE_BYTES - 1,
            HASH_SLICE_BYTES,
            HASH_SLICE_BYTES + 1,
            2 * HASH_SLICE_BYTES,
            3 * HASH_SLICE_BYTES + 7,
            YIELD_EVERY_BYTES + 5,
        ];
        for size in sizes {
            let data = pattern(size);
            let expected = one_shot(&data);

            let (mut writer, seen) = recording_writer(None);
            writer.write(Bytes::from(data.clone())).await.unwrap();
            let commit = writer.commit().await.unwrap();
            assert_eq!(commit, expected, "single chunk of {size} bytes");
            assert_eq!(seen.lock().unwrap().data, data);
            assert!(seen.lock().unwrap().finished);
        }
    }

    #[tokio::test]
    async fn many_small_chunks_equal_one_big_chunk() {
        let data = pattern(500_000);
        let expected = one_shot(&data);
        let (mut writer, _seen) = recording_writer(None);
        for chunk in data.chunks(1000) {
            writer.write(Bytes::copy_from_slice(chunk)).await.unwrap();
        }
        assert_eq!(writer.commit().await.unwrap(), expected);
    }

    #[tokio::test]
    async fn must_not_hand_the_backend_slices_larger_than_64_kib() {
        let (mut writer, seen) = recording_writer(None);
        writer
            .write(Bytes::from(vec![7_u8; 5 * HASH_SLICE_BYTES + 123]))
            .await
            .unwrap();
        writer.commit().await.unwrap();
        let largest = seen.lock().unwrap().largest_slice;
        assert!(
            largest <= HASH_SLICE_BYTES,
            "backend saw a {largest} byte slice"
        );
    }

    #[tokio::test]
    async fn empty_stream_commits_with_empty_digests() {
        let (writer, seen) = recording_writer(None);
        let commit = writer.commit().await.unwrap();
        assert_eq!(commit.size_bytes, 0);
        assert_eq!(commit.checksums.sha256, VECTORS[0].sha256);
        assert!(seen.lock().unwrap().finished);
    }

    #[tokio::test]
    async fn abort_discards_the_sink() {
        let (mut writer, seen) = recording_writer(None);
        writer.write(Bytes::from_static(b"partial")).await.unwrap();
        writer.abort().await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.discarded);
        assert!(!seen.finished);
    }

    #[tokio::test]
    async fn must_not_commit_after_a_failed_write() {
        let (mut writer, seen) = recording_writer(Some(10));
        assert!(
            writer
                .write(Bytes::from_static(b"0123456789abcdef"))
                .await
                .is_err()
        );
        assert!(matches!(
            writer.write(Bytes::from_static(b"more")).await,
            Err(BlobError::WriterClosed)
        ));
        assert!(matches!(
            writer.commit().await,
            Err(BlobError::WriterClosed)
        ));
        let seen = seen.lock().unwrap();
        assert!(seen.discarded, "a poisoned commit must discard the sink");
        assert!(!seen.finished);
    }

    proptest! {
        #[test]
        fn any_chunking_yields_the_same_digests(
            data in proptest::collection::vec(any::<u8>(), 0..300_000),
            cuts in proptest::collection::vec(1_usize..70_000, 1..12),
        ) {
            let expected = one_shot(&data);
            let mut hasher = BlobHasher::new();
            let mut offset = 0;
            let mut cut = cuts.iter().cycle();
            while offset < data.len() {
                let end = (offset + cut.next().copied().unwrap_or(1)).min(data.len());
                hasher.update(&data[offset..end]);
                offset = end;
            }
            prop_assert_eq!(hasher.size(), data.len() as u64);
            prop_assert_eq!(hasher.finish(), expected);
        }
    }
}
