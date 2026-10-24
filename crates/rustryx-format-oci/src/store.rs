//! How images are stored in the content ports, and the lookups over that layout.
//!
//! Core has no table of tags or referrers, so the registry keeps all of its state in assets
//! and components, which retention, garbage collection and scanning already understand:
//!
//! | what | asset path (below the Rustryx repository) | component |
//! |---|---|---|
//! | manifest | `<image>/manifests/<digest>` | `name = <image>`, `version = <digest>` |
//! | tag | `<image>/manifests/<tag>`, the SAME blob as the manifest | that manifest's component |
//! | blob | `<image>/blobs/<digest>` | none |
//! | referrer link | `<image>/referrers/<subject digest>/<referrer digest>`, the blob of the referrer manifest | the referrer's component |
//!
//! Consequences worth knowing:
//!
//! * an asset path IS the normalized request path, so authorization and storage see one
//!   string;
//! * a tag costs one asset row and no bytes (`AssetRepo::link` shares the blob), and moving
//!   a tag is one transaction, guarded by the same write policy, immutability and hold
//!   guards as every other path;
//! * deleting a manifest is deleting its component: the manifest, every tag that points at it
//!   and the referrer links it owns go in one transaction;
//! * listing tags or images scans assets or components, bounded by the caps of
//!   [`crate::config::OciConfig`]. A table keyed by `(repository, image, tag)` would make
//!   them index lookups; it is a core change (see `docs/format-oci.md`, open issues).

use std::collections::BTreeSet;

use bytes::Bytes;
use rustryx_core::model::Asset;
use rustryx_core::ports::{BlobKey, ComponentFilter, PageRequest};
use rustryx_format_core::FormatContext;
use rustryx_format_core::stream::collect_bounded;

use crate::error::OciError;
use crate::parse::digest::Digest;
use crate::parse::name::RepositoryName;
use crate::parse::reference::is_tag;

/// Rows asked of a port per call while scanning.
const SCAN_PAGE: u32 = 1000;

/// `<image>/manifests/<reference>`: a manifest by digest or a tag.
#[must_use]
pub fn manifest_path(image: &RepositoryName, reference: &str) -> String {
    format!("{image}/manifests/{reference}")
}

/// `<image>/blobs/<digest>`.
#[must_use]
pub fn blob_path(image: &RepositoryName, digest: &Digest) -> String {
    format!("{image}/blobs/{digest}")
}

/// `<image>/referrers/<subject>/`: the prefix of the referrer links of a subject.
#[must_use]
pub fn referrer_prefix(image: &RepositoryName, subject: &Digest) -> String {
    format!("{image}/referrers/{subject}/")
}

/// `<image>/referrers/<subject>/<referrer>`.
#[must_use]
pub fn referrer_path(image: &RepositoryName, subject: &Digest, referrer: &Digest) -> String {
    format!("{}{referrer}", referrer_prefix(image, subject))
}

/// Looks an asset up in the context's repository.
pub(crate) async fn get_asset(cx: &FormatContext, path: &str) -> Result<Option<Asset>, OciError> {
    Ok(cx
        .ports()
        .assets
        .get_by_path(cx.repository().id, path)
        .await
        .map_err(rustryx_format_core::FormatError::from)?)
}

/// True when the repository has at least one manifest of `image`.
pub(crate) async fn image_exists(
    cx: &FormatContext,
    image: &RepositoryName,
) -> Result<bool, OciError> {
    let filter = ComponentFilter {
        repository_id: Some(cx.repository().id),
        name: Some(image.as_str().to_owned()),
        ..ComponentFilter::default()
    };
    let page = PageRequest {
        limit: 1,
        after: None,
    };
    let found = cx
        .ports()
        .components
        .list(&filter, &page)
        .await
        .map_err(rustryx_format_core::FormatError::from)?;
    Ok(!found.items.is_empty())
}

/// All tags of an image, sorted by byte order (the "lexical order" of the specification).
///
/// Reads every asset below `<image>/manifests/`; a path counts as a tag when what follows the
/// prefix is one valid tag (the digest assets, which contain a `:`, and the entries of nested
/// images, which contain a `/`, do not).
///
/// # Errors
/// `UNAVAILABLE` when more than `cap` assets would have to be read: a truncated list would
/// be read by the client as "that tag does not exist".
pub(crate) async fn list_tags(
    cx: &FormatContext,
    image: &RepositoryName,
    cap: usize,
) -> Result<Vec<String>, OciError> {
    let prefix = format!("{image}/manifests/");
    let mut tags = BTreeSet::new();
    let mut scanned = 0usize;
    let mut after = None;
    loop {
        let page = cx
            .ports()
            .assets
            .list(
                cx.repository().id,
                &prefix,
                &PageRequest {
                    limit: SCAN_PAGE,
                    after,
                },
            )
            .await
            .map_err(rustryx_format_core::FormatError::from)?;
        scanned = scanned.saturating_add(page.items.len());
        if scanned > cap {
            return Err(too_many(
                "the image has too many manifests to list its tags",
            ));
        }
        for asset in &page.items {
            if let Some(tag) = asset.path.strip_prefix(&prefix)
                && is_tag(tag)
            {
                tags.insert(tag.to_owned());
            }
        }
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(tags.into_iter().collect())
}

/// The image names of a repository (the names of its components), sorted.
///
/// # Errors
/// `UNAVAILABLE` when more than `cap` components would have to be read.
pub(crate) async fn list_images(
    components: &dyn rustryx_core::ports::ComponentRepo,
    repository: rustryx_core::ids::RepositoryId,
    cap: usize,
) -> Result<Vec<String>, OciError> {
    let filter = ComponentFilter {
        repository_id: Some(repository),
        ..ComponentFilter::default()
    };
    let mut names = BTreeSet::new();
    let mut scanned = 0usize;
    let mut after = None;
    loop {
        let page = components
            .list(
                &filter,
                &PageRequest {
                    limit: SCAN_PAGE,
                    after,
                },
            )
            .await
            .map_err(rustryx_format_core::FormatError::from)?;
        scanned = scanned.saturating_add(page.items.len());
        if scanned > cap {
            return Err(too_many(
                "the repository has too many manifests to list its images",
            ));
        }
        names.extend(page.items.into_iter().map(|component| component.name));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(names.into_iter().collect())
}

/// Reads the bytes of an asset into memory, at most `limit`. Only for manifests and other
/// small documents.
pub(crate) async fn read_small(
    cx: &FormatContext,
    asset: &Asset,
    limit: u64,
) -> Result<Bytes, OciError> {
    if asset.size_bytes > limit {
        return Err(OciError::internal(
            "a stored document is larger than its limit",
        ));
    }
    let record = cx
        .ports()
        .blobs
        .get(asset.blob_id)
        .await
        .map_err(rustryx_format_core::FormatError::from)?
        .ok_or_else(|| {
            OciError::internal("an asset references a blob record that does not exist")
        })?;
    let store = cx.store_for_blob(&record).await?;
    let read = store
        .get(&BlobKey::new(record.key), None)
        .await
        .map_err(rustryx_format_core::FormatError::from)?;
    collect_bounded(read.stream, limit)
        .await
        .map_err(|error| OciError::internal(format!("reading a stored document: {error}")))
}

fn too_many(message: &str) -> OciError {
    OciError::new(
        http::StatusCode::SERVICE_UNAVAILABLE,
        crate::error::OciErrorCode::Unavailable,
        message,
    )
}

#[cfg(test)]
mod tests;
