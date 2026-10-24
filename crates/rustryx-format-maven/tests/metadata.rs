//! Generated, uploaded and snapshot metadata of hosted repositories.
#![allow(
    clippy::unwrap_used,
    clippy::unused_self,
    clippy::format_collect,
    clippy::duration_suboptimal_units,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::large_futures,
    clippy::format_push_string,
    clippy::case_sensitive_file_extension_comparisons,
    reason = "tests: a panic is the failure signal and scenarios read best as straight-line code"
)]

mod support;

use std::time::Duration;

use axum_core::body::Body;
use http::{Method, header};
use rustryx_format_maven::metadata::parse_metadata;
use support::*;

const ARTIFACT_METADATA: &str = "org/acme/lib/maven-metadata.xml";
const SNAPSHOT_DIR: &str = "org/acme/lib/1.0-SNAPSHOT";

async fn deploy_build(
    w: &World,
    repo: &std::sync::Arc<rustryx_core::model::Repository>,
    who: &rustryx_core::identity::Principal,
    stamp: &str,
    extensions: &[&str],
) {
    for ext in extensions {
        let path = format!("{SNAPSHOT_DIR}/lib-1.0-{stamp}.{ext}");
        let response = w.put(repo, &path, who, format!("{ext} {stamp}")).await;
        assert_eq!(status(&response), 201, "{path}");
    }
}

#[tokio::test]
async fn artifact_metadata_lists_versions_in_maven_order_with_latest_and_release() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    for version in ["1.0", "1.10", "1.9", "2.0-SNAPSHOT", "1.2-beta-1"] {
        let dir = format!("org/acme/lib/{version}");
        let path = if version.ends_with("-SNAPSHOT") {
            format!("{dir}/lib-2.0-20240102.030405-1.jar")
        } else {
            format!("{dir}/lib-{version}.jar")
        };
        assert_eq!(
            status(&w.put(&repo, &path, &admin, "x").await),
            201,
            "{path}"
        );
    }
    let response = w.get(&repo, ARTIFACT_METADATA, &admin).await;
    assert_eq!(status(&response), 200);
    assert_eq!(content_type(&response), "application/xml");
    let text = body_text(response).await;
    let parsed = parse_metadata(text.as_bytes()).unwrap();
    assert_eq!(parsed.group_id.as_deref(), Some("org.acme"));
    assert_eq!(parsed.artifact_id.as_deref(), Some("lib"));
    let versioning = parsed.versioning.unwrap();
    assert_eq!(
        versioning.versions,
        ["1.0", "1.2-beta-1", "1.9", "1.10", "2.0-SNAPSHOT"]
    );
    assert_eq!(versioning.latest.as_deref(), Some("2.0-SNAPSHOT"));
    assert_eq!(versioning.release.as_deref(), Some("1.10"));
    let updated = versioning.last_updated.unwrap();
    assert_eq!(updated.len(), 14);
    assert_eq!(updated, "20231114221320");
}

#[tokio::test]
async fn metadata_follows_deploys_and_last_updated_moves_with_the_clock() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    let first = body_text(w.get(&repo, ARTIFACT_METADATA, &admin).await).await;
    w.clock.advance(Duration::from_secs(90));
    w.deploy(&repo, &admin, "org/acme", "lib", "1.1").await;
    let second = body_text(w.get(&repo, ARTIFACT_METADATA, &admin).await).await;
    assert_ne!(first, second);
    let v = parse_metadata(second.as_bytes())
        .unwrap()
        .versioning
        .unwrap();
    assert_eq!(v.versions, ["1.0", "1.1"]);
    assert_eq!(v.release.as_deref(), Some("1.1"));
    assert_eq!(v.last_updated.as_deref(), Some("20231114221450"));
}

#[tokio::test]
async fn metadata_is_the_same_on_every_read_and_has_a_stable_etag() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    let a = w.get(&repo, ARTIFACT_METADATA, &admin).await;
    let etag = header_of(a.headers(), "etag").unwrap().to_owned();
    let a_body = body_text(a).await;
    let b = w.get(&repo, ARTIFACT_METADATA, &admin).await;
    assert_eq!(header_of(b.headers(), "etag").unwrap(), etag);
    assert_eq!(body_text(b).await, a_body);

    let mut conditional = w.head(Method::GET, ARTIFACT_METADATA, &admin);
    conditional
        .headers
        .insert(header::IF_NONE_MATCH, header_value(&etag));
    assert_eq!(
        status(&w.send_with(&repo, conditional, Body::empty()).await),
        304
    );

    let head = w.head_request(&repo, ARTIFACT_METADATA, &admin).await;
    assert_eq!(
        header_of(head.headers(), "content-length").unwrap(),
        a_body.len().to_string()
    );
}

#[tokio::test]
async fn metadata_checksum_files_describe_the_generated_document() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    let xml = body_text(w.get(&repo, ARTIFACT_METADATA, &admin).await).await;
    for (suffix, expected) in [
        ("sha1", sha1_hex(xml.as_bytes())),
        ("md5", md5_hex(xml.as_bytes())),
        ("sha256", sha256_hex(xml.as_bytes())),
        ("sha512", sha512_hex(xml.as_bytes())),
    ] {
        let response = w
            .get(&repo, &format!("{ARTIFACT_METADATA}.{suffix}"), &admin)
            .await;
        assert_eq!(status(&response), 200, "{suffix}");
        assert_eq!(body_text(response).await, expected, "{suffix}");
    }
}

#[tokio::test]
async fn metadata_of_an_unknown_artifact_or_group_level_is_404() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    assert_eq!(
        status(&w.get(&repo, "org/none/maven-metadata.xml", &admin).await),
        404
    );
    assert_eq!(
        status(&w.get(&repo, "org/acme/maven-metadata.xml", &admin).await),
        404
    );
    assert_eq!(
        status(
            &w.get(&repo, "org/none/maven-metadata.xml.sha1", &admin)
                .await
        ),
        404
    );
    assert_eq!(
        status(
            &w.get(
                &repo,
                "org/acme/lib/1.0-SNAPSHOT/maven-metadata.xml",
                &admin
            )
            .await
        ),
        404
    );
}

#[tokio::test]
async fn metadata_never_mixes_repositories() {
    let w = World::new();
    let one = w.hosted("one").await;
    let two = w.hosted("two").await;
    let admin = w.admin(&[&one, &two]);
    w.deploy(&one, &admin, "org/acme", "lib", "1.0").await;
    w.deploy(&two, &admin, "org/acme", "lib", "2.0").await;
    let v = parse_metadata(
        body_text(w.get(&one, ARTIFACT_METADATA, &admin).await)
            .await
            .as_bytes(),
    )
    .unwrap()
    .versioning
    .unwrap();
    assert_eq!(v.versions, ["1.0"]);
}

// ---- snapshots ------------------------------------------------------------------------

#[tokio::test]
async fn timestamped_snapshot_deploys_are_listed_in_the_version_metadata() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    deploy_build(&w, &repo, &admin, "20240102.030405-1", &["jar", "pom"]).await;
    w.put(
        &repo,
        &format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1-sources.jar"),
        &admin,
        "s1",
    )
    .await;
    deploy_build(&w, &repo, &admin, "20240103.030405-2", &["jar", "pom"]).await;

    let response = w
        .get(&repo, &format!("{SNAPSHOT_DIR}/maven-metadata.xml"), &admin)
        .await;
    assert_eq!(status(&response), 200);
    let text = body_text(response).await;
    assert!(text.contains("modelVersion=\"1.1.0\""));
    let parsed = parse_metadata(text.as_bytes()).unwrap();
    assert_eq!(parsed.version.as_deref(), Some("1.0-SNAPSHOT"));
    let v = parsed.versioning.unwrap();
    let snapshot = v.snapshot.unwrap();
    assert_eq!(snapshot.timestamp.as_deref(), Some("20240103.030405"));
    assert_eq!(snapshot.build_number, Some(2));
    assert_eq!(v.last_updated.as_deref(), Some("20240103030405"));
    let entries: Vec<(Option<&str>, &str, &str)> = v
        .snapshot_versions
        .iter()
        .map(|e| {
            (
                e.classifier.as_deref(),
                e.extension.as_str(),
                e.value.as_str(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            (None, "jar", "1.0-20240103.030405-2"),
            (None, "pom", "1.0-20240103.030405-2"),
            (Some("sources"), "jar", "1.0-20240102.030405-1"),
        ]
    );
    // The artifact level document lists the base version once.
    let artifact = body_text(w.get(&repo, ARTIFACT_METADATA, &admin).await).await;
    let versions = parse_metadata(artifact.as_bytes())
        .unwrap()
        .versioning
        .unwrap()
        .versions;
    assert_eq!(versions, ["1.0-SNAPSHOT"]);
}

#[tokio::test]
async fn a_snapshot_file_without_a_timestamp_resolves_to_the_newest_build() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    deploy_build(&w, &repo, &admin, "20240102.030405-1", &["jar"]).await;
    deploy_build(&w, &repo, &admin, "20240103.030405-2", &["jar"]).await;
    w.put(
        &repo,
        &format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1-sources.jar"),
        &admin,
        "sources 1",
    )
    .await;

    let jar = w
        .get(
            &repo,
            &format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar"),
            &admin,
        )
        .await;
    assert_eq!(status(&jar), 200);
    assert_eq!(body_text(jar).await, "jar 20240103.030405-2");
    let sources = w
        .get(
            &repo,
            &format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT-sources.jar"),
            &admin,
        )
        .await;
    assert_eq!(body_text(sources).await, "sources 1");
    let checksum = w
        .get(
            &repo,
            &format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar.sha1"),
            &admin,
        )
        .await;
    assert_eq!(
        body_text(checksum).await,
        sha1_hex(b"jar 20240103.030405-2")
    );
    assert_eq!(
        status(
            &w.get(
                &repo,
                &format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.zip"),
                &admin
            )
            .await
        ),
        404
    );
}

#[tokio::test]
async fn a_non_unique_snapshot_is_served_and_overwritten_in_place() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    let path = format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar");
    assert_eq!(status(&w.put(&repo, &path, &admin, "first").await), 201);
    assert_eq!(status(&w.put(&repo, &path, &admin, "second").await), 200);
    assert_eq!(body_text(w.get(&repo, &path, &admin).await).await, "second");
    // No build to resolve, so no version metadata.
    assert_eq!(
        status(
            &w.get(&repo, &format!("{SNAPSHOT_DIR}/maven-metadata.xml"), &admin)
                .await
        ),
        404
    );
}

#[tokio::test]
async fn snapshot_builds_are_unique_files_under_allow_once() {
    let w = World::new();
    let repo = w
        .hosted_with(
            "snaps",
            rustryx_core::model::WritePolicy::AllowOnce,
            serde_json::json!({"version_policy": "snapshot"}),
        )
        .await;
    let admin = w.admin(&[&repo]);
    deploy_build(&w, &repo, &admin, "20240102.030405-1", &["jar"]).await;
    deploy_build(&w, &repo, &admin, "20240103.030405-2", &["jar"]).await;
    let again = w
        .put(
            &repo,
            &format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1.jar"),
            &admin,
            "x",
        )
        .await;
    assert_eq!(status(&again), 409);
}

// ---- uploaded metadata ----------------------------------------------------------------

#[tokio::test]
async fn uploaded_metadata_is_accepted_and_the_generated_document_is_unchanged() {
    let w = World::new();
    let repo = w.releases("releases").await;
    let admin = w.admin(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    let before = body_text(w.get(&repo, ARTIFACT_METADATA, &admin).await).await;
    let claimed = "<metadata><groupId>org.acme</groupId><artifactId>lib</artifactId>\
        <versioning><latest>99.0</latest><versions><version>99.0</version></versions></versioning></metadata>";
    // Twice: a retry of `mvn deploy` must not trip over allow_once on a document that is not stored.
    for _ in 0..2 {
        assert_eq!(
            status(&w.put(&repo, ARTIFACT_METADATA, &admin, claimed).await),
            201
        );
        let digest = sha1_hex(claimed.as_bytes());
        assert_eq!(
            status(
                &w.put(&repo, &format!("{ARTIFACT_METADATA}.sha1"), &admin, digest)
                    .await
            ),
            201
        );
    }
    assert_eq!(
        body_text(w.get(&repo, ARTIFACT_METADATA, &admin).await).await,
        before
    );
}

#[tokio::test]
async fn must_not_resolve_an_xxe_document_in_an_upload() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    let xxe = "<?xml version=\"1.0\"?><!DOCTYPE metadata [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]>\
        <metadata><groupId>org.acme</groupId><artifactId>&xxe;</artifactId></metadata>";
    let response = w.put(&repo, ARTIFACT_METADATA, &admin, xxe).await;
    assert_eq!(status(&response), 422);
    assert!(!body_text(response).await.contains("root:"));
}

#[tokio::test]
async fn must_not_expand_a_billion_laughs_upload() {
    use std::fmt::Write as _;
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    let mut bomb = String::from("<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\">");
    for level in 1..=9 {
        let previous = if level == 1 {
            "lol".to_owned()
        } else {
            format!("lol{}", level - 1)
        };
        write!(
            bomb,
            "<!ENTITY lol{level} \"{}\">",
            format!("&{previous};").repeat(10)
        )
        .unwrap();
    }
    bomb.push_str("]><metadata><groupId>&lol9;</groupId></metadata>");
    let started = std::time::Instant::now();
    assert_eq!(
        status(&w.put(&repo, ARTIFACT_METADATA, &admin, bomb).await),
        422
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "refused, not expanded"
    );
}

#[tokio::test]
async fn must_not_accept_oversized_metadata() {
    let w = World::with_config(rustryx_format_maven::MavenConfig {
        max_metadata_bytes: 2048,
        ..rustryx_format_maven::MavenConfig::default()
    });
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    let mut xml = String::from(
        "<metadata><groupId>org.acme</groupId><artifactId>lib</artifactId><versioning><versions>",
    );
    for i in 0..400 {
        xml.push_str(&format!("<version>{i}.0</version>"));
    }
    xml.push_str("</versions></versioning></metadata>");
    assert!(xml.len() > 2048);
    // Declared length over the cap: refused before a byte is read.
    let mut declared = w.head(Method::PUT, ARTIFACT_METADATA, &admin);
    declared
        .headers
        .insert(header::CONTENT_LENGTH, header_value(&xml.len().to_string()));
    let response = w.send_with(&repo, declared, Body::from(xml.clone())).await;
    assert_eq!(status(&response), 413);
    // No declared length (chunked): refused when the actual bytes cross the cap.
    let response = w.put(&repo, ARTIFACT_METADATA, &admin, xml).await;
    assert_eq!(status(&response), 413);
}

#[tokio::test]
async fn uploaded_metadata_must_be_xml_and_must_describe_its_path() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    assert_eq!(
        status(
            &w.put(&repo, ARTIFACT_METADATA, &admin, "not xml at all")
                .await
        ),
        422
    );
    assert_eq!(
        status(&w.put(&repo, ARTIFACT_METADATA, &admin, "<other/>").await),
        422
    );
    let wrong = "<metadata><groupId>org.evil</groupId><artifactId>lib</artifactId></metadata>";
    assert_eq!(
        status(&w.put(&repo, ARTIFACT_METADATA, &admin, wrong).await),
        400
    );
    let right = "<metadata><groupId>org.acme</groupId><artifactId>lib</artifactId></metadata>";
    assert_eq!(
        status(&w.put(&repo, ARTIFACT_METADATA, &admin, right).await),
        201
    );
}

#[tokio::test]
async fn plugin_group_metadata_is_accepted_and_dropped() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    let plugins = "<metadata><groupId>org.acme.tools</groupId><plugins><plugin><name>Tool</name>\
        <prefix>tool</prefix><artifactId>tool-maven-plugin</artifactId></plugin></plugins></metadata>";
    assert_eq!(
        status(
            &w.put(&repo, "org/acme/tools/maven-metadata.xml", &admin, plugins)
                .await
        ),
        201
    );
    assert_eq!(
        status(
            &w.get(&repo, "org/acme/tools/maven-metadata.xml", &admin)
                .await
        ),
        404
    );
}

#[tokio::test]
async fn a_release_repository_refuses_snapshot_metadata_uploads() {
    let w = World::new();
    let repo = w.releases("releases").await;
    let admin = w.admin(&[&repo]);
    let doc = "<metadata><groupId>org.acme</groupId><artifactId>lib</artifactId><version>1.0-SNAPSHOT</version></metadata>";
    assert_eq!(
        status(
            &w.put(
                &repo,
                &format!("{SNAPSHOT_DIR}/maven-metadata.xml"),
                &admin,
                doc
            )
            .await
        ),
        400
    );
}
