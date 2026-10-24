//! Building `index.yaml` from the charts stored in a hosted repository, and pointing every
//! `urls` entry of a proxied index at this repository.

use std::collections::BTreeMap;

use bytes::Bytes;
use rustryx_core::model::Asset;
use rustryx_core::ports::{BlobKey, PageRequest};
use rustryx_format_core::stream::{BoundedReadError, collect_bounded};
use rustryx_format_core::{FormatContext, FormatError};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::blocking::BlockingPool;
use crate::chart::{ChartError, ChartLimits, read_chart};
use crate::model::{ChartVersion, IndexFile};
use crate::names::{archive_file_name, is_valid_name, parse_version};
use crate::route::is_safe_chart_filename;
use crate::yaml::{YamlLimits, parse, render};

/// Most charts one generated index will list. One more and the read is refused, so a partial
/// index is never served as if it were complete.
pub(crate) const DEFAULT_MAX_CHARTS: usize = 4096;

const PAGE_LIMIT: u32 = 200;
const CHARTS_PREFIX: &str = "charts/";

/// One stored chart, ready to become an index entry.
struct ListedChart {
    metadata: ChartVersion,
}

/// Reads every stored chart and builds an index whose `urls` point at `repository_url`.
///
/// # Errors
/// A store failure, a stored archive that no longer passes [`read_chart`], or too many charts.
pub(crate) async fn generate(
    cx: &FormatContext,
    pool: &BlockingPool,
    limits: &ChartLimits,
    max_charts: usize,
    repository_url: &str,
) -> Result<IndexFile, FormatError> {
    let assets = list_charts(cx, max_charts).await?;
    let mut entries: BTreeMap<String, Vec<ChartVersion>> = BTreeMap::new();
    for asset in assets {
        let listed = load_chart(cx, pool, limits, &asset).await?;
        let mut metadata = listed.metadata;
        let file = archive_file_name(&metadata.name, &metadata.version);
        metadata.urls = vec![format!("{repository_url}/charts/{file}")];
        metadata.digest = Some(asset.checksums.sha256.clone());
        metadata.created = Some(rfc3339(asset.created_at));
        metadata.removed = false;
        entries
            .entry(metadata.name.clone())
            .or_default()
            .push(metadata);
    }
    sort_versions(&mut entries);
    Ok(IndexFile {
        entries,
        generated: Some(rfc3339(cx.now())),
        ..IndexFile::default()
    })
}

/// Rewrites every download URL so a client stays on this repository.
///
/// The file name is the last safe segment of an upstream URL when there is one, because that
/// is the path the proxy can fetch. Otherwise it is `<name>-<version>.tgz` when that spelling
/// is canonical. An entry with neither loses its URLs: an absolute upstream URL would send the
/// client around the repository.
pub(crate) fn rewrite_urls(index: &mut IndexFile, repository_url: &str) {
    for versions in index.entries.values_mut() {
        for version in versions {
            version.urls = proxied_file_name(version)
                .map(|file| vec![format!("{repository_url}/charts/{file}")])
                .unwrap_or_default();
        }
    }
    sort_versions(&mut index.entries);
}

/// The archive file name a proxy should expose for this entry.
#[must_use]
pub(crate) fn proxied_file_name(version: &ChartVersion) -> Option<String> {
    for url in &version.urls {
        if let Some(file) = url_file_name(url)
            && is_safe_chart_filename(file)
        {
            return Some(file.to_owned());
        }
    }
    (is_valid_name(&version.name) && parse_version(&version.version).is_ok())
        .then(|| archive_file_name(&version.name, &version.version))
}

fn url_file_name(url: &str) -> Option<&str> {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let segment = without_query
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_query);
    (!segment.is_empty()).then_some(segment)
}

/// Renders an index to YAML bytes.
///
/// # Errors
/// [`FormatError::Internal`] when the document cannot be serialized. The parser text stays in
/// the log.
pub(crate) fn to_bytes(index: &IndexFile) -> Result<Bytes, FormatError> {
    let text = render(index).map_err(|error| {
        tracing::error!(detail = error.detail(), "index.yaml could not be rendered");
        FormatError::internal("index.yaml could not be rendered")
    })?;
    Ok(Bytes::from(text))
}

/// Parses an upstream or member index.
///
/// # Errors
/// The caller's `refuse` value when the document is not valid YAML within the limits.
pub(crate) fn parse_index<E>(bytes: &[u8], refuse: impl FnOnce() -> E) -> Result<IndexFile, E> {
    let limit = bytes.len().saturating_add(1).max(1);
    parse(bytes, YamlLimits::index(limit)).map_err(|error| {
        tracing::info!(detail = error.detail(), "an index.yaml was refused");
        refuse()
    })
}

fn sort_versions(entries: &mut BTreeMap<String, Vec<ChartVersion>>) {
    for versions in entries.values_mut() {
        versions.sort_by(|left, right| cmp_version_desc(&left.version, &right.version));
    }
}

fn cmp_version_desc(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_version(left).ok(), parse_version(right).ok()) {
        (Some(newer), Some(older)) => older.cmp(&newer),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => right.cmp(left),
    }
}

async fn list_charts(cx: &FormatContext, max_charts: usize) -> Result<Vec<Asset>, FormatError> {
    let mut after = None;
    let mut found = Vec::new();
    loop {
        let page = cx
            .ports()
            .assets
            .list(
                cx.repository().id,
                CHARTS_PREFIX,
                &PageRequest {
                    limit: PAGE_LIMIT,
                    after: after.clone(),
                },
            )
            .await?;
        for asset in page.items {
            let Some(file) = asset.path.strip_prefix(CHARTS_PREFIX) else {
                continue;
            };
            if crate::names::split_chart_file(file).is_none_or(|parsed| parsed.provenance) {
                continue;
            }
            if found.len() >= max_charts {
                return Err(FormatError::unprocessable(
                    "the repository has more charts than one index can list",
                ));
            }
            found.push(asset);
        }
        match page.next {
            Some(token) => after = Some(token),
            None => return Ok(found),
        }
    }
}

async fn load_chart(
    cx: &FormatContext,
    pool: &BlockingPool,
    limits: &ChartLimits,
    asset: &Asset,
) -> Result<ListedChart, FormatError> {
    let bytes = read_asset(cx, asset, limits.max_archive_bytes).await?;
    let limits = *limits;
    let info = pool
        .run(move || read_chart(&bytes, &limits))
        .await?
        .map_err(|error| {
            FormatError::internal(format!(
                "stored chart {} no longer passes the chart reader: {error}",
                asset.path
            ))
        })?;
    Ok(ListedChart {
        metadata: info.metadata,
    })
}

async fn read_asset(cx: &FormatContext, asset: &Asset, limit: u64) -> Result<Bytes, FormatError> {
    if asset.size_bytes > limit {
        return Err(FormatError::internal(
            "stored chart is larger than the archive limit",
        ));
    }
    let record = cx.ports().blobs.get(asset.blob_id).await?.ok_or_else(|| {
        FormatError::internal("asset references a blob record that does not exist")
    })?;
    let store = cx.store_for_blob(&record).await?;
    let key = BlobKey::new(record.key);
    let read = store.get(&key, None).await.map_err(|error| {
        FormatError::internal(format!("stored chart could not be read: {error}"))
    })?;
    collect_bounded(read.stream, limit)
        .await
        .map_err(|error| match error {
            BoundedReadError::TooLarge { .. } => {
                FormatError::internal("stored chart is larger than the archive limit")
            }
            BoundedReadError::Read(detail) => {
                FormatError::internal(format!("stored chart could not be read: {detail}"))
            }
        })
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Maps a refused upload to a client error. The chart reader's text is already client-safe.
pub(crate) fn chart_failure(error: ChartError, archive_limit: u64) -> FormatError {
    match error {
        ChartError::ArchiveTooLarge => FormatError::PayloadTooLarge {
            limit: archive_limit,
        },
        other => FormatError::bad_request(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(name: &str, version: &str, url: &str) -> ChartVersion {
        ChartVersion {
            name: name.to_owned(),
            version: version.to_owned(),
            urls: vec![url.to_owned()],
            ..ChartVersion::default()
        }
    }

    #[test]
    fn rewrite_uses_the_last_safe_segment_of_an_upstream_url() {
        let mut index = IndexFile {
            entries: BTreeMap::from([(
                "web".into(),
                vec![version(
                    "web",
                    "1.2.3",
                    "https://upstream.example.test/charts/web-1.2.3.tgz?raw=1",
                )],
            )]),
            ..IndexFile::default()
        };
        rewrite_urls(&mut index, "https://repo.example");
        assert_eq!(
            index.entries["web"][0].urls,
            ["https://repo.example/charts/web-1.2.3.tgz"]
        );
    }

    #[test]
    fn an_entry_without_a_safe_file_name_loses_its_urls() {
        let mut index = IndexFile {
            entries: BTreeMap::from([(
                "Web".into(),
                vec![version("Web", "1.2", "https://evil.example/archive.bin")],
            )]),
            ..IndexFile::default()
        };
        rewrite_urls(&mut index, "https://repo.example");
        assert_eq!(index.entries["Web"][0].urls, [] as [String; 0]);
    }

    #[test]
    fn versions_are_listed_newest_first() {
        let mut entries = BTreeMap::from([(
            "web".into(),
            vec![version("web", "1.0.0", ""), version("web", "1.2.0", "")],
        )]);
        sort_versions(&mut entries);
        let order: Vec<_> = entries["web"]
            .iter()
            .map(|entry| entry.version.as_str())
            .collect();
        assert_eq!(order, ["1.2.0", "1.0.0"]);
    }
}
