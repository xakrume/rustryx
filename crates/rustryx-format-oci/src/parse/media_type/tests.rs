use proptest::prelude::*;

use super::*;

#[test]
fn classifies_the_four_manifest_types() {
    assert_eq!(
        ManifestType::of(OCI_MANIFEST),
        Some(ManifestType::Image(Flavor::Oci))
    );
    assert_eq!(
        ManifestType::of(OCI_INDEX),
        Some(ManifestType::Index(Flavor::Oci))
    );
    assert_eq!(
        ManifestType::of(DOCKER_MANIFEST),
        Some(ManifestType::Image(Flavor::Docker))
    );
    assert_eq!(
        ManifestType::of(DOCKER_MANIFEST_LIST),
        Some(ManifestType::Index(Flavor::Docker))
    );
    assert_eq!(ManifestType::of("application/json"), None);
    assert_eq!(ManifestType::of(""), None);
    for media_type in MANIFEST_TYPES {
        assert_eq!(
            ManifestType::of(media_type).unwrap().media_type(),
            media_type
        );
        assert!(is_manifest_type(media_type));
    }
    assert!(ManifestType::of(OCI_INDEX).unwrap().is_index());
    assert!(!ManifestType::of(OCI_MANIFEST).unwrap().is_index());
}

#[test]
fn parameters_and_case_do_not_change_the_type() {
    assert_eq!(
        ManifestType::of("Application/VND.OCI.image.manifest.v1+json; charset=utf-8"),
        Some(ManifestType::Image(Flavor::Oci))
    );
    assert_eq!(
        essence(" application/json ; q=1").as_deref(),
        Some("application/json")
    );
}

#[test]
fn rejects_malformed_media_types() {
    let long = format!("application/{}", "a".repeat(300));
    for bad in [
        "",
        "application",
        "/json",
        "application/",
        "a b/c",
        "a/b c",
        "-a/b",
        "a/-b",
        long.as_str(),
        "a/b/c",
    ] {
        assert!(!is_valid_media_type(bad), "{bad:?}");
    }
    assert!(is_valid_media_type("application/vnd.example+json"));
    assert!(is_valid_media_type(OCI_EMPTY));
}

fn accept(values: &[&str]) -> Accept {
    Accept::parse(values.iter().copied())
}

#[test]
fn no_accept_header_accepts_everything() {
    let none = accept(&[]);
    assert!(none.is_unrestricted());
    assert!(none.accepts(OCI_MANIFEST));
    assert_eq!(none.manifest_types(), MANIFEST_TYPES.to_vec());
}

#[test]
fn exact_wildcard_and_subtype_wildcard_match() {
    let exact = accept(&["application/vnd.oci.image.manifest.v1+json"]);
    assert!(exact.accepts(OCI_MANIFEST));
    assert!(!exact.accepts(OCI_INDEX));
    assert_eq!(exact.manifest_types(), vec![OCI_MANIFEST]);

    let wildcard = accept(&["*/*"]);
    assert!(wildcard.accepts(OCI_INDEX));
    let family = accept(&["application/*"]);
    assert!(family.accepts(DOCKER_MANIFEST));
    assert!(!accept(&["text/*"]).accepts(DOCKER_MANIFEST));
}

#[test]
fn quality_zero_excludes_a_type() {
    let header = accept(&["*/*, application/vnd.oci.image.index.v1+json;q=0"]);
    assert!(header.accepts(OCI_MANIFEST));
    assert!(!header.accepts(OCI_INDEX));
    let only_zero = accept(&["application/vnd.oci.image.index.v1+json;q=0"]);
    assert!(!only_zero.accepts(OCI_INDEX));
    assert!(!only_zero.accepts(OCI_MANIFEST));
}

#[test]
fn several_headers_and_lists_are_combined() {
    let combined = accept(&[
        "application/vnd.docker.distribution.manifest.v2+json",
        "application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json;q=0.5",
    ]);
    assert_eq!(
        combined.manifest_types(),
        vec![OCI_INDEX, OCI_MANIFEST, DOCKER_MANIFEST]
    );
}

#[test]
fn garbage_entries_are_skipped_and_oversized_headers_ignored() {
    let skipped = accept(&["garbage, application/vnd.oci.image.manifest.v1+json, */json, ;q=1"]);
    assert!(skipped.accepts(OCI_MANIFEST));
    assert!(!skipped.accepts(OCI_INDEX));
    let huge = "a/b,".repeat(5000);
    assert!(accept(&[huge.as_str()]).is_unrestricted());
    let many = (0..200)
        .map(|i| format!("a/b{i}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(Accept::parse([many.as_str()]).entries.len() <= 64);
}

proptest! {
    #[test]
    fn parsers_never_panic(text in ".{0,300}") {
        let _ = essence(&text);
        let parsed = Accept::parse([text.as_str()]);
        let _ = parsed.accepts(OCI_MANIFEST);
        let _ = parsed.manifest_types();
        let _ = ManifestType::of(&text);
    }
}
