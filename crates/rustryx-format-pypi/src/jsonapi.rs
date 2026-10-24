//! The minimal JSON API: `/pypi/<project>/json` and `/pypi/<project>/<version>/json`.
//!
//! Tools that predate the simple API (and humans with `curl`) read a project's releases from
//! this document. It is derived from the same [`ProjectPage`] the simple index is rendered from,
//! so it holds for hosted, proxy and group repositories alike and cannot disagree with the index.
//! Only what a page knows is in it: names, versions, files with digests, sizes, upload times,
//! `requires_python` and the yank state. Fields that need the full metadata of a release (summary,
//! classifiers, license) are not part of this repository's JSON API.

use rustryx_format_core::FormatError;
use serde_json::{Map, Value, json};

use crate::filename::DistFilename;
use crate::page::{FileEntry, ProjectPage};
use crate::version::Version;

/// One release: a version and its files.
struct Release<'a> {
    version: Version,
    files: Vec<&'a FileEntry>,
}

fn releases<'a>(page: &'a ProjectPage, only: Option<&Version>) -> Vec<Release<'a>> {
    let mut found: Vec<Release<'_>> = Vec::new();
    for file in &page.files {
        let Some(version) = file.version() else {
            continue;
        };
        if only.is_some_and(|wanted| *wanted != version) {
            continue;
        }
        match found.iter_mut().find(|release| release.version == version) {
            Some(release) => release.files.push(file),
            None => found.push(Release {
                version,
                files: vec![file],
            }),
        }
    }
    found.sort_by(|a, b| a.version.cmp(&b.version));
    found
}

/// The version `info` describes: the highest release that is neither a pre-release nor fully
/// yanked, else the highest that is not fully yanked, else the highest.
fn latest<'a, 'b>(releases: &'b [Release<'a>]) -> Option<&'b Release<'a>> {
    let live = |release: &&Release<'a>| release.files.iter().any(|file| file.yanked.is_none());
    releases
        .iter()
        .rev()
        .find(|release| live(release) && !release.version.is_prerelease())
        .or_else(|| releases.iter().rev().find(live))
        .or_else(|| releases.last())
}

fn file_json(file: &FileEntry) -> Value {
    let dist = DistFilename::parse(&file.filename).ok();
    let mut digests = Map::new();
    for (algo, hex) in &file.hashes {
        digests.insert(algo.clone(), Value::String(hex.clone()));
    }
    let upload = file.upload_time.as_deref();
    json!({
        "filename": file.filename,
        "url": file.url,
        "size": file.size,
        "digests": digests,
        "packagetype": dist.as_ref().map(|d| d.kind().filetype()),
        "python_version": dist
            .as_ref()
            .map_or("source", |d| d.python_tag().unwrap_or("source")),
        "requires_python": file.requires_python,
        "upload_time_iso_8601": upload,
        "upload_time": upload.map(|time| time.trim_end_matches('Z')),
        "yanked": file.yanked.is_some(),
        "yanked_reason": file.yanked.as_ref().filter(|reason| !reason.is_empty()),
        "has_sig": false,
        "comment_text": "",
    })
}

/// Renders the document. With `only` the document is the one of that version (no `releases`
/// map).
///
/// # Errors
/// [`FormatError::NotFound`] when a requested version has no file.
pub(crate) fn render(page: &ProjectPage, only: Option<&Version>) -> Result<Vec<u8>, FormatError> {
    let all = releases(page, only);
    if only.is_some() && all.is_empty() {
        return Err(FormatError::NotFound);
    }
    let current = latest(&all);
    let first = current.and_then(|release| release.files.first());
    let urls: Vec<Value> = current
        .map(|release| release.files.iter().map(|file| file_json(file)).collect())
        .unwrap_or_default();
    let all_yanked =
        current.is_some_and(|release| release.files.iter().all(|f| f.yanked.is_some()));
    let reason = first
        .and_then(|file| file.yanked.as_ref())
        .filter(|reason| !reason.is_empty());
    let mut document = json!({
        "info": {
            "name": page.name,
            "version": current.map(|release| release.version.to_string()),
            "requires_python": first.and_then(|file| file.requires_python.as_ref()),
            "yanked": all_yanked,
            "yanked_reason": if all_yanked { reason } else { None },
        },
        "last_serial": 0,
        "urls": urls,
        "vulnerabilities": [],
    });
    if only.is_none() {
        let mut map = Map::new();
        for release in &all {
            let files: Vec<Value> = release.files.iter().map(|file| file_json(file)).collect();
            map.insert(release.version.to_string(), Value::Array(files));
        }
        if let Some(object) = document.as_object_mut() {
            object.insert("releases".to_owned(), Value::Object(map));
        }
    }
    serde_json::to_vec(&document).map_err(|error| FormatError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, yanked: Option<&str>) -> FileEntry {
        let mut entry = FileEntry::new(name, format!("https://r.test/packages/foo/{name}"));
        entry.hashes.insert("sha256".into(), "ab".repeat(32));
        entry.size = Some(7);
        entry.upload_time = Some("2024-05-01T10:00:00.000000Z".into());
        entry.requires_python = Some(">=3.8".into());
        entry.yanked = yanked.map(str::to_owned);
        entry
    }

    fn page() -> ProjectPage {
        ProjectPage {
            name: "foo".into(),
            files: vec![
                entry("foo-1.0.tar.gz", None),
                entry("foo-1.0-py3-none-any.whl", None),
                entry("foo-2.0.tar.gz", Some("broken")),
                entry("foo-1.5rc1.tar.gz", None),
                entry("foo-0.9.tar.gz", None),
            ],
        }
    }

    fn doc(only: Option<&str>) -> Value {
        let only = only.map(|text| Version::parse(text).unwrap());
        serde_json::from_slice(&render(&page(), only.as_ref()).unwrap()).unwrap()
    }

    #[test]
    fn the_project_document_has_info_releases_and_urls() {
        let value = doc(None);
        // 2.0 is yanked and 1.5rc1 is a pre-release: the latest is 1.0.
        assert_eq!(value["info"]["version"], "1.0");
        assert_eq!(value["info"]["name"], "foo");
        assert_eq!(value["info"]["requires_python"], ">=3.8");
        assert_eq!(value["info"]["yanked"], false);
        let releases = value["releases"].as_object().unwrap();
        let versions: Vec<_> = releases.keys().cloned().collect();
        assert_eq!(versions.len(), 4);
        assert!(versions.contains(&"1.5rc1".to_owned()));
        assert_eq!(releases["1.0"].as_array().unwrap().len(), 2);
        assert_eq!(value["urls"].as_array().unwrap().len(), 2);
        let wheel = releases["1.0"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["packagetype"] == "bdist_wheel")
            .unwrap();
        assert_eq!(wheel["python_version"], "py3");
        assert_eq!(wheel["digests"]["sha256"], "ab".repeat(32));
        assert_eq!(wheel["upload_time_iso_8601"], "2024-05-01T10:00:00.000000Z");
        assert_eq!(wheel["upload_time"], "2024-05-01T10:00:00.000000");
        assert_eq!(wheel["yanked"], false);
        let yanked = &releases["2.0"][0];
        assert_eq!(yanked["yanked"], true);
        assert_eq!(yanked["yanked_reason"], "broken");
        assert!(releases["1.0"][0]["python_version"].as_str().is_some());
    }

    #[test]
    fn a_version_document_has_no_releases_map() {
        let value = doc(Some("1.0.0"));
        assert!(value.get("releases").is_none());
        assert_eq!(value["info"]["version"], "1.0");
        assert_eq!(value["urls"].as_array().unwrap().len(), 2);
        let yanked = doc(Some("2.0"));
        assert_eq!(yanked["info"]["yanked"], true);
        assert_eq!(yanked["info"]["yanked_reason"], "broken");
    }

    #[test]
    fn an_unknown_version_is_404() {
        let wanted = Version::parse("9.9").unwrap();
        assert!(matches!(
            render(&page(), Some(&wanted)),
            Err(FormatError::NotFound)
        ));
    }

    #[test]
    fn an_empty_project_renders_without_a_version() {
        let empty = ProjectPage {
            name: "foo".into(),
            files: Vec::new(),
        };
        let value: Value = serde_json::from_slice(&render(&empty, None).unwrap()).unwrap();
        assert!(value["info"]["version"].is_null());
        assert_eq!(value["releases"].as_object().unwrap().len(), 0);
        assert_eq!(value["urls"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn when_only_prereleases_or_yanked_files_exist_the_highest_is_described() {
        let only_pre = ProjectPage {
            name: "foo".into(),
            files: vec![
                entry("foo-1.0rc1.tar.gz", None),
                entry("foo-1.0rc2.tar.gz", None),
            ],
        };
        let value: Value = serde_json::from_slice(&render(&only_pre, None).unwrap()).unwrap();
        assert_eq!(value["info"]["version"], "1.0rc2");
        let all_yanked = ProjectPage {
            name: "foo".into(),
            files: vec![
                entry("foo-1.0.tar.gz", Some("")),
                entry("foo-1.1.tar.gz", Some("")),
            ],
        };
        let value: Value = serde_json::from_slice(&render(&all_yanked, None).unwrap()).unwrap();
        assert_eq!(value["info"]["version"], "1.1");
        assert_eq!(value["info"]["yanked"], true);
        assert!(value["info"]["yanked_reason"].is_null());
    }
}
