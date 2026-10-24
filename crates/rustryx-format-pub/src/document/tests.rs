//! Tests of the listing document.

use serde_json::{Value, json};

use super::*;

fn name(text: &str) -> PackageName {
    PackageName::parse(text).unwrap()
}

fn version(text: &str) -> PubVersion {
    PubVersion::parse(text).unwrap()
}

fn pubspec(package: &str, ver: &str) -> Value {
    json!({ "name": package, "version": ver, "environment": { "sdk": "^3.0.0" } })
}

fn upstream_entry(package: &str, ver: &str) -> Value {
    json!({
        "version": ver,
        "archive_url": format!("https://upstream.example/api/archives/{package}-{ver}.tar.gz"),
        "archive_sha256": "A".repeat(64),
        "pubspec": pubspec(package, ver),
        "published": "2024-01-02T03:04:05.000000Z",
    })
}

fn listing(package: &str, entries: &[Value]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name": package,
        "advisoriesUpdated": "2024-01-01T00:00:00Z",
        "latest": entries.last().cloned().unwrap_or(Value::Null),
        "versions": entries,
    }))
    .unwrap()
}

fn rewrite(package: &'static str) -> impl Fn(&PubVersion) -> String {
    move |v| format!("https://rustryx.test/repository/pub/packages/{package}/versions/{v}.tar.gz")
}

fn parse_upstream(package: &'static str, bytes: &[u8]) -> Result<Parsed, DocError> {
    let urls = rewrite(package);
    PackageDoc::parse(bytes, &name(package), &ArchiveUrls::Rewrite(&urls), 1000)
}

fn entry(ver: &str, retracted: bool) -> VersionEntry {
    VersionEntry {
        version: version(ver),
        retracted,
        archive_url: format!("https://r.test/{ver}"),
        archive_sha256: None,
        pubspec: pubspec("demo", ver).as_object().unwrap().clone(),
        published: None,
    }
}

#[test]
fn an_upstream_listing_is_rewritten_to_our_urls() {
    let bytes = listing(
        "demo",
        &[
            upstream_entry("demo", "1.0.0"),
            upstream_entry("demo", "1.1.0"),
        ],
    );
    let parsed = parse_upstream("demo", &bytes).unwrap();
    assert_eq!(parsed.dropped, 0);
    let doc = parsed.doc;
    assert_eq!(doc.versions().len(), 2);
    for entry in doc.versions() {
        assert!(
            entry
                .archive_url
                .starts_with("https://rustryx.test/repository/pub/packages/demo/versions/")
        );
        assert!(!entry.archive_url.contains("upstream.example"));
        // The digest is normalised to lowercase.
        assert_eq!(
            entry.archive_sha256.as_deref(),
            Some("a".repeat(64).as_str())
        );
    }
    let json = doc.to_json();
    assert_eq!(json["name"], "demo");
    assert_eq!(json["latest"]["version"], "1.1.0");
    assert!(
        json["versions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["retracted"] == false)
    );
}

#[test]
fn must_not_forward_fields_that_are_not_on_the_whitelist() {
    let mut raw = upstream_entry("demo", "1.0.0");
    raw["attestationUrl"] = json!("https://upstream.example/attest");
    raw["advisories"] = json!([]);
    let mut bytes: Value = serde_json::from_slice(&listing("demo", &[raw])).unwrap();
    bytes["advisoriesUpdated"] = json!("2024-01-01T00:00:00Z");
    bytes["extra"] = json!(true);
    let parsed = parse_upstream("demo", &serde_json::to_vec(&bytes).unwrap()).unwrap();
    let rendered = serde_json::to_string(&parsed.doc.to_json()).unwrap();
    for field in ["advisoriesUpdated", "attestationUrl", "advisories", "extra"] {
        assert!(!rendered.contains(field), "{field} was forwarded");
    }
    assert!(rendered.contains("published"));
}

#[test]
fn entries_that_cannot_be_trusted_are_dropped_and_counted() {
    let good = upstream_entry("demo", "1.0.0");
    let mut bad_version = upstream_entry("demo", "1.0.0");
    bad_version["version"] = json!("01.0.0");
    let mut other_package = upstream_entry("other", "2.0.0");
    other_package["version"] = json!("2.0.0");
    let mut wrong_manifest_version = upstream_entry("demo", "3.0.0");
    wrong_manifest_version["pubspec"]["version"] = json!("3.0.1");
    let mut bad_sha = upstream_entry("demo", "4.0.0");
    bad_sha["archive_sha256"] = json!("not-a-digest");
    let mut bad_retracted = upstream_entry("demo", "5.0.0");
    bad_retracted["retracted"] = json!("yes");
    let mut no_pubspec = upstream_entry("demo", "6.0.0");
    no_pubspec.as_object_mut().unwrap().remove("pubspec");
    let not_an_object = json!("1.0.0");
    let duplicate = upstream_entry("demo", "1.0.0");
    let bytes = listing(
        "demo",
        &[
            good,
            bad_version,
            other_package,
            wrong_manifest_version,
            bad_sha,
            bad_retracted,
            no_pubspec,
            not_an_object,
            duplicate,
        ],
    );
    let parsed = parse_upstream("demo", &bytes).unwrap();
    assert_eq!(parsed.doc.versions().len(), 1);
    assert_eq!(parsed.dropped, 8);
}

#[test]
fn a_missing_digest_is_allowed_and_stays_missing() {
    let mut raw = upstream_entry("demo", "1.0.0");
    raw.as_object_mut().unwrap().remove("archive_sha256");
    let parsed = parse_upstream("demo", &listing("demo", &[raw])).unwrap();
    assert_eq!(parsed.doc.versions()[0].archive_sha256, None);
    assert!(
        parsed.doc.to_json()["versions"][0]
            .get("archive_sha256")
            .is_none()
    );
}

#[test]
fn must_not_read_a_listing_of_another_package_or_of_another_shape() {
    let bytes = listing("other", &[upstream_entry("other", "1.0.0")]);
    assert_eq!(
        parse_upstream("demo", &bytes).unwrap_err(),
        DocError::NameMismatch
    );
    assert_eq!(
        parse_upstream("demo", b"not json").unwrap_err(),
        DocError::NotJson
    );
    assert_eq!(parse_upstream("demo", b"[1]").unwrap_err(), DocError::Shape);
    assert_eq!(
        parse_upstream("demo", br#"{"name":"demo"}"#).unwrap_err(),
        DocError::Shape
    );
    assert_eq!(
        parse_upstream("demo", br#"{"name":"demo","versions":{}}"#).unwrap_err(),
        DocError::Shape
    );
    let many = listing(
        "demo",
        &[
            upstream_entry("demo", "1.0.0"),
            upstream_entry("demo", "1.0.1"),
        ],
    );
    let urls = rewrite("demo");
    assert_eq!(
        PackageDoc::parse(&many, &name("demo"), &ArchiveUrls::Rewrite(&urls), 1).unwrap_err(),
        DocError::TooManyVersions
    );
}

#[test]
fn our_own_listing_keeps_its_urls_and_needs_them() {
    let doc = PackageDoc::new(name("demo"), false, None, [entry("1.0.0", false)]);
    let again = PackageDoc::parse(&doc.to_bytes(), &name("demo"), &ArchiveUrls::Keep, 10).unwrap();
    assert_eq!(again.doc, doc);
    assert_eq!(again.dropped, 0);
    // A member listing without archive_url is not a listing of ours.
    let mut value = doc.to_json();
    value["versions"][0]
        .as_object_mut()
        .unwrap()
        .remove("archive_url");
    let parsed = PackageDoc::parse(
        &serde_json::to_vec(&value).unwrap(),
        &name("demo"),
        &ArchiveUrls::Keep,
        10,
    )
    .unwrap();
    assert!(parsed.doc.versions().is_empty());
    assert_eq!(parsed.dropped, 1);
}

#[test]
fn versions_are_unique_sorted_and_the_first_entry_wins() {
    let mut first = entry("1.0.0", false);
    first.archive_url = "https://first.test/1.0.0".into();
    let mut second = entry("1.0.0", true);
    second.archive_url = "https://second.test/1.0.0".into();
    let doc = PackageDoc::new(
        name("demo"),
        false,
        None,
        [entry("2.0.0", false), first, entry("1.5.0", false), second],
    );
    let versions: Vec<String> = doc
        .versions()
        .iter()
        .map(|e| e.version.to_string())
        .collect();
    assert_eq!(versions, ["1.0.0", "1.5.0", "2.0.0"]);
    assert_eq!(doc.versions()[0].archive_url, "https://first.test/1.0.0");
    assert!(!doc.versions()[0].retracted);
    assert!(doc.find(&version("1.5.0")).is_some());
    assert!(doc.find(&version("9.9.9")).is_none());
}

#[test]
fn latest_prefers_stable_then_not_retracted() {
    let latest = |entries: Vec<VersionEntry>| {
        PackageDoc::new(name("demo"), false, None, entries)
            .latest()
            .map(|e| e.version.to_string())
    };
    assert_eq!(latest(vec![]), None);
    assert_eq!(
        latest(vec![entry("1.0.0", false), entry("2.0.0-beta", false)]),
        Some("1.0.0".into())
    );
    assert_eq!(
        latest(vec![entry("1.0.0", false), entry("2.0.0", true)]),
        Some("1.0.0".into())
    );
    assert_eq!(
        latest(vec![entry("1.0.0", true), entry("2.0.0-beta", false)]),
        Some("2.0.0-beta".into())
    );
    assert_eq!(
        latest(vec![entry("1.0.0", true), entry("2.0.0", true)]),
        Some("2.0.0".into())
    );
}

#[test]
fn discontinuation_is_written_only_when_true() {
    let plain = PackageDoc::new(
        name("demo"),
        false,
        Some(name("other")),
        [entry("1.0.0", false)],
    );
    let json = plain.to_json();
    assert!(json.get("isDiscontinued").is_none());
    assert!(json.get("replacedBy").is_none());
    let dead = PackageDoc::new(
        name("demo"),
        true,
        Some(name("other")),
        [entry("1.0.0", false)],
    );
    assert_eq!(dead.to_json()["isDiscontinued"], true);
    assert_eq!(dead.to_json()["replacedBy"], "other");
    // Read back from an upstream listing, an invalid replacement name is ignored.
    let mut value: Value =
        serde_json::from_slice(&listing("demo", &[upstream_entry("demo", "1.0.0")])).unwrap();
    value["isDiscontinued"] = json!(true);
    value["replacedBy"] = json!("Not A Name");
    let parsed = parse_upstream("demo", &serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(parsed.doc.discontinued);
    assert_eq!(parsed.doc.replaced_by, None);
}

#[test]
fn the_stored_form_has_no_url_and_reads_back() {
    let mut original = entry("1.2.3", false);
    original.archive_sha256 = Some("b".repeat(64));
    original.published = Some("2024-01-02T03:04:05Z".into());
    let stored = original.to_stored_json();
    assert!(stored.get("archive_url").is_none());
    let read = VersionEntry::from_stored(&stored, &name("demo"), "https://x/y".into()).unwrap();
    assert_eq!(read.version, original.version);
    assert_eq!(read.archive_sha256, original.archive_sha256);
    assert_eq!(read.archive_url, "https://x/y");
    // The manifest must describe the package and version it is filed under.
    assert!(VersionEntry::from_stored(&stored, &name("another"), "u".into()).is_none());
}

#[test]
fn the_single_version_document_and_the_success_answer() {
    let single: Value =
        serde_json::from_slice(&PackageDoc::single_version_bytes(&entry("1.0.0", false))).unwrap();
    assert_eq!(single["version"], "1.0.0");
    assert!(single["archive_url"].is_string());
    assert!(single["pubspec"].is_object());
    let ok: Value = serde_json::from_slice(&success_json("done")).unwrap();
    assert_eq!(ok, json!({ "success": { "message": "done" } }));
}
