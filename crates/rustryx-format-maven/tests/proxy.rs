//! Proxy repositories over a scripted upstream: TTLs, immutability, verification, snapshots.
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
use rustryx_core::ports::{AssetRepo, ComponentFilter, ComponentRepo, PageRequest};
use rustryx_format_core::testing::Reply;
use support::*;

const JAR: &str = "org/acme/lib/1.0/lib-1.0.jar";
const METADATA: &str = "org/acme/lib/maven-metadata.xml";
const SNAPSHOT_DIR: &str = "org/acme/lib/1.0-SNAPSHOT";

fn up(path: &str) -> String {
    format!("/maven2/{path}")
}

fn metadata_xml(versions: &[&str]) -> String {
    let list: String = versions
        .iter()
        .map(|v| format!("<version>{v}</version>"))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<metadata><groupId>org.acme</groupId>\
         <artifactId>lib</artifactId><versioning><versions>{list}</versions>\
         <lastUpdated>20240101000000</lastUpdated></versioning></metadata>\n"
    )
}

fn script_artifact(w: &World, path: &str, body: &str) {
    w.upstream
        .route(&up(path), vec![Reply::ok(body.to_owned())]);
    w.upstream.route(
        &up(&format!("{path}.sha1")),
        vec![Reply::ok(sha1_hex(body.as_bytes()))],
    );
}

async fn cached(
    w: &World,
    repo: &std::sync::Arc<rustryx_core::model::Repository>,
    path: &str,
) -> bool {
    AssetRepo::get_by_path(&w.store, repo.id, path)
        .await
        .unwrap()
        .is_some()
}

// ---- metadata -------------------------------------------------------------------------

#[tokio::test]
async fn metadata_is_served_byte_for_byte_and_cached_for_the_metadata_ttl() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    let xml = metadata_xml(&["1.0", "1.1"]);
    w.upstream
        .route(&up(METADATA), vec![Reply::ok(xml.clone())]);

    let first = w.get(&proxy, METADATA, &user).await;
    assert_eq!(status(&first), 200);
    assert_eq!(content_type(&first), "application/xml");
    assert_eq!(header_of(first.headers(), "x-rustryx-cache"), Some("MISS"));
    assert_eq!(body_text(first).await, xml);

    let second = w.get(&proxy, METADATA, &user).await;
    assert_eq!(header_of(second.headers(), "x-rustryx-cache"), Some("HIT"));
    assert_eq!(body_text(second).await, xml);
    assert_eq!(
        w.upstream.calls_for(&up(METADATA)),
        1,
        "inside the TTL the upstream is not asked"
    );

    // Fixture metadata TTL is 600 s: past it the upstream is asked again.
    w.clock.advance(Duration::from_secs(601));
    let third = w.get(&proxy, METADATA, &user).await;
    assert_eq!(status(&third), 200);
    assert_eq!(w.upstream.calls_for(&up(METADATA)), 2);
}

#[tokio::test]
async fn metadata_checksum_files_are_derived_from_the_served_bytes() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    let xml = metadata_xml(&["1.0"]);
    w.upstream
        .route(&up(METADATA), vec![Reply::ok(xml.clone())]);
    // The upstream's own sidecar is deliberately wrong: it must never be asked for.
    w.upstream.route(
        &up(&format!("{METADATA}.sha1")),
        vec![Reply::ok("0".repeat(40))],
    );
    for (suffix, expected) in [
        ("sha1", sha1_hex(xml.as_bytes())),
        ("md5", md5_hex(xml.as_bytes())),
        ("sha256", sha256_hex(xml.as_bytes())),
        ("sha512", sha512_hex(xml.as_bytes())),
    ] {
        let response = w.get(&proxy, &format!("{METADATA}.{suffix}"), &user).await;
        assert_eq!(status(&response), 200, "{suffix}");
        assert_eq!(body_text(response).await, expected, "{suffix}");
    }
    assert_eq!(w.upstream.calls_for(&up(&format!("{METADATA}.sha1"))), 0);
    assert_eq!(
        w.upstream.calls_for(&up(METADATA)),
        1,
        "one fetch serves the document and its digests"
    );
}

#[tokio::test]
async fn metadata_is_served_stale_when_the_upstream_fails_after_the_ttl() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    let xml = metadata_xml(&["1.0"]);
    w.upstream.route(
        &up(METADATA),
        vec![Reply::ok(xml.clone()), Reply::status(503)],
    );
    assert_eq!(status(&w.get(&proxy, METADATA, &user).await), 200);
    w.clock.advance(Duration::from_secs(700));
    let stale = w.get(&proxy, METADATA, &user).await;
    assert_eq!(status(&stale), 200);
    assert_eq!(header_of(stale.headers(), "x-rustryx-cache"), Some("STALE"));
    assert!(
        header_of(stale.headers(), "warning")
            .unwrap()
            .starts_with("110")
    );
    assert_eq!(body_text(stale).await, xml);
}

#[tokio::test]
async fn metadata_supports_conditional_requests() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    w.upstream
        .route(&up(METADATA), vec![Reply::ok(metadata_xml(&["1.0"]))]);
    let first = w.get(&proxy, METADATA, &user).await;
    let etag = header_of(first.headers(), "etag").unwrap().to_owned();
    let mut conditional = w.head(Method::GET, METADATA, &user);
    conditional
        .headers
        .insert(header::IF_NONE_MATCH, header_value(&etag));
    assert_eq!(
        status(&w.send_with(&proxy, conditional, Body::empty()).await),
        304
    );
}

#[tokio::test]
async fn a_missing_metadata_document_is_a_404_and_remembered() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    assert_eq!(status(&w.get(&proxy, METADATA, &user).await), 404);
    assert_eq!(status(&w.get(&proxy, METADATA, &user).await), 404);
    assert_eq!(
        w.upstream.calls_for(&up(METADATA)),
        1,
        "the 404 is cached negatively"
    );
}

// ---- artifacts ------------------------------------------------------------------------

#[tokio::test]
async fn a_release_is_verified_against_its_sha1_and_then_cached_for_good() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    script_artifact(&w, JAR, "jar bytes");

    let first = w.get(&proxy, JAR, &user).await;
    assert_eq!(status(&first), 200);
    assert_eq!(body_text(first).await, "jar bytes");
    assert_eq!(w.upstream.calls_for(&up(JAR)), 1);
    assert_eq!(w.upstream.calls_for(&up(&format!("{JAR}.sha1"))), 1);

    let calls = w.upstream.calls();
    // A year later the release is still served from the cache: it cannot change at the upstream.
    w.clock.advance(Duration::from_secs(365 * 24 * 3600));
    let again = w.get(&proxy, JAR, &user).await;
    assert_eq!(status(&again), 200);
    assert_eq!(header_of(again.headers(), "x-rustryx-cache"), Some("HIT"));
    assert_eq!(body_text(again).await, "jar bytes");
    assert_eq!(w.upstream.calls(), calls, "no upstream request at all");
}

#[tokio::test]
async fn must_not_serve_or_cache_a_file_that_does_not_match_the_upstream_sha1() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    w.upstream
        .route(&up(JAR), vec![Reply::ok("poisoned bytes")]);
    w.upstream.route(
        &up(&format!("{JAR}.sha1")),
        vec![Reply::ok(sha1_hex(b"the real bytes"))],
    );

    let response = w.get(&proxy, JAR, &user).await;
    assert_eq!(status(&response), 502);
    let text = body_text(response).await;
    assert!(!text.contains("poisoned"));
    assert!(
        !cached(&w, &proxy, JAR).await,
        "a file that failed verification is not cached"
    );
    assert!(
        w.audit
            .events()
            .iter()
            .any(|e| e.action.contains("checksum_mismatch")),
        "the mismatch is audited"
    );
}

#[tokio::test]
async fn a_sha256_sidecar_verifies_when_there_is_no_sha1() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(&up(JAR), vec![Reply::ok("jar bytes")]);
    w.upstream.route(
        &up(&format!("{JAR}.sha256")),
        vec![Reply::ok("f".repeat(64))],
    );
    assert_eq!(status(&w.get(&proxy, JAR, &user).await), 502);
    w.upstream.route(
        &up(&format!("{JAR}.sha256")),
        vec![Reply::ok(sha256_hex(b"jar bytes"))],
    );
    // The wrong sidecar was cached as a document with the metadata lifetime (600 s); past it the
    // corrected one is fetched. The earlier 404 of `.sha1` was remembered for 60 s only.
    w.clock.advance(Duration::from_secs(601));
    assert_eq!(status(&w.get(&proxy, JAR, &user).await), 200);
}

#[tokio::test]
async fn a_file_without_any_sidecar_is_served_unverified() {
    let w = World::new();
    let proxy = w.proxy("mirror").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(&up(JAR), vec![Reply::ok("jar bytes")]);
    let response = w.get(&proxy, JAR, &user).await;
    assert_eq!(status(&response), 200);
    assert_eq!(body_text(response).await, "jar bytes");
    w.settle().await;
    assert!(cached(&w, &proxy, JAR).await);
}

#[tokio::test]
async fn must_not_serve_a_file_that_contradicts_the_digest_in_the_upstream_headers() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(
        &up(JAR),
        vec![Reply::ok("jar bytes").header("x-checksum-sha1", &sha1_hex(b"something else"))],
    );
    assert_eq!(status(&w.get(&proxy, JAR, &user).await), 502);
    assert!(!cached(&w, &proxy, JAR).await);
}

#[tokio::test]
async fn a_checksum_file_is_fetched_as_a_file_and_cached() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    script_artifact(&w, JAR, "jar bytes");
    let response = w.get(&proxy, &format!("{JAR}.sha1"), &user).await;
    assert_eq!(status(&response), 200);
    assert_eq!(body_text(response).await, sha1_hex(b"jar bytes"));
    let again = w.get(&proxy, &format!("{JAR}.sha1"), &user).await;
    assert_eq!(header_of(again.headers(), "x-rustryx-cache"), Some("HIT"));
    assert_eq!(w.upstream.calls_for(&up(&format!("{JAR}.sha1"))), 1);
}

#[tokio::test]
async fn a_proxied_file_creates_a_component_for_browsing_and_cleanup() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    script_artifact(&w, JAR, "jar bytes");
    w.get(&proxy, JAR, &user).await;
    w.settle().await;
    let filter = ComponentFilter {
        repository_id: Some(proxy.id),
        group_id: Some("org.acme".into()),
        name: Some("lib".into()),
        ..ComponentFilter::default()
    };
    let page = ComponentRepo::list(&w.store, &filter, &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].version.as_deref(), Some("1.0"));
}

#[tokio::test]
async fn must_not_forward_an_upstream_401_or_403_to_the_client() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    for code in [401u16, 403] {
        let path = format!("org/acme/lib/1.{code}/lib-1.{code}.jar");
        w.upstream.route(&up(&path), vec![Reply::status(code)]);
        let response = w.get(&proxy, &path, &user).await;
        assert_eq!(status(&response), 502, "upstream {code}");
        assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
    }
}

#[tokio::test]
async fn an_unknown_artifact_is_a_404_and_remembered() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    assert_eq!(status(&w.get(&proxy, JAR, &user).await), 404);
    let calls = w.upstream.calls();
    assert_eq!(status(&w.get(&proxy, JAR, &user).await), 404);
    assert_eq!(w.upstream.calls(), calls, "the negative cache answers");
}

#[tokio::test]
async fn writes_to_a_proxy_are_refused_before_the_body_is_read() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let admin = w.admin(&[&proxy]);
    for method in [Method::PUT, Method::DELETE] {
        let response = w.send(&proxy, method, JAR, &admin, Body::from("x")).await;
        assert_eq!(status(&response), 405);
        assert_eq!(header_of(response.headers(), "allow"), Some("GET, HEAD"));
    }
}

#[tokio::test]
async fn range_and_head_work_on_a_cached_release() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    script_artifact(&w, JAR, "0123456789");
    w.get(&proxy, JAR, &user).await;
    let mut ranged = w.head(Method::GET, JAR, &user);
    ranged
        .headers
        .insert(header::RANGE, header_value("bytes=0-3"));
    let partial = w.send_with(&proxy, ranged, Body::empty()).await;
    assert_eq!(status(&partial), 206);
    assert_eq!(body_text(partial).await, "0123");
    let head = w.head_request(&proxy, JAR, &user).await;
    assert_eq!(status(&head), 200);
    assert_eq!(header_of(head.headers(), "content-length"), Some("10"));
}

#[tokio::test]
async fn a_head_on_a_miss_asks_the_upstream_and_caches_nothing() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(
        &up(JAR),
        vec![
            Reply::ok("")
                .header("content-length", "42")
                .header("etag", "\"abc\""),
        ],
    );
    let head = w.head_request(&proxy, JAR, &user).await;
    assert_eq!(status(&head), 200);
    assert_eq!(header_of(head.headers(), "content-length"), Some("42"));
    assert!(!cached(&w, &proxy, JAR).await);
}

#[tokio::test]
async fn a_path_outside_the_layout_is_forwarded_like_any_file() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(
        &up("archetype-catalog.xml"),
        vec![Reply::ok("<archetype-catalog/>")],
    );
    let response = w.get(&proxy, "archetype-catalog.xml", &user).await;
    assert_eq!(status(&response), 200);
    assert_eq!(body_text(response).await, "<archetype-catalog/>");
}

// ---- version policy -------------------------------------------------------------------

#[tokio::test]
async fn a_release_proxy_does_not_ask_the_upstream_for_snapshots() {
    let w = World::new();
    let proxy = w
        .proxy_with("central", serde_json::json!({"version_policy": "release"}))
        .await;
    let user = w.reader(&[&proxy]);
    for path in [
        format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar"),
        format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1.jar"),
        format!("{SNAPSHOT_DIR}/maven-metadata.xml"),
    ] {
        assert_eq!(status(&w.get(&proxy, &path, &user).await), 404, "{path}");
    }
    assert_eq!(w.upstream.calls(), 0);
}

#[tokio::test]
async fn a_snapshot_proxy_does_not_ask_the_upstream_for_releases() {
    let w = World::new();
    let proxy = w
        .proxy_with("snaps", serde_json::json!({"version_policy": "snapshot"}))
        .await;
    let user = w.reader(&[&proxy]);
    assert_eq!(status(&w.get(&proxy, JAR, &user).await), 404);
    assert_eq!(w.upstream.calls(), 0);
}

// ---- snapshots ------------------------------------------------------------------------

fn snapshot_metadata(stamp: &str, build: u32) -> String {
    format!(
        "<metadata modelVersion=\"1.1.0\"><groupId>org.acme</groupId><artifactId>lib</artifactId>\
         <version>1.0-SNAPSHOT</version><versioning><snapshot><timestamp>{stamp}</timestamp>\
         <buildNumber>{build}</buildNumber></snapshot><lastUpdated>20240102030405</lastUpdated>\
         <snapshotVersions><snapshotVersion><extension>jar</extension>\
         <value>1.0-{stamp}-{build}</value><updated>20240102030405</updated></snapshotVersion>\
         </snapshotVersions></versioning></metadata>"
    )
}

#[tokio::test]
async fn a_snapshot_pointer_resolves_through_the_metadata_and_the_build_is_cached_for_good() {
    let w = World::new();
    let proxy = w.proxy("snapshots").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(
        &up(&format!("{SNAPSHOT_DIR}/maven-metadata.xml")),
        vec![Reply::ok(snapshot_metadata("20240102.030405", 1))],
    );
    script_artifact(
        &w,
        &format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1.jar"),
        "build one",
    );

    let pointer = format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar");
    let response = w.get(&proxy, &pointer, &user).await;
    assert_eq!(status(&response), 200);
    assert_eq!(body_text(response).await, "build one");
    assert!(
        cached(
            &w,
            &proxy,
            &format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1.jar")
        )
        .await
    );
    assert!(
        !cached(&w, &proxy, &pointer).await,
        "the pointer itself is never cached"
    );

    // A new build appears at the upstream: inside the metadata TTL the pointer still names build
    // one, after it the pointer names build two. Build one stays cached.
    w.upstream.route(
        &up(&format!("{SNAPSHOT_DIR}/maven-metadata.xml")),
        vec![Reply::ok(snapshot_metadata("20240103.030405", 2))],
    );
    script_artifact(
        &w,
        &format!("{SNAPSHOT_DIR}/lib-1.0-20240103.030405-2.jar"),
        "build two",
    );
    assert_eq!(
        body_text(w.get(&proxy, &pointer, &user).await).await,
        "build one"
    );
    w.clock.advance(Duration::from_secs(601));
    assert_eq!(
        body_text(w.get(&proxy, &pointer, &user).await).await,
        "build two"
    );
    assert!(
        cached(
            &w,
            &proxy,
            &format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1.jar")
        )
        .await
    );
}

#[tokio::test]
async fn a_snapshot_pointer_checksum_is_the_checksum_of_the_resolved_build() {
    let w = World::new();
    let proxy = w.proxy("snapshots").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(
        &up(&format!("{SNAPSHOT_DIR}/maven-metadata.xml")),
        vec![Reply::ok(snapshot_metadata("20240102.030405", 1))],
    );
    script_artifact(
        &w,
        &format!("{SNAPSHOT_DIR}/lib-1.0-20240102.030405-1.jar"),
        "build one",
    );
    let response = w
        .get(
            &proxy,
            &format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar.sha1"),
            &user,
        )
        .await;
    assert_eq!(status(&response), 200);
    assert_eq!(body_text(response).await, sha1_hex(b"build one"));
}

#[tokio::test]
async fn without_version_metadata_the_pointer_is_fetched_as_a_plain_file() {
    let w = World::new();
    let proxy = w.proxy("snapshots").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(
        &up(&format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar")),
        vec![Reply::ok("legacy")],
    );
    let response = w
        .get(
            &proxy,
            &format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar"),
            &user,
        )
        .await;
    assert_eq!(status(&response), 200);
    assert_eq!(body_text(response).await, "legacy");
}

#[tokio::test]
async fn garbage_version_metadata_falls_back_to_the_plain_file() {
    let w = World::new();
    let proxy = w.proxy("snapshots").await;
    let user = w.reader(&[&proxy]);
    w.upstream.route(
        &up(&format!("{SNAPSHOT_DIR}/maven-metadata.xml")),
        vec![Reply::ok("<html>login required</html>")],
    );
    w.upstream.route(
        &up(&format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar")),
        vec![Reply::ok("legacy")],
    );
    let response = w
        .get(
            &proxy,
            &format!("{SNAPSHOT_DIR}/lib-1.0-SNAPSHOT.jar"),
            &user,
        )
        .await;
    assert_eq!(body_text(response).await, "legacy");
}

#[tokio::test]
async fn concurrent_first_requests_cause_one_upstream_fetch_of_the_file() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let user = w.reader(&[&proxy]);
    script_artifact(&w, JAR, "jar bytes");
    let (a, b, c) = tokio::join!(
        w.get(&proxy, JAR, &user),
        w.get(&proxy, JAR, &user),
        w.get(&proxy, JAR, &user)
    );
    for response in [a, b, c] {
        assert_eq!(status(&response), 200);
        assert_eq!(body_text(response).await, "jar bytes");
    }
    assert_eq!(w.upstream.calls_for(&up(JAR)), 1);
}
