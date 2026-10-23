//! `list_keys` for S3: one page of the bucket listing, restricted to what this crate wrote.
//!
//! The cursor is the location (`<b0>/<b1>/<key>`, without the store prefix) of the last
//! key of the previous page, passed to `list_with_offset` (S3 `start-after`), so a key
//! deleted between two pages does not break the walk. Order is the lexicographic order of
//! the object names, which is stable and, because the shard is a hash of the key, not the
//! order of the keys themselves.
//!
//! A bucket may hold objects that are not ours (a shared bucket with no prefix, the health
//! probe name, a console upload). Anything that is not `<b0>/<b1>/<valid key>` with the
//! shard of that key is skipped and counted in one log line: such an object cannot be
//! reached by `get`, `head` or `delete`, so reporting it as a blob would invite a caller to
//! act on something the store cannot address.

use futures::StreamExt;
use object_store::ObjectStore;
use object_store::path::Path as ObjectPath;
use rustryx_core::BlobError;
use rustryx_core::ports::{BlobKey, KeyPage};

use super::{S3BlobStore, blob_meta, store_error};
use crate::error::{backend, with_deadline};
use crate::key::{MAX_LIST_PAGE, key_of_location};

impl S3BlobStore {
    pub(super) async fn list_page(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<KeyPage, BlobError> {
        let want = limit.clamp(1, MAX_LIST_PAGE) as usize;
        let prefix = &self.backend.prefix;
        let offset = match after {
            Some(cursor) => {
                // A client-held cursor is validated before it reaches the bucket.
                key_of_location(cursor).ok_or(BlobError::InvalidKey("invalid list cursor"))?;
                Some(
                    ObjectPath::parse(format!("{prefix}{cursor}"))
                        .map_err(|e| backend("list cursor", e))?,
                )
            }
            None => None,
        };
        let root = match prefix.strip_suffix('/') {
            Some(name) => Some(ObjectPath::parse(name).map_err(|e| backend("list prefix", e))?),
            None => None,
        };
        let mut stream = match &offset {
            Some(offset) => self.backend.store.list_with_offset(root.as_ref(), offset),
            None => self.backend.store.list(root.as_ref()),
        };

        let (found, skipped) = with_deadline("s3 list", self.deadline(), async {
            let mut found = Vec::new();
            let mut skipped = 0_usize;
            let mut previous: Option<String> = None;
            // One more than the page: its existence is what makes `next` honest.
            while found.len() <= want {
                let Some(item) = stream.next().await else {
                    break;
                };
                let meta = item.map_err(|e| store_error("list objects", e))?;
                let name = meta.location.as_ref();
                // The cursor is only sound if names arrive in ascending order: S3 and every
                // dialect we target list that way, but the `ObjectStore` trait does not
                // promise it. A backend that breaks it would make a walk repeat or miss keys
                // without any error, so it is an error here.
                if previous.as_deref().is_some_and(|last| name <= last) {
                    return Err(backend(
                        "list objects",
                        "the object store listed names out of order",
                    ));
                }
                previous = Some(name.to_owned());
                let location = name.strip_prefix(prefix.as_str()).unwrap_or(name);
                match key_of_location(location) {
                    Some(key) => {
                        found.push((BlobKey::new(key), blob_meta(&meta), location.to_owned()));
                    }
                    None => skipped += 1,
                }
            }
            Ok((found, skipped))
        })
        .await?;
        if skipped > 0 {
            tracing::warn!(
                skipped,
                "list_keys skipped objects that are not blobs of this store"
            );
        }

        let more = found.len() > want;
        let mut keys = Vec::with_capacity(found.len().min(want));
        let mut next = None;
        for (key, meta, location) in found.into_iter().take(want) {
            next = more.then_some(location);
            keys.push((key, meta));
        }
        Ok(KeyPage { keys, next })
    }
}
