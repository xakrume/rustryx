//! Unit tests of the parent module: the provided methods and their fail closed defaults.

use futures::executor::block_on;

use super::*;
use crate::error::PolicyViolation;
use crate::ids::{ApiTokenId, RepositoryId};
use crate::model::{BlobRecord, Checksums};
use crate::testing::at;

struct StubBlobs;

#[async_trait]
impl BlobRepo for StubBlobs {
    async fn get(&self, _: BlobId) -> Result<Option<BlobRecord>, StoreError> {
        Ok(None)
    }
    async fn insert(&self, _: &BlobRecord) -> Result<(), StoreError> {
        Ok(())
    }
    async fn used_bytes_by_repository(&self, _: RepositoryId) -> Result<u64, StoreError> {
        Ok(0)
    }
    async fn used_bytes_by_store(&self, _: BlobStoreId) -> Result<u64, StoreError> {
        Ok(0)
    }
    async fn list_unreferenced(
        &self,
        _: OffsetDateTime,
        _: u32,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        Ok(Vec::new())
    }
    async fn list_unreferenced_in(
        &self,
        _: BlobStoreId,
        _: OffsetDateTime,
        _: u32,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        Ok(Vec::new())
    }
    async fn delete(&self, _: BlobId) -> Result<(), StoreError> {
        Ok(())
    }
    async fn delete_if_unreferenced(
        &self,
        _: BlobId,
        _: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        Ok(false)
    }
}

#[test]
fn the_provided_move_moves_nothing() {
    let moved = block_on(StubBlobs.move_blob(
        BlobId::generate(),
        BlobStoreId::generate(),
        BlobStoreId::generate(),
    ));
    assert!(matches!(moved, Ok(false)));
}

#[test]
fn must_not_report_an_empty_page_when_listing_by_store_is_not_implemented() {
    let listed = block_on(StubBlobs.list_page_by_store(BlobStoreId::generate(), None, 100, false));
    assert!(
        matches!(listed, Err(StoreError::Invalid(_))),
        "an empty page would make verification report every object as an orphan"
    );
}

struct StubTokens;

#[async_trait]
impl TokenRepo for StubTokens {
    async fn create(&self, _: &ApiToken) -> Result<(), StoreError> {
        Ok(())
    }
    async fn find_by_hash(&self, _: &str) -> Result<Option<ApiToken>, StoreError> {
        Ok(None)
    }
    async fn list_for_user(&self, _: UserId) -> Result<Vec<ApiToken>, StoreError> {
        Ok(Vec::new())
    }
    async fn revoke(&self, _: ApiTokenId, _: OffsetDateTime) -> Result<(), StoreError> {
        Ok(())
    }
    async fn touch_last_used(&self, _: ApiTokenId, _: OffsetDateTime) -> Result<(), StoreError> {
        Ok(())
    }
}

#[test]
fn the_provided_token_expiry_methods_are_empty_and_harmless() {
    block_on(async {
        let expiring = StubTokens
            .list_expiring(Duration::from_hours(7 * 24), at(), 100)
            .await
            .expect("ok");
        assert!(expiring.is_empty());
        StubTokens
            .mark_expiry_notified(ApiTokenId::generate(), at())
            .await
            .expect("ok");
    });
}

/// A repository whose only real behaviour is a refusal from `publish_with`.
struct Refusing(PolicyViolation);

#[async_trait]
impl AssetRepo for Refusing {
    async fn get_by_path(&self, _: RepositoryId, _: &str) -> Result<Option<Asset>, StoreError> {
        Ok(None)
    }
    async fn get(&self, _: AssetId) -> Result<Option<Asset>, StoreError> {
        Ok(None)
    }
    async fn publish_with(
        &self,
        _: &NewAsset,
        _: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        Err(MutationError::Violation(self.0.clone()))
    }
    async fn link(
        &self,
        _: &NewAssetLink,
        _: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        Err(MutationError::Violation(self.0.clone()))
    }
    async fn list(
        &self,
        _: RepositoryId,
        _: &str,
        _: &PageRequest,
    ) -> Result<Page<Asset>, StoreError> {
        Ok(Page {
            items: Vec::new(),
            next: None,
        })
    }
    async fn list_by_component(&self, _: ComponentId) -> Result<Vec<Asset>, StoreError> {
        Ok(Vec::new())
    }
    async fn delete_by_path(
        &self,
        _: RepositoryId,
        _: &str,
    ) -> Result<Option<BlobRecord>, StoreError> {
        Ok(None)
    }
    async fn delete_by_path_checked(
        &self,
        _: RepositoryId,
        _: &str,
        _: &WriteOptions,
    ) -> Result<Option<BlobRecord>, MutationError> {
        Ok(None)
    }
    async fn touch_downloaded(&self, _: &[(AssetId, OffsetDateTime)]) -> Result<(), StoreError> {
        Ok(())
    }
    async fn touch_fresh(&self, _: AssetId, _: OffsetDateTime) -> Result<(), StoreError> {
        Ok(())
    }
    async fn record_downloads(&self, _: &[DownloadTick]) -> Result<(), StoreError> {
        Ok(())
    }
}

fn blob() -> BlobRecord {
    BlobRecord {
        id: BlobId::generate(),
        blob_store_id: BlobStoreId::generate(),
        key: "k".into(),
        size_bytes: 1,
        checksums: Checksums {
            sha1: "a".into(),
            sha256: "b".into(),
            md5: "c".into(),
        },
        created_at: at(),
    }
}

#[test]
fn the_provided_publish_flattens_a_violation_into_a_conflict() {
    let repo = Refusing(PolicyViolation::ReadOnly);
    let new = NewAsset::new(RepositoryId::generate(), "p", "text/plain", blob());
    let outcome = block_on(repo.publish(&new));
    assert!(
        matches!(outcome, Err(StoreError::Conflict(ref message)) if message == "read only mode")
    );
}

#[test]
fn the_provided_publish_passes_default_write_options() {
    use std::sync::Mutex;

    struct Recording(Mutex<Option<(bool, usize)>>);

    #[async_trait]
    impl AssetRepo for Recording {
        async fn get_by_path(&self, _: RepositoryId, _: &str) -> Result<Option<Asset>, StoreError> {
            Ok(None)
        }
        async fn get(&self, _: AssetId) -> Result<Option<Asset>, StoreError> {
            Ok(None)
        }
        async fn publish_with(
            &self,
            new: &NewAsset,
            options: &WriteOptions,
        ) -> Result<PublishOutcome, MutationError> {
            *self.0.lock().expect("lock") =
                Some((options.override_immutable, options.events.len()));
            let asset = Asset::from_blob(
                AssetId::generate(),
                new.repository_id,
                &new.path,
                &new.content_type,
                &new.blob,
                at(),
            );
            Ok(PublishOutcome::new(asset, None, true))
        }
        async fn link(
            &self,
            _: &NewAssetLink,
            _: &WriteOptions,
        ) -> Result<PublishOutcome, MutationError> {
            Err(MutationError::Store(StoreError::NotFound("n/a".into())))
        }
        async fn list(
            &self,
            _: RepositoryId,
            _: &str,
            _: &PageRequest,
        ) -> Result<Page<Asset>, StoreError> {
            Ok(Page {
                items: Vec::new(),
                next: None,
            })
        }
        async fn list_by_component(&self, _: ComponentId) -> Result<Vec<Asset>, StoreError> {
            Ok(Vec::new())
        }
        async fn delete_by_path(
            &self,
            _: RepositoryId,
            _: &str,
        ) -> Result<Option<BlobRecord>, StoreError> {
            Ok(None)
        }
        async fn delete_by_path_checked(
            &self,
            _: RepositoryId,
            _: &str,
            _: &WriteOptions,
        ) -> Result<Option<BlobRecord>, MutationError> {
            Ok(None)
        }
        async fn touch_downloaded(
            &self,
            _: &[(AssetId, OffsetDateTime)],
        ) -> Result<(), StoreError> {
            Ok(())
        }
        async fn touch_fresh(&self, _: AssetId, _: OffsetDateTime) -> Result<(), StoreError> {
            Ok(())
        }
        async fn record_downloads(&self, _: &[DownloadTick]) -> Result<(), StoreError> {
            Ok(())
        }
    }

    let repo = Recording(Mutex::new(None));
    let new = NewAsset::new(RepositoryId::generate(), "p", "text/plain", blob());
    let outcome = block_on(repo.publish(&new)).expect("published");
    assert!(outcome.created);
    assert_eq!(
        *repo.0.lock().expect("lock"),
        Some((false, 0)),
        "the wrapper never overrides immutability"
    );
}

#[test]
fn page_request_defaults_to_a_hundred_items_from_the_start() {
    let page = PageRequest::default();
    assert_eq!((page.limit, page.after), (100, None));
}

#[test]
fn page_serializes_items_and_the_continuation_token() {
    let page = Page {
        items: vec![1, 2],
        next: Some("t".to_owned()),
    };
    assert_eq!(
        serde_json::to_string(&page).expect("json"),
        r#"{"items":[1,2],"next":"t"}"#
    );
}
