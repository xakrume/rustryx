//! Policy hooks of the hosted pipeline: the seam that immutability rules, lifecycle holds and
//! quotas plug into.
//!
//! Two layers refuse a write, and they have different jobs:
//!
//! * **The hook** ([`ContentPolicy`]) runs in this crate BEFORE the request body is read. It
//!   exists so that a refusal costs nothing: no byte is received, no blob is staged. A quota
//!   pre check with the declared length (`QuotaRepo::headroom`) and an immutability look-up
//!   are the intended implementations. It is advisory.
//! * **The port** (`AssetRepo::publish_with`, `delete_by_path_checked`) enforces the same
//!   policies inside the transaction that switches the asset (SQL guards `RX004` to `RX008`).
//!   It is the authority: a hook that missed a race, or no hook at all, cannot let a
//!   protected item change.
//!
//! The pipeline treats both refusals identically: a [`rustryx_core::error::PolicyViolation`] becomes
//! [`FormatError::Policy`](crate::FormatError::Policy) (or the older variant of the same
//! meaning, see `From<PolicyViolation> for FormatError`), the staged or committed blob is
//! removed, and `policy_violations_total{reason}` counts it. A hook that cannot decide (its
//! own store is down) returns [`MutationError::Store`], which fails the request closed: an
//! undecidable write is never an allowed write.
//!
//! The default is [`NoopContentPolicy`]: with no hook configured the port guards are the
//! only enforcement, which is correct and merely refuses later.

use async_trait::async_trait;
use rustryx_core::error::MutationError;
use rustryx_core::identity::Principal;
use rustryx_core::model::{Asset, Repository};

use crate::hosted::ComponentSpec;
use crate::path::NormalizedPath;

/// What the hosted pipeline is about to write. Built by the pipeline; `#[non_exhaustive]` so
/// that a field added later does not break an implementer.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct WriteIntent<'a> {
    /// The repository that takes the write.
    pub repository: &'a Repository,
    /// Who writes.
    pub principal: &'a Principal,
    /// The normalized storage path.
    pub path: &'a NormalizedPath,
    /// The declared body length, when the client declared one. `None` for a chunked upload
    /// that has not finished: the finalize step asks again with the real total.
    pub declared_length: Option<u64>,
    /// The asset the write would replace, if the path exists.
    pub existing: Option<&'a Asset>,
    /// The component coordinate of the upload, when the format knows it.
    pub component: Option<&'a ComponentSpec>,
}

impl<'a> WriteIntent<'a> {
    /// An intent without declared length, existing asset or component.
    #[must_use]
    pub fn new(
        repository: &'a Repository,
        principal: &'a Principal,
        path: &'a NormalizedPath,
    ) -> Self {
        Self {
            repository,
            principal,
            path,
            declared_length: None,
            existing: None,
            component: None,
        }
    }
}

/// What the hosted pipeline is about to delete.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DeleteIntent<'a> {
    /// The repository the asset lives in.
    pub repository: &'a Repository,
    /// Who deletes.
    pub principal: &'a Principal,
    /// The normalized storage path.
    pub path: &'a NormalizedPath,
    /// The asset that would be removed.
    pub asset: &'a Asset,
}

impl<'a> DeleteIntent<'a> {
    /// An intent to delete `asset`.
    #[must_use]
    pub fn new(
        repository: &'a Repository,
        principal: &'a Principal,
        path: &'a NormalizedPath,
        asset: &'a Asset,
    ) -> Self {
        Self {
            repository,
            principal,
            path,
            asset,
        }
    }
}

/// Pre checks of writes and deletes on hosted repositories. See the module docs for how they
/// relate to the port guards. Both methods are cancel safe and must not block.
#[async_trait]
pub trait ContentPolicy: Send + Sync {
    /// Called before the body of an upload is read. `Ok(())` lets the pipeline proceed.
    ///
    /// # Errors
    /// [`MutationError::Violation`] for a refusal, [`MutationError::Store`] when the hook
    /// cannot decide (the request then fails closed).
    async fn check_write(&self, _intent: &WriteIntent<'_>) -> Result<(), MutationError> {
        Ok(())
    }

    /// Called before an asset is deleted.
    ///
    /// # Errors
    /// As [`ContentPolicy::check_write`].
    async fn check_delete(&self, _intent: &DeleteIntent<'_>) -> Result<(), MutationError> {
        Ok(())
    }
}

/// The hook that allows everything. The port guards still apply.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopContentPolicy;

#[async_trait]
impl ContentPolicy for NoopContentPolicy {}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::model::{RepoFormat, RepoType};

    fn repository() -> Repository {
        Repository::new(
            rustryx_core::ids::RepositoryId::generate(),
            rustryx_core::names::RepoName::parse("r").unwrap(),
            RepoFormat::Raw,
            RepoType::Hosted,
            rustryx_core::ids::BlobStoreId::generate(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
    }

    #[tokio::test]
    async fn the_default_hook_allows_everything() {
        let repository = repository();
        let principal = Principal::anonymous();
        let path = crate::path::NormalizedPath::parse("a/b").unwrap();
        let intent = WriteIntent::new(&repository, &principal, &path);
        assert!(intent.declared_length.is_none());
        assert!(intent.existing.is_none());
        assert!(intent.component.is_none());
        assert!(NoopContentPolicy.check_write(&intent).await.is_ok());
        let asset = Asset::new(
            rustryx_core::ids::AssetId::generate(),
            repository.id,
            "a/b",
            rustryx_core::ids::BlobId::generate(),
            "text/plain",
            1,
            rustryx_core::model::Checksums {
                sha1: String::new(),
                sha256: String::new(),
                md5: String::new(),
            },
            time::OffsetDateTime::UNIX_EPOCH,
        );
        let delete = DeleteIntent::new(&repository, &principal, &path, &asset);
        assert_eq!(delete.asset.size_bytes, 1);
        assert!(NoopContentPolicy.check_delete(&delete).await.is_ok());
    }

    #[tokio::test]
    async fn an_implementer_overrides_one_side_and_inherits_the_other() {
        struct RefuseDeletes;
        #[async_trait]
        impl ContentPolicy for RefuseDeletes {
            async fn check_delete(&self, _: &DeleteIntent<'_>) -> Result<(), MutationError> {
                Err(rustryx_core::error::PolicyViolation::ReadOnly.into())
            }
        }
        let repository = repository();
        let principal = Principal::anonymous();
        let path = crate::path::NormalizedPath::parse("a/b").unwrap();
        assert!(
            RefuseDeletes
                .check_write(&WriteIntent::new(&repository, &principal, &path))
                .await
                .is_ok()
        );
    }
}
