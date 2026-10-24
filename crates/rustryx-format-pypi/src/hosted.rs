//! Hosted repositories: the pages generated from what was uploaded, and the yank and delete
//! operations on a file.
//!
//! The data model on top of the asset store (all paths are below `packages/<project>/`):
//!
//! | asset | meaning |
//! |---|---|
//! | `<file>` | the wheel or sdist, a component `(project, version)` |
//! | `<file>.metadata` | the wheel's `METADATA`, written at upload (PEP 658) |
//! | `<file>.info.json` | `requires-python` and `metadata-version`, written at upload |
//! | `<file>.yanked` | exists while the file is yanked, holds the reason (PEP 592) |
//!
//! A project page is one paged listing of the project directory plus the small records of the
//! files in it, read through a bounded cache. Index size is bounded in files and in bytes; a
//! project over the bound is refused rather than truncated, because a page that silently lacks
//! files lets a resolver pick an older version.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum_core::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_core::model::{Asset, RepoType};
use rustryx_core::ports::{BlobKey, ComponentFilter, PageRequest};
use rustryx_format_core::stream::{BoundedReadError, collect_bounded, read_bounded};
use rustryx_format_core::{
    ArtifactRequest, ComponentSpec, FormatContext, FormatError, NormalizedPath, OverwriteRule,
    UploadSpec,
};
use time::OffsetDateTime;

use crate::filename::DistFilename;
use crate::handler::PypiHandler;
use crate::name::ProjectName;
use crate::page::{CoreMetadataRef, FileEntry, ProjectPage, RootPage};
use crate::route::{
    PACKAGES, file_path, file_url, info_path, metadata_path, project_prefix, yank_path,
};
use crate::sidecar::{
    FileInfo, MAX_SIDECAR_BYTES, MAX_YANK_REASON_BYTES, ReasonError, validate_reason, yank_marker,
    yank_reason,
};

/// Assets listed per request to the store.
const LISTING_PAGE: u32 = 1000;
/// Sidecars (`metadata`, `info`, `yanked`) a file can have next to it: the listing of a project
/// is bounded by `max_index_files` times this plus the files themselves.
const ASSETS_PER_FILE: usize = 4;

/// The assets of one file name in a project directory.
#[derive(Default)]
struct Parts {
    dist: Option<Asset>,
    metadata: Option<Asset>,
    info: Option<Asset>,
    yanked: Option<Asset>,
}

/// `2024-05-01T10:00:00.123456Z`, the `upload-time` format of PEP 700.
pub(crate) fn upload_time(at: OffsetDateTime) -> String {
    const FORMAT: &[time::format_description::BorrowedFormatItem<'static>] = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z"
    );
    at.to_offset(time::UtcOffset::UTC)
        .format(&FORMAT)
        .unwrap_or_default()
}

fn blob_error(error: &BoundedReadError) -> FormatError {
    FormatError::internal(format!("a record could not be read: {error}"))
}

impl PypiHandler {
    /// Reads a small asset (a record or a metadata file) through the store its blob row names.
    pub(crate) async fn read_asset(
        cx: &FormatContext,
        asset: &Asset,
        limit: u64,
    ) -> Result<Result<Bytes, BoundedReadError>, FormatError> {
        let record = cx
            .ports()
            .blobs
            .get(asset.blob_id)
            .await?
            .ok_or_else(|| FormatError::internal("the blob row of an asset is missing"))?;
        let store = cx.store_for_blob(&record).await?;
        let read = store.get(&BlobKey::new(record.key.clone()), None).await?;
        Ok(collect_bounded(read.stream, limit).await)
    }

    async fn file_info(
        &self,
        cx: &FormatContext,
        asset: &Asset,
    ) -> Result<Arc<FileInfo>, FormatError> {
        if let Some(info) = self.infos.get(&asset.blob_id).await {
            return Ok(info);
        }
        let info = match Self::read_asset(cx, asset, MAX_SIDECAR_BYTES).await? {
            Ok(bytes) => FileInfo::from_bytes(&bytes),
            Err(BoundedReadError::TooLarge { .. }) => FileInfo::default(),
            Err(error) => return Err(blob_error(&error)),
        };
        let info = Arc::new(info);
        self.infos.insert(asset.blob_id, Arc::clone(&info)).await;
        Ok(info)
    }

    async fn yank_reason_of(
        &self,
        cx: &FormatContext,
        asset: &Asset,
    ) -> Result<Arc<str>, FormatError> {
        if let Some(reason) = self.yanks.get(&asset.blob_id).await {
            return Ok(reason);
        }
        let reason: Arc<str> = match Self::read_asset(cx, asset, MAX_SIDECAR_BYTES).await? {
            Ok(bytes) => yank_reason(&bytes).into(),
            Err(BoundedReadError::TooLarge { .. }) => "".into(),
            Err(error) => return Err(blob_error(&error)),
        };
        self.yanks.insert(asset.blob_id, Arc::clone(&reason)).await;
        Ok(reason)
    }

    /// Lists the project directory and groups the assets by file name.
    async fn project_parts(
        &self,
        cx: &FormatContext,
        name: &ProjectName,
    ) -> Result<BTreeMap<String, Parts>, FormatError> {
        let prefix = project_prefix(name);
        let repository = cx.repository();
        let limit_files = self.config.max_index_files;
        let mut parts: BTreeMap<String, Parts> = BTreeMap::new();
        let (mut after, mut seen, mut files) = (None, 0usize, 0usize);
        loop {
            let page = cx
                .ports()
                .assets
                .list(
                    repository.id,
                    &prefix,
                    &PageRequest {
                        limit: LISTING_PAGE,
                        after,
                    },
                )
                .await?;
            for asset in page.items {
                seen += 1;
                let Some(rest) = asset
                    .path
                    .strip_prefix(&prefix)
                    .filter(|r| !r.contains('/'))
                else {
                    continue;
                };
                let rest = rest.to_owned();
                if let Some(base) = rest.strip_suffix(".metadata") {
                    parts.entry(base.to_owned()).or_default().metadata = Some(asset);
                } else if let Some(base) = rest.strip_suffix(".info.json") {
                    parts.entry(base.to_owned()).or_default().info = Some(asset);
                } else if let Some(base) = rest.strip_suffix(".yanked") {
                    parts.entry(base.to_owned()).or_default().yanked = Some(asset);
                } else if DistFilename::parse(&rest).is_ok_and(|d| d.project() == name) {
                    files += 1;
                    parts.entry(rest).or_default().dist = Some(asset);
                }
            }
            if files > limit_files || seen > limit_files.saturating_mul(ASSETS_PER_FILE) {
                return Err(FormatError::IndexTooLarge {
                    limit: limit_files as u64,
                });
            }
            match page.next {
                Some(next) => after = Some(next),
                None => return Ok(parts),
            }
        }
    }

    /// The project page of a hosted repository.
    ///
    /// `NotFound` when the repository has no component of the project at all. A project whose
    /// files were all deleted still has its components, so the page is empty and the name stays
    /// claimed (a group must not start asking a public proxy about it).
    pub(crate) async fn hosted_project_page(
        &self,
        cx: &FormatContext,
        name: &ProjectName,
        origin_url: &str,
    ) -> Result<ProjectPage, FormatError> {
        let probe = cx
            .ports()
            .components
            .list(
                &ComponentFilter {
                    repository_id: Some(cx.repository().id),
                    name: Some(name.to_string()),
                    ..ComponentFilter::default()
                },
                &PageRequest {
                    limit: 1,
                    after: None,
                },
            )
            .await?;
        if probe.items.is_empty() {
            return Err(FormatError::NotFound);
        }
        let parts = self.project_parts(cx, name).await?;
        let entries = futures::stream::iter(parts.into_iter().filter(|(_, p)| p.dist.is_some()))
            .map(|(filename, parts)| async move {
                self.entry_of(cx, name, origin_url, &filename, parts).await
            })
            .buffered(self.config.sidecar_read_concurrency.max(1))
            .collect::<Vec<_>>()
            .await;
        let mut page = ProjectPage {
            name: name.to_string(),
            files: entries.into_iter().collect::<Result<Vec<_>, _>>()?,
        };
        page.sort_files();
        Ok(page)
    }

    async fn entry_of(
        &self,
        cx: &FormatContext,
        name: &ProjectName,
        origin_url: &str,
        filename: &str,
        parts: Parts,
    ) -> Result<FileEntry, FormatError> {
        let Some(dist) = parts.dist else {
            return Err(FormatError::internal("an entry without a distribution"));
        };
        let mut entry = FileEntry::new(filename, file_url(origin_url, name, filename));
        entry
            .hashes
            .insert("sha256".into(), dist.checksums.sha256.to_ascii_lowercase());
        entry.size = Some(dist.size_bytes);
        entry.upload_time = Some(upload_time(dist.created_at));
        if let Some(info) = &parts.info {
            let info = self.file_info(cx, info).await?;
            entry.requires_python.clone_from(&info.requires_python);
        }
        if let Some(metadata) = &parts.metadata {
            entry.core_metadata = Some(CoreMetadataRef::Hashes(
                [(
                    "sha256".to_owned(),
                    metadata.checksums.sha256.to_ascii_lowercase(),
                )]
                .into_iter()
                .collect(),
            ));
        }
        if let Some(yanked) = &parts.yanked {
            entry.yanked = Some(self.yank_reason_of(cx, yanked).await?.to_string());
        }
        Ok(entry)
    }

    /// The root index of a hosted repository: the distinct project names of its components.
    /// The second value is true when the scan stopped at a bound and the list is incomplete.
    pub(crate) async fn hosted_root(
        &self,
        cx: &FormatContext,
    ) -> Result<(RootPage, bool), FormatError> {
        let mut names = std::collections::BTreeSet::new();
        let (mut after, mut scanned) = (None, 0usize);
        let mut truncated = false;
        loop {
            let page = cx
                .ports()
                .components
                .list(
                    &ComponentFilter {
                        repository_id: Some(cx.repository().id),
                        ..ComponentFilter::default()
                    },
                    &PageRequest {
                        limit: LISTING_PAGE,
                        after,
                    },
                )
                .await?;
            scanned += page.items.len();
            for component in page.items {
                if ProjectName::parse(&component.name).is_ok() {
                    names.insert(component.name);
                }
            }
            if names.len() >= self.config.max_root_projects
                || scanned >= self.config.max_root_scan_components
            {
                truncated = page.next.is_some() || names.len() > self.config.max_root_projects;
                break;
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        let mut names: Vec<String> = names.into_iter().collect();
        names.truncate(self.config.max_root_projects);
        if truncated {
            tracing::warn!(
                repository = %cx.repository().name,
                "the simple root index was cut at a bound; the list of projects is incomplete"
            );
        }
        Ok((RootPage::from_names(names), truncated))
    }

    /// `DELETE` of a file: its records first, the file last, so that a failure leaves a file that
    /// is still listed (and can be deleted again) rather than records nobody points at.
    pub(crate) async fn delete_file(
        cx: &FormatContext,
        name: &ProjectName,
        file: &DistFilename,
    ) -> Result<Response<Body>, FormatError> {
        let main = NormalizedPath::from_segments([PACKAGES, name.as_str(), file.as_str()])?;
        if cx
            .ports()
            .assets
            .get_by_path(cx.repository().id, main.as_str())
            .await?
            .is_none()
        {
            return Err(FormatError::NotFound);
        }
        for path in [
            yank_path(name, file.as_str()),
            info_path(name, file.as_str()),
            metadata_path(name, file.as_str()),
        ] {
            delete_if_present(cx, &path).await?;
        }
        cx.hosted().delete(&main).await?;
        Ok(status_only(StatusCode::NO_CONTENT))
    }

    /// `GET`, `PUT` and `DELETE` of the yank marker of a file.
    pub(crate) async fn serve_yank(
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
        file: &DistFilename,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        if cx.repository().repo_type != RepoType::Hosted {
            return Err(FormatError::NotFound);
        }
        let repository = cx.repository();
        let main = file_path(name, file.as_str());
        if cx
            .ports()
            .assets
            .get_by_path(repository.id, &main)
            .await?
            .is_none()
        {
            return Err(FormatError::NotFound);
        }
        let marker = cx
            .ports()
            .assets
            .get_by_path(repository.id, &yank_path(name, file.as_str()))
            .await?;
        match *req.method() {
            http::Method::PUT => {
                if let Some(existing) = marker {
                    let reason = Self::read_reason(cx, &existing).await?;
                    return Ok(json_response(StatusCode::OK, &yank_body(&reason)));
                }
                let reason = reason_of(req, body).await?;
                let path = NormalizedPath::from_segments([
                    PACKAGES,
                    name.as_str(),
                    &format!("{}{}", file.as_str(), crate::route::YANKED_SUFFIX),
                ])?;
                let bytes = yank_marker(&reason);
                let mut spec = UploadSpec::new(path);
                spec.content_type = Some("application/json".to_owned());
                spec.content_length = Some(bytes.len() as u64);
                spec.component = Some(component_spec(name, file));
                spec.overwrite = OverwriteRule::Never;
                cx.hosted().upload(spec, Body::from(bytes)).await?;
                crate::metrics::yank("yank");
                Ok(json_response(StatusCode::CREATED, &yank_body(&reason)))
            }
            http::Method::DELETE => {
                if marker.is_none() {
                    return Err(FormatError::NotFound);
                }
                delete_if_present(cx, &yank_path(name, file.as_str())).await?;
                crate::metrics::yank("unyank");
                Ok(status_only(StatusCode::NO_CONTENT))
            }
            _ => match marker {
                Some(existing) => {
                    let reason = Self::read_reason(cx, &existing).await?;
                    Ok(json_response(StatusCode::OK, &yank_body(&reason)))
                }
                None => Err(FormatError::NotFound),
            },
        }
    }

    async fn read_reason(cx: &FormatContext, marker: &Asset) -> Result<String, FormatError> {
        match Self::read_asset(cx, marker, MAX_SIDECAR_BYTES).await? {
            Ok(bytes) => Ok(yank_reason(&bytes)),
            Err(BoundedReadError::TooLarge { .. }) => Ok(String::new()),
            Err(error) => Err(blob_error(&error)),
        }
    }
}

/// The reason of a yank request: the body when it has one, else `?reason=`.
async fn reason_of(req: &ArtifactRequest, body: Body) -> Result<String, FormatError> {
    let bytes = read_bounded(body, (MAX_YANK_REASON_BYTES * 4) as u64)
        .await
        .map_err(|error| match error {
            BoundedReadError::TooLarge { .. } => {
                FormatError::bad_request("the yank reason is too long")
            }
            BoundedReadError::Read(_) => {
                FormatError::bad_request("the request body could not be read")
            }
        })?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| FormatError::bad_request("the yank reason is not valid UTF-8"))?;
    let chosen = if text.trim().is_empty() {
        req.head.query_param("reason").unwrap_or_default()
    } else {
        text.to_owned()
    };
    validate_reason(&chosen)
        .map(str::to_owned)
        .map_err(|error| match error {
            ReasonError::TooLong => FormatError::bad_request("the yank reason is too long"),
            ReasonError::ControlCharacter => {
                FormatError::bad_request("the yank reason contains a control character")
            }
        })
}

fn yank_body(reason: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "yanked": true, "reason": reason })).unwrap_or_default()
}

pub(crate) fn component_spec(name: &ProjectName, file: &DistFilename) -> ComponentSpec {
    ComponentSpec {
        group: None,
        name: name.to_string(),
        version: Some(file.version().to_string()),
    }
}

/// Deletes the asset at `path` when it exists; a missing one is not an error.
pub(crate) async fn delete_if_present(cx: &FormatContext, path: &str) -> Result<(), FormatError> {
    let path = NormalizedPath::parse(path)?;
    match cx.hosted().delete(&path).await {
        Ok(_) | Err(FormatError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn status_only(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

pub(crate) fn json_response(status: StatusCode, body: &[u8]) -> Response<Body> {
    let mut response = Response::new(Body::from(Bytes::copy_from_slice(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len() as u64));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_time_is_pep_700_iso_8601_utc() {
        let at = OffsetDateTime::from_unix_timestamp_nanos(1_714_557_600_123_456_000).unwrap();
        assert_eq!(upload_time(at), "2024-05-01T10:00:00.123456Z");
        let offset = at.to_offset(time::UtcOffset::from_hms(5, 0, 0).unwrap());
        assert_eq!(upload_time(offset), "2024-05-01T10:00:00.123456Z");
    }

    #[test]
    fn the_yank_body_is_json() {
        let value: serde_json::Value = serde_json::from_slice(&yank_body("r \"x\"")).unwrap();
        assert_eq!(value["yanked"], true);
        assert_eq!(value["reason"], "r \"x\"");
    }
}
