//! Reads of a hosted repository: the listing, the single version, the archive.
//!
//! What is stored, per published version (see `finalize`):
//!
//! * `packages/<package>/versions/<version>.tar.gz`, the archive, linked to the blob the upload
//!   staged, so the bytes are written once;
//! * `api/packages/<package>/versions/<version>`, a small JSON document with the manifest, the
//!   digest and the publication time.
//!
//! Both belong to the component `(package, version)`, so deleting the component through the REST
//! API removes both, and the hosted member of a group claims the package name through that
//! component. The listing is the version documents of the package read back and joined. The
//! `archive_url` is not stored: it depends on the address the client used (and on the group it
//! went through), so it is built here, from the repository the client addressed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum_core::body::Body;
use futures::{StreamExt, TryStreamExt, stream};
use http::Response;
use rustryx_core::model::Asset;
use rustryx_core::ports::PageRequest;
use rustryx_format_core::{ArtifactRequest, FormatContext, FormatError};

use crate::blobs;
use crate::config::PubConfig;
use crate::document::{PackageDoc, VersionEntry};
use crate::name::PackageName;
use crate::response;
use crate::route::{archive_path, version_prefix};
use crate::version::PubVersion;

/// Documents fetched per database page.
const PAGE_LIMIT: u32 = 500;

/// `GET api/packages/<package>`.
pub(crate) async fn list(
    cx: &FormatContext,
    cfg: &PubConfig,
    req: &ArtifactRequest,
    name: &PackageName,
) -> Result<Response<Body>, FormatError> {
    let assets = version_assets(cx, cfg, name).await?;
    let budget = Arc::new(AtomicU64::new(0));
    let entries: Vec<Option<VersionEntry>> = stream::iter(assets)
        .map(|asset| {
            let budget = Arc::clone(&budget);
            async move { read_entry(cx, cfg, req, name, &asset, &budget).await }
        })
        .buffered(cfg.version_read_concurrency.max(1))
        .try_collect()
        .await?;
    let doc = PackageDoc::new(name.clone(), false, None, entries.into_iter().flatten());
    if doc.versions().is_empty() {
        return Err(FormatError::NotFound);
    }
    Ok(response::document(req, doc.to_bytes()))
}

/// `GET api/packages/<package>/versions/<version>` (the deprecated single version document).
pub(crate) async fn version(
    cx: &FormatContext,
    cfg: &PubConfig,
    req: &ArtifactRequest,
    name: &PackageName,
    version: &PubVersion,
) -> Result<Response<Body>, FormatError> {
    let asset = cx
        .ports()
        .assets
        .get_by_path(cx.repository().id, req.path.as_str())
        .await?
        .ok_or(FormatError::NotFound)?;
    let budget = AtomicU64::new(0);
    let entry = read_entry(cx, cfg, req, name, &asset, &budget)
        .await?
        .filter(|entry| &entry.version == version)
        .ok_or(FormatError::NotFound)?;
    Ok(response::document(
        req,
        PackageDoc::single_version_bytes(&entry),
    ))
}

/// `GET packages/<package>/versions/<version>.tar.gz`: streamed from the blob store.
pub(crate) async fn archive(
    cx: &FormatContext,
    req: &ArtifactRequest,
) -> Result<Response<Body>, FormatError> {
    cx.hosted().download(req).await
}

/// Every version document asset of a package, paged, refusing more than the configured count.
async fn version_assets(
    cx: &FormatContext,
    cfg: &PubConfig,
    name: &PackageName,
) -> Result<Vec<Asset>, FormatError> {
    let prefix = version_prefix(name);
    let mut assets = Vec::new();
    let mut after = None;
    loop {
        let page = cx
            .ports()
            .assets
            .list(
                cx.repository().id,
                &prefix,
                &PageRequest {
                    limit: PAGE_LIMIT,
                    after: after.take(),
                },
            )
            .await?;
        assets.extend(page.items);
        if assets.len() > cfg.max_versions_per_package {
            // A truncated list would hide versions without saying so.
            return Err(FormatError::internal(
                "a package has more versions than the configured limit",
            ));
        }
        match page.next {
            Some(token) => after = Some(token),
            None => return Ok(assets),
        }
    }
}

/// Reads one stored version document. `None` means the document is not trustworthy (it does
/// not describe the version its path names) and the version is left out, with a warning.
async fn read_entry(
    cx: &FormatContext,
    cfg: &PubConfig,
    req: &ArtifactRequest,
    name: &PackageName,
    asset: &Asset,
    budget: &AtomicU64,
) -> Result<Option<VersionEntry>, FormatError> {
    // All documents of one listing together stay under the document limit of format-core.
    let used = budget.fetch_add(asset.size_bytes, Ordering::Relaxed) + asset.size_bytes;
    if used > cx.limits().max_document_bytes {
        return Err(FormatError::internal(
            "the stored version documents of a package are larger than the document limit",
        ));
    }
    let bytes = blobs::read_small(cx, asset, cfg.max_version_document_bytes).await?;
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, path = %asset.path, "a stored version document is not JSON");
            return Ok(None);
        }
    };
    let path_version = asset.path.rsplit('/').next().unwrap_or_default();
    let Some(stored_version) = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| PubVersion::parse(text).ok())
    else {
        tracing::warn!(path = %asset.path, "a stored version document has no valid version");
        return Ok(None);
    };
    if stored_version.to_string() != path_version {
        tracing::warn!(path = %asset.path, "a stored version document names another version");
        return Ok(None);
    }
    let url = format!(
        "{}/{}",
        req.head.repository_url(&cx.origin_repository().name),
        archive_path(name, &stored_version)
    );
    let entry = VersionEntry::from_stored(&value, name, url);
    if entry.is_none() {
        tracing::warn!(path = %asset.path, "a stored version document is not a valid entry");
    }
    Ok(entry)
}
