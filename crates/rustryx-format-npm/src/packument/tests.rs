use proptest::prelude::*;
use serde_json::{Value, json};
use time::macros::datetime;

use super::*;

fn name(text: &str) -> PackageName {
    PackageName::parse(text).unwrap()
}

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn manifest(version: &str) -> Map<String, Value> {
    json!({
        "name": "p",
        "version": version,
        "description": format!("description of {version}"),
        "readme": format!("readme of {version}"),
        "dependencies": { "left-pad": "^1.0.0" },
        "scripts": { "test": "true" },
        "dist": { "shasum": "a".repeat(40), "integrity": "sha512-xxx" },
    })
    .as_object()
    .unwrap()
    .clone()
}

fn at(second: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000 + second).unwrap()
}

fn sample() -> Packument {
    let mut doc = Packument::new(&name("p"));
    doc.add_version(v("1.0.0"), manifest("1.0.0"), &["latest".to_owned()], at(0))
        .unwrap();
    doc.add_version(
        v("1.1.0"),
        manifest("1.1.0"),
        &["latest".to_owned()],
        at(60),
    )
    .unwrap();
    doc.add_version(
        v("2.0.0-rc.1"),
        manifest("2.0.0-rc.1"),
        &["next".to_owned()],
        at(120),
    )
    .unwrap();
    doc
}

#[test]
fn iso_millis_prints_utc_with_milliseconds() {
    assert_eq!(
        iso_millis(datetime!(2015-03-24 00:12:24.039_999 UTC)),
        "2015-03-24T00:12:24.039Z"
    );
    assert_eq!(
        iso_millis(datetime!(2015-03-24 02:12:24 +2)),
        "2015-03-24T00:12:24Z"
    );
}

#[test]
fn rev_is_the_version_count_and_a_digest_prefix() {
    assert_eq!(
        rev_of(3, &"ab".repeat(32)),
        format!("3-{}", "ab".repeat(16))
    );
    assert_eq!(rev_of(0, "abc"), "1-abc");
}

#[test]
fn adding_versions_stamps_times_tags_and_hoists_the_latest_readme() {
    let doc = sample();
    assert_eq!(doc.versions().len(), 3);
    assert_eq!(doc.dist_tags().get("latest"), Some(&v("1.1.0")));
    assert_eq!(doc.dist_tags().get("next"), Some(&v("2.0.0-rc.1")));
    assert_eq!(doc.time().get("created"), Some(&iso_millis(at(0))));
    assert_eq!(doc.time().get("modified"), Some(&iso_millis(at(120))));
    assert_eq!(doc.time().get("1.1.0"), Some(&iso_millis(at(60))));
    // The top shows the readme of the version `latest` points at, not of the newest publish.
    assert_eq!(doc.extra()["readme"], "readme of 1.1.0");
    assert_eq!(doc.extra()["description"], "description of 1.1.0");
    assert_eq!(doc.latest(), Some(&v("1.1.0")));
}

#[test]
fn a_first_publish_without_tags_still_gets_latest() {
    let mut doc = Packument::new(&name("p"));
    doc.add_version(v("0.1.0"), manifest("0.1.0"), &[], at(0))
        .unwrap();
    assert_eq!(doc.dist_tags().get("latest"), Some(&v("0.1.0")));
}

#[test]
fn an_invalid_tag_is_refused_before_anything_is_added() {
    let mut doc = Packument::new(&name("p"));
    let refused = doc.add_version(v("1.0.0"), manifest("1.0.0"), &["1.0.0".to_owned()], at(0));
    assert!(refused.is_err());
}

#[test]
fn build_metadata_does_not_open_a_second_release() {
    let mut doc = Packument::new(&name("p"));
    doc.add_version(v("1.0.0+a"), manifest("1.0.0+a"), &[], at(0))
        .unwrap();
    assert!(doc.has_release(&v("1.0.0+b")));
    assert!(doc.has_release(&v("1.0.0+a")));
    assert!(!doc.has_release(&v("1.0.1")));
}

#[test]
fn deprecation_sets_and_clears() {
    let mut doc = sample();
    assert!(doc.set_deprecated(&v("1.0.0"), Some("use 1.1.0")));
    assert_eq!(doc.versions()[&v("1.0.0")]["deprecated"], "use 1.1.0");
    assert!(doc.set_deprecated(&v("1.0.0"), Some("")));
    assert!(!doc.versions()[&v("1.0.0")].contains_key("deprecated"));
    assert!(doc.set_deprecated(&v("1.0.0"), Some("again")));
    assert!(doc.set_deprecated(&v("1.0.0"), None));
    assert!(!doc.versions()[&v("1.0.0")].contains_key("deprecated"));
    assert!(!doc.set_deprecated(&v("9.9.9"), Some("x")));
}

#[test]
fn removing_the_latest_version_moves_latest() {
    let mut doc = sample();
    assert!(doc.remove_version(&v("1.1.0"), None));
    assert_eq!(doc.dist_tags().get("latest"), Some(&v("1.0.0")));
    assert!(!doc.time().contains_key("1.1.0"));
    assert!(!doc.remove_version(&v("1.1.0"), None));
    // The client's own choice is honoured when it still exists.
    let mut doc = sample();
    doc.add_version(v("1.2.0"), manifest("1.2.0"), &[], at(200))
        .unwrap();
    doc.dist_tags_mut().set("latest", v("1.1.0")).unwrap();
    assert!(doc.remove_version(&v("1.1.0"), Some(&v("1.0.0"))));
    assert_eq!(doc.dist_tags().get("latest"), Some(&v("1.0.0")));
}

#[test]
fn removing_a_version_drops_the_tags_that_pointed_at_it() {
    let mut doc = sample();
    assert!(doc.remove_version(&v("2.0.0-rc.1"), None));
    assert!(doc.dist_tags().get("next").is_none());
}

#[test]
fn removing_the_last_version_leaves_no_latest() {
    let mut doc = Packument::new(&name("p"));
    doc.add_version(v("1.0.0"), manifest("1.0.0"), &[], at(0))
        .unwrap();
    assert!(doc.remove_version(&v("1.0.0"), None));
    assert!(doc.dist_tags().is_empty());
    assert!(doc.latest().is_none());
}

#[test]
fn tarball_urls_point_at_the_repository_the_client_used() {
    let mut doc = Packument::new(&name("@acme/p"));
    let mut broken = manifest("1.0.0");
    broken.insert("dist".to_owned(), json!("not an object"));
    doc.add_version(v("1.0.0"), broken, &[], at(0)).unwrap();
    let mut with_upstream_url = manifest("1.1.0");
    with_upstream_url["dist"]["tarball"] = json!("https://evil.example/x.tgz");
    doc.add_version(v("1.1.0"), with_upstream_url, &[], at(1))
        .unwrap();
    doc.rewrite_tarballs(&name("@acme/p"), "https://r.example/repository/npm/");
    for (version, manifest) in doc.versions() {
        assert_eq!(
            manifest["dist"]["tarball"],
            format!("https://r.example/repository/npm/@acme/p/-/p-{version}.tgz")
        );
    }
    // The digests of a well formed dist survive.
    assert_eq!(
        doc.versions()[&v("1.1.0")]["dist"]["shasum"],
        "a".repeat(40)
    );
}

#[test]
fn the_abbreviated_document_keeps_install_fields_only() {
    let mut doc = sample();
    let mut with_install_script = manifest("3.0.0");
    with_install_script["scripts"] = json!({ "postinstall": "node x.js" });
    with_install_script.insert("bin".to_owned(), json!({ "p": "cli.js" }));
    with_install_script.insert("engines".to_owned(), json!({ "node": ">=18" }));
    with_install_script.insert("deprecated".to_owned(), json!("gone"));
    doc.add_version(
        v("3.0.0"),
        with_install_script,
        &["old".to_owned()],
        at(300),
    )
    .unwrap();
    let short = doc.abbreviated();
    assert_eq!(short["name"], "p");
    assert_eq!(short["modified"], iso_millis(at(300)));
    assert_eq!(short["dist-tags"]["latest"], "1.1.0");
    let entry = &short["versions"]["3.0.0"];
    assert_eq!(entry["hasInstallScript"], true);
    assert_eq!(entry["bin"]["p"], "cli.js");
    assert_eq!(entry["deprecated"], "gone");
    assert_eq!(entry["dependencies"]["left-pad"], "^1.0.0");
    for dropped in ["description", "readme", "scripts", "_id"] {
        assert!(entry.get(dropped).is_none(), "{dropped}");
    }
    assert!(short["versions"]["1.0.0"].get("hasInstallScript").is_none());
    assert!(short.get("readme").is_none());
}

#[test]
fn the_stored_form_round_trips() {
    let doc = sample();
    let again = Packument::from_slice(&doc.to_stored_vec(), ParseMode::Strict, None).unwrap();
    assert_eq!(again, doc);
    let value = doc.to_value(Some("3-abc"));
    assert_eq!(value["_rev"], "3-abc");
    assert_eq!(value["_id"], "p");
    assert!(doc.to_value(None).get("_rev").is_none());
}

#[test]
fn derived_keys_are_not_carried_as_extra() {
    let doc = Packument::from_value(
        json!({
            "_id": "p", "_rev": "9-x", "_attachments": {"a": {}}, "name": "p",
            "dist-tags": {}, "versions": {}, "time": {}, "keywords": ["a"], "users": {"u": true}
        }),
        ParseMode::Strict,
        None,
    )
    .unwrap();
    let keys: Vec<&String> = doc.extra().keys().collect();
    assert_eq!(keys, ["keywords", "users"]);
}

#[test]
fn strict_parsing_refuses_what_lenient_parsing_skips() {
    let text = json!({
        "name": "p",
        "dist-tags": { "latest": "1.0.0", "broken": "nope" },
        "versions": { "1.0.0": {"name": "p"}, "not.a.version": {"name": "p"}, "2.0.0": "string" },
        "time": { "1.0.0": "2020-01-01T00:00:00.000Z", "modified": 5 },
    });
    assert!(Packument::from_value(text.clone(), ParseMode::Strict, None).is_err());
    let lenient = Packument::from_value(text, ParseMode::Lenient, None).unwrap();
    assert_eq!(lenient.versions().len(), 1);
    assert_eq!(lenient.dist_tags().len(), 1);
    assert_eq!(lenient.time().len(), 1);
}

#[test]
fn malformed_documents_are_errors_not_panics() {
    for (text, expected) in [
        ("[]", PackumentError::NotAnObject),
        ("{}", PackumentError::NoName),
        (r#"{"name":5}"#, PackumentError::NoName),
        (r#"{"name":"p","versions":[]}"#, PackumentError::Versions),
        (
            r#"{"name":"p","versions":{"x":{}}}"#,
            PackumentError::Versions,
        ),
        (
            r#"{"name":"p","versions":{"1.0.0":1}}"#,
            PackumentError::Versions,
        ),
        (r#"{"name":"p","time":[]}"#, PackumentError::Time),
        (r#"{"name":"p","time":{"a":1}}"#, PackumentError::Time),
    ] {
        assert_eq!(
            Packument::from_slice(text.as_bytes(), ParseMode::Strict, None).unwrap_err(),
            expected,
            "{text}"
        );
    }
    assert!(matches!(
        Packument::from_slice(b"{", ParseMode::Strict, None).unwrap_err(),
        PackumentError::Json(_)
    ));
    let deep = format!("{}1{}", "[".repeat(300), "]".repeat(300));
    assert!(Packument::from_slice(deep.as_bytes(), ParseMode::Lenient, None).is_err());
}

#[test]
fn a_lenient_document_without_a_name_takes_the_one_of_the_request() {
    let doc = Packument::from_value(
        json!({ "versions": {} }),
        ParseMode::Lenient,
        Some(&name("@s/p")),
    )
    .unwrap();
    assert_eq!(doc.name(), "@s/p");
    assert!(Packument::from_value(json!({}), ParseMode::Lenient, None).is_err());
}

#[test]
fn must_not_accept_more_versions_than_the_limit() {
    let mut versions = Map::new();
    for n in 0..=MAX_VERSIONS {
        versions.insert(format!("1.0.{n}"), json!({}));
    }
    let doc = json!({ "name": "p", "versions": Value::Object(versions) });
    assert_eq!(
        Packument::from_value(doc, ParseMode::Strict, None).unwrap_err(),
        PackumentError::TooManyVersions
    );
}

#[test]
fn resolve_finds_versions_and_tags() {
    let doc = sample();
    assert_eq!(doc.resolve("1.0.0"), Some(v("1.0.0")));
    assert_eq!(doc.resolve("latest"), Some(v("1.1.0")));
    assert_eq!(doc.resolve("next"), Some(v("2.0.0-rc.1")));
    assert_eq!(doc.resolve("9.9.9"), None);
    assert_eq!(doc.resolve("nothing"), None);
    assert!(doc.version_value(&v("1.0.0")).is_some());
    assert!(doc.version_value(&v("9.9.9")).is_none());
}

fn member(versions: &[&str], tags: &[(&str, &str)], created: i64, extra: &str) -> Packument {
    let mut doc = Packument::new(&name("p"));
    for version in versions {
        let mut m = manifest(version);
        m.insert("origin".to_owned(), json!(extra));
        doc.add_version(v(version), m, &[], at(created)).unwrap();
    }
    for (tag, version) in tags {
        doc.dist_tags_mut().set(tag, v(version)).unwrap();
    }
    doc.extra_for_test(extra);
    doc
}

impl Packument {
    fn extra_for_test(&mut self, marker: &str) {
        self.extra.insert("marker".to_owned(), json!(marker));
    }
}

#[test]
fn merge_takes_the_union_and_the_first_member_wins_a_shared_version() {
    let first = member(&["1.0.0", "1.1.0"], &[("latest", "1.1.0")], 0, "first");
    let second = member(&["1.1.0", "2.0.0"], &[("latest", "2.0.0")], 100, "second");
    let merged = Packument::merge(&[(first, false), (second, false)]).unwrap();
    let keys: Vec<String> = merged.versions().keys().map(ToString::to_string).collect();
    assert_eq!(keys, ["1.0.0", "1.1.0", "2.0.0"]);
    assert_eq!(merged.versions()[&v("1.1.0")]["origin"], "first");
    assert_eq!(merged.versions()[&v("2.0.0")]["origin"], "second");
    // Without a claim a tag takes the highest version.
    assert_eq!(merged.dist_tags().get("latest"), Some(&v("2.0.0")));
    // Times: the winner's time per version, the earliest creation, the latest change.
    assert_eq!(merged.time().get("1.1.0"), Some(&iso_millis(at(0))));
    assert_eq!(merged.time().get("2.0.0"), Some(&iso_millis(at(100))));
    assert_eq!(merged.time().get("created"), Some(&iso_millis(at(0))));
    assert_eq!(merged.time().get("modified"), Some(&iso_millis(at(100))));
    assert_eq!(merged.extra()["marker"], "first");
}

#[test]
fn merge_gives_the_claim_holder_its_tags_and_its_top_level_fields() {
    let public = member(&["1.0.0", "99.0.0"], &[("latest", "99.0.0")], 0, "public");
    let internal = member(&["1.0.0", "1.1.0"], &[("latest", "1.1.0")], 50, "internal");
    let merged = Packument::merge(&[(public, false), (internal, true)]).unwrap();
    assert_eq!(
        merged.dist_tags().get("latest"),
        Some(&v("1.1.0")),
        "the claim holder decides the tags it has"
    );
    assert_eq!(merged.extra()["marker"], "internal");
    assert_eq!(merged.name(), "p");
}

#[test]
fn merge_keeps_tags_only_one_member_has_and_repairs_latest() {
    let a = member(&["1.0.0"], &[("beta", "1.0.0")], 0, "a");
    let b = member(&["2.0.0"], &[("canary", "2.0.0")], 1, "b");
    let merged = Packument::merge(&[(a, false), (b, false)]).unwrap();
    assert_eq!(merged.dist_tags().get("beta"), Some(&v("1.0.0")));
    assert_eq!(merged.dist_tags().get("canary"), Some(&v("2.0.0")));
    assert_eq!(merged.dist_tags().get("latest"), Some(&v("2.0.0")));
    assert!(Packument::merge(&[]).is_none());
}

#[test]
fn merge_of_abbreviated_documents_keeps_the_modified_stamp() {
    let doc = member(&["1.0.0"], &[("latest", "1.0.0")], 10, "a");
    let short = Packument::from_value(doc.abbreviated(), ParseMode::Lenient, None).unwrap();
    assert_eq!(short.modified(), Some(iso_millis(at(10)).as_str()));
    let merged = Packument::merge(&[(short, false)]).unwrap();
    assert_eq!(merged.abbreviated()["modified"], iso_millis(at(10)));
}

fn arbitrary_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        "[a-z0-9.+-]{0,12}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            proptest::collection::btree_map("[a-z0-9.@/_-]{0,10}", inner, 0..6)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

fn arbitrary_packument_json() -> impl Strategy<Value = Value> {
    (
        proptest::option::of("[a-z@/.-]{0,10}"),
        arbitrary_json(),
        arbitrary_json(),
        arbitrary_json(),
        arbitrary_json(),
    )
        .prop_map(|(name, tags, versions, time, extra)| {
            json!({ "name": name, "dist-tags": tags, "versions": versions, "time": time, "extra": extra })
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn parsing_never_panics_in_either_mode(doc in arbitrary_packument_json()) {
        let _ = Packument::from_value(doc.clone(), ParseMode::Strict, None);
        if let Ok(parsed) = Packument::from_value(doc, ParseMode::Lenient, Some(&name("fallback"))) {
            let _ = parsed.abbreviated();
            let _ = parsed.to_stored_vec();
        }
    }

    #[test]
    fn normalization_is_idempotent(doc in arbitrary_packument_json()) {
        if let Ok(once) = Packument::from_value(doc, ParseMode::Lenient, Some(&name("fallback"))) {
            let stored = once.to_stored_vec();
            let twice = Packument::from_slice(&stored, ParseMode::Strict, None).unwrap();
            prop_assert_eq!(&once, &twice);
            prop_assert_eq!(stored, twice.to_stored_vec());
        }
    }

    #[test]
    fn merge_never_loses_a_version(
        left in proptest::collection::btree_set(0u8..8, 0..6),
        right in proptest::collection::btree_set(0u8..8, 0..6),
    ) {
        let make = |set: &std::collections::BTreeSet<u8>, tag: &str| {
            let versions: Vec<String> = set.iter().map(|n| format!("1.{n}.0")).collect();
            let refs: Vec<&str> = versions.iter().map(String::as_str).collect();
            member(&refs, &[], 0, tag)
        };
        let merged = Packument::merge(&[(make(&left, "l"), false), (make(&right, "r"), false)]).unwrap();
        let expected: std::collections::BTreeSet<u8> = left.union(&right).copied().collect();
        prop_assert_eq!(merged.versions().len(), expected.len());
    }
}
