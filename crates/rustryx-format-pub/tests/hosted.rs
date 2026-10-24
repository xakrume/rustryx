//! Contract tests of the hosted repository, through `FormatEngine` over the in-memory ports:
//! the three step publication, the documents, and every refusal.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::large_futures,
    clippy::duration_suboptimal_units,
    reason = "test scenarios read best as straight-line code"
)]

mod support;

use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum_core::body::Body;
use http::{Method, StatusCode};
use rustryx_core::model::{NewComponent, RepoFormat, WritePolicy};
use rustryx_core::ports::ComponentRepo;
use rustryx_format_pub::PubConfig;
use rustryx_format_pub::testing::{
    BOUNDARY, Tar, gzip, multipart, package_archive, pubspec_text, sha256_hex,
};
use support::{
    Answer, BASE, PUB_JSON, World, anonymous, endless_body, path_below, principal, tracked_body,
};

async fn world_with_alice() -> (
    World,
    Arc<rustryx_core::model::Repository>,
    rustryx_core::identity::Principal,
) {
    let world = World::new();
    let repo = world.hosted("pub-hosted").await;
    world.rbac.grant_all("alice", "pub-hosted");
    (world, repo, principal("alice"))
}

fn archive_url(repo: &str, name: &str, version: &str) -> String {
    format!("{BASE}/repository/{repo}/packages/{name}/versions/{version}.tar.gz")
}

async fn assert_nothing_published(world: &World, repo: &rustryx_core::model::Repository) {
    assert_eq!(
        world.asset_paths(repo, "packages/").await,
        Vec::<String>::new()
    );
    assert_eq!(
        world.asset_paths(repo, "api/packages/").await,
        Vec::<String>::new()
    );
}

// ---------------------------------------------------------------------------------------------
// The good path
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_published_package_is_listed_and_downloadable() {
    let (world, repo, alice) = world_with_alice().await;
    let archive = world.publish_ok(&repo, &alice, "demo", "1.0.0").await;

    let list = world.list(&repo, "demo", &alice).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.header("content-type"), Some(PUB_JSON));
    let json = list.json();
    assert_eq!(json["name"], "demo");
    assert_eq!(json["latest"]["version"], "1.0.0");
    let entry = &json["versions"][0];
    assert_eq!(entry["version"], "1.0.0");
    assert_eq!(entry["retracted"], false);
    assert_eq!(
        entry["archive_url"],
        archive_url("pub-hosted", "demo", "1.0.0")
    );
    assert_eq!(entry["archive_sha256"], sha256_hex(&archive));
    assert_eq!(entry["pubspec"]["name"], "demo");
    assert_eq!(entry["pubspec"]["environment"]["sdk"], "^3.0.0");
    for forbidden in [
        "advisoriesUpdated",
        "attestationUrl",
        "isDiscontinued",
        "replacedBy",
    ] {
        assert!(
            !json.to_string().contains(forbidden),
            "{forbidden} must be omitted in phase 1"
        );
    }

    // The archive is served from the archive_url, byte for byte.
    let download = world
        .get(
            &repo,
            &path_below(&repo, entry["archive_url"].as_str().unwrap()),
            &alice,
        )
        .await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.body.as_ref(), archive.as_slice());
    assert_eq!(
        download.header("content-length"),
        Some(archive.len().to_string().as_str())
    );
    assert_eq!(
        download.header("x-checksum-sha256"),
        Some(sha256_hex(&archive).as_str())
    );

    // The deprecated single version document.
    let single = world
        .get(&repo, "api/packages/demo/versions/1.0.0", &alice)
        .await;
    assert_eq!(single.status, StatusCode::OK);
    assert_eq!(single.header("content-type"), Some(PUB_JSON));
    let single = single.json();
    assert_eq!(single["version"], "1.0.0");
    assert_eq!(single["archive_url"], entry["archive_url"]);
    assert_eq!(single["pubspec"]["name"], "demo");

    // The component exists: it is what a hosted member claims a package name with.
    let components: &dyn ComponentRepo = &world.store;
    let mut coordinate = NewComponent::new(repo.id, RepoFormat::Pub, "demo");
    coordinate.version = Some("1.0.0".into());
    assert!(components.find(&coordinate).await.unwrap().is_some());
    // Nothing is left staged.
    assert_eq!(world.staged(&repo).await, Vec::<String>::new());
}

#[tokio::test]
async fn versions_are_listed_in_order_and_latest_skips_prereleases_and_retractions() {
    let (world, repo, alice) = world_with_alice().await;
    for version in ["2.0.0-beta.1", "1.0.0", "1.5.0"] {
        world.publish_ok(&repo, &alice, "demo", version).await;
    }
    let json = world.list(&repo, "demo", &alice).await.json();
    let versions: Vec<&str> = json["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["1.0.0", "1.5.0", "2.0.0-beta.1"]);
    assert_eq!(json["latest"]["version"], "1.5.0");
}

#[tokio::test]
async fn head_and_conditional_requests_work_on_documents() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    let get = world.list(&repo, "demo", &alice).await;
    let etag = get.header("etag").unwrap().to_owned();
    let head = world
        .send(
            &repo,
            Method::HEAD,
            "api/packages/demo",
            &alice,
            Body::empty(),
        )
        .await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        Some(get.body.len().to_string().as_str())
    );
    let mut conditional = world.head(Method::GET, "api/packages/demo", &alice);
    conditional
        .headers
        .insert(http::header::IF_NONE_MATCH, etag.parse().unwrap());
    let not_modified = support::answer(world.serve(&repo, conditional, Body::empty()).await).await;
    assert_eq!(not_modified.status, StatusCode::NOT_MODIFIED);
    assert!(not_modified.body.is_empty());
}

#[tokio::test]
async fn a_large_archive_is_stored_and_served_intact() {
    let (world, repo, alice) = world_with_alice().await;
    // Incompressible content, so the stored archive really is several megabytes.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let noise: Vec<u8> = (0..3 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            u8::try_from((state >> 24) & 0xff).unwrap_or(0)
        })
        .collect();
    let archive = Tar::new()
        .file("pubspec.yaml", pubspec_text("big", "1.0.0").as_bytes())
        .file("assets/noise.bin", &noise)
        .finish_gz();
    assert!(archive.len() > 3_000_000);
    let done = world.publish(&repo, &alice, &archive).await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&done.body)
    );
    let list = world.list(&repo, "big", &alice).await.json();
    assert_eq!(list["versions"][0]["archive_sha256"], sha256_hex(&archive));
    let download = world
        .get(&repo, "packages/big/versions/1.0.0.tar.gz", &alice)
        .await;
    assert_eq!(download.body.len(), archive.len());
    assert_eq!(sha256_hex(&download.body), sha256_hex(&archive));
}

#[tokio::test]
async fn the_publication_is_announced_and_audited_once_per_path() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    let published = world.events.published_paths();
    assert!(
        published.contains(&"packages/demo/versions/1.0.0.tar.gz".to_owned()),
        "{published:?}"
    );
    assert!(
        published.contains(&"api/packages/demo/versions/1.0.0".to_owned()),
        "{published:?}"
    );
    let audited: Vec<String> = world
        .audit
        .events()
        .into_iter()
        .filter(|e| e.action == "asset.publish")
        .filter_map(|e| e.target)
        .collect();
    assert!(
        audited.contains(&"pub-hosted:packages/demo/versions/1.0.0.tar.gz".to_owned()),
        "{audited:?}"
    );
}

#[tokio::test]
async fn deleting_the_component_removes_the_version_everywhere() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    world.publish_ok(&repo, &alice, "demo", "2.0.0").await;
    let components: &dyn ComponentRepo = &world.store;
    let mut coordinate = NewComponent::new(repo.id, RepoFormat::Pub, "demo");
    coordinate.version = Some("1.0.0".into());
    let component = components.find(&coordinate).await.unwrap().unwrap();
    components.delete(component.id).await.unwrap();
    let list = world.list(&repo, "demo", &alice).await.json();
    assert_eq!(list["versions"].as_array().unwrap().len(), 1);
    assert_eq!(list["versions"][0]["version"], "2.0.0");
    world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &alice)
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------------------------
// Immutability
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn must_not_publish_a_version_twice() {
    let (world, repo, alice) = world_with_alice().await;
    let original = world.publish_ok(&repo, &alice, "demo", "1.0.0").await;

    // The same bytes again, and different bytes under the same coordinate.
    let different = Tar::new()
        .file("pubspec.yaml", pubspec_text("demo", "1.0.0").as_bytes())
        .file("lib/changed.dart", b"// changed")
        .finish_gz();
    for archive in [original.clone(), different] {
        let again = world.publish(&repo, &alice, &archive).await;
        again.assert_pub_error(StatusCode::CONFLICT);
        assert_eq!(again.error_code(), "conflict");
        assert!(
            again.error_message().contains("already exists"),
            "{}",
            again.error_message()
        );
    }

    // What was published is what is still served.
    let list = world.list(&repo, "demo", &alice).await.json();
    assert_eq!(list["versions"].as_array().unwrap().len(), 1);
    assert_eq!(list["versions"][0]["archive_sha256"], sha256_hex(&original));
    let download = world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &alice)
        .await;
    assert_eq!(download.body.as_ref(), original.as_slice());
    assert_eq!(
        world.staged(&repo).await,
        Vec::<String>::new(),
        "a refused upload is not kept"
    );
}

#[tokio::test]
async fn must_not_let_two_concurrent_publications_of_one_version_both_win() {
    let (world, repo, alice) = world_with_alice().await;
    let first = package_archive("race", "1.0.0");
    let second = Tar::new()
        .file("pubspec.yaml", pubspec_text("race", "1.0.0").as_bytes())
        .file("lib/other.dart", b"// the other one")
        .finish_gz();
    // Both uploads are staged before either is finalized.
    let mut staged = Vec::new();
    for archive in [&first, &second] {
        let (_, path) = world.begin(&repo, &alice, None).await;
        staged.push(world.upload(&repo, &alice, &path.unwrap(), archive).await);
    }
    let (a, b) = tokio::join!(
        world.finish(&repo, &alice, &staged[0]),
        world.finish(&repo, &alice, &staged[1])
    );
    let mut statuses = [a.status, b.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let list = world.list(&repo, "race", &alice).await.json();
    assert_eq!(list["versions"].as_array().unwrap().len(), 1);
    let winner = if a.status == StatusCode::OK {
        &first
    } else {
        &second
    };
    assert_eq!(list["versions"][0]["archive_sha256"], sha256_hex(winner));
    assert_eq!(world.staged(&repo).await, Vec::<String>::new());
}

#[tokio::test]
async fn the_write_policy_allow_once_still_publishes_new_versions() {
    let world = World::new();
    let repo = world
        .hosted_with("pub-once", |r| r.write_policy = WritePolicy::AllowOnce)
        .await;
    world.rbac.grant_all("alice", "pub-once");
    let alice = principal("alice");
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    world.publish_ok(&repo, &alice, "demo", "1.0.1").await;
    world
        .publish(&repo, &alice, &package_archive("demo", "1.0.0"))
        .await
        .assert_pub_error(StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_repository_that_takes_no_writes_refuses_the_publication_at_its_start() {
    let world = World::new();
    let repo = world
        .hosted_with("pub-frozen", |r| r.write_policy = WritePolicy::Deny)
        .await;
    world.rbac.grant_all("alice", "pub-frozen");
    let (begun, path) = world.begin(&repo, &principal("alice"), None).await;
    assert!(path.is_none());
    begun.assert_pub_error(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn read_only_mode_refuses_an_upload_with_retry_after() {
    let (world, repo, alice) = world_with_alice().await;
    world.store.set_read_only(true, "backup").unwrap();
    let (_, path) = world.begin(&repo, &alice, None).await;
    let refused = world
        .upload(
            &repo,
            &alice,
            &path.unwrap(),
            &package_archive("demo", "1.0.0"),
        )
        .await;
    refused.assert_pub_error(StatusCode::SERVICE_UNAVAILABLE);
    assert!(refused.header("retry-after").is_some());
    world.store.set_read_only(false, "").unwrap();
    assert_nothing_published(&world, &repo).await;
    assert_eq!(world.staged(&repo).await, Vec::<String>::new());
}

// ---------------------------------------------------------------------------------------------
// The publish coordinate
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_declared_coordinate_is_checked_against_the_archive() {
    let (world, repo, alice) = world_with_alice().await;
    let (begun, path) = world
        .begin(&repo, &alice, Some("package=demo&version=1.0.0"))
        .await;
    let path = path.unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(&begun.body)));
    assert!(path.ends_with("/demo/1.0.0"), "{path}");
    // The authorizer saw the coordinate of the request before any body was read.
    assert!(
        world
            .rbac
            .checks()
            .iter()
            .any(|c| c.path == "api/packages/versions/new"
                && c.name.as_deref() == Some("demo")
                && c.version.as_deref() == Some("1.0.0"))
    );
    let uploaded = world
        .upload(&repo, &alice, &path, &package_archive("demo", "1.0.0"))
        .await;
    assert_eq!(uploaded.status, StatusCode::NO_CONTENT);
    assert!(
        uploaded
            .header("location")
            .unwrap()
            .ends_with("/demo/1.0.0")
    );
    assert_eq!(
        world.finish(&repo, &alice, &uploaded).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn must_not_publish_an_archive_whose_pubspec_differs_from_the_declared_coordinate() {
    let (world, repo, alice) = world_with_alice().await;
    for (archive, label) in [
        (package_archive("other", "1.0.0"), "name"),
        (package_archive("demo", "1.0.1"), "version"),
        (package_archive("other", "9.9.9"), "both"),
    ] {
        let (_, path) = world
            .begin(&repo, &alice, Some("package=demo&version=1.0.0"))
            .await;
        let uploaded = world.upload(&repo, &alice, &path.unwrap(), &archive).await;
        let done = world.finish(&repo, &alice, &uploaded).await;
        done.assert_pub_error(StatusCode::BAD_REQUEST);
        assert!(
            done.error_message().contains("declared"),
            "{label}: {}",
            done.error_message()
        );
        assert_nothing_published(&world, &repo).await;
        assert_eq!(world.staged(&repo).await, Vec::<String>::new(), "{label}");
    }
}

#[tokio::test]
async fn must_not_accept_a_declaration_that_is_half_given_or_invalid() {
    let (world, repo, alice) = world_with_alice().await;
    for query in [
        "package=demo",
        "version=1.0.0",
        "package=Demo&version=1.0.0",
        "package=demo&version=1.0",
    ] {
        let (answer, path) = world.begin(&repo, &alice, Some(query)).await;
        assert!(path.is_none(), "{query}");
        answer.assert_pub_error(StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn the_coordinate_of_the_archive_is_authorized_again_when_the_archive_has_been_read() {
    let (world, repo, alice) = world_with_alice().await;
    world.rbac.deny_package("secret");
    let done = world
        .publish(&repo, &alice, &package_archive("secret", "1.0.0"))
        .await;
    done.assert_pub_error(StatusCode::FORBIDDEN);
    assert_nothing_published(&world, &repo).await;
    assert_eq!(world.staged(&repo).await, Vec::<String>::new());
    let finalize_check = world
        .rbac
        .checks()
        .into_iter()
        .find(|c| c.path == "packages/secret/versions/1.0.0.tar.gz")
        .expect("the coordinate read from the archive was authorized");
    assert_eq!(finalize_check.name.as_deref(), Some("secret"));
    assert_eq!(finalize_check.version.as_deref(), Some("1.0.0"));
    assert_eq!(finalize_check.action, rustryx_core::identity::Action::Write);
    // Another package is unaffected.
    world.publish_ok(&repo, &alice, "fine", "1.0.0").await;
}

// ---------------------------------------------------------------------------------------------
// Hostile archives
// ---------------------------------------------------------------------------------------------

async fn publish_hostile(
    world: &World,
    repo: &Arc<rustryx_core::model::Repository>,
    archive: &[u8],
) -> Answer {
    let done = world.publish(repo, &principal("alice"), archive).await;
    assert_nothing_published(world, repo).await;
    assert_eq!(
        world.staged(repo).await,
        Vec::<String>::new(),
        "a refused archive is not kept"
    );
    done
}

#[tokio::test]
async fn must_not_publish_a_tar_slip_archive() {
    let world = World::new();
    let repo = world.hosted("pub-hosted").await;
    world.rbac.grant_all("alice", "pub-hosted");
    let manifest = pubspec_text("evil", "1.0.0");
    for (label, archive) in [
        (
            "parent directory",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .entry(b"../../etc/cron.d/x", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "absolute path",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .entry(b"/etc/passwd", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "backslash",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .entry(b"..\\..\\x", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "pax path",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .pax(&[("path", "../../evil")])
                .entry(b"ok", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "symlink out",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .entry(b"link", b'2', b"", b"/etc")
                .finish_gz(),
        ),
        (
            "hard link",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .entry(b"link", b'1', b"", b"pubspec.yaml")
                .finish_gz(),
        ),
    ] {
        let done = publish_hostile(&world, &repo, &archive).await;
        done.assert_pub_error(StatusCode::BAD_REQUEST);
        assert!(
            !done.error_message().contains("etc"),
            "{label}: the message must not echo the path"
        );
        assert!(
            done.error_message().contains("unsafe")
                || done.error_message().contains("not a regular file"),
            "{label}: {}",
            done.error_message()
        );
    }
}

#[tokio::test]
async fn must_not_publish_a_gzip_bomb() {
    let world = World::new();
    let repo = world.hosted("pub-hosted").await;
    world.rbac.grant_all("alice", "pub-hosted");
    // 96 MiB of zeros in a few tens of kilobytes.
    let bomb = Tar::new()
        .file("pubspec.yaml", pubspec_text("bomb", "1.0.0").as_bytes())
        .file("assets/zeros.bin", &vec![0u8; 96 * 1024 * 1024])
        .finish_gz();
    assert!(bomb.len() < 400_000, "not a bomb: {}", bomb.len());
    let started = std::time::Instant::now();
    let done = publish_hostile(&world, &repo, &bomb).await;
    done.assert_pub_error(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        done.error_message().contains("expands too much"),
        "{}",
        done.error_message()
    );
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[tokio::test]
async fn must_not_publish_something_that_is_not_a_package() {
    let world = World::new();
    let repo = world.hosted("pub-hosted").await;
    world.rbac.grant_all("alice", "pub-hosted");
    let manifest = pubspec_text("odd", "1.0.0");
    let good = package_archive("odd", "1.0.0");
    for (label, archive) in [
        ("not gzip", b"this is not an archive".to_vec()),
        ("empty", Vec::new()),
        ("gzip of text", gzip(b"just text, not a tar")),
        ("truncated", good[..good.len() / 2].to_vec()),
        (
            "no manifest",
            Tar::new().file("lib/a.dart", b"x").finish_gz(),
        ),
        (
            "nested manifest",
            Tar::new()
                .file("odd/pubspec.yaml", manifest.as_bytes())
                .finish_gz(),
        ),
        (
            "two manifests",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .file("pubspec.yaml", manifest.as_bytes())
                .finish_gz(),
        ),
        (
            "manifest look-alike",
            Tar::new()
                .file("pubspec.yaml", manifest.as_bytes())
                .file("PUBSPEC.yaml", b"name: x")
                .finish_gz(),
        ),
        (
            "bad name",
            Tar::new()
                .file("pubspec.yaml", pubspec_text("Bad-Name", "1.0.0").as_bytes())
                .finish_gz(),
        ),
        (
            "bad version",
            Tar::new()
                .file("pubspec.yaml", pubspec_text("odd", "1.0").as_bytes())
                .finish_gz(),
        ),
        (
            "not yaml",
            Tar::new()
                .file("pubspec.yaml", b"name: [unclosed")
                .finish_gz(),
        ),
        ("trailing data", [good.clone(), good.clone()].concat()),
    ] {
        let done = publish_hostile(&world, &repo, &archive).await;
        assert_eq!(
            done.status,
            StatusCode::BAD_REQUEST,
            "{label}: {}",
            String::from_utf8_lossy(&done.body)
        );
        done.assert_pub_error(StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn must_not_expand_an_alias_bomb_in_the_manifest() {
    let world = World::new();
    let repo = world.hosted("pub-hosted").await;
    world.rbac.grant_all("alice", "pub-hosted");
    let mut manifest =
        String::from("name: lolz\nversion: 1.0.0\nl0: &l0 [x, x, x, x, x, x, x, x, x, x]\n");
    for level in 1..=9 {
        let refs = vec![format!("*l{}", level - 1); 10].join(", ");
        let _ = writeln!(manifest, "l{level}: &l{level} [{refs}]");
    }
    let archive = Tar::new()
        .file("pubspec.yaml", manifest.as_bytes())
        .finish_gz();
    let done = publish_hostile(&world, &repo, &archive).await;
    done.assert_pub_error(StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------------------------------------------
// The upload step
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn must_not_accept_an_upload_that_is_not_a_single_file_part() {
    let (world, repo, alice) = world_with_alice().await;
    let archive = package_archive("demo", "1.0.0");

    // Not multipart at all.
    let (_, path) = world.begin(&repo, &alice, None).await;
    let path = path.unwrap();
    let mut head = world.head(Method::POST, &path, &alice);
    head.headers.insert(
        http::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    let answer = support::answer(world.serve(&repo, head, Body::from(archive.clone())).await).await;
    answer.assert_pub_error(StatusCode::BAD_REQUEST);

    // Multipart without the file.
    let only_field = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhi\r\n--{BOUNDARY}--\r\n"
    );
    let answer = world
        .upload_body(
            &repo,
            &alice,
            &path,
            only_field.len(),
            Body::from(only_field),
        )
        .await;
    answer.assert_pub_error(StatusCode::BAD_REQUEST);

    // Two files.
    let mut two = multipart(&[], &archive);
    let closing = format!("--{BOUNDARY}--\r\n");
    two.truncate(two.len() - closing.len());
    two.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"b.tar.gz\"\r\n\r\nsecond\r\n--{BOUNDARY}--\r\n"
        )
        .as_bytes(),
    );
    let answer = world
        .upload_body(&repo, &alice, &path, two.len(), Body::from(two))
        .await;
    answer.assert_pub_error(StatusCode::BAD_REQUEST);
    assert_eq!(
        world.staged(&repo).await,
        Vec::<String>::new(),
        "a refused upload is not kept"
    );

    // A body that ends in the middle.
    let mut broken = multipart(&[], &archive);
    broken.truncate(broken.len() / 2);
    let answer = world
        .upload_body(&repo, &alice, &path, broken.len(), Body::from(broken))
        .await;
    answer.assert_pub_error(StatusCode::BAD_REQUEST);
    assert_eq!(world.staged(&repo).await, Vec::<String>::new());
}

#[tokio::test]
async fn must_not_accept_a_second_upload_to_the_same_id() {
    let (world, repo, alice) = world_with_alice().await;
    let (_, path) = world.begin(&repo, &alice, None).await;
    let path = path.unwrap();
    let first = world
        .upload(&repo, &alice, &path, &package_archive("demo", "1.0.0"))
        .await;
    assert_eq!(first.status, StatusCode::NO_CONTENT);
    let (polled, body) = tracked_body(b"second upload");
    let second = world.upload_body(&repo, &alice, &path, 13, body).await;
    second.assert_pub_error(StatusCode::CONFLICT);
    assert!(
        !polled.load(Ordering::SeqCst),
        "the body of a refused upload must not be read"
    );
    // The first one is untouched and can be finished.
    assert_eq!(
        world.finish(&repo, &alice, &first).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn must_not_read_the_body_of_an_upload_that_declares_too_much() {
    let world = World::with_config(PubConfig {
        max_upload_bytes: 4096,
        ..PubConfig::default()
    });
    let repo = world.hosted("pub-hosted").await;
    world.rbac.grant_all("alice", "pub-hosted");
    let alice = principal("alice");
    let (_, path) = world.begin(&repo, &alice, None).await;
    let (polled, body) = tracked_body(b"never read");
    let refused = world
        .upload_body(&repo, &alice, &path.unwrap(), 10_000_000, body)
        .await;
    refused.assert_pub_error(StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn must_not_store_more_than_the_upload_limit_whatever_the_declared_length_says() {
    let world = World::with_config(PubConfig {
        max_upload_bytes: 64 * 1024,
        ..PubConfig::default()
    });
    let repo = world.hosted("pub-hosted").await;
    world.rbac.grant_all("alice", "pub-hosted");
    let alice = principal("alice");
    let (_, path) = world.begin(&repo, &alice, None).await;
    // The framing of a real upload, then an endless file, with a length that lies.
    let prefix = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"package.tar.gz\"\r\n\r\n"
    )
    .into_bytes();
    let pulled = Arc::new(AtomicU64::new(0));
    let refused = world
        .upload_body(
            &repo,
            &alice,
            &path.unwrap(),
            1000,
            endless_body(prefix, Arc::clone(&pulled)),
        )
        .await;
    refused.assert_pub_error(StatusCode::PAYLOAD_TOO_LARGE);
    let pulled = pulled.load(Ordering::SeqCst);
    assert!(
        pulled < 256 * 1024,
        "{pulled} bytes were read from an endless body"
    );
    assert_eq!(
        world.staged(&repo).await,
        Vec::<String>::new(),
        "nothing of the oversized upload is kept"
    );
    assert_eq!(world.blobs.len(), 0, "no blob object is left");
    assert_eq!(world.blobs.staging_count(), 0, "no staged object is left");
}

// ---------------------------------------------------------------------------------------------
// The finalize step
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn must_not_let_another_writer_finalize_an_upload() {
    let (world, repo, alice) = world_with_alice().await;
    world.rbac.grant_all("mallory", "pub-hosted");
    let mallory = principal("mallory");
    let (_, path) = world.begin(&repo, &alice, None).await;
    let uploaded = world
        .upload(
            &repo,
            &alice,
            &path.unwrap(),
            &package_archive("demo", "1.0.0"),
        )
        .await;
    let stolen = world.finish(&repo, &mallory, &uploaded).await;
    stolen.assert_pub_error(StatusCode::FORBIDDEN);
    // The refusal says nothing about the upload and does not remove it: alice can finish.
    assert_eq!(world.staged(&repo).await.len(), 1);
    assert_eq!(
        world.finish(&repo, &alice, &uploaded).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_unknown_or_finished_upload_is_not_found() {
    let (world, repo, alice) = world_with_alice().await;
    let (_, path) = world.begin(&repo, &alice, None).await;
    let uploaded = world
        .upload(
            &repo,
            &alice,
            &path.unwrap(),
            &package_archive("demo", "1.0.0"),
        )
        .await;
    assert_eq!(
        world.finish(&repo, &alice, &uploaded).await.status,
        StatusCode::OK
    );
    // Finishing it again: the staging copy is gone.
    world
        .finish(&repo, &alice, &uploaded)
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    // An id nobody started.
    let unknown = world
        .get(
            &repo,
            "api/packages/versions/finish/0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10",
            &alice,
        )
        .await;
    unknown.assert_pub_error(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_finalize_that_loses_its_client_still_completes() {
    let (world, repo, alice) = world_with_alice().await;
    let (_, path) = world.begin(&repo, &alice, None).await;
    let uploaded = world
        .upload(
            &repo,
            &alice,
            &path.unwrap(),
            &package_archive("demo", "1.0.0"),
        )
        .await;
    let location = path_below(&repo, uploaded.header("location").unwrap());
    // The client goes away right after the request started: the future is dropped.
    let head = world.head(Method::GET, &location, &alice);
    let call = world.serve(&repo, head, Body::empty());
    let _ = tokio::time::timeout(Duration::ZERO, call).await;
    world.settle().await;
    let list = world.list(&repo, "demo", &alice).await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "the publication was cut in half"
    );
    assert_eq!(world.staged(&repo).await, Vec::<String>::new());
}

#[tokio::test]
async fn staged_uploads_that_were_never_finished_are_reaped_when_a_new_one_starts() {
    let (world, repo, alice) = world_with_alice().await;
    let (_, old) = world.begin(&repo, &alice, None).await;
    world
        .upload(
            &repo,
            &alice,
            &old.unwrap(),
            &package_archive("old", "1.0.0"),
        )
        .await;
    world.clock.advance(Duration::from_secs(25 * 3600));
    let (_, fresh) = world.begin(&repo, &alice, None).await;
    let fresh = fresh.unwrap();
    world
        .upload(&repo, &alice, &fresh, &package_archive("fresh", "1.0.0"))
        .await;
    world.settle().await;
    // The start of the next publication reaps what is older than a day, and only that.
    let (_, _) = world.begin(&repo, &alice, None).await;
    world.settle().await;
    assert_eq!(world.staged(&repo).await, [fresh]);
}

// ---------------------------------------------------------------------------------------------
// Authentication and authorization: the 401 and 403 rules
// ---------------------------------------------------------------------------------------------

const UPLOAD_ID: &str = "0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10";

fn every_route() -> Vec<(Method, String)> {
    vec![
        (Method::GET, "api/packages/demo".into()),
        (Method::HEAD, "api/packages/demo".into()),
        (Method::GET, "api/packages/demo/versions/1.0.0".into()),
        (Method::GET, "api/packages/demo/advisories".into()),
        (Method::GET, "packages/demo/versions/1.0.0.tar.gz".into()),
        (Method::GET, "api/packages/versions/new".into()),
        (
            Method::POST,
            format!("api/packages/versions/upload/{UPLOAD_ID}"),
        ),
        (
            Method::GET,
            format!("api/packages/versions/finish/{UPLOAD_ID}"),
        ),
    ]
}

#[tokio::test]
async fn an_anonymous_caller_is_told_to_authenticate_on_every_route() {
    let (world, repo, _) = world_with_alice().await;
    for (method, path) in every_route() {
        let answer = world
            .send(&repo, method.clone(), &path, &anonymous(), Body::empty())
            .await;
        if method == Method::HEAD {
            assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{method} {path}");
            continue;
        }
        answer.assert_pub_error(StatusCode::UNAUTHORIZED);
        assert!(
            answer.error_message().contains("dart pub token add"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn must_not_answer_401_to_a_valid_token_that_lacks_the_right() {
    let (world, repo, _) = world_with_alice().await;
    // Bob has a valid token and no rights on the repository: it is hidden from him, and the
    // answer is 403, never 404 and never 401 (the client would erase a working token on 401).
    let bob = principal("bob");
    for (method, path) in every_route() {
        let answer = world
            .send(&repo, method.clone(), &path, &bob, Body::empty())
            .await;
        if method == Method::HEAD {
            assert_eq!(answer.status, StatusCode::FORBIDDEN, "{method} {path}");
            continue;
        }
        answer.assert_pub_error(StatusCode::FORBIDDEN);
        assert_eq!(answer.error_code(), "forbidden", "{path}");
    }
    // Read rights without write rights: the publish steps are 403 too.
    world.rbac.grant_read("bob", "pub-hosted");
    for path in [
        "api/packages/versions/new".to_owned(),
        format!("api/packages/versions/finish/{UPLOAD_ID}"),
    ] {
        world
            .get(&repo, &path, &bob)
            .await
            .assert_pub_error(StatusCode::FORBIDDEN);
    }
    world
        .send(
            &repo,
            Method::POST,
            &format!("api/packages/versions/upload/{UPLOAD_ID}"),
            &bob,
            Body::empty(),
        )
        .await
        .assert_pub_error(StatusCode::FORBIDDEN);
    // And a publication by a reader leaves nothing behind.
    world
        .publish(&repo, &bob, &package_archive("demo", "1.0.0"))
        .await
        .assert_pub_error(StatusCode::FORBIDDEN);
    assert_nothing_published(&world, &repo).await;
}

#[tokio::test]
async fn a_failing_authorization_backend_is_403_not_401_and_never_an_allow() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    world.rbac.set_failing(true);
    world
        .list(&repo, "demo", &alice)
        .await
        .assert_pub_error(StatusCode::FORBIDDEN);
    world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &alice)
        .await
        .assert_pub_error(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn anonymous_reads_need_the_repository_flag_and_anonymous_writes_never_work() {
    let world = World::new();
    let repo = world
        .hosted_with("pub-open", |r| r.allow_anonymous = true)
        .await;
    world.rbac.grant_all("alice", "pub-open");
    world.rbac.grant_read("anonymous", "pub-open");
    let alice = principal("alice");
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    let read = world.list(&repo, "demo", &anonymous()).await;
    assert_eq!(read.status, StatusCode::OK);
    // Writing as nobody: told to authenticate.
    world
        .get(&repo, "api/packages/versions/new", &anonymous())
        .await
        .assert_pub_error(StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------------------------
// Protocol details
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn accept_is_negotiated_and_a_missing_header_means_version_2() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    let plain = world.get(&repo, "api/packages/demo", &alice).await;
    assert_eq!(plain.status, StatusCode::OK);
    assert_eq!(plain.header("content-type"), Some(PUB_JSON));
    for accept in [
        "application/vnd.pub.v2+json",
        "*/*",
        "application/json",
        "text/html, */*;q=0.8",
    ] {
        let answer = world
            .get_accepting(&repo, "api/packages/demo", &alice, accept)
            .await;
        assert_eq!(answer.status, StatusCode::OK, "{accept}");
    }
    let future = world
        .get_accepting(
            &repo,
            "api/packages/demo",
            &alice,
            "application/vnd.pub.v3+json",
        )
        .await;
    future.assert_pub_error(StatusCode::NOT_ACCEPTABLE);
    assert_eq!(future.error_code(), "not_acceptable");
    // The archive is a file; it has no version of the protocol to negotiate.
    let archive = world
        .get_accepting(
            &repo,
            "packages/demo/versions/1.0.0.tar.gz",
            &alice,
            "application/octet-stream",
        )
        .await;
    assert_eq!(archive.status, StatusCode::OK);
}

#[tokio::test]
async fn advisories_are_not_part_of_phase_1_and_are_never_faked() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    let answer = world
        .get(&repo, "api/packages/demo/advisories", &alice)
        .await;
    answer.assert_pub_error(StatusCode::NOT_FOUND);
    // Not an empty list that would say "no known vulnerabilities".
    assert!(!answer.json().to_string().contains("advisories\":"));
    let list = world.list(&repo, "demo", &alice).await;
    assert!(!list.json().to_string().contains("advisoriesUpdated"));
}

#[tokio::test]
async fn unknown_packages_and_versions_are_pub_errors() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    for path in [
        "api/packages/nothing",
        "api/packages/demo/versions/9.9.9",
        "packages/demo/versions/9.9.9.tar.gz",
        "packages/nothing/versions/1.0.0.tar.gz",
        "api/unknown",
        "something/else",
    ] {
        world
            .get(&repo, path, &alice)
            .await
            .assert_pub_error(StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn must_not_change_published_content_through_other_methods() {
    let (world, repo, alice) = world_with_alice().await;
    let original = world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    for method in [Method::PUT, Method::DELETE, Method::PATCH, Method::POST] {
        for path in [
            "packages/demo/versions/1.0.0.tar.gz",
            "api/packages/demo",
            "api/packages/demo/versions/1.0.0",
        ] {
            let answer = world
                .send(&repo, method.clone(), path, &alice, Body::from("x"))
                .await;
            answer.assert_pub_error(StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(answer.header("allow"), Some("GET, HEAD"));
        }
    }
    let download = world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &alice)
        .await;
    assert_eq!(download.body.as_ref(), original.as_slice());
}

#[tokio::test]
async fn must_not_let_a_path_trick_reach_storage_or_the_authorizer() {
    let (world, repo, alice) = world_with_alice().await;
    world.publish_ok(&repo, &alice, "demo", "1.0.0").await;
    let before = world.rbac.checks().len();
    for path in [
        "api/packages/../../etc/passwd",
        "api/packages/%2e%2e/%2e%2e/etc/passwd",
        "packages/demo/versions/..%2f..%2fx.tar.gz",
        "packages/demo/versions/%2e%2e.tar.gz",
        "packages/demo/versions/1.0.0.tar.gz/../../../x",
        "packages/demo%2fversions/1.0.0.tar.gz",
        "api/packages/demo%252fversions",
        "packages//demo/versions/1.0.0.tar.gz",
        "packages\\demo\\versions\\1.0.0.tar.gz",
        "api/packages/demo/versions/1.0.0/",
        "api/packages/demo\u{0}",
    ] {
        let answer = world.get(&repo, path, &alice).await;
        answer.assert_pub_error(StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        world.rbac.checks().len(),
        before,
        "the authorizer must never see a path that did not normalise"
    );
}

#[tokio::test]
async fn archive_urls_stay_under_the_hosted_url_prefix() {
    let (world, repo, alice) = world_with_alice().await;
    for version in ["1.0.0", "1.1.0", "2.0.0-rc.1+3"] {
        world.publish_ok(&repo, &alice, "demo", version).await;
    }
    let prefix = format!("{BASE}/repository/pub-hosted/");
    let list = world.list(&repo, "demo", &alice).await.json();
    let single = world
        .get(&repo, "api/packages/demo/versions/1.1.0", &alice)
        .await
        .json();
    let mut urls: Vec<String> = list["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["archive_url"].as_str().unwrap().to_owned())
        .collect();
    urls.push(list["latest"]["archive_url"].as_str().unwrap().to_owned());
    urls.push(single["archive_url"].as_str().unwrap().to_owned());
    for url in &urls {
        assert!(url.starts_with(&prefix), "{url} is outside {prefix}");
        // The client attaches `Authorization` only below the prefix; no query, no fragment.
        assert!(!url.contains('?') && !url.contains('#'), "{url}");
    }
    // The upload and finalize URLs obey the same rule.
    let (_, upload) = world.begin(&repo, &alice, None).await;
    assert!(upload.is_some(), "path_below asserts the prefix");
    let uploaded = world
        .upload(
            &repo,
            &alice,
            &upload.unwrap(),
            &package_archive("later", "1.0.0"),
        )
        .await;
    assert!(uploaded.header("location").unwrap().starts_with(&prefix));
}
