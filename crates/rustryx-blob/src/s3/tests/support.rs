//! Fixtures for the S3 store tests: a fake multipart API over an in-memory store.

use std::collections::HashMap;
use std::sync::Mutex;

use bytes::Bytes;
use object_store::memory::InMemory;
use object_store::multipart::PartId;
use object_store::{MultipartId, PutPayload, PutResult};

use super::super::*;
use crate::hashing::BlobHasher;

pub(super) const PART: usize = 1000;

#[derive(Default, Debug)]
pub(super) struct FakeState {
    pub(super) next_id: u64,
    pub(super) open: HashMap<String, Vec<Option<Bytes>>>,
    pub(super) created: usize,
    pub(super) aborted: Vec<String>,
    pub(super) part_sizes: Vec<usize>,
    pub(super) fail_part_at: Option<usize>,
    pub(super) fail_complete: bool,
    pub(super) fail_abort: bool,
}

/// A multipart API that assembles completed uploads into an `InMemory` store.
#[derive(Debug)]
pub(super) struct FakeMultipart {
    pub(super) target: Arc<InMemory>,
    pub(super) state: Mutex<FakeState>,
}

/// Plain copy of what the fake recorded, so tests never hold the lock over an await.
pub(super) struct Snapshot {
    pub(super) created: usize,
    pub(super) open: usize,
    pub(super) aborted: usize,
    pub(super) part_sizes: Vec<usize>,
}

impl FakeMultipart {
    pub(super) fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    pub(super) fn snap(&self) -> Snapshot {
        let state = self.state();
        Snapshot {
            created: state.created,
            open: state.open.len(),
            aborted: state.aborted.len(),
            part_sizes: state.part_sizes.clone(),
        }
    }
}

pub(super) fn assert_empty<T: std::fmt::Debug + PartialEq>(items: &[T]) {
    assert_eq!(items, &[] as &[T]);
}

pub(super) fn injected(what: &'static str) -> object_store::Error {
    object_store::Error::Generic {
        store: "fake",
        source: what.into(),
    }
}

#[async_trait]
impl MultipartStore for FakeMultipart {
    async fn create_multipart(&self, _path: &ObjectPath) -> object_store::Result<MultipartId> {
        let mut state = self.state();
        state.next_id += 1;
        state.created += 1;
        let id = format!("upload-{}", state.next_id);
        state.open.insert(id.clone(), Vec::new());
        Ok(id)
    }

    async fn put_part(
        &self,
        _path: &ObjectPath,
        id: &MultipartId,
        part_idx: usize,
        data: PutPayload,
    ) -> object_store::Result<PartId> {
        let mut state = self.state();
        if state.fail_part_at == Some(part_idx) {
            return Err(injected("part failure"));
        }
        let bytes = Bytes::from(data);
        state.part_sizes.push(bytes.len());
        let parts = state
            .open
            .get_mut(id)
            .ok_or_else(|| injected("unknown upload"))?;
        if parts.len() <= part_idx {
            parts.resize(part_idx + 1, None);
        }
        parts[part_idx] = Some(bytes);
        Ok(PartId {
            content_id: format!("{id}-{part_idx}"),
        })
    }

    async fn complete_multipart(
        &self,
        path: &ObjectPath,
        id: &MultipartId,
        parts: Vec<PartId>,
    ) -> object_store::Result<PutResult> {
        let assembled = {
            let mut state = self.state();
            if state.fail_complete {
                return Err(injected("complete failure"));
            }
            let stored = state
                .open
                .remove(id)
                .ok_or_else(|| injected("unknown upload"))?;
            assert_eq!(stored.len(), parts.len(), "complete must list every part");
            let mut all = Vec::new();
            for part in stored {
                all.extend_from_slice(&part.ok_or_else(|| injected("missing part"))?);
            }
            all
        };
        self.target.put(path, PutPayload::from(assembled)).await
    }

    async fn abort_multipart(
        &self,
        _path: &ObjectPath,
        id: &MultipartId,
    ) -> object_store::Result<()> {
        let mut state = self.state();
        if state.fail_abort {
            return Err(injected("abort failure"));
        }
        state.open.remove(id);
        state.aborted.push(id.clone());
        Ok(())
    }
}

pub(super) struct Fixture {
    pub(super) store: S3BlobStore,
    pub(super) target: Arc<InMemory>,
    pub(super) fake: Arc<FakeMultipart>,
}

pub(super) fn settings(max_uploads: usize) -> S3Settings {
    S3Settings {
        part_size: PART,
        max_concurrent_uploads: max_uploads,
        permit_timeout: Duration::from_millis(60),
        metadata_timeout: Duration::from_secs(5),
        part_timeout: Duration::from_secs(5),
    }
}

pub(super) fn fixture_with(prefix: &str, settings: S3Settings) -> Fixture {
    let target = Arc::new(InMemory::new());
    let fake = Arc::new(FakeMultipart {
        target: Arc::clone(&target),
        state: Mutex::default(),
    });
    let store = S3BlobStore::from_backends(
        Arc::clone(&target) as Arc<dyn ObjectStore>,
        Arc::clone(&fake) as Arc<dyn MultipartStore>,
        prefix,
        settings,
    )
    .unwrap();
    Fixture {
        store,
        target,
        fake,
    }
}

pub(super) fn fixture() -> Fixture {
    fixture_with("", settings(4))
}

pub(super) fn key(name: &str) -> BlobKey {
    BlobKey::new(name)
}

pub(super) fn pattern(len: usize) -> Bytes {
    Bytes::from(
        (0..len)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect::<Vec<u8>>(),
    )
}

pub(super) async fn put(
    fx: &Fixture,
    name: &str,
    data: &Bytes,
    chunk: usize,
) -> rustryx_core::ports::BlobCommit {
    let mut writer = fx
        .store
        .begin(&key(name), Some(data.len() as u64))
        .await
        .unwrap();
    for piece in data.chunks(chunk) {
        writer.write(Bytes::copy_from_slice(piece)).await.unwrap();
    }
    writer.commit().await.unwrap()
}

pub(super) async fn read_all(store: &S3BlobStore, name: &str, range: Option<ByteRange>) -> Vec<u8> {
    let read = store.get(&key(name), range).await.unwrap();
    let chunks: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    chunks.concat()
}

pub(super) fn digest(data: &[u8]) -> rustryx_core::ports::BlobCommit {
    let mut hasher = BlobHasher::new();
    hasher.update(data);
    hasher.finish()
}
