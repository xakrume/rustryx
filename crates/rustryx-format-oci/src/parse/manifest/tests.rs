use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;
use crate::parse::digest::DigestAlgorithm;
use crate::parse::media_type::{
    DOCKER_MANIFEST, DOCKER_MANIFEST_LIST, OCI_CONFIG, OCI_EMPTY, OCI_INDEX, OCI_MANIFEST,
};

const MAX: usize = 64;

fn digest(seed: char) -> String {
    format!("sha256:{}", seed.to_string().repeat(64))
}

fn descriptor(media_type: &str, seed: char, size: u64) -> Value {
    json!({"mediaType": media_type, "digest": digest(seed), "size": size})
}

fn image() -> Value {
    json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "config": descriptor(OCI_CONFIG, 'a', 10),
        "layers": [descriptor("application/vnd.oci.image.layer.v1.tar+gzip", 'b', 20)],
    })
}

fn parse(value: &Value, content_type: Option<&str>) -> Result<ParsedManifest, ManifestError> {
    ParsedManifest::parse(value.to_string().as_bytes(), content_type, MAX)
}

#[test]
fn parses_an_oci_image_manifest() {
    let manifest = parse(&image(), Some(OCI_MANIFEST)).unwrap();
    assert_eq!(manifest.media_type(), OCI_MANIFEST);
    assert!(!manifest.manifest_type.is_index());
    assert_eq!(manifest.config.as_ref().unwrap().size, 10);
    assert_eq!(manifest.layers.len(), 1);
    assert_eq!(manifest.required_blobs().count(), 2);
    assert_eq!(
        manifest.referrer_artifact_type().as_deref(),
        Some(OCI_CONFIG)
    );
}

#[test]
fn parses_a_docker_schema_2_manifest() {
    let mut value = image();
    value["mediaType"] = json!(DOCKER_MANIFEST);
    value["config"]["mediaType"] = json!("application/vnd.docker.container.image.v1+json");
    let manifest = parse(&value, Some(DOCKER_MANIFEST)).unwrap();
    assert_eq!(manifest.media_type(), DOCKER_MANIFEST);
}

#[test]
fn parses_an_index_and_a_manifest_list() {
    for (media_type, canonical) in [
        (OCI_INDEX, OCI_INDEX),
        (DOCKER_MANIFEST_LIST, DOCKER_MANIFEST_LIST),
    ] {
        let value = json!({
            "schemaVersion": 2,
            "mediaType": media_type,
            "manifests": [
                {"mediaType": OCI_MANIFEST, "digest": digest('c'), "size": 5, "platform": {"architecture": "amd64", "os": "linux"}},
                {"mediaType": "application/vnd.unknown", "digest": digest('d'), "size": 6},
            ],
        });
        let manifest = parse(&value, Some(media_type)).unwrap();
        assert_eq!(manifest.media_type(), canonical);
        assert!(manifest.manifest_type.is_index());
        assert_eq!(manifest.manifests.len(), 2);
        // An entry of an unknown type is stored but never required to exist.
        assert_eq!(manifest.required_children().count(), 1);
        assert_eq!(manifest.required_blobs().count(), 0);
    }
}

#[test]
fn an_empty_index_is_valid() {
    let value = json!({"schemaVersion": 2, "mediaType": OCI_INDEX, "manifests": []});
    assert!(parse(&value, Some(OCI_INDEX)).unwrap().manifests.is_empty());
}

#[test]
fn the_type_comes_from_the_header_when_the_document_has_none() {
    let mut value = image();
    value.as_object_mut().unwrap().remove("mediaType");
    assert_eq!(
        parse(&value, Some(OCI_MANIFEST)).unwrap().media_type(),
        OCI_MANIFEST
    );
    assert_eq!(
        parse(&value, None),
        Err(ManifestError::UnsupportedMediaType)
    );
    assert_eq!(
        parse(&value, Some("application/octet-stream")),
        Err(ManifestError::UnsupportedMediaType)
    );
}

#[test]
fn the_header_must_match_the_document() {
    assert_eq!(
        parse(&image(), Some(DOCKER_MANIFEST)),
        Err(ManifestError::MediaTypeMismatch)
    );
    assert_eq!(
        parse(&image(), Some("application/json")),
        Err(ManifestError::MediaTypeMismatch)
    );
    // Parameters on the header do not count as a mismatch.
    assert!(
        parse(
            &image(),
            Some("application/vnd.oci.image.manifest.v1+json; charset=utf-8")
        )
        .is_ok()
    );
    // A header is optional when the document names its own type.
    assert!(parse(&image(), None).is_ok());
}

#[test]
fn a_document_media_type_that_is_not_a_manifest_is_refused() {
    let mut value = image();
    value["mediaType"] = json!("application/vnd.oci.image.config.v1+json");
    assert_eq!(
        parse(&value, None),
        Err(ManifestError::UnsupportedMediaType)
    );
}

#[test]
fn schema_version_must_be_present_and_2() {
    let mut value = image();
    value["schemaVersion"] = json!(1);
    assert_eq!(
        parse(&value, None),
        Err(ManifestError::UnsupportedSchema(1))
    );
    value.as_object_mut().unwrap().remove("schemaVersion");
    assert_eq!(
        parse(&value, None),
        Err(ManifestError::MissingSchemaVersion)
    );
    value["schemaVersion"] = json!("2");
    assert_eq!(parse(&value, None), Err(ManifestError::NotJson));
}

#[test]
fn required_parts_are_required() {
    let mut no_config = image();
    no_config.as_object_mut().unwrap().remove("config");
    assert_eq!(
        parse(&no_config, None),
        Err(ManifestError::MissingField("config"))
    );
    let mut no_layers = image();
    no_layers.as_object_mut().unwrap().remove("layers");
    assert_eq!(
        parse(&no_layers, None),
        Err(ManifestError::MissingField("layers"))
    );
    let no_manifests = json!({"schemaVersion": 2, "mediaType": OCI_INDEX});
    assert_eq!(
        parse(&no_manifests, None),
        Err(ManifestError::MissingField("manifests"))
    );
}

#[test]
fn descriptors_are_validated() {
    for mutate in [
        |v: &mut Value| v["config"]["digest"] = json!("sha256:short"),
        |v: &mut Value| v["config"]["digest"] = json!(12),
        |v: &mut Value| v["config"]["size"] = json!(-1),
        |v: &mut Value| v["config"]["size"] = json!("10"),
        |v: &mut Value| v["config"]["mediaType"] = json!("not a media type"),
        |v: &mut Value| {
            v["config"].as_object_mut().unwrap().remove("size");
        },
        |v: &mut Value| v["layers"][0]["urls"] = json!(vec!["x"; 40]),
        |v: &mut Value| v["artifactType"] = json!("bad type"),
        |v: &mut Value| v["layers"][0]["artifactType"] = json!("bad type"),
    ] {
        let mut value = image();
        mutate(&mut value);
        assert!(
            matches!(
                parse(&value, None),
                Err(ManifestError::InvalidField(_) | ManifestError::NotJson)
            ),
            "{value}"
        );
    }
}

#[test]
fn an_unsupported_digest_algorithm_is_named_as_such() {
    let mut value = image();
    value["layers"][0]["digest"] = json!("blake3:abcdef0123456789");
    assert_eq!(parse(&value, None), Err(ManifestError::UnsupportedDigest));
}

#[test]
fn not_json_is_refused_without_echoing_the_content() {
    for body in [
        &b""[..],
        b"{",
        b"[]",
        b"null",
        b"\xff\xfe",
        b"{\"schemaVersion\": 2,",
    ] {
        let error = ParsedManifest::parse(body, Some(OCI_MANIFEST), MAX).unwrap_err();
        assert_eq!(error, ManifestError::NotJson);
    }
}

#[test]
fn too_many_descriptors_are_refused() {
    let layers: Vec<Value> = (0..=MAX)
        .map(|_| descriptor("application/x", 'b', 1))
        .collect();
    let mut value = image();
    value["layers"] = json!(layers);
    assert_eq!(
        parse(&value, None),
        Err(ManifestError::TooManyDescriptors(MAX))
    );
}

#[test]
fn foreign_layers_are_not_required_blobs() {
    let mut value = image();
    value["layers"] = json!([
        {"mediaType": "application/vnd.docker.image.rootfs.foreign.diff.tar.gzip", "digest": digest('e'), "size": 3, "urls": ["https://example.com/l"]},
        descriptor("application/x", 'f', 4),
    ]);
    let manifest = parse(&value, None).unwrap();
    let required: Vec<String> = manifest
        .required_blobs()
        .map(|d| d.digest.to_string())
        .collect();
    assert_eq!(required, [digest('a'), digest('f')]);
}

#[test]
fn an_empty_config_needs_an_artifact_type() {
    let mut value = image();
    value["config"] = descriptor(OCI_EMPTY, '4', 2);
    assert_eq!(parse(&value, None), Err(ManifestError::MissingArtifactType));
    value["artifactType"] = json!("application/vnd.example.sbom");
    let manifest = parse(&value, None).unwrap();
    assert_eq!(
        manifest.referrer_artifact_type().as_deref(),
        Some("application/vnd.example.sbom")
    );
}

#[test]
fn a_subject_is_a_validated_descriptor_and_may_dangle() {
    let mut value = image();
    value["subject"] = descriptor(OCI_MANIFEST, '9', 7);
    let manifest = parse(&value, None).unwrap();
    assert_eq!(
        manifest.subject.as_ref().unwrap().digest.to_string(),
        digest('9')
    );
    value["subject"]["digest"] = json!("nope");
    assert!(matches!(
        parse(&value, None),
        Err(ManifestError::InvalidField("subject"))
    ));
}

#[test]
fn unknown_fields_are_tolerated_and_annotations_kept() {
    let mut value = image();
    value["annotations"] = json!({"org.example.k": "v"});
    value["x-unknown"] = json!({"a": [1, 2, 3]});
    let manifest = parse(&value, None).unwrap();
    assert_eq!(
        manifest
            .annotations
            .get("org.example.k")
            .map(String::as_str),
        Some("v")
    );
    value["annotations"] = json!({"k": 5});
    assert_eq!(parse(&value, None), Err(ManifestError::NotJson));
}

#[test]
fn referrer_descriptor_carries_type_and_annotations() {
    let mut value = image();
    value["artifactType"] = json!("application/vnd.example.sig");
    value["annotations"] = json!({"a": "b"});
    let manifest = parse(&value, None).unwrap();
    let digest = Digest::parse(&digest('1')).unwrap();
    let referrer = manifest.as_referrer(&digest, 321);
    assert_eq!(
        referrer.to_json(),
        json!({
            "mediaType": OCI_MANIFEST,
            "digest": digest.to_string(),
            "size": 321,
            "artifactType": "application/vnd.example.sig",
            "annotations": {"a": "b"},
        })
    );
}

proptest! {
    #[test]
    fn parse_never_panics_on_bytes(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = ParsedManifest::parse(&body, Some(OCI_MANIFEST), MAX);
        let _ = ParsedManifest::parse(&body, None, MAX);
    }

    #[test]
    fn parse_never_panics_on_json_soup(depth in 0usize..6, key in "[a-zA-Z]{1,12}", number in any::<i64>(), text in ".{0,20}") {
        let mut value = json!({key.clone(): number, "mediaType": text, "schemaVersion": 2});
        for _ in 0..depth {
            value = json!({"config": value, "layers": [{"x": 1}], "manifests": [], "subject": {"digest": 5}});
        }
        let _ = ParsedManifest::parse(value.to_string().as_bytes(), Some(OCI_MANIFEST), MAX);
    }

    #[test]
    fn a_generated_valid_manifest_always_parses(layer_count in 0usize..10, sizes in proptest::collection::vec(0u64..1_000_000, 10)) {
        let layers: Vec<Value> = (0..layer_count)
            .map(|i| json!({"mediaType": "application/x", "digest": Digest::of(DigestAlgorithm::Sha256, &[u8::try_from(i).unwrap()]).to_string(), "size": sizes[i]}))
            .collect();
        let value = json!({"schemaVersion": 2, "mediaType": OCI_MANIFEST, "config": descriptor(OCI_CONFIG, 'a', 1), "layers": layers});
        let manifest = parse(&value, Some(OCI_MANIFEST)).unwrap();
        prop_assert_eq!(manifest.layers.len(), layer_count);
        prop_assert_eq!(manifest.required_blobs().count(), layer_count + 1);
    }
}
