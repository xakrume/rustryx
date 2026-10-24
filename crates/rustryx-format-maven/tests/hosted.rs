//! Hosted repositories through the real `FormatEngine`: deploy, download, checksum files,
//! version policy, write policy, delete, authorization and the negative requirements.
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
use rustryx_core::identity::Action;
use rustryx_core::model::WritePolicy;
use support::*;

const JAR: &str = "org/acme/lib/1.0/lib-1.0.jar";

#[tokio::test]
async fn deploys_and_downloads_a_jar_with_the_protocol_headers() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);

    let response = w.put(&repo, JAR, &admin, "jar bytes").await;
    assert_eq!(status(&response), 201);
    assert!(response.headers().contains_key(header::ETAG));

    let response = w.get(&repo, JAR, &admin).await;
    assert_eq!(status(&response), 200);
    let headers = response.headers().clone();
    assert_eq!(header_of(&headers, "content-length"), Some("9"));
    assert_eq!(
        header_of(&headers, "etag").map(str::to_owned),
        Some(format!("\"{}\"", sha256_hex(b"jar bytes")))
    );
    assert!(headers.contains_key(header::LAST_MODIFIED));
    assert_eq!(header_of(&headers, "accept-ranges"), Some("bytes"));
    assert_eq!(
        header_of(&headers, "x-content-type-options"),
        Some("nosniff")
    );
    assert_eq!(
        header_of(&headers, "x-checksum-sha1"),
        Some(sha1_hex(b"jar bytes").as_str())
    );
    assert_eq!(body_text(response).await, "jar bytes");
}

#[tokio::test]
async fn an_overwrite_under_allow_is_a_200() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    assert_eq!(status(&w.put(&repo, JAR, &admin, "one").await), 201);
    assert_eq!(status(&w.put(&repo, JAR, &admin, "two").await), 200);
    assert_eq!(body_text(w.get(&repo, JAR, &admin).await).await, "two");
}

#[tokio::test]
async fn must_not_overwrite_a_release_under_allow_once() {
    let w = World::new();
    let repo = w.releases("releases").await;
    let admin = w.admin(&[&repo]);
    assert_eq!(status(&w.put(&repo, JAR, &admin, "original").await), 201);
    let second = w.put(&repo, JAR, &admin, "tampered").await;
    assert_eq!(status(&second), 409);
    assert_eq!(body_text(w.get(&repo, JAR, &admin).await).await, "original");
}

#[tokio::test]
async fn must_not_write_to_a_read_only_repository() {
    let w = World::new();
    let repo = w
        .hosted_with("frozen", WritePolicy::Deny, serde_json::json!({}))
        .await;
    let admin = w.admin(&[&repo]);
    assert_eq!(status(&w.put(&repo, JAR, &admin, "x").await), 403);
    // The no-op writes are refused too: a read-only repository accepts nothing.
    let sha = sha1_hex(b"x");
    assert_eq!(
        status(&w.put(&repo, &format!("{JAR}.sha1"), &admin, sha).await),
        403
    );
    assert_eq!(
        status(
            &w.put(
                &repo,
                "org/acme/lib/maven-metadata.xml",
                &admin,
                "<metadata/>"
            )
            .await
        ),
        403
    );
}

#[tokio::test]
async fn classifier_files_and_signatures_are_stored_next_to_the_main_artifact() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    for file in [
        "lib-1.0.jar",
        "lib-1.0.pom",
        "lib-1.0-sources.jar",
        "lib-1.0-javadoc.jar",
        "lib-1.0.jar.asc",
        "lib-1.0-bin.tar.gz",
    ] {
        let response = w
            .put(&repo, &format!("org/acme/lib/1.0/{file}"), &admin, file)
            .await;
        assert_eq!(status(&response), 201, "{file}");
    }
    let sources = w
        .get(&repo, "org/acme/lib/1.0/lib-1.0-sources.jar", &admin)
        .await;
    assert_eq!(body_text(sources).await, "lib-1.0-sources.jar");
    let metadata = body_text(
        w.get(&repo, "org/acme/lib/maven-metadata.xml", &admin)
            .await,
    )
    .await;
    assert_eq!(
        metadata.matches("<version>1.0</version>").count(),
        1,
        "one component, many files"
    );
}

#[tokio::test]
async fn the_coordinate_reaches_authorization_with_an_empty_classifier_for_the_main_file() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "x").await;
    w.put(&repo, "org/acme/lib/1.0/lib-1.0-sources.jar", &admin, "s")
        .await;
    let checks = w.rbac.checks();
    let main = checks.iter().find(|c| c.path == JAR).unwrap();
    let coordinate = main.coordinate.clone().unwrap();
    assert_eq!(main.action, Action::Write);
    assert_eq!(coordinate.group.as_deref(), Some("org.acme"));
    assert_eq!(coordinate.name.as_deref(), Some("lib"));
    assert_eq!(coordinate.version.as_deref(), Some("1.0"));
    assert_eq!(coordinate.extension.as_deref(), Some("jar"));
    assert_eq!(coordinate.classifier.as_deref(), Some(""));
    let sources = checks
        .iter()
        .find(|c| c.path.ends_with("sources.jar"))
        .unwrap();
    assert_eq!(
        sources.coordinate.clone().unwrap().classifier.as_deref(),
        Some("sources")
    );
}

#[tokio::test]
async fn range_head_and_conditional_requests_on_a_stored_file() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "0123456789").await;

    let mut head = w.head(Method::GET, JAR, &admin);
    head.headers
        .insert(header::RANGE, header_value("bytes=2-4"));
    let partial = w.send_with(&repo, head, Body::empty()).await;
    assert_eq!(status(&partial), 206);
    assert_eq!(
        header_of(partial.headers(), "content-range"),
        Some("bytes 2-4/10")
    );
    assert_eq!(body_text(partial).await, "234");

    let head_response = w.head_request(&repo, JAR, &admin).await;
    assert_eq!(status(&head_response), 200);
    assert_eq!(
        header_of(head_response.headers(), "content-length"),
        Some("10")
    );

    let etag = format!("\"{}\"", sha256_hex(b"0123456789"));
    let mut conditional = w.head(Method::GET, JAR, &admin);
    conditional
        .headers
        .insert(header::IF_NONE_MATCH, header_value(&etag));
    assert_eq!(
        status(&w.send_with(&repo, conditional, Body::empty()).await),
        304
    );
}

// ---- checksum files -------------------------------------------------------------------

#[tokio::test]
async fn checksum_files_are_derived_for_every_algorithm() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "payload").await;
    for (suffix, expected) in [
        ("sha1", sha1_hex(b"payload")),
        ("md5", md5_hex(b"payload")),
        ("sha256", sha256_hex(b"payload")),
        ("sha512", sha512_hex(b"payload")),
    ] {
        let response = w.get(&repo, &format!("{JAR}.{suffix}"), &admin).await;
        assert_eq!(status(&response), 200, "{suffix}");
        assert!(content_type(&response).starts_with("text/plain"));
        assert_eq!(body_text(response).await, expected, "{suffix}");
    }
    assert_eq!(
        status(
            &w.get(&repo, "org/acme/lib/1.0/lib-1.0.pom.sha1", &admin)
                .await
        ),
        404
    );
}

#[tokio::test]
async fn a_supplied_checksum_that_matches_is_accepted_and_not_stored() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "payload").await;
    for (suffix, digest) in [
        ("sha1", sha1_hex(b"payload")),
        ("md5", md5_hex(b"payload")),
        ("sha256", sha256_hex(b"payload")),
        ("sha512", sha512_hex(b"payload")),
    ] {
        let body = format!("{digest}  lib-1.0.jar\n");
        let response = w.put(&repo, &format!("{JAR}.{suffix}"), &admin, body).await;
        assert_eq!(status(&response), 201, "{suffix}");
    }
    // Nothing was stored for the sidecars: the store holds the jar only.
    let assets = rustryx_core::ports::AssetRepo::list(
        &w.store,
        repo.id,
        "org/",
        &rustryx_core::ports::PageRequest::default(),
    )
    .await
    .unwrap();
    assert_eq!(assets.items.len(), 1);
}

#[tokio::test]
async fn must_not_accept_a_checksum_that_does_not_match_the_file() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "payload").await;
    for (suffix, wrong) in [
        ("sha1", sha1_hex(b"other")),
        ("md5", md5_hex(b"other")),
        ("sha256", sha256_hex(b"other")),
        ("sha512", sha512_hex(b"other")),
    ] {
        let response = w
            .put(&repo, &format!("{JAR}.{suffix}"), &admin, wrong)
            .await;
        assert_eq!(status(&response), 400, "{suffix}");
    }
    let rejected: Vec<_> = w
        .audit
        .events()
        .into_iter()
        .filter(|e| e.action == "asset.rejected")
        .collect();
    assert_eq!(rejected.len(), 4, "every mismatch is audited");
}

#[tokio::test]
async fn a_malformed_checksum_file_is_a_400_and_an_oversized_one_a_413() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "payload").await;
    assert_eq!(
        status(
            &w.put(&repo, &format!("{JAR}.sha1"), &admin, "not a digest")
                .await
        ),
        400
    );
    assert_eq!(
        status(&w.put(&repo, &format!("{JAR}.sha1"), &admin, "").await),
        400
    );
    let big = "a".repeat(5000);
    assert_eq!(
        status(&w.put(&repo, &format!("{JAR}.sha1"), &admin, big).await),
        413
    );
}

#[tokio::test]
async fn a_checksum_for_a_file_that_was_not_uploaded_is_a_conflict() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    let response = w
        .put(&repo, &format!("{JAR}.sha1"), &admin, sha1_hex(b"x"))
        .await;
    assert_eq!(status(&response), 409);
}

#[tokio::test]
async fn a_checksum_put_is_idempotent_under_allow_once() {
    let w = World::new();
    let repo = w.releases("releases").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "payload").await;
    for _ in 0..2 {
        let response = w
            .put(&repo, &format!("{JAR}.sha1"), &admin, sha1_hex(b"payload"))
            .await;
        assert_eq!(
            status(&response),
            201,
            "a retried deploy must not fail on its checksum"
        );
    }
}

// ---- version policy -------------------------------------------------------------------

#[tokio::test]
async fn a_release_repository_refuses_snapshots_with_a_clear_400() {
    let w = World::new();
    let repo = w.releases("releases").await;
    let admin = w.admin(&[&repo]);
    let path = "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-1.jar";
    let response = w.put(&repo, path, &admin, "x").await;
    assert_eq!(status(&response), 400);
    assert!(body_text(response).await.contains("only release versions"));
    assert_eq!(status(&w.put(&repo, JAR, &admin, "x").await), 201);
}

#[tokio::test]
async fn a_snapshot_repository_refuses_releases_with_a_clear_400() {
    let w = World::new();
    let repo = w
        .hosted_with(
            "snaps",
            WritePolicy::Allow,
            serde_json::json!({"version_policy": "snapshot"}),
        )
        .await;
    let admin = w.admin(&[&repo]);
    let response = w.put(&repo, JAR, &admin, "x").await;
    assert_eq!(status(&response), 400);
    assert!(body_text(response).await.contains("only snapshot versions"));
    let snapshot = "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-1.jar";
    assert_eq!(status(&w.put(&repo, snapshot, &admin, "x").await), 201);
}

#[tokio::test]
async fn a_mixed_repository_takes_both() {
    let w = World::new();
    let repo = w
        .hosted_with(
            "mixed",
            WritePolicy::Allow,
            serde_json::json!({"version_policy": "mixed"}),
        )
        .await;
    let admin = w.admin(&[&repo]);
    assert_eq!(status(&w.put(&repo, JAR, &admin, "x").await), 201);
    let snapshot = "org/acme/lib/2.0-SNAPSHOT/lib-2.0-20240102.030405-1.jar";
    assert_eq!(status(&w.put(&repo, snapshot, &admin, "x").await), 201);
}

#[tokio::test]
async fn an_unknown_version_policy_fails_closed() {
    let w = World::new();
    let repo = w
        .hosted_with(
            "typo",
            WritePolicy::Allow,
            serde_json::json!({"version_policy": "relase"}),
        )
        .await;
    let admin = w.admin(&[&repo]);
    assert_eq!(status(&w.put(&repo, JAR, &admin, "x").await), 500);
}

// ---- layout ---------------------------------------------------------------------------

#[tokio::test]
async fn a_write_outside_the_maven_layout_is_a_400() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    for path in [
        "readme.txt",
        "org/acme/readme.txt",
        "org/acme/lib/1.0/other-1.0.jar",
        "org/acme.lib/lib/1.0/lib-1.0.jar",
        "org/acme/lib/1.0/lib-1.0",
        "org/acme/lib/1.0/",
    ] {
        let response = w.put(&repo, path, &admin, "x").await;
        assert_eq!(status(&response), 400, "{path}");
    }
}

#[tokio::test]
async fn methods_outside_the_protocol_are_405_with_allow() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    for method in [Method::POST, Method::PATCH, Method::OPTIONS] {
        let response = w.send(&repo, method, JAR, &admin, Body::empty()).await;
        assert_eq!(status(&response), 405);
        assert_eq!(
            header_of(response.headers(), "allow"),
            Some("GET, HEAD, PUT, DELETE")
        );
    }
}

// ---- delete ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_the_last_file_of_a_version_removes_it_from_the_metadata() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    w.deploy(&repo, &admin, "org/acme", "lib", "1.1").await;

    assert_eq!(
        status(
            &w.delete(&repo, "org/acme/lib/1.0/lib-1.0.jar", &admin)
                .await
        ),
        204
    );
    let metadata = body_text(
        w.get(&repo, "org/acme/lib/maven-metadata.xml", &admin)
            .await,
    )
    .await;
    assert!(
        metadata.contains("<version>1.0</version>"),
        "the pom is still there"
    );
    assert_eq!(
        status(
            &w.delete(&repo, "org/acme/lib/1.0/lib-1.0.pom", &admin)
                .await
        ),
        204
    );
    let metadata = body_text(
        w.get(&repo, "org/acme/lib/maven-metadata.xml", &admin)
            .await,
    )
    .await;
    assert!(!metadata.contains("<version>1.0</version>"));
    assert!(metadata.contains("<version>1.1</version>"));
    assert_eq!(
        status(&w.get(&repo, "org/acme/lib/1.0/lib-1.0.jar", &admin).await),
        404
    );
}

#[tokio::test]
async fn deleting_a_version_directory_removes_all_its_files() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    w.deploy(&repo, &admin, "org/acme", "lib", "1.1").await;
    w.put(&repo, "org/acme/lib/1.0/lib-1.0-sources.jar", &admin, "s")
        .await;

    assert_eq!(
        status(&w.delete(&repo, "org/acme/lib/1.0/", &admin).await),
        204
    );
    for file in ["lib-1.0.jar", "lib-1.0.pom", "lib-1.0-sources.jar"] {
        assert_eq!(
            status(
                &w.get(&repo, &format!("org/acme/lib/1.0/{file}"), &admin)
                    .await
            ),
            404
        );
    }
    assert_eq!(
        status(&w.get(&repo, "org/acme/lib/1.1/lib-1.1.jar", &admin).await),
        200
    );
    assert_eq!(
        status(&w.delete(&repo, "org/acme/lib/1.0/", &admin).await),
        404
    );
    let removed = w
        .audit
        .events()
        .into_iter()
        .find(|e| e.action == "component.delete")
        .expect("the delete of a version is audited");
    assert_eq!(removed.detail["files"], 3);
}

#[tokio::test]
async fn must_not_delete_what_the_caller_may_not_delete() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    let reader = w.reader(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    assert_eq!(
        status(
            &w.delete(&repo, "org/acme/lib/1.0/lib-1.0.jar", &reader)
                .await
        ),
        403
    );
    assert_eq!(
        status(&w.delete(&repo, "org/acme/lib/1.0/", &reader).await),
        403
    );
    assert_eq!(
        status(&w.get(&repo, "org/acme/lib/1.0/lib-1.0.jar", &reader).await),
        200
    );
}

#[tokio::test]
async fn generated_documents_cannot_be_deleted() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.deploy(&repo, &admin, "org/acme", "lib", "1.0").await;
    for path in [
        "org/acme/lib/maven-metadata.xml",
        "org/acme/lib/maven-metadata.xml.sha1",
        "org/acme/lib/1.0/lib-1.0.jar.sha1",
    ] {
        let response = w.delete(&repo, path, &admin).await;
        assert_eq!(status(&response), 405, "{path}");
        assert_eq!(
            header_of(response.headers(), "allow"),
            Some("GET, HEAD, PUT")
        );
    }
    assert_eq!(
        status(&w.delete(&repo, "org/acme/lib/readme", &admin).await),
        400
    );
}

// ---- authorization --------------------------------------------------------------------

#[tokio::test]
async fn anonymous_gets_401_with_a_basic_challenge_and_an_unauthorized_user_gets_403() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "x").await;

    let anonymous_response = w.get(&repo, JAR, &anonymous()).await;
    assert_eq!(status(&anonymous_response), 401);
    assert_eq!(
        header_of(anonymous_response.headers(), "www-authenticate"),
        Some("Basic realm=\"Rustryx\"")
    );

    // The user may see the repository (read) but not write to it.
    let reader = w.reader(&[&repo]);
    let forbidden = w
        .put(&repo, "org/acme/lib/2.0/lib-2.0.jar", &reader, "x")
        .await;
    assert_eq!(status(&forbidden), 403);
    assert!(!forbidden.headers().contains_key(header::WWW_AUTHENTICATE));

    // A user who cannot see the repository at all is told it does not exist.
    let stranger = principal("stranger");
    assert_eq!(status(&w.get(&repo, JAR, &stranger).await), 404);
}

#[tokio::test]
async fn error_bodies_are_plain_text_and_carry_no_internals() {
    let w = World::new();
    let repo = w.releases("releases").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "x").await;
    let response = w.put(&repo, JAR, &admin, "y").await;
    assert_eq!(status(&response), 409);
    assert!(content_type(&response).starts_with("text/plain"));
    let text = body_text(response).await;
    assert!(!text.contains("org/acme"), "no path in the body: {text}");
    assert!(!text.contains('{'), "not the JSON envelope: {text}");
}

// ---- traversal ------------------------------------------------------------------------

#[tokio::test]
async fn must_not_resolve_traversal_paths_and_authorization_never_sees_them() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "x").await;
    let before = w.rbac.checks().len();
    for path in [
        "../etc/passwd",
        "org/../../etc/passwd",
        "org/acme/lib/1.0/../1.0/lib-1.0.jar",
        "org/%2e%2e/acme/lib/1.0/lib-1.0.jar",
        "org/%252e%252e/acme/lib/1.0/lib-1.0.jar",
        "org%2facme/lib/1.0/lib-1.0.jar",
        "org\\acme\\lib\\1.0\\lib-1.0.jar",
        "org/acme/lib/1.0/lib-1.0.jar%00.png",
        "org//acme/lib/1.0/lib-1.0.jar",
        "org/acme/lib/./1.0/lib-1.0.jar",
    ] {
        for method in [Method::GET, Method::PUT, Method::DELETE] {
            let response = w
                .send(&repo, method.clone(), path, &admin, Body::from("x"))
                .await;
            assert_eq!(status(&response), 400, "{method} {path}");
        }
    }
    assert_eq!(
        w.rbac.checks().len(),
        before,
        "a bad path never reaches the authorizer"
    );
}

// ---- timing ---------------------------------------------------------------------------

#[tokio::test]
async fn last_modified_follows_the_store_clock() {
    let w = World::new();
    let repo = w.hosted("hosted").await;
    let admin = w.admin(&[&repo]);
    w.put(&repo, JAR, &admin, "x").await;
    w.clock.advance(Duration::from_secs(3600));
    let response = w.get(&repo, JAR, &admin).await;
    assert_eq!(
        header_of(response.headers(), "last-modified"),
        Some("Tue, 14 Nov 2023 22:13:20 GMT")
    );
}
