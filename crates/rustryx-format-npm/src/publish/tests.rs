use serde_json::json;
use sha2::{Digest, Sha512};

use super::*;

fn name(text: &str) -> PackageName {
    PackageName::parse(text).unwrap()
}

fn head(extra_manifest: &Value) -> Value {
    let mut manifest = json!({ "name": "p", "version": "1.0.0", "description": "d" });
    if let (Some(base), Some(extra)) = (manifest.as_object_mut(), extra_manifest.as_object()) {
        base.extend(extra.clone());
    }
    json!({
        "_id": "p",
        "name": "p",
        "dist-tags": { "latest": "1.0.0" },
        "versions": { "1.0.0": manifest },
        "access": null,
        "_attachments": { "p-1.0.0.tgz": { "content_type": "x", "data": "" } },
    })
}

fn parse(doc: &Value) -> Result<PublishRequest, FormatError> {
    PublishRequest::parse(doc.to_string().as_bytes(), &name("p"), "p-1.0.0.tgz")
}

fn message(error: FormatError) -> String {
    match error {
        FormatError::BadRequest(message) => message,
        other => panic!("not a bad request: {other:?}"),
    }
}

fn sri(bytes: &[u8]) -> String {
    format!("sha512-{}", STANDARD.encode(bytes))
}

#[test]
fn reads_what_npm_publish_sends() {
    let digest: [u8; 64] = Sha512::digest(b"tarball").into();
    let doc = head(&json!({
        "dist": { "shasum": "A".repeat(40), "integrity": sri(&digest), "tarball": "http://x/p/-/p-1.0.0.tgz" }
    }));
    let request = parse(&doc).unwrap();
    assert_eq!(request.version, Version::new(1, 0, 0));
    assert_eq!(request.tags, ["latest"]);
    assert_eq!(request.attachment, "p-1.0.0.tgz");
    assert_eq!(request.claims.sha1, Some("a".repeat(40)));
    assert_eq!(request.claims.sha512, Some(digest));
    assert_eq!(request.manifest["description"], "d");
}

#[test]
fn scoped_packages_use_the_scope_in_the_attachment_name() {
    let mut doc = head(&json!({ "name": "@s/p" }));
    doc["name"] = json!("@s/p");
    doc["_id"] = json!("@s/p");
    let package = name("@s/p");
    for attachment in ["@s/p-1.0.0.tgz", "p-1.0.0.tgz"] {
        assert!(
            PublishRequest::parse(doc.to_string().as_bytes(), &package, attachment).is_ok(),
            "{attachment}"
        );
    }
    for attachment in [
        "s/p-1.0.0.tgz",
        "@s/p-1.0.1.tgz",
        "other-1.0.0.tgz",
        "p-1.0.0.tar",
    ] {
        assert!(
            PublishRequest::parse(doc.to_string().as_bytes(), &package, attachment).is_err(),
            "{attachment}"
        );
    }
}

#[test]
fn must_not_accept_a_document_for_another_package() {
    let mut doc = head(&json!({}));
    doc["name"] = json!("other");
    assert!(message(parse(&doc).unwrap_err()).contains("name"));
    let mut doc = head(&json!({}));
    doc["_id"] = json!("other");
    assert!(message(parse(&doc).unwrap_err()).contains("_id"));
    let mut doc = head(&json!({ "name": "other" }));
    assert!(message(parse(&doc).unwrap_err()).contains("name of the version"));
    doc.as_object_mut().unwrap().remove("name");
    assert!(parse(&doc).is_err());
}

#[test]
fn must_not_accept_more_or_fewer_than_one_version() {
    let mut doc = head(&json!({}));
    doc["versions"]["1.0.1"] = json!({ "name": "p", "version": "1.0.1" });
    assert!(message(parse(&doc).unwrap_err()).contains("exactly one"));
    doc["versions"] = json!({});
    assert!(message(parse(&doc).unwrap_err()).contains("exactly one"));
    doc.as_object_mut().unwrap().remove("versions");
    assert!(message(parse(&doc).unwrap_err()).contains("no versions"));
    doc["versions"] = json!("x");
    assert!(parse(&doc).is_err());
}

#[test]
fn must_not_accept_a_version_that_is_not_semver_or_disagrees_with_its_key() {
    let mut doc = head(&json!({ "version": "1.0.1" }));
    assert!(message(parse(&doc).unwrap_err()).contains("version key"));
    doc = head(&json!({}));
    doc["versions"] = json!({ "v1.0.0": { "name": "p" } });
    assert!(message(parse(&doc).unwrap_err()).contains("semantic version"));
    doc["versions"] = json!({ "1.0.0": "not an object" });
    assert!(message(parse(&doc).unwrap_err()).contains("object"));
}

#[test]
fn dist_tags_may_only_point_at_the_published_version() {
    let mut doc = head(&json!({}));
    doc["dist-tags"] = json!({ "latest": "1.0.0", "next": "1.0.0" });
    assert_eq!(parse(&doc).unwrap().tags, ["latest", "next"]);
    doc["dist-tags"] = json!({ "latest": "0.9.0" });
    assert!(message(parse(&doc).unwrap_err()).contains("dist-tags"));
    doc["dist-tags"] = json!({ "1.0.0": "1.0.0" });
    assert!(message(parse(&doc).unwrap_err()).contains("dist-tag"));
    doc["dist-tags"] = json!([]);
    assert!(parse(&doc).is_err());
    doc["dist-tags"] = Value::Null;
    assert_eq!(parse(&doc).unwrap().tags, Vec::<String>::new());
    doc.as_object_mut().unwrap().remove("dist-tags");
    assert_eq!(parse(&doc).unwrap().tags, Vec::<String>::new());
}

#[test]
fn must_not_accept_malformed_digest_claims() {
    for dist in [
        json!({ "shasum": "abc" }),
        json!({ "shasum": "z".repeat(40) }),
        json!({ "shasum": 5 }),
        json!({ "integrity": 5 }),
        json!({ "integrity": "sha512-!!!!" }),
        json!({ "integrity": "sha512-QUJD" }),
        json!({ "integrity": "" }),
        json!({ "integrity": "x".repeat(MAX_INTEGRITY_LEN + 1) }),
        json!({ "integrity": "nodash" }),
        json!("string"),
    ] {
        let doc = head(&json!({ "dist": dist }));
        assert!(parse(&doc).is_err(), "{dist}");
    }
    let sha1 = [7u8; 20];
    let doc = head(
        &json!({ "dist": { "shasum": "00".repeat(20), "integrity": format!("sha1-{}", STANDARD.encode(sha1)) } }),
    );
    assert!(message(parse(&doc).unwrap_err()).contains("disagree"));
}

#[test]
fn the_pipeline_expectation_prefers_sha1_then_sha256() {
    let mut claims = DigestClaims::default();
    assert!(claims.pipeline_expectation().is_none());
    claims.sha256 = Some([1; 32]);
    assert_eq!(
        claims.pipeline_expectation().unwrap().algo(),
        HashAlgo::Sha256
    );
    claims.sha1 = Some("ab".repeat(20));
    assert_eq!(
        claims.pipeline_expectation().unwrap().algo(),
        HashAlgo::Sha1
    );
    claims.sha1 = Some("not hex".to_owned());
    assert!(claims.pipeline_expectation().is_none());
}

#[test]
fn integrity_strings() {
    let sha1 = STANDARD.encode([1u8; 20]);
    let sha256 = STANDARD.encode([2u8; 32]);
    let sha512 = STANDARD.encode([3u8; 64]);
    let all = parse_integrity(&format!(
        "sha1-{sha1} sha256-{sha256}?opt=1 sha512-{sha512}"
    ))
    .unwrap();
    assert_eq!(all.sha1, Some([1; 20]));
    assert_eq!(all.sha256, Some([2; 32]));
    assert_eq!(all.sha512, Some([3; 64]));
    // Unknown algorithms are ignored, but a string with nothing usable at all is not an integrity.
    let sha384 = STANDARD.encode([4u8; 48]);
    let only_unknown = parse_integrity(&format!("sha384-{sha384}")).unwrap();
    assert_eq!(only_unknown, Integrity::default());
    for bad in ["", "   ", "sha512-", "sha512", "sha512-AAAA", "sha256-AAAA"] {
        assert!(parse_integrity(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn the_stored_manifest_has_the_registrys_digests_and_the_publishers_name() {
    let request = parse(&head(&json!({
        "_attachments": { "x": 1 },
        "_rev": "5-x",
        "_npmUser": { "name": "somebody else" },
        "dist": { "shasum": "f".repeat(40), "tarball": "http://elsewhere/x.tgz", "fileCount": 9 },
    })))
    .unwrap();
    let digest = [9u8; 64];
    let stored = normalized_manifest(&request, &"c".repeat(40), &digest, "alice");
    assert_eq!(stored["_id"], "p@1.0.0");
    assert_eq!(stored["dist"]["shasum"], "c".repeat(40));
    assert_eq!(stored["dist"]["integrity"], sri(&digest));
    assert_eq!(stored["dist"]["tarball"], "p/-/p-1.0.0.tgz");
    assert!(stored["dist"].get("fileCount").is_none());
    assert_eq!(stored["_npmUser"]["name"], "alice");
    for owned in ["_rev", "_revisions", "_attachments"] {
        assert!(!stored.contains_key(owned), "{owned}");
    }
    assert_eq!(stored["description"], "d");
}

#[test]
fn deprecation_patch_reads_messages_and_clears() {
    let body = json!({
        "_id": "p", "name": "p", "_rev": "3-x",
        "versions": {
            "1.0.0": { "deprecated": "old" },
            "1.1.0": { "deprecated": "" },
            "2.0.0": {},
            "3.0.0": { "deprecated": 5 },
        },
    });
    let patch = deprecation_patch(body.to_string().as_bytes(), &name("p")).unwrap();
    let by_version: std::collections::BTreeMap<String, Option<String>> =
        patch.into_iter().map(|(v, m)| (v.to_string(), m)).collect();
    assert_eq!(by_version["1.0.0"], Some("old".to_owned()));
    assert_eq!(by_version["1.1.0"], None);
    assert_eq!(by_version["2.0.0"], None);
    assert_eq!(by_version["3.0.0"], None);
    let none = deprecation_patch(br#"{"name":"p"}"#, &name("p")).unwrap();
    assert_eq!(none, Vec::new());
}

#[test]
fn must_not_apply_a_document_of_another_package_or_a_malformed_one() {
    for body in [
        r#"{"name":"other","versions":{}}"#,
        r#"{"name":"p","versions":[]}"#,
        r#"{"name":"p","versions":{"nope":{}}}"#,
        r"[]",
        r"{",
    ] {
        assert!(
            deprecation_patch(body.as_bytes(), &name("p")).is_err(),
            "{body}"
        );
        assert!(
            retained_versions(body.as_bytes(), &name("p")).is_err(),
            "{body}"
        );
    }
}

#[test]
fn retained_versions_and_the_preferred_latest() {
    let body = json!({
        "name": "p",
        "dist-tags": { "latest": "1.2.0" },
        "versions": { "1.0.0": {}, "1.2.0": {} },
    });
    let (retained, latest) = retained_versions(body.to_string().as_bytes(), &name("p")).unwrap();
    assert_eq!(retained.len(), 2);
    assert_eq!(latest, Some(Version::new(1, 2, 0)));
    let (none, no_latest) = retained_versions(br#"{"name":"p"}"#, &name("p")).unwrap();
    assert_eq!(none, Vec::new());
    assert!(no_latest.is_none());
}

#[test]
fn must_not_panic_on_arbitrary_documents() {
    use proptest::prelude::*;
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&proptest::collection::vec(any::<u8>(), 0..200), |bytes| {
            let package = name("p");
            let _ = PublishRequest::parse(&bytes, &package, "p-1.0.0.tgz");
            let _ = deprecation_patch(&bytes, &package);
            let _ = retained_versions(&bytes, &package);
            if let Ok(text) = std::str::from_utf8(&bytes) {
                let _ = parse_integrity(text);
            }
            Ok(())
        })
        .unwrap();
}
