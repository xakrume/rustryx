use serde_json::json;

use super::*;

#[test]
fn defaults_follow_the_specification() {
    let config = OciConfig::default();
    assert_eq!(config.manifest_max_bytes, 4 * 1024 * 1024);
    assert_eq!(config.chunk_min_length, None);
    assert!(config.max_descriptors >= 4096);
}

#[test]
fn library_namespace_defaults_by_host() {
    let options = RepositoryOptions::default();
    for hub in [
        "https://registry-1.docker.io",
        "https://REGISTRY-1.docker.io/",
        "https://index.docker.io",
    ] {
        assert!(
            options.uses_library_namespace(&Url::parse(hub).unwrap()),
            "{hub}"
        );
    }
    for other in [
        "https://ghcr.io",
        "https://registry.example.com",
        "https://docker.io.evil.example",
    ] {
        assert!(
            !options.uses_library_namespace(&Url::parse(other).unwrap()),
            "{other}"
        );
    }
}

#[test]
fn the_option_overrides_the_host_rule() {
    let on = RepositoryOptions::from_format_config(&json!({"library_namespace": true}));
    assert!(on.uses_library_namespace(&Url::parse("https://registry.example.com").unwrap()));
    let off = RepositoryOptions::from_format_config(&json!({"library_namespace": false}));
    assert!(!off.uses_library_namespace(&Url::parse("https://registry-1.docker.io").unwrap()));
}

#[test]
fn bad_option_values_are_ignored() {
    for value in [
        json!({"library_namespace": "yes"}),
        json!([]),
        json!(null),
        json!({"x": 1}),
    ] {
        assert_eq!(
            RepositoryOptions::from_format_config(&value),
            RepositoryOptions::default()
        );
    }
}
