//! Contract tests of a hosted npm repository: the real format engine, the real authorizer and the
//! in-memory ports behind an HTTP listener (`rustryx-testserver`). No external tool is needed;
//! the real npm client is exercised in `real_clients.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::large_futures,
    reason = "tests: a panic is the failure signal and scenarios read best as straight-line code"
)]

mod support;

use rustryx_testserver::reqwest::Method;
use serde_json::{Value, json};
use support::{
    Publish, bytes_of, component_names, fixture, fixture_with, sha1_hex, sri256, sri512,
    tar_with_traversal_name,
};

async fn body_text(response: rustryx_testserver::reqwest::Response) -> String {
    response.text().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_then_read_the_document_and_the_tarball() {
    let f = fixture().await;
    let publish = Publish::new("demo", "1.0.0");
    assert_eq!(f.publish(&f.alice, &publish).await, 201);

    let (status, doc) = f.get_json(&f.hosted("demo"), Some(&f.alice)).await;
    assert_eq!(status, 200);
    assert_eq!(doc["name"], "demo");
    assert_eq!(doc["_id"], "demo");
    assert_eq!(doc["dist-tags"]["latest"], "1.0.0");
    let version = &doc["versions"]["1.0.0"];
    assert_eq!(version["dist"]["shasum"], sha1_hex(&publish.tarball));
    assert_eq!(version["dist"]["integrity"], sri512(&publish.tarball));
    assert_eq!(version["_id"], "demo@1.0.0");
    assert_eq!(version["_npmUser"]["name"], "alice");
    assert_eq!(doc["readme"], "# demo\n");
    assert_eq!(doc["time"]["1.0.0"], doc["time"]["created"]);
    assert!(doc["_rev"].as_str().unwrap().starts_with("1-"));

    // The tarball URL names the repository the client used, never the one the publisher sent.
    let tarball_url = version["dist"]["tarball"].as_str().unwrap();
    assert_eq!(tarball_url, f.hosted("demo/-/demo-1.0.0.tgz"));
    let tarball = f.get(tarball_url, Some(&f.alice)).await;
    assert_eq!(tarball.status(), 200);
    assert_eq!(
        tarball.bytes().await.unwrap().as_ref(),
        publish.tarball.as_slice()
    );
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_packages_are_reachable_with_a_slash_or_an_encoded_slash() {
    let f = fixture().await;
    let publish = Publish::new("@acme/billing", "2.1.0");
    assert_eq!(publish.package_path(), "@acme%2fbilling");
    assert_eq!(f.publish(&f.alice, &publish).await, 201);
    for path in [
        "@acme%2fbilling",
        "@acme%2Fbilling",
        "@acme/billing",
        "%40acme%2fbilling",
    ] {
        let (status, doc) = f.get_json(&f.hosted(path), Some(&f.alice)).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(doc["name"], "@acme/billing");
    }
    let doc = f.packument("@acme%2fbilling").await;
    assert_eq!(
        doc["versions"]["2.1.0"]["dist"]["tarball"],
        f.hosted("@acme/billing/-/billing-2.1.0.tgz")
    );
    let tarball = f
        .get(
            &f.hosted("@acme/billing/-/billing-2.1.0.tgz"),
            Some(&f.alice),
        )
        .await;
    assert_eq!(tarball.status(), 200);
    assert_eq!(component_names(&f).await, ["@acme/billing@2.1.0"]);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_abbreviated_document_is_served_on_request_and_revalidates() {
    let f = fixture().await;
    let publish = Publish::new("demo", "1.0.0").manifest(json!({
        "dependencies": { "left-pad": "^1.0.0" }, "scripts": { "postinstall": "true" }
    }));
    assert_eq!(f.publish(&f.alice, &publish).await, 201);
    let url = f.hosted("demo");
    let response = f
        .request(Method::GET, &url, Some(&f.alice))
        .header(
            "accept",
            "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/vnd.npm.install-v1+json"
    );
    assert_eq!(response.headers()["vary"], "Accept");
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    let doc: Value = response.json().await.unwrap();
    assert!(doc.get("readme").is_none());
    assert!(doc.get("_id").is_none());
    assert_eq!(
        doc["versions"]["1.0.0"]["dependencies"]["left-pad"],
        "^1.0.0"
    );
    assert_eq!(doc["versions"]["1.0.0"]["hasInstallScript"], true);
    assert!(doc["versions"]["1.0.0"].get("scripts").is_none());

    // Same validator, 304; another variant has another validator.
    let again = f
        .request(Method::GET, &url, Some(&f.alice))
        .header("accept", "application/vnd.npm.install-v1+json")
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 304);
    let full = f
        .request(Method::GET, &url, Some(&f.alice))
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(
        full.status(),
        200,
        "the full document is another representation"
    );
    assert_ne!(full.headers()["etag"].to_str().unwrap(), etag);

    let head = f
        .request(Method::HEAD, &url, Some(&f.alice))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert!(head.bytes().await.unwrap().is_empty());
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_is_visible_at_once_and_changes_the_validator() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    let url = f.hosted("demo");
    let first = f.get(&url, Some(&f.alice)).await;
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    f.publish(&f.alice, &Publish::new("demo", "1.1.0")).await;
    let second = f
        .request(Method::GET, &url, Some(&f.alice))
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    let doc: Value = second.json().await.unwrap();
    assert_eq!(doc["dist-tags"]["latest"], "1.1.0");
    assert_eq!(doc["versions"].as_object().unwrap().len(), 2);
    assert!(doc["_rev"].as_str().unwrap().starts_with("2-"));
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_replace_a_published_version() {
    let f = fixture().await;
    assert_eq!(
        f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await,
        201
    );
    let original = f.packument("demo").await;
    // The same version again, with different content: refused, and nothing changed.
    let again = Publish::new("demo", "1.0.0").tarball(bytes_of(99, 4096));
    let response = f.put(&f.hosted("demo"), Some(&f.alice), again.body()).await;
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("cannot publish over")
    );
    assert_eq!(f.packument("demo").await, original);
    let tarball = f
        .get(&f.hosted("demo/-/demo-1.0.0.tgz"), Some(&f.alice))
        .await
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        tarball.as_ref(),
        Publish::new("demo", "1.0.0").tarball.as_slice()
    );
    // Build metadata does not make another release.
    let plus = Publish::new("demo", "1.0.0+build.7");
    assert_eq!(f.publish(&f.alice, &plus).await, 409);
    assert_eq!(component_names(&f).await, ["demo@1.0.0"]);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_version_stays_immutable_under_every_write_policy() {
    use rustryx_core::model::{RepoFormat, WritePolicy};
    let f = fixture().await;
    for (name, policy) in [
        ("npm-once", WritePolicy::AllowOnce),
        ("npm-allow", WritePolicy::Allow),
    ] {
        f.server
            .add_repository(
                rustryx_testserver::NewTestRepo::hosted(name, RepoFormat::Npm).write_policy(policy),
            )
            .await;
        // Two versions of one package: the package document changes with the second publish,
        // which `allow_once` would refuse if the document were overwritten in place.
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            let publish = Publish::new("demo", version);
            let status = f
                .put(&f.url(name, "demo"), Some(&f.alice), publish.body())
                .await
                .status()
                .as_u16();
            assert_eq!(status, 201, "{name} {version}");
        }
        let (_, doc) = f.get_json(&f.url(name, "demo"), Some(&f.alice)).await;
        assert_eq!(doc["versions"].as_object().unwrap().len(), 3, "{name}");
        let again = f
            .put(
                &f.url(name, "demo"),
                Some(&f.alice),
                Publish::new("demo", "1.1.0").body(),
            )
            .await;
        assert_eq!(again.status(), 409, "{name}");
    }
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_store_anything_for_a_wrong_digest() {
    let f = fixture().await;
    let good = Publish::new("demo", "1.0.0");
    let other = bytes_of(7, 2048);
    for (label, publish) in [
        ("wrong shasum", good.clone().shasum(Some(sha1_hex(&other)))),
        (
            "wrong integrity",
            good.clone().integrity(Some(sri512(&other))),
        ),
        (
            "wrong sha256 integrity",
            good.clone().shasum(None).integrity(Some(sri256(&other))),
        ),
    ] {
        let response = f
            .put(&f.hosted("demo"), Some(&f.alice), publish.body())
            .await;
        assert_eq!(response.status(), 400, "{label}");
        let body = body_text(response).await;
        assert!(body.contains("digest"), "{label}: {body}");
    }
    assert_eq!(component_names(&f).await, Vec::<String>::new());
    let (status, _) = f.get_json(&f.hosted("demo"), Some(&f.alice)).await;
    assert_eq!(status, 404);
    // And the correct digests are what lets it through.
    assert_eq!(f.publish(&f.alice, &good).await, 201);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn publishing_without_declared_digests_computes_them() {
    let f = fixture().await;
    let publish = Publish::new("demo", "1.0.0").shasum(None).integrity(None);
    assert_eq!(f.publish(&f.alice, &publish).await, 201);
    let doc = f.packument("demo").await;
    assert_eq!(
        doc["versions"]["1.0.0"]["dist"]["shasum"],
        sha1_hex(&publish.tarball)
    );
    assert_eq!(
        doc["versions"]["1.0.0"]["dist"]["integrity"],
        sri512(&publish.tarball)
    );
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_accept_a_malformed_attachment() {
    let f = fixture().await;
    let base = Publish::new("demo", "1.0.0");
    for (label, publish) in [
        ("not base64", base.clone().data("***not base64***")),
        ("incomplete group", base.clone().data("QUJD1")),
        ("padding in the middle", base.clone().data("QQ==QUJD")),
        ("url safe alphabet", base.clone().data("QUJD-_")),
    ] {
        let response = f
            .put(&f.hosted("demo"), Some(&f.alice), publish.body())
            .await;
        assert_eq!(response.status(), 400, "{label}");
    }
    assert_eq!(component_names(&f).await, Vec::<String>::new());
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_accept_a_document_that_does_not_belong_to_the_url() {
    let f = fixture().await;
    let doc = Publish::new("demo", "1.0.0");
    // Another package in the body than in the URL.
    let response = f.put(&f.hosted("other"), Some(&f.alice), doc.body()).await;
    assert_eq!(response.status(), 400);
    // A wrong attachment name, two versions, uppercase name, a version after the attachment.
    let bad_attachment = doc.clone().attachment("demo-9.9.9.tgz");
    assert_eq!(f.publish(&f.alice, &bad_attachment).await, 400);
    let uppercase = Publish::new("Demo", "1.0.0");
    assert_eq!(f.publish(&f.alice, &uppercase).await, 400);
    let two_versions = doc.body().replacen(
        r#""versions":{"#,
        r#""versions":{"1.0.1":{"name":"demo","version":"1.0.1"},"#,
        1,
    );
    let response = f.put(&f.hosted("demo"), Some(&f.alice), two_versions).await;
    assert_eq!(response.status(), 400);
    assert!(body_text(response).await.contains("exactly one"));
    // The scanner needs `versions` before `_attachments`; a document that reverses them is
    // refused (serde_json sorts keys, which puts `_attachments` first).
    let reversed =
        serde_json::to_string(&serde_json::from_str::<Value>(&doc.body()).unwrap()).unwrap();
    let response = f.put(&f.hosted("demo"), Some(&f.alice), reversed).await;
    assert_eq!(response.status(), 400);
    assert!(body_text(response).await.contains("precede"));
    assert_eq!(component_names(&f).await, Vec::<String>::new());
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_accept_a_truncated_or_broken_body() {
    let f = fixture().await;
    let publish = Publish::new("demo", "1.0.0");
    let body = publish.body();
    for cut in [body.len() - 1, body.len() - 30, body.len() / 2, 100] {
        let response = f
            .put(&f.hosted("demo"), Some(&f.alice), body[..cut].to_owned())
            .await;
        assert_eq!(response.status(), 400, "cut {cut}");
    }
    assert_eq!(
        f.put(
            &f.hosted("demo"),
            Some(&f.alice),
            format!("{body} trailing")
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        f.put(&f.hosted("demo"), Some(&f.alice), "not json")
            .await
            .status(),
        400
    );
    assert_eq!(
        f.put(&f.hosted("demo"), Some(&f.alice), "").await.status(),
        400
    );
    // A body that breaks in the middle of the payload: nothing is published or left staged.
    let stream = futures::stream::iter(vec![
        Ok::<_, std::io::Error>(bytes::Bytes::from(body[..body.len() / 2].to_owned())),
        Err(std::io::Error::other("connection reset")),
    ]);
    let result = f
        .request(Method::PUT, &f.hosted("demo"), Some(&f.alice))
        .body(rustryx_testserver::reqwest::Body::wrap_stream(stream))
        .send()
        .await;
    if let Ok(response) = result {
        assert!(response.status().is_client_error() || response.status().is_server_error());
    }
    f.server.settle().await;
    assert_eq!(component_names(&f).await, Vec::<String>::new());
    assert_eq!(
        f.server.blobs().staging_count(),
        0,
        "no partial blob is left"
    );
    assert!(
        f.server.blobs().is_empty(),
        "no blob without a published asset"
    );
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_accept_an_oversized_metadata_part() {
    let handler = support::config_with(|c| c.max_publish_metadata_bytes = 4096);
    let f = fixture_with(rustryx_testserver::TestServer::builder().register(handler)).await;
    let publish = Publish::new("demo", "1.0.0").manifest(json!({ "readme": "x".repeat(10_000) }));
    let response = f
        .put(&f.hosted("demo"), Some(&f.alice), publish.body())
        .await;
    assert_eq!(response.status(), 413);
    // The tarball itself is not metadata: a large one is fine under the same limit.
    let large = Publish::new("demo", "1.0.0").tarball(bytes_of(1, 1_000_000));
    assert_eq!(f.publish(&f.alice, &large).await, 201);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tarball_with_a_traversal_name_is_stored_as_is_and_never_opened() {
    let f = fixture().await;
    let archive = tar_with_traversal_name();
    let publish = Publish::new("demo", "1.0.0").tarball(archive.clone());
    assert_eq!(f.publish(&f.alice, &publish).await, 201);
    let got = f
        .get(&f.hosted("demo/-/demo-1.0.0.tgz"), Some(&f.alice))
        .await
        .bytes()
        .await
        .unwrap();
    assert_eq!(got.as_ref(), archive.as_slice(), "stored byte for byte");
    // Storage keys come from ids, never from names in the archive.
    assert!(!std::path::Path::new("/tmp/rustryx-must-not-exist").exists());
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authentication_and_authorization_are_checked_before_the_body_is_read() {
    let f = fixture().await;
    let publish = Publish::new("demo", "1.0.0");
    // 401: no credentials, or credentials that are not valid.
    assert_eq!(
        f.put(&f.hosted("demo"), None, publish.body())
            .await
            .status(),
        401
    );
    let bad = f
        .request(Method::PUT, &f.hosted("demo"), None)
        .header("authorization", "Bearer ryx_not-a-token")
        .body(publish.body())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    // 403: authenticated, may read, may not write.
    let forbidden = f
        .put(&f.hosted("demo"), Some(&f.reader), publish.body())
        .await;
    assert_eq!(forbidden.status(), 403);
    let body: Value = forbidden.json().await.unwrap();
    assert_eq!(body["code"], "forbidden");
    // 404: a caller who holds nothing on the repository does not learn that it exists.
    let nobody = f.server.add_user("nobody", &[]).await;
    assert_eq!(
        f.put(&f.hosted("demo"), Some(&nobody), publish.body())
            .await
            .status(),
        404
    );
    // Reading follows the same rules.
    assert_eq!(f.get(&f.hosted("demo"), None).await.status(), 401);
    assert_eq!(component_names(&f).await, Vec::<String>::new());
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_401_names_a_challenge_and_an_error_body_is_the_registrys() {
    let f = fixture().await;
    let response = f.get(&f.hosted("demo"), None).await;
    assert_eq!(response.status(), 401);
    assert!(response.headers().contains_key("www-authenticate"));
    assert_eq!(response.headers()["content-type"], "application/json");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "authentication required");
    let missing = f.get(&f.hosted("demo"), Some(&f.alice)).await;
    assert_eq!(missing.status(), 404);
    let body: Value = missing.json().await.unwrap();
    assert_eq!(body["error"], "not found");
    assert_eq!(body["code"], "not_found");
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_resolve_traversal_in_any_encoding() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    let auth = f.alice.bearer_header();
    for path in [
        "..",
        "../demo",
        "demo/../demo",
        "%2e%2e/demo",
        "%2e%2e%2fdemo",
        "demo/%2e%2e/%2e%2e",
        "demo%2f..%2fdemo",
        "%252e%252e/demo",
        "demo/-/..%5cdemo-1.0.0.tgz",
        "demo//-/demo-1.0.0.tgz",
        "demo/-/demo-1.0.0.tgz/",
        "demo/-/%2e%2e/demo-1.0.0.tgz",
        "@acme/../demo",
        "demo%00",
        "demo/-/demo-1.0.0.tgz%00.png",
        ".",
        "./demo",
        "demo/-/demo-1.0.0.tgz%2f..%2f..%2fdemo",
        "-/package/demo/dist-tags/%2e%2e",
        "demo/-rev/%2e%2e",
    ] {
        let target = format!("/repository/{}/{path}", support::HOSTED);
        let response =
            support::raw(&f.server, "GET", &target, &[("authorization", &auth)], b"").await;
        assert!(
            matches!(response.status, 400 | 404),
            "{path}: {} {}",
            response.status,
            String::from_utf8_lossy(&response.body)
        );
        // The same vectors on the write routes: refused the same way, nothing deleted.
        let deleted = support::raw(
            &f.server,
            "DELETE",
            &target,
            &[("authorization", &f.admin.bearer_header())],
            b"",
        )
        .await;
        assert!(
            matches!(deleted.status, 400 | 404 | 405),
            "{path}: {}",
            deleted.status
        );
    }
    assert_eq!(component_names(&f).await, ["demo@1.0.0"]);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_internal_layout_is_not_reachable_through_the_protocol() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    for path in [
        "demo/-/doc/00000000000000000001.json",
        "demo/-/abbreviated.json",
        "demo/-/demo-1.0.0.json",
        "-/doc",
    ] {
        let status = f
            .get(&f.hosted(path), Some(&f.admin))
            .await
            .status()
            .as_u16();
        assert_eq!(status, 404, "{path}");
    }
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dist_tags_can_be_listed_set_and_removed() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    f.publish(&f.alice, &Publish::new("demo", "1.1.0").tags(&["latest"]))
        .await;
    f.publish(
        &f.alice,
        &Publish::new("demo", "2.0.0-rc.1").tags(&["next"]),
    )
    .await;
    let tags_url = f.hosted("-/package/demo/dist-tags");
    let (status, tags) = f.get_json(&tags_url, Some(&f.alice)).await;
    assert_eq!(status, 200);
    assert_eq!(tags, json!({"latest": "1.1.0", "next": "2.0.0-rc.1"}));

    // npm sends the version as a JSON string.
    let put = f
        .put(
            &f.hosted("-/package/demo/dist-tags/stable"),
            Some(&f.alice),
            "\"1.0.0\"",
        )
        .await;
    assert_eq!(put.status(), 200);
    let (_, tags) = f.get_json(&tags_url, Some(&f.alice)).await;
    assert_eq!(tags["stable"], "1.0.0");
    assert_eq!(f.packument("demo").await["dist-tags"]["stable"], "1.0.0");

    // A developer may write a tag but deleting needs the delete privilege.
    let denied = f
        .delete(&f.hosted("-/package/demo/dist-tags/stable"), Some(&f.alice))
        .await;
    assert_eq!(denied.status(), 403);
    let deleted = f
        .delete(&f.hosted("-/package/demo/dist-tags/stable"), Some(&f.admin))
        .await;
    assert_eq!(deleted.status(), 200);
    assert!(
        f.get_json(&tags_url, Some(&f.alice))
            .await
            .1
            .get("stable")
            .is_none()
    );

    // Refusals: a version that does not exist, a bad body, a bad tag, removing latest or a tag
    // that is not there.
    let missing = f
        .put(
            &f.hosted("-/package/demo/dist-tags/stage"),
            Some(&f.alice),
            "\"9.9.9\"",
        )
        .await;
    assert_eq!(missing.status(), 404);
    for body in ["1.0.0", "\"v1\"", "{}", "\"1.0.0\" \"1.0.1\"", ""] {
        let response = f
            .put(
                &f.hosted("-/package/demo/dist-tags/stage"),
                Some(&f.alice),
                body,
            )
            .await;
        assert_eq!(response.status(), 400, "{body:?}");
    }
    let as_version = f
        .put(
            &f.hosted("-/package/demo/dist-tags/1.0.0"),
            Some(&f.alice),
            "\"1.0.0\"",
        )
        .await;
    assert_eq!(as_version.status(), 404);
    let latest = f
        .delete(&f.hosted("-/package/demo/dist-tags/latest"), Some(&f.admin))
        .await;
    assert_eq!(latest.status(), 400);
    let absent = f
        .delete(
            &f.hosted("-/package/demo/dist-tags/nothing"),
            Some(&f.admin),
        )
        .await;
    assert_eq!(absent.status(), 404);
    let unknown_package = f
        .put(
            &f.hosted("-/package/nope/dist-tags/x"),
            Some(&f.alice),
            "\"1.0.0\"",
        )
        .await;
    assert_eq!(unknown_package.status(), 404);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retagging_latest_moves_the_hoisted_fields_only_on_publish() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    f.publish(&f.alice, &Publish::new("demo", "1.1.0")).await;
    let put = f
        .put(
            &f.hosted("-/package/demo/dist-tags/latest"),
            Some(&f.alice),
            "\"1.0.0\"",
        )
        .await;
    assert_eq!(put.status(), 200);
    let doc = f.packument("demo").await;
    assert_eq!(doc["dist-tags"]["latest"], "1.0.0");
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deprecate_writes_the_document_back() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    f.publish(&f.alice, &Publish::new("demo", "1.1.0")).await;
    let url = f.hosted("demo");
    // What `npm deprecate demo@1.0.0 "use 1.1.0"` does: read the document, set the field, put it.
    let mut doc = f.packument("demo").await;
    doc["versions"]["1.0.0"]["deprecated"] = json!("use 1.1.0");
    // Stale parts of the snapshot must not undo anything else.
    doc["dist-tags"] = json!({"latest": "1.0.0", "hacked": "1.0.0"});
    let response = f.put(&url, Some(&f.alice), doc.to_string()).await;
    assert_eq!(response.status(), 200, "{}", body_text(response).await);
    let after = f.packument("demo").await;
    assert_eq!(after["versions"]["1.0.0"]["deprecated"], "use 1.1.0");
    assert!(after["versions"]["1.1.0"].get("deprecated").is_none());
    assert_eq!(
        after["dist-tags"],
        json!({"latest": "1.1.0"}),
        "a plain PUT never changes tags"
    );

    let abbreviated = f
        .request(Method::GET, &url, Some(&f.alice))
        .header("accept", "application/vnd.npm.install-v1+json")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(abbreviated["versions"]["1.0.0"]["deprecated"], "use 1.1.0");

    // An empty message undeprecates.
    let mut doc = f.packument("demo").await;
    doc["versions"]["1.0.0"]["deprecated"] = json!("");
    assert_eq!(
        f.put(&url, Some(&f.alice), doc.to_string()).await.status(),
        200
    );
    assert!(
        f.packument("demo").await["versions"]["1.0.0"]
            .get("deprecated")
            .is_none()
    );

    // A document that lists a version this registry does not have is not a deprecation.
    let mut doc = f.packument("demo").await;
    doc["versions"]["9.9.9"] = json!({"name": "demo", "version": "9.9.9"});
    assert_eq!(
        f.put(&url, Some(&f.alice), doc.to_string()).await.status(),
        400
    );
    // Nor is a document of another package, or for a package that has no document.
    let other = json!({"name": "other", "versions": {}});
    assert_eq!(
        f.put(&url, Some(&f.alice), other.to_string())
            .await
            .status(),
        400
    );
    let none = json!({"name": "ghost", "versions": {"1.0.0": {"deprecated": "x"}}});
    assert_eq!(
        f.put(&f.hosted("ghost"), Some(&f.alice), none.to_string())
            .await
            .status(),
        404
    );
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_noop_deprecation_does_not_write_a_new_document() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    let before = f.packument("demo").await["_rev"].clone();
    let doc = f.packument("demo").await;
    assert_eq!(
        f.put(&f.hosted("demo"), Some(&f.alice), doc.to_string())
            .await
            .status(),
        200
    );
    assert_eq!(f.packument("demo").await["_rev"], before);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpublish_one_version_the_way_npm_does() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    f.publish(&f.alice, &Publish::new("demo", "1.1.0")).await;
    f.publish(&f.alice, &Publish::new("demo", "1.2.0")).await;
    // 1. the document without the version, `latest` moved by the client, to /demo/-rev/<rev>.
    let mut doc = f.packument("demo").await;
    let rev = doc["_rev"].as_str().unwrap().to_owned();
    doc["versions"].as_object_mut().unwrap().remove("1.2.0");
    doc["dist-tags"] = json!({"latest": "1.0.0"});
    // The developer may write but not delete: the -rev form needs the delete privilege.
    let denied = f
        .put(
            &f.hosted(&format!("demo/-rev/{rev}")),
            Some(&f.alice),
            doc.to_string(),
        )
        .await;
    assert_eq!(denied.status(), 403);
    assert!(f.packument("demo").await["versions"].get("1.2.0").is_some());
    let put = f
        .put(
            &f.hosted(&format!("demo/-rev/{rev}")),
            Some(&f.admin),
            doc.to_string(),
        )
        .await;
    assert_eq!(put.status(), 200, "{}", body_text(put).await);
    let after = f.packument("demo").await;
    assert!(after["versions"].get("1.2.0").is_none());
    assert_eq!(
        after["dist-tags"]["latest"], "1.0.0",
        "the client's choice is honoured"
    );
    // 2. the tarball, with the revision of a second read. It is gone already: 404, which npm
    // treats as done.
    let rev = after["_rev"].as_str().unwrap();
    let tarball = f
        .delete(
            &f.hosted(&format!("demo/-/demo-1.2.0.tgz/-rev/{rev}")),
            Some(&f.admin),
        )
        .await;
    assert_eq!(tarball.status(), 404);
    assert_eq!(
        f.get(&f.hosted("demo/-/demo-1.2.0.tgz"), Some(&f.admin))
            .await
            .status(),
        404
    );
    assert_eq!(component_names(&f).await, ["demo@1.0.0", "demo@1.1.0"]);
    // The version can be published again after it was removed.
    assert_eq!(
        f.publish(&f.alice, &Publish::new("demo", "1.2.0")).await,
        201
    );
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_tarball_with_its_revision_removes_the_version() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    f.publish(&f.alice, &Publish::new("demo", "1.1.0")).await;
    let response = f
        .delete(
            &f.hosted("demo/-/demo-1.1.0.tgz/-rev/2-abc"),
            Some(&f.admin),
        )
        .await;
    assert_eq!(response.status(), 200);
    let doc = f.packument("demo").await;
    assert_eq!(doc["dist-tags"]["latest"], "1.0.0");
    assert_eq!(doc["versions"].as_object().unwrap().len(), 1);
    // The last version takes the whole document with it.
    let last = f
        .delete(
            &f.hosted("demo/-/demo-1.0.0.tgz/-rev/3-abc"),
            Some(&f.admin),
        )
        .await;
    assert_eq!(last.status(), 200);
    assert_eq!(f.get(&f.hosted("demo"), Some(&f.admin)).await.status(), 404);
    assert_eq!(component_names(&f).await, Vec::<String>::new());
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpublish_the_whole_package() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    f.publish(&f.alice, &Publish::new("demo", "1.1.0")).await;
    f.publish(&f.alice, &Publish::new("keep", "1.0.0")).await;
    f.publish(&f.alice, &Publish::new("demo-extra", "1.0.0"))
        .await;
    let denied = f.delete(&f.hosted("demo/-rev/3-abc"), Some(&f.alice)).await;
    assert_eq!(denied.status(), 403);
    let response = f.delete(&f.hosted("demo/-rev/3-abc"), Some(&f.admin)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(f.get(&f.hosted("demo"), Some(&f.admin)).await.status(), 404);
    assert_eq!(
        f.get(&f.hosted("demo/-/demo-1.0.0.tgz"), Some(&f.admin))
            .await
            .status(),
        404
    );
    // Neighbours with a similar name are untouched.
    assert_eq!(f.get(&f.hosted("keep"), Some(&f.admin)).await.status(), 200);
    assert_eq!(
        f.get(&f.hosted("demo-extra"), Some(&f.admin))
            .await
            .status(),
        200
    );
    assert_eq!(
        component_names(&f).await,
        ["keep@1.0.0", "demo-extra@1.0.0"]
    );
    // Again: there is nothing left.
    assert_eq!(
        f.delete(&f.hosted("demo/-rev/3-abc"), Some(&f.admin))
            .await
            .status(),
        404
    );
    // The name can be reused.
    assert_eq!(
        f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await,
        201
    );
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpublishing_every_version_through_the_document_removes_the_package() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    let mut doc = f.packument("demo").await;
    doc["versions"] = json!({});
    let response = f
        .put(
            &f.hosted("demo/-rev/1-abc"),
            Some(&f.admin),
            doc.to_string(),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(f.get(&f.hosted("demo"), Some(&f.admin)).await.status(), 404);
    assert_eq!(component_names(&f).await, Vec::<String>::new());
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_publishes_of_one_package_lose_no_version() {
    let f = std::sync::Arc::new(fixture().await);
    let versions: Vec<String> = (0..8).map(|n| format!("1.{n}.0")).collect();
    let mut tasks = Vec::new();
    for version in &versions {
        let f = std::sync::Arc::clone(&f);
        let version = version.clone();
        tasks.push(tokio::spawn(async move {
            let publish = Publish::new("racy", &version);
            let response = f
                .put(&f.hosted("racy"), Some(&f.alice), publish.body())
                .await;
            (response.status().as_u16(), response.text().await.unwrap())
        }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!(status, 201, "{body}");
    }
    let doc = f.packument("racy").await;
    let listed: Vec<&String> = doc["versions"].as_object().unwrap().keys().collect();
    assert_eq!(listed.len(), 8, "{listed:?}");
    assert_eq!(component_names(&f).await.len(), 8);
    for version in &versions {
        let tarball = f
            .get(
                &f.hosted(&format!("racy/-/racy-{version}.tgz")),
                Some(&f.alice),
            )
            .await;
        assert_eq!(tarball.status(), 200, "{version}");
    }
    // Racing for the same version: exactly one wins.
    let mut tasks = Vec::new();
    for n in 0..4u8 {
        let f = std::sync::Arc::clone(&f);
        tasks.push(tokio::spawn(async move {
            let publish = Publish::new("same", "1.0.0").tarball(bytes_of(n, 1000 + usize::from(n)));
            f.publish(&f.alice, &publish).await
        }));
    }
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 409, 409, 409]);
    let doc = f.packument("same").await;
    assert_eq!(doc["versions"].as_object().unwrap().len(), 1);
    let f = std::sync::Arc::try_unwrap(f).ok().unwrap();
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn old_generations_of_the_document_are_cleaned_up() {
    use rustryx_core::ports::{AssetRepo, PageRequest};
    let f = fixture().await;
    for n in 0..5 {
        f.publish(&f.alice, &Publish::new("demo", &format!("1.{n}.0")))
            .await;
    }
    let assets: std::sync::Arc<dyn AssetRepo> = std::sync::Arc::new(f.server.store().clone());
    let repository = {
        use rustryx_core::ports::RepositoryRepo;
        let repos: std::sync::Arc<dyn RepositoryRepo> =
            std::sync::Arc::new(f.server.store().clone());
        repos
            .get_by_name(&rustryx_core::names::RepoName::parse(support::HOSTED).unwrap())
            .await
            .unwrap()
            .unwrap()
    };
    let page = assets
        .list(
            repository.id,
            "demo/-/doc/",
            &PageRequest {
                limit: 100,
                after: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "only the current generation remains");
    assert!(page.items[0].path.ends_with("00000000000000000005.json"));
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn version_documents_and_tags_resolve() {
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("demo", "1.0.0")).await;
    f.publish(
        &f.alice,
        &Publish::new("demo", "2.0.0-rc.1").tags(&["next"]),
    )
    .await;
    for (spec, expected) in [
        ("1.0.0", "1.0.0"),
        ("latest", "1.0.0"),
        ("next", "2.0.0-rc.1"),
    ] {
        let (status, doc) = f
            .get_json(&f.hosted(&format!("demo/{spec}")), Some(&f.alice))
            .await;
        assert_eq!(status, 200, "{spec}");
        assert_eq!(doc["version"], expected);
        assert_eq!(
            doc["dist"]["tarball"],
            f.hosted(&format!("demo/-/demo-{expected}.tgz"))
        );
    }
    for spec in ["9.9.9", "nothing"] {
        assert_eq!(
            f.get(&f.hosted(&format!("demo/{spec}")), Some(&f.alice))
                .await
                .status(),
            404
        );
    }
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_whoami_audit_and_search() {
    let f = fixture().await;
    let ping = f.get(&f.hosted("-/ping"), Some(&f.alice)).await;
    assert_eq!(ping.status(), 200);
    assert_eq!(body_text(ping).await, "{}");
    let (status, who) = f.get_json(&f.hosted("-/whoami"), Some(&f.alice)).await;
    assert_eq!((status, who["username"].as_str()), (200, Some("alice")));
    assert_eq!(f.get(&f.hosted("-/whoami"), None).await.status(), 401);

    // Audit: no advisories, in the shape of each generation of the endpoint.
    for (path, expected_key) in [
        ("-/npm/v1/security/advisories/bulk", None),
        ("-/npm/v1/security/audits/quick", Some("advisories")),
        ("-/npm/v1/security/audits", Some("metadata")),
    ] {
        let response = f
            .request(Method::POST, &f.hosted(path), Some(&f.alice))
            .header("content-encoding", "gzip")
            .body(vec![0x1f, 0x8b, 0, 0])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let body: Value = response.json().await.unwrap();
        match expected_key {
            None => assert_eq!(body, json!({})),
            Some(key) => assert!(body.get(key).is_some(), "{path}: {body}"),
        }
    }
    assert_eq!(
        f.request(Method::POST, &f.hosted("-/npm/v1/security/audits"), None)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    f.publish(&f.alice, &Publish::new("alpha-lib", "1.0.0"))
        .await;
    f.publish(&f.alice, &Publish::new("@acme/beta-lib", "1.0.0"))
        .await;
    f.publish(&f.alice, &Publish::new("gamma", "1.0.0")).await;
    let (status, found) = f
        .get_json(&f.hosted("-/v1/search?text=lib"), Some(&f.alice))
        .await;
    assert_eq!(status, 200);
    let names: Vec<&str> = found["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["package"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"alpha-lib") && names.contains(&"@acme/beta-lib"));
    assert_eq!(found["total"], 2);
    let first = &found["objects"][0]["package"];
    assert_eq!(first["version"], "1.0.0");
    assert_eq!(first["publisher"]["username"], "alice");
    let (_, scoped) = f
        .get_json(&f.hosted("-/v1/search?text=scope:acme"), Some(&f.alice))
        .await;
    assert_eq!(scoped["objects"].as_array().unwrap().len(), 1);
    let (_, page) = f
        .get_json(&f.hosted("-/v1/search?size=1&from=1"), Some(&f.alice))
        .await;
    assert_eq!(page["objects"].as_array().unwrap().len(), 1);
    assert_eq!(page["total"], 3);
    let (_, none) = f
        .get_json(&f.hosted("-/v1/search?text=zzz"), Some(&f.alice))
        .await;
    assert_eq!(none["objects"], json!([]));
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn content_selectors_see_scope_name_and_version() {
    use rustryx_testserver::{READ, TestPrivilege};
    let f = fixture().await;
    f.publish(&f.alice, &Publish::new("@acme/billing", "1.0.0"))
        .await;
    f.publish(&f.alice, &Publish::new("@other/secret", "1.0.0"))
        .await;
    f.publish(&f.alice, &Publish::new("@acme/billing", "2.0.0"))
        .await;
    let selector = f
        .server
        .add_selector(
            "acme-v1",
            r#"coordinate.group == "acme" && coordinate.name == "billing" && coordinate.version.startsWith("1.")"#,
        )
        .await;
    f.server
        .add_role_with_privileges(
            "acme-reader",
            &[TestPrivilege::content_selector(
                support::HOSTED,
                &selector,
                &READ,
            )],
        )
        .await;
    let bob = f.server.add_user("bob", &["acme-reader"]).await;
    // The tarball of 1.x of the acme package, however its name is spelled in the URL.
    for path in [
        "@acme/billing/-/billing-1.0.0.tgz",
        "@acme%2fbilling/-/billing-1.0.0.tgz",
    ] {
        assert_eq!(
            f.get(&f.hosted(path), Some(&bob)).await.status(),
            200,
            "{path}"
        );
    }
    for path in [
        "@acme/billing/-/billing-2.0.0.tgz",
        "@other/secret/-/secret-1.0.0.tgz",
        "@other%2fsecret/-/secret-1.0.0.tgz",
    ] {
        assert_eq!(
            f.get(&f.hosted(path), Some(&bob)).await.status(),
            403,
            "{path}"
        );
    }
    f.server.shutdown().await;
}
