//! Row structs shared by more than one repository.

use rustryx_core::StoreError;
use rustryx_core::ids::{AssetId, BlobId, BlobStoreId, ComponentId, RepositoryId, UserId};
use rustryx_core::model::{Asset, BlobRecord, Checksums};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, to_u64};

#[derive(Debug, FromRow)]
pub(crate) struct BlobRow {
    pub id: Uuid,
    pub blob_store_id: Uuid,
    pub key: String,
    pub size_bytes: i64,
    pub sha1: String,
    pub sha256: String,
    pub md5: String,
    pub created_at: OffsetDateTime,
}

impl BlobRow {
    pub(crate) fn into_record(self) -> Result<BlobRecord, StoreError> {
        Ok(BlobRecord {
            id: BlobId::from_uuid(self.id),
            blob_store_id: BlobStoreId::from_uuid(self.blob_store_id),
            key: self.key,
            size_bytes: to_u64(self.size_bytes, "read blob")?,
            checksums: Checksums {
                sha1: self.sha1,
                sha256: self.sha256,
                md5: self.md5,
            },
            created_at: self.created_at,
        })
    }
}

/// `SELECT` of an asset joined with its current blob, with a literal tail appended. Size and
/// digests come from the blob row, their single source of truth, so they cannot drift from
/// what the asset points to. A macro because `sqlx` 0.9 accepts only `'static` SQL, and
/// `concat!` is the way to build one from literals.
macro_rules! asset_select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT a.id, a.repository_id, a.component_id, a.path, a.blob_id, a.content_type, \
             b.size_bytes, b.sha1, b.sha256, b.md5, a.last_downloaded_at, a.created_by, \
             a.created_at, a.updated_at FROM assets a JOIN blobs b ON b.id = a.blob_id ",
            $($tail),*
        )
    };
}
pub(crate) use asset_select;

#[derive(Debug, FromRow)]
pub(crate) struct AssetRow {
    pub id: Uuid,
    pub repository_id: Uuid,
    pub component_id: Option<Uuid>,
    pub path: String,
    pub blob_id: Uuid,
    pub content_type: String,
    pub size_bytes: i64,
    pub sha1: String,
    pub sha256: String,
    pub md5: String,
    pub last_downloaded_at: Option<OffsetDateTime>,
    pub created_by: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl AssetRow {
    pub(crate) fn into_asset(self) -> Result<Asset, StoreError> {
        if self.path.is_empty() {
            return Err(corrupt("read asset"));
        }
        let mut asset = Asset::new(
            AssetId::from_uuid(self.id),
            RepositoryId::from_uuid(self.repository_id),
            self.path,
            BlobId::from_uuid(self.blob_id),
            self.content_type,
            to_u64(self.size_bytes, "read asset")?,
            Checksums {
                sha1: self.sha1,
                sha256: self.sha256,
                md5: self.md5,
            },
            self.created_at,
        );
        asset.component_id = self.component_id.map(ComponentId::from_uuid);
        asset.last_downloaded_at = self.last_downloaded_at;
        asset.created_by = self.created_by.map(UserId::from_uuid);
        asset.updated_at = self.updated_at;
        Ok(asset)
    }
}
