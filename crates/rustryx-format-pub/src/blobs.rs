//! Reading stored blobs for the handler's own needs (version documents, the staged archive).

use bytes::Bytes;
use rustryx_core::error::BlobError;
use rustryx_core::model::{Asset, BlobRecord};
use rustryx_core::ports::{BlobKey, BlobRead};
use rustryx_format_core::stream::{BoundedReadError, collect_bounded};
use rustryx_format_core::{FormatContext, FormatError};

/// The blob row and an open read of the asset's content. The row is read first because it names
/// the store that holds the bytes: for a repository on a group store that is the member, and a
/// read never probes the members.
pub(crate) async fn open(
    cx: &FormatContext,
    asset: &Asset,
) -> Result<(BlobRecord, BlobRead), FormatError> {
    let record = cx.ports().blobs.get(asset.blob_id).await?.ok_or_else(|| {
        FormatError::internal("asset references a blob record that does not exist")
    })?;
    let store = cx.store_for_blob(&record).await?;
    let read = match store.get(&BlobKey::new(record.key.clone()), None).await {
        Ok(read) => read,
        // The row says the object exists. A store that says it does not has lost data, and
        // that is our fault, not a missing package.
        Err(BlobError::NotFound) => {
            return Err(FormatError::internal(
                "blob object missing in the store for an existing asset",
            ));
        }
        Err(other) => return Err(other.into()),
    };
    Ok((record, read))
}

/// The whole content of a small asset, refusing more than `limit` bytes.
pub(crate) async fn read_small(
    cx: &FormatContext,
    asset: &Asset,
    limit: u64,
) -> Result<Bytes, FormatError> {
    let (_, read) = open(cx, asset).await?;
    collect_bounded(read.stream, limit)
        .await
        .map_err(|error| match error {
            BoundedReadError::TooLarge { .. } => {
                FormatError::internal("a stored document is larger than its limit")
            }
            BoundedReadError::Read(detail) => {
                FormatError::internal(format!("a stored document could not be read: {detail}"))
            }
        })
}
