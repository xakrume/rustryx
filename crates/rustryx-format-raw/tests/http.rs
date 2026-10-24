//! The raw format behind a real HTTP listener with the real `Rbac`: content selectors,
//! anonymous gates, headers on the wire, and request targets exactly as a client writes them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::assert_is_empty,
    clippy::duration_suboptimal_units,
    reason = "test scenarios read best as straight-line code"
)]

mod common;

use common::{client, get, header, put, raw_builder, raw_server, raw_status};
use rustryx_core::model::RepoFormat;
use rustryx_testserver::{NewTestRepo, READ, TestPrivilege};

#[tokio::test]
async fn round_trips_a_file_over_http_with_the_documented_headers() {
    let server = raw_server().await;
    let user = server.add_user("publisher", &["nx-developer"]).await;
    let http = client();
    let url = format!("{}/docs/guide.txt", server.repository_url("files"));
    let auth = user.basic_header();

    let created = put(&http, &url, Some(&auth), b"guide text".to_vec()).await;
    assert_eq!(created.status(), 201);
    let got = get(&http, &url, Some(&auth)).await;
    assert_eq!(got.status(), 200);
    assert_eq!(header(&got, "content-type"), Some("text/plain"));
    assert_eq!(header(&got, "x-content-type-options"), Some("nosniff"));
    assert!(header(&got, "x-checksum-sha256").is_some());
    assert_eq!(got.bytes().await.unwrap().as_ref(), b"guide text");

    // The API token works as the Basic password and as a Bearer token.
    let by_token = get(&http, &url, Some(&user.token_basic_header())).await;
    assert_eq!(by_token.status(), 200);
    let by_bearer = get(&http, &url, Some(&user.bearer_header())).await;
    assert_eq!(by_bearer.status(), 200);
}

#[tokio::test]
async fn streams_a_large_upload_and_returns_the_same_bytes() {
    let server = raw_server().await;
    let user = server.add_user("publisher", &["nx-developer"]).await;
    let http = client();
    let url = format!("{}/big/blob.bin", server.repository_url("files"));
    let auth = user.basic_header();
    let data: Vec<u8> = (0..6_000_000u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    assert_eq!(
        put(&http, &url, Some(&auth), data.clone()).await.status(),
        201
    );
    let got = get(&http, &url, Some(&auth)).await;
    assert_eq!(header(&got, "content-length"), Some("6000000"));
    assert_eq!(got.bytes().await.unwrap().as_ref(), data.as_slice());
}

#[tokio::test]
async fn wrong_credentials_are_401_and_a_missing_right_is_403() {
    let server = raw_server().await;
    server
        .add_role_with_privileges(
            "files-reader",
            &[TestPrivilege::repository_view("files", &READ)],
        )
        .await;
    let reader = server.add_user("reader", &["files-reader"]).await;
    let publisher = server.add_user("publisher", &["nx-developer"]).await;
    let http = client();
    let url = format!("{}/a.txt", server.repository_url("files"));
    assert_eq!(
        put(&http, &url, Some(&publisher.basic_header()), b"x".to_vec())
            .await
            .status(),
        201
    );

    let anonymous = get(&http, &url, None).await;
    assert_eq!(anonymous.status(), 401);
    assert!(header(&anonymous, "www-authenticate").is_some_and(|v| v.starts_with("Basic")));
    let wrong = get(&http, &url, Some("Basic YmFkOmNyZWRz")).await;
    assert_eq!(wrong.status(), 401);
    assert_eq!(
        get(&http, &url, Some(&reader.basic_header()))
            .await
            .status(),
        200
    );
    let forbidden = put(&http, &url, Some(&reader.basic_header()), b"y".to_vec()).await;
    assert_eq!(forbidden.status(), 403);
    let deleted = http
        .delete(&url)
        .header("authorization", reader.basic_header())
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 403);
}

#[tokio::test]
async fn a_content_selector_limits_both_downloads_and_the_listing() {
    let server = raw_server().await;
    let selector = server
        // The engine authorizes the browse of a directory on its path without the trailing
        // slash (`/public`), so a selector that should allow listing it names both forms.
        .add_selector(
            "public-only",
            "path == \"/public\" || path.startsWith(\"/public/\")",
        )
        .await;
    server
        .add_role_with_privileges(
            "public-reader",
            &[TestPrivilege::content_selector("files", &selector, &READ)],
        )
        .await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let reader = server.add_user("reader", &["public-reader"]).await;
    let http = client();
    let base = server.repository_url("files");
    for path in ["public/a.txt", "public/b.txt", "private/c.txt", "top.txt"] {
        let response = put(
            &http,
            &format!("{base}/{path}"),
            Some(&admin.basic_header()),
            b"x".to_vec(),
        )
        .await;
        assert_eq!(response.status(), 201, "{path}");
    }
    let auth = reader.basic_header();
    assert_eq!(
        get(&http, &format!("{base}/public/a.txt"), Some(&auth))
            .await
            .status(),
        200
    );
    assert_eq!(
        get(&http, &format!("{base}/private/c.txt"), Some(&auth))
            .await
            .status(),
        403
    );
    assert_eq!(
        get(&http, &format!("{base}/top.txt"), Some(&auth))
            .await
            .status(),
        403
    );

    let list = |url: String| {
        let http = http.clone();
        let auth = auth.clone();
        async move {
            http.get(url)
                .header("authorization", auth)
                .header("accept", "application/json")
                .send()
                .await
                .unwrap()
        }
    };
    let public = list(format!("{base}/public/")).await;
    assert_eq!(public.status(), 200);
    let names: Vec<String> = public.json::<serde_json::Value>().await.unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["a.txt", "b.txt"]);
    // The selector does not allow `private/`, so the directory is refused outright.
    assert_eq!(list(format!("{base}/private/")).await.status(), 403);
}

#[tokio::test]
async fn anonymous_read_needs_the_switch_and_the_repository_flag() {
    let server = raw_builder().anonymous_enabled(true).start().await;
    server
        .add_repository(NewTestRepo::hosted("open", RepoFormat::Raw).allow_anonymous(true))
        .await;
    server
        .add_repository(NewTestRepo::hosted("shut", RepoFormat::Raw))
        .await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let http = client();
    for name in ["open", "shut"] {
        let url = format!("{}/a.txt", server.repository_url(name));
        assert_eq!(
            put(&http, &url, Some(&admin.basic_header()), b"x".to_vec())
                .await
                .status(),
            201
        );
    }
    let open = format!("{}/a.txt", server.repository_url("open"));
    let shut = format!("{}/a.txt", server.repository_url("shut"));
    assert_eq!(get(&http, &open, None).await.status(), 200);
    assert_eq!(get(&http, &shut, None).await.status(), 401);
    // Anonymous never writes.
    assert_eq!(put(&http, &open, None, b"y".to_vec()).await.status(), 401);
    server.set_anonymous_enabled(false);
    assert_eq!(get(&http, &open, None).await.status(), 401);
}

#[tokio::test]
async fn must_not_resolve_a_traversal_target_as_written_by_the_client() {
    let server = raw_server().await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let auth = admin.basic_header();
    let http = client();
    let secret = format!("{}/safe.txt", server.repository_url("files"));
    assert_eq!(
        put(&http, &secret, Some(&auth), b"x".to_vec())
            .await
            .status(),
        201
    );

    let targets = [
        "/repository/files/../etc/passwd",
        "/repository/files/a/../../x",
        "/repository/files/%2e%2e/x",
        "/repository/files/%2E%2E%2Fx",
        "/repository/files/a/%2e%2e%2f%2e%2e/x",
        "/repository/files/..%5c..%5cx",
        "/repository/files/a%5c..%5cb",
        "/repository/files/%252e%252e/x",
        "/repository/files/a%00b",
        "/repository/files/a//b",
        "/repository/files//x",
    ];
    for target in targets {
        let status = raw_status(&server, target, &auth).await;
        assert!(
            status == 400 || status == 404,
            "{target}: answered {status}, expected 400 or 404"
        );
        assert_ne!(status, 200, "{target}");
    }
    // The three that name `safe.txt` through a dot segment must not reach it either.
    for target in [
        "/repository/files/x/../safe.txt",
        "/repository/files/./safe.txt",
        "/repository/files/%2e/safe.txt",
    ] {
        assert_eq!(raw_status(&server, target, &auth).await, 400, "{target}");
    }
}

#[tokio::test]
async fn html_uploads_come_back_as_attachments_over_http() {
    let server = raw_server().await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let http = client();
    let url = format!("{}/site/index.html", server.repository_url("files"));
    let response = http
        .put(&url)
        .header("authorization", admin.basic_header())
        .header("content-type", "text/html")
        .body("<script>alert(1)</script>")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let got = get(&http, &url, Some(&admin.basic_header())).await;
    assert_eq!(header(&got, "content-type"), Some("text/html"));
    assert!(header(&got, "content-disposition").is_some_and(|v| v.starts_with("attachment;")));
    assert_eq!(header(&got, "x-content-type-options"), Some("nosniff"));
    assert!(header(&got, "content-security-policy").is_some_and(|v| v.contains("sandbox")));
}

#[tokio::test]
async fn a_repository_can_opt_in_to_inline_html() {
    let server = raw_builder().start().await;
    server
        .add_repository(
            NewTestRepo::hosted("site", RepoFormat::Raw)
                .format_config(serde_json::json!({ "content_disposition": "inline" })),
        )
        .await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let http = client();
    let url = format!("{}/index.html", server.repository_url("site"));
    http.put(&url)
        .header("authorization", admin.basic_header())
        .header("content-type", "text/html")
        .body("<p>hi</p>")
        .send()
        .await
        .unwrap();
    let got = get(&http, &url, Some(&admin.basic_header())).await;
    assert_eq!(header(&got, "content-disposition"), None);
    assert_eq!(header(&got, "x-content-type-options"), Some("nosniff"));
}

#[tokio::test]
async fn a_group_over_http_serves_the_first_member_and_refuses_writes() {
    let server = raw_builder().start().await;
    for name in ["one", "two"] {
        server
            .add_repository(NewTestRepo::hosted(name, RepoFormat::Raw))
            .await;
    }
    server
        .add_repository(NewTestRepo::group("both", RepoFormat::Raw, &["one", "two"]))
        .await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let auth = admin.basic_header();
    let http = client();
    put(
        &http,
        &format!("{}/f.txt", server.repository_url("two")),
        Some(&auth),
        b"two".to_vec(),
    )
    .await;
    put(
        &http,
        &format!("{}/f.txt", server.repository_url("one")),
        Some(&auth),
        b"one".to_vec(),
    )
    .await;
    let got = get(
        &http,
        &format!("{}/f.txt", server.repository_url("both")),
        Some(&auth),
    )
    .await;
    assert_eq!(got.text().await.unwrap(), "one");
    let write = put(
        &http,
        &format!("{}/g.txt", server.repository_url("both")),
        Some(&auth),
        b"x".to_vec(),
    )
    .await;
    assert_eq!(write.status(), 405);
    assert_eq!(header(&write, "allow"), Some("GET, HEAD"));
}
