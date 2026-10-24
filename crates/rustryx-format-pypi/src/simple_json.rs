//! The JSON simple API (PEP 691, with the additions of PEP 700 and PEP 714).
//!
//! Specification: <https://packaging.python.org/en/latest/specifications/simple-repository-api/>.
//! Rendering writes API version 1.1 (`versions`, `size`, `upload-time`) when every file has a
//! size, which PEP 700 makes mandatory from 1.1, and 1.0 otherwise. The core metadata flag is
//! written under both names (`core-metadata` and the deprecated `dist-info-metadata`) so that
//! clients from before PEP 714 find it.
//!
//! Parsing is lenient where PEP 691 says clients must be (unknown keys are ignored) and strict
//! about what the model relies on: a file entry without `filename` or `url` is skipped, a hash
//! that is not hex is dropped, and the project name is the one the caller asked for, never the
//! one in the document.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::name::ProjectName;
use crate::page::{
    CoreMetadataRef, FileEntry, MAX_FILES_PER_PAGE, MAX_PROJECTS_PER_INDEX, ParseError,
    ProjectPage, RootPage, is_hex,
};

#[derive(Serialize)]
struct Meta {
    #[serde(rename = "api-version")]
    api_version: &'static str,
}

#[derive(Serialize)]
struct RenderedFile<'a> {
    filename: &'a str,
    url: &'a str,
    hashes: &'a BTreeMap<String, String>,
    #[serde(rename = "requires-python", skip_serializing_if = "Option::is_none")]
    requires_python: Option<&'a str>,
    #[serde(rename = "core-metadata", skip_serializing_if = "Option::is_none")]
    core_metadata: Option<Value>,
    #[serde(rename = "dist-info-metadata", skip_serializing_if = "Option::is_none")]
    dist_info_metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    yanked: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(rename = "upload-time", skip_serializing_if = "Option::is_none")]
    upload_time: Option<&'a str>,
}

#[derive(Serialize)]
struct RenderedProject<'a> {
    meta: Meta,
    name: &'a str,
    files: Vec<RenderedFile<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    versions: Option<Vec<String>>,
}

fn metadata_value(reference: &CoreMetadataRef) -> Value {
    match reference {
        CoreMetadataRef::Available => Value::Bool(true),
        CoreMetadataRef::Hashes(hashes) => Value::Object(
            hashes
                .iter()
                .map(|(algo, hex)| (algo.clone(), Value::String(hex.clone())))
                .collect::<Map<_, _>>(),
        ),
    }
}

/// Renders a project page as a PEP 691 document.
#[must_use]
pub fn render_project(page: &ProjectPage) -> Vec<u8> {
    let all_sized = page.files.iter().all(|file| file.size.is_some());
    let files = page
        .files
        .iter()
        .map(|file| RenderedFile {
            filename: &file.filename,
            url: &file.url,
            hashes: &file.hashes,
            requires_python: file.requires_python.as_deref(),
            core_metadata: file.core_metadata.as_ref().map(metadata_value),
            dist_info_metadata: file.core_metadata.as_ref().map(metadata_value),
            yanked: file.yanked.as_ref().map(|reason| {
                if reason.is_empty() {
                    Value::Bool(true)
                } else {
                    Value::String(reason.clone())
                }
            }),
            size: file.size,
            upload_time: file.upload_time.as_deref(),
        })
        .collect();
    let document = RenderedProject {
        meta: Meta {
            api_version: if all_sized { "1.1" } else { "1.0" },
        },
        name: &page.name,
        files,
        versions: all_sized.then(|| page.versions()),
    };
    // Serializing plain strings, numbers and maps cannot fail; an empty body would be caught by
    // every client as an invalid document rather than hidden.
    serde_json::to_vec(&document).unwrap_or_default()
}

/// Renders the root index as a PEP 691 document.
#[must_use]
pub fn render_root(root: &RootPage) -> Vec<u8> {
    let projects: Vec<Value> = root
        .projects
        .iter()
        .map(|name| serde_json::json!({ "name": name }))
        .collect();
    let document = serde_json::json!({
        "meta": { "api-version": "1.0" },
        "projects": projects,
    });
    serde_json::to_vec(&document).unwrap_or_default()
}

fn hashes_of(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(algo, hex)| {
                    let hex = hex.as_str()?.to_ascii_lowercase();
                    is_hex(&hex).then(|| (algo.to_ascii_lowercase(), hex))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn core_metadata_of(value: Option<&Value>) -> Option<CoreMetadataRef> {
    match value? {
        Value::Bool(true) => Some(CoreMetadataRef::Available),
        Value::Object(_) => {
            let hashes = hashes_of(value);
            Some(if hashes.is_empty() {
                CoreMetadataRef::Available
            } else {
                CoreMetadataRef::Hashes(hashes)
            })
        }
        _ => None,
    }
}

fn yanked_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null | Value::Bool(false) => None,
        Value::String(reason) => Some(reason.clone()),
        // `true`, and anything a hostile index puts here: hidden from resolution is the safe side.
        _ => Some(String::new()),
    }
}

fn file_of(value: &Value) -> Option<FileEntry> {
    let object = value.as_object()?;
    let filename = object.get("filename")?.as_str()?;
    let url = object.get("url")?.as_str()?;
    if filename.is_empty() || url.is_empty() {
        return None;
    }
    let mut entry = FileEntry::new(filename, url);
    entry.hashes = hashes_of(object.get("hashes"));
    entry.requires_python = object
        .get("requires-python")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    entry.core_metadata = core_metadata_of(object.get("core-metadata"))
        .or_else(|| core_metadata_of(object.get("dist-info-metadata")));
    entry.yanked = yanked_of(object.get("yanked"));
    entry.size = object.get("size").and_then(Value::as_u64);
    entry.upload_time = object
        .get("upload-time")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(entry)
}

/// Parses a project document. `name` is the normalized name the request was for.
///
/// # Errors
/// [`ParseError::Malformed`] when the bytes are not a JSON object with a `files` array,
/// [`ParseError::TooMany`] when there are more files than [`MAX_FILES_PER_PAGE`].
pub fn parse_project(bytes: &[u8], name: &str) -> Result<ProjectPage, ParseError> {
    let document: Value = serde_json::from_slice(bytes).map_err(|_| ParseError::Malformed)?;
    let files = document
        .get("files")
        .and_then(Value::as_array)
        .ok_or(ParseError::Malformed)?;
    if files.len() > MAX_FILES_PER_PAGE {
        return Err(ParseError::TooMany);
    }
    Ok(ProjectPage {
        name: name.to_owned(),
        files: files.iter().filter_map(file_of).collect(),
    })
}

/// Parses a root document. Names that are not valid project names are dropped, the others are
/// normalized.
///
/// # Errors
/// [`ParseError::Malformed`] or [`ParseError::TooMany`].
pub fn parse_root(bytes: &[u8]) -> Result<RootPage, ParseError> {
    let document: Value = serde_json::from_slice(bytes).map_err(|_| ParseError::Malformed)?;
    let projects = document
        .get("projects")
        .and_then(Value::as_array)
        .ok_or(ParseError::Malformed)?;
    if projects.len() > MAX_PROJECTS_PER_INDEX {
        return Err(ParseError::TooMany);
    }
    let names = projects
        .iter()
        .filter_map(|project| project.get("name")?.as_str())
        .filter_map(|name| ProjectName::parse(name).ok())
        .map(|name| name.as_str().to_owned())
        .collect();
    Ok(RootPage::from_names(names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> ProjectPage {
        let mut wheel = FileEntry::new(
            "foo-1.0-py3-none-any.whl",
            "https://repo.test/repository/r/packages/foo/foo-1.0-py3-none-any.whl",
        );
        wheel.hashes.insert("sha256".into(), "ab".repeat(32));
        wheel.requires_python = Some(">=3.8".into());
        wheel.size = Some(10);
        wheel.upload_time = Some("2024-05-01T10:00:00.000000Z".into());
        wheel.core_metadata = Some(CoreMetadataRef::Hashes(
            [("sha256".to_owned(), "cd".repeat(32))]
                .into_iter()
                .collect(),
        ));
        let mut sdist = FileEntry::new("foo-1.0.tar.gz", "https://repo.test/x/foo-1.0.tar.gz");
        sdist.size = Some(20);
        sdist.yanked = Some(String::new());
        let mut yanked = FileEntry::new("foo-0.9.tar.gz", "https://repo.test/x/foo-0.9.tar.gz");
        yanked.size = Some(5);
        yanked.yanked = Some("broken build".into());
        ProjectPage {
            name: "foo".into(),
            files: vec![yanked, wheel, sdist],
        }
    }

    #[test]
    fn renders_pep_691_with_the_pep_700_and_714_fields() {
        let page = sample();
        let value: Value = serde_json::from_slice(&render_project(&page)).unwrap();
        assert_eq!(value["meta"]["api-version"], "1.1");
        assert_eq!(value["name"], "foo");
        assert_eq!(value["versions"], serde_json::json!(["0.9", "1.0"]));
        let files = value["files"].as_array().unwrap();
        assert_eq!(files[0]["yanked"], "broken build");
        assert_eq!(files[1]["requires-python"], ">=3.8");
        assert_eq!(files[1]["hashes"]["sha256"], "ab".repeat(32));
        assert_eq!(files[1]["core-metadata"]["sha256"], "cd".repeat(32));
        assert_eq!(files[1]["dist-info-metadata"]["sha256"], "cd".repeat(32));
        assert_eq!(files[1]["upload-time"], "2024-05-01T10:00:00.000000Z");
        assert_eq!(files[1]["size"], 10);
        assert_eq!(files[2]["yanked"], true);
        assert!(files[2].get("requires-python").is_none());
        assert!(files[2]["hashes"].as_object().unwrap().is_empty());
    }

    #[test]
    fn a_page_with_an_unsized_file_is_version_1_0_without_versions() {
        let mut page = sample();
        page.files[0].size = None;
        let value: Value = serde_json::from_slice(&render_project(&page)).unwrap();
        assert_eq!(value["meta"]["api-version"], "1.0");
        assert!(value.get("versions").is_none());
    }

    #[test]
    fn a_rendered_page_parses_back_to_itself() {
        let page = sample();
        let parsed = parse_project(&render_project(&page), "foo").unwrap();
        assert_eq!(parsed, page);
    }

    #[test]
    fn parsing_is_lenient_about_what_the_pep_allows_and_strict_about_the_rest() {
        let doc = serde_json::json!({
            "meta": {"api-version": "1.0"},
            "name": "someone-else",
            "unknown": 1,
            "files": [
                {"filename": "a-1.tar.gz", "url": "https://h/a", "hashes": {"SHA256": "ABCD", "bad": "zz"}, "yanked": false, "requires-python": null, "extra": true},
                {"filename": "b-1.tar.gz", "url": "https://h/b", "hashes": {}, "yanked": "why", "dist-info-metadata": true},
                {"filename": "c-1.tar.gz", "url": "https://h/c", "hashes": {}, "yanked": true, "core-metadata": {"sha256": "ef"}},
                {"filename": "d-1.tar.gz", "hashes": {}},
                {"url": "https://h/e"},
                "not an object",
                {"filename": "", "url": "https://h/f"}
            ]
        });
        let page = parse_project(doc.to_string().as_bytes(), "wanted").unwrap();
        assert_eq!(page.name, "wanted", "the name comes from the request");
        assert_eq!(page.files.len(), 3);
        assert_eq!(
            page.files[0].hashes.get("sha256").map(String::as_str),
            Some("abcd")
        );
        assert!(!page.files[0].hashes.contains_key("bad"));
        assert_eq!(page.files[0].yanked, None);
        assert_eq!(page.files[1].yanked.as_deref(), Some("why"));
        assert_eq!(
            page.files[1].core_metadata,
            Some(CoreMetadataRef::Available)
        );
        assert_eq!(page.files[2].yanked.as_deref(), Some(""));
        assert!(matches!(
            page.files[2].core_metadata,
            Some(CoreMetadataRef::Hashes(_))
        ));
    }

    #[test]
    fn must_not_read_a_hostile_yanked_value_as_not_yanked() {
        let doc = serde_json::json!({"files": [
            {"filename": "a-1.tar.gz", "url": "https://h/a", "yanked": {"x": 1}},
            {"filename": "b-1.tar.gz", "url": "https://h/b", "yanked": 1},
            {"filename": "c-1.tar.gz", "url": "https://h/c", "yanked": null}
        ]});
        let page = parse_project(doc.to_string().as_bytes(), "p").unwrap();
        assert!(page.files[0].yanked.is_some());
        assert!(page.files[1].yanked.is_some());
        assert!(page.files[2].yanked.is_none());
    }

    #[test]
    fn refuses_documents_it_cannot_use() {
        assert_eq!(parse_project(b"not json", "p"), Err(ParseError::Malformed));
        assert_eq!(parse_project(b"[]", "p"), Err(ParseError::Malformed));
        assert_eq!(
            parse_project(b"{\"files\": 3}", "p"),
            Err(ParseError::Malformed)
        );
        assert_eq!(parse_root(b"{}"), Err(ParseError::Malformed));
        let many =
            serde_json::json!({"files": vec![serde_json::json!({}); MAX_FILES_PER_PAGE + 1]});
        assert_eq!(
            parse_project(many.to_string().as_bytes(), "p"),
            Err(ParseError::TooMany)
        );
    }

    #[test]
    fn the_root_round_trips_and_normalizes() {
        let root = RootPage::from_names(vec!["foo-bar".into(), "baz".into()]);
        assert_eq!(parse_root(&render_root(&root)).unwrap(), root);
        let doc = br#"{"meta":{"api-version":"1.0"},"projects":[{"name":"Foo_Bar"},{"name":"-bad"},{"name":"baz"},{"x":1}]}"#;
        assert_eq!(parse_root(doc).unwrap().projects, ["baz", "foo-bar"]);
    }

    proptest! {
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = parse_project(&bytes, "p");
            let _ = parse_root(&bytes);
        }

        #[test]
        fn generated_pages_round_trip(
            files in proptest::collection::vec(("[a-z]{1,6}", 0u32..20, 0u64..1_000_000, any::<bool>(), proptest::option::of("[ -~]{0,12}")), 0..8)
        ) {
            let mut page = ProjectPage { name: "p".into(), files: Vec::new() };
            for (stem, minor, size, yanked, reason) in files {
                let mut entry = FileEntry::new(format!("{stem}-1.{minor}.tar.gz"), format!("https://h/{stem}/{minor}"));
                entry.size = Some(size);
                entry.hashes.insert("sha256".into(), "ab".repeat(32));
                if yanked {
                    entry.yanked = Some(reason.unwrap_or_default());
                }
                page.files.push(entry);
            }
            let parsed = parse_project(&render_project(&page), "p").unwrap();
            prop_assert_eq!(parsed, page);
        }
    }
}
