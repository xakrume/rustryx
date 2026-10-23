//! Unit tests of the parent module.

use super::*;
use crate::events::EventKind;
use crate::testing::{assert_wire, at, round_trip_json};

fn repo_name() -> RepoName {
    RepoName::parse("maven-central").expect("name")
}

fn blob() -> BlobRecord {
    BlobRecord {
        id: BlobId::generate(),
        blob_store_id: BlobStoreId::generate(),
        key: "k".into(),
        size_bytes: 42,
        checksums: Checksums {
            sha1: "a".into(),
            sha256: "b".into(),
            md5: "c".into(),
        },
        created_at: at(),
    }
}

#[test]
fn format_labels_round_trip() {
    for f in RepoFormat::ALL {
        assert_eq!(f.as_str().parse::<RepoFormat>().expect("parse"), *f);
    }
    assert!("cargo".parse::<RepoFormat>().is_err());
}

#[test]
fn format_all_lists_the_eight_phase_one_formats_in_order() {
    let labels: Vec<_> = RepoFormat::ALL.iter().map(|f| f.as_str()).collect();
    assert_eq!(
        labels,
        [
            "maven2", "npm", "pypi", "docker", "oci", "helm", "raw", "pub"
        ]
    );
}

#[test]
fn format_serde_uses_lowercase_label() {
    let json = serde_json::to_string(&RepoFormat::Maven2).expect("serialize");
    assert_eq!(json, "\"maven2\"");
    for f in RepoFormat::ALL {
        assert_wire(f, &format!("\"{}\"", f.as_str()));
    }
}

#[test]
fn repo_type_and_write_policy_wire_names() {
    for (t, name) in [
        (RepoType::Hosted, "hosted"),
        (RepoType::Proxy, "proxy"),
        (RepoType::Group, "group"),
    ] {
        assert_wire(&t, &format!("\"{name}\""));
        assert_eq!(name.parse::<RepoType>().expect("parse"), t);
    }
    assert!("mirror".parse::<RepoType>().is_err());
    assert_wire(&WritePolicy::AllowOnce, "\"allow_once\"");
    assert_wire(&WritePolicy::Deny, "\"deny\"");
}

#[test]
fn blob_store_kinds_include_azure_and_group() {
    for (kind, name) in [
        (BlobStoreKind::Local, "local"),
        (BlobStoreKind::S3, "s3"),
        (BlobStoreKind::Azure, "azure"),
        (BlobStoreKind::Group, "group"),
    ] {
        assert_wire(&kind, &format!("\"{name}\""));
    }
}

#[test]
fn proxy_config_debug_hides_password() {
    let mut cfg = ProxyConfig::new("https://example.org");
    cfg.auth = Some(UpstreamAuth {
        username: "u".into(),
        password: Secret::new("p4ss"),
    });
    assert!(!format!("{cfg:?}").contains("p4ss"));
}

#[test]
fn must_not_leak_an_upstream_password_through_a_repository_debug() {
    let mut repo = Repository::new(
        RepositoryId::generate(),
        repo_name(),
        RepoFormat::Maven2,
        RepoType::Proxy,
        BlobStoreId::generate(),
        at(),
    );
    let mut proxy = ProxyConfig::new("https://example.org");
    proxy.auth = Some(UpstreamAuth {
        username: "u".into(),
        password: Secret::new("canary-secret"),
    });
    repo.proxy = Some(proxy);
    assert!(!format!("{repo:?}").contains("canary-secret"));
}

#[test]
fn proxy_config_constructor_sets_the_documented_defaults() {
    let cfg = ProxyConfig::new("https://repo.example.org/maven");
    assert_eq!(cfg.remote_url, "https://repo.example.org/maven");
    assert!(cfg.auth.is_none() && cfg.registry_id.is_none() && cfg.bandwidth_limit_kbps.is_none());
    assert!(cfg.ca_certificate_ids.is_empty() && cfg.namespace_allowlist.is_empty());
    assert_eq!(
        (
            cfg.content_max_age_secs,
            cfg.metadata_max_age_secs,
            cfg.negative_cache_ttl_secs,
            cfg.max_concurrent_upstream
        ),
        (3600, 300, 60, 8)
    );
}

#[test]
fn proxy_config_reads_json_written_before_the_new_fields_existed() {
    let old = r#"{"remote_url":"https://x","auth":null,"content_max_age_secs":1,"metadata_max_age_secs":2,"negative_cache_ttl_secs":3,"max_concurrent_upstream":4}"#;
    let cfg: ProxyConfig = serde_json::from_str(old).expect("N-1 JSON still parses");
    assert!(cfg.registry_id.is_none() && cfg.ca_certificate_ids.is_empty());
    assert!(cfg.namespace_allowlist.is_empty() && cfg.bandwidth_limit_kbps.is_none());
}

#[test]
fn repository_constructor_sets_the_documented_defaults() {
    let repo = Repository::new(
        RepositoryId::generate(),
        repo_name(),
        RepoFormat::Maven2,
        RepoType::Hosted,
        BlobStoreId::generate(),
        at(),
    );
    assert_eq!(repo.write_policy, WritePolicy::Allow);
    assert!(!repo.allow_anonymous && repo.online);
    assert!(repo.quota_bytes.is_none() && repo.routing_rule_id.is_none());
    assert_eq!(repo.cleanup_policy_ids.len(), 0);
    assert!(repo.proxy.is_none() && repo.group.is_none() && repo.project.is_none());
    assert!(
        repo.format_config
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
    );
    assert_eq!((repo.created_at, repo.updated_at), (at(), at()));
}

#[test]
fn repository_serializes_timestamps_as_rfc3339() {
    let mut repo = Repository::new(
        RepositoryId::generate(),
        repo_name(),
        RepoFormat::Npm,
        RepoType::Hosted,
        BlobStoreId::generate(),
        at(),
    );
    repo.project = Some(ProjectRef {
        id: ProjectId::generate(),
        name: ProjectName::parse("payments").expect("name"),
        visibility: ProjectVisibility::Public,
    });
    let json = round_trip_json(&repo);
    assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["updated_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["project"]["name"], "payments");
    assert_eq!(json["project"]["visibility"], "public");
    assert_eq!(json["write_policy"], "allow");
}

#[test]
fn repository_json_without_a_project_field_still_parses() {
    let repo = Repository::new(
        RepositoryId::generate(),
        repo_name(),
        RepoFormat::Raw,
        RepoType::Hosted,
        BlobStoreId::generate(),
        at(),
    );
    let mut json = serde_json::to_value(&repo).expect("serialize");
    json.as_object_mut().expect("object").remove("project");
    let back: Repository = serde_json::from_value(json).expect("N-1 JSON parses");
    assert!(back.project.is_none());
}

#[test]
fn blob_store_def_constructor_and_wire_form() {
    let def = BlobStoreDef::new(
        BlobStoreId::generate(),
        "default",
        BlobStoreKind::Local,
        serde_json::json!({"path": "/data"}),
        at(),
    );
    assert!(def.quota_bytes.is_none() && def.soft_quota.is_none() && def.group.is_none());
    let json = round_trip_json(&def);
    assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["kind"], "local");
}

#[test]
fn blob_record_serializes_its_creation_time() {
    assert_eq!(
        round_trip_json(&blob())["created_at"],
        "2026-10-24T10:00:00Z"
    );
}

#[test]
fn component_constructors_and_wire_form() {
    let repo = RepositoryId::generate();
    let new = NewComponent::new(repo, RepoFormat::Maven2, "lib");
    assert!(new.group_id.is_none() && new.version.is_none() && new.created_by.is_none());
    let component = Component::new(
        ComponentId::generate(),
        repo,
        RepoFormat::Maven2,
        "lib",
        at(),
    );
    assert!(component.created_by.is_none() && component.last_downloaded_at.is_none());
    let mut downloaded = component;
    downloaded.last_downloaded_at = Some(at());
    let json = round_trip_json(&downloaded);
    assert_eq!(json["last_downloaded_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
}

#[test]
fn asset_takes_size_and_digests_from_its_blob() {
    let blob = blob();
    let asset = Asset::from_blob(
        AssetId::generate(),
        RepositoryId::generate(),
        "a/b.jar",
        "application/java-archive",
        &blob,
        at(),
    );
    assert_eq!((asset.blob_id, asset.size_bytes), (blob.id, 42));
    assert_eq!(asset.checksums, blob.checksums);
    assert!(asset.component_id.is_none() && asset.created_by.is_none());
    let json = round_trip_json(&asset);
    assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    assert!(json["last_downloaded_at"].is_null());
}

#[test]
fn asset_new_takes_the_columns_and_leaves_the_optional_fields_empty() {
    let blob = blob();
    let asset = Asset::new(
        AssetId::generate(),
        RepositoryId::generate(),
        "a/b.jar",
        blob.id,
        "application/java-archive",
        7,
        blob.checksums.clone(),
        at(),
    );
    assert_eq!((asset.size_bytes, asset.blob_id), (7, blob.id));
    assert!(asset.component_id.is_none() && asset.created_by.is_none());
    assert!(asset.last_downloaded_at.is_none());
    assert_eq!((asset.created_at, asset.updated_at), (at(), at()));
}

#[test]
fn new_asset_and_link_constructors() {
    let repo = RepositoryId::generate();
    let publish = NewAsset::new(repo, "x", "text/plain", blob());
    assert!(publish.component_id.is_none() && publish.created_by.is_none());
    let link = NewAssetLink::new(repo, "x", "text/plain", BlobId::generate());
    assert!(link.component_id.is_none() && link.created_by.is_none());
}

#[test]
fn publish_outcome_records_whether_the_path_is_new() {
    let blob = blob();
    let asset = Asset::from_blob(
        AssetId::generate(),
        RepositoryId::generate(),
        "p",
        "t",
        &blob,
        at(),
    );
    let outcome = PublishOutcome::new(asset, Some(blob), false);
    assert!(!outcome.created && outcome.replaced.is_some());
}

#[test]
fn publish_mode_defaults_to_upsert() {
    assert_eq!(PublishMode::default(), PublishMode::Upsert);
}

#[test]
fn must_not_override_immutability_unless_asked_for_explicitly() {
    assert!(!WriteOptions::new().override_immutable);
    assert!(!WriteOptions::default().override_immutable);
    let built = WriteOptions::new()
        .with_mode(PublishMode::CreateOnly)
        .with_actor(ActorRef::system("gc"))
        .with_request_id("req-1");
    assert!(
        !built.override_immutable,
        "no builder step but the explicit one touches it"
    );
    assert!(
        WriteOptions::new()
            .with_override_immutable(true)
            .override_immutable
    );
}

#[test]
fn write_options_builder_collects_everything() {
    let event = EventDraft::new(
        EventKind::Delete,
        ActorRef::system("t"),
        serde_json::json!({}),
    );
    let options = WriteOptions::new()
        .with_mode(PublishMode::CreateOnly)
        .with_actor(ActorRef::system("fill"))
        .with_request_id("r")
        .with_event(event.clone())
        .with_event(event.clone());
    assert_eq!(options.mode, PublishMode::CreateOnly);
    assert_eq!(options.request_id.as_deref(), Some("r"));
    assert_eq!(options.events.len(), 2);
    assert_eq!(
        options.actor.as_ref().map(|a| a.name.as_str()),
        Some("fill")
    );
    assert_eq!(WriteOptions::new().with_events(vec![event]).events.len(), 1);
}

#[test]
fn only_client_reads_count_as_downloads() {
    for (source, counts) in [
        (DownloadSource::Client, true),
        (DownloadSource::Scanner, false),
        (DownloadSource::Replication, false),
        (DownloadSource::Internal, false),
    ] {
        assert_eq!(source.counts_as_download(), counts, "{source:?}");
    }
    assert_wire(&DownloadSource::Replication, "\"replication\"");
}

#[test]
fn project_ref_wire_form() {
    let id = ProjectId::generate();
    let project = ProjectRef {
        id,
        name: ProjectName::parse("payments").expect("name"),
        visibility: ProjectVisibility::Private,
    };
    assert_wire(
        &project,
        &format!(r#"{{"id":"{id}","name":"payments","visibility":"private"}}"#),
    );
}
