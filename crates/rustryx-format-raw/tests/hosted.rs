//! Contract tests of the raw format on a hosted repository, through the real engine.
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

mod support;

use http::{Method, StatusCode};
use rustryx_core::model::WritePolicy;
use support::{
    World, anonymous, body_bytes, body_json, body_text, entry_names, header_of, principal,
};

#[tokio::test]
async fn uploads_downloads_and_deletes_a_file() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);

    let created = world
        .put(&repo, "docs/readme.txt", &admin, b"hello raw")
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert!(header_of(&created, "etag").is_some());

    let got = world.get(&repo, "docs/readme.txt", &admin).await;
    assert_eq!(got.status(), StatusCode::OK);
    assert_eq!(header_of(&got, "content-type"), Some("text/plain"));
    assert_eq!(header_of(&got, "content-length"), Some("9"));
    assert_eq!(header_of(&got, "accept-ranges"), Some("bytes"));
    assert_eq!(header_of(&got, "x-content-type-options"), Some("nosniff"));
    assert!(header_of(&got, "x-checksum-sha256").is_some_and(|v| v.len() == 64));
    assert!(header_of(&got, "x-checksum-sha1").is_some_and(|v| v.len() == 40));
    assert!(header_of(&got, "content-security-policy").is_some_and(|v| v.contains("sandbox")));
    assert_eq!(&body_bytes(got).await[..], b"hello raw");

    let overwritten = world
        .put(&repo, "docs/readme.txt", &admin, b"hello again")
        .await;
    assert_eq!(overwritten.status(), StatusCode::OK);
    let again = world.get(&repo, "docs/readme.txt", &admin).await;
    assert_eq!(&body_bytes(again).await[..], b"hello again");

    let deleted = world.delete(&repo, "docs/readme.txt", &admin).await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let gone = world.get(&repo, "docs/readme.txt", &admin).await;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    let twice = world.delete(&repo, "docs/readme.txt", &admin).await;
    assert_eq!(twice.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn head_answers_without_a_body_and_with_the_same_headers() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a.bin", b"0123456789").await;
    let admin = world.admin(&[&repo]);
    let head = world.head_of(&repo, "a.bin", &admin).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(header_of(&head, "content-length"), Some("10"));
    assert!(header_of(&head, "etag").is_some());
    assert!(body_bytes(head).await.is_empty());
}

#[tokio::test]
async fn serves_a_byte_range_and_refuses_one_past_the_end() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a.bin", b"0123456789").await;
    let admin = world.admin(&[&repo]);

    let part = world
        .get_with(&repo, "a.bin", &admin, &[("range", "bytes=2-5")])
        .await;
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_of(&part, "content-range"), Some("bytes 2-5/10"));
    assert_eq!(&body_bytes(part).await[..], b"2345");

    let suffix = world
        .get_with(&repo, "a.bin", &admin, &[("range", "bytes=-3")])
        .await;
    assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body_bytes(suffix).await[..], b"789");

    let past = world
        .get_with(&repo, "a.bin", &admin, &[("range", "bytes=20-30")])
        .await;
    assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header_of(&past, "content-range"), Some("bytes */10"));
}

#[tokio::test]
async fn answers_conditional_reads_with_304() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a.txt", b"payload").await;
    let admin = world.admin(&[&repo]);
    let first = world.get(&repo, "a.txt", &admin).await;
    let etag = header_of(&first, "etag").unwrap().to_owned();
    let modified = header_of(&first, "last-modified").unwrap().to_owned();

    let not_modified = world
        .get_with(&repo, "a.txt", &admin, &[("if-none-match", &etag)])
        .await;
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    assert!(body_bytes(not_modified).await.is_empty());

    let by_date = world
        .get_with(&repo, "a.txt", &admin, &[("if-modified-since", &modified)])
        .await;
    assert_eq!(by_date.status(), StatusCode::NOT_MODIFIED);

    let changed = world
        .get_with(&repo, "a.txt", &admin, &[("if-none-match", "\"other\"")])
        .await;
    assert_eq!(changed.status(), StatusCode::OK);

    let failed = world
        .get_with(&repo, "a.txt", &admin, &[("if-match", "\"other\"")])
        .await;
    assert_eq!(failed.status(), StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn a_conditional_put_checks_what_it_overwrites() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);

    // If-None-Match: * creates only.
    let created = world
        .put_with(&repo, "once.txt", &admin, b"one", &[("if-none-match", "*")])
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let etag = header_of(&created, "etag").unwrap().to_owned();
    let refused = world
        .put_with(&repo, "once.txt", &admin, b"two", &[("if-none-match", "*")])
        .await;
    assert_eq!(refused.status(), StatusCode::PRECONDITION_FAILED);

    // If-Match replaces only the version the client saw.
    let replaced = world
        .put_with(&repo, "once.txt", &admin, b"three", &[("if-match", &etag)])
        .await;
    assert_eq!(replaced.status(), StatusCode::OK);
    let stale = world
        .put_with(&repo, "once.txt", &admin, b"four", &[("if-match", &etag)])
        .await;
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
    let missing = world
        .put_with(&repo, "none.txt", &admin, b"x", &[("if-match", "*")])
        .await;
    assert_eq!(missing.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        &body_bytes(world.get(&repo, "once.txt", &admin).await).await[..],
        b"three"
    );
}

#[tokio::test]
async fn anonymous_gets_401_a_reader_cannot_write_and_a_stranger_sees_nothing() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a.txt", b"secret").await;
    world.grant_read("reader", &[&repo]);

    let anonymous_read = world.get(&repo, "a.txt", &anonymous()).await;
    assert_eq!(anonymous_read.status(), StatusCode::UNAUTHORIZED);

    let reader = principal("reader");
    assert_eq!(
        world.get(&repo, "a.txt", &reader).await.status(),
        StatusCode::OK
    );
    let write = world.put(&repo, "b.txt", &reader, b"x").await;
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
    let delete = world.delete(&repo, "a.txt", &reader).await;
    assert_eq!(delete.status(), StatusCode::FORBIDDEN);

    // A caller who may not even see the repository learns nothing about it.
    let stranger = principal("stranger");
    assert_eq!(
        world.get(&repo, "a.txt", &stranger).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.put(&repo, "b.txt", &anonymous(), b"x").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn must_not_allow_anonymous_access_without_the_repository_flag() {
    let world = World::new();
    let mut repository = rustryx_testkit::fixtures::repository(
        "public",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repository.allow_anonymous = false;
    let repo = world.add(repository).await;
    world.seed(&repo, "a.txt", b"x").await;
    world.rbac.grant_read("anonymous", "public");
    let response = world.get(&repo, "a.txt", &anonymous()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_write_policy_decides_about_a_second_upload() {
    let world = World::new();
    let deny = world.hosted_with("sealed", WritePolicy::Deny).await;
    let once = world.hosted_with("once", WritePolicy::AllowOnce).await;
    let open = world.hosted_with("open", WritePolicy::Allow).await;
    let admin = world.admin(&[&deny, &once, &open]);

    assert_eq!(
        world.put(&deny, "a", &admin, b"1").await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        world.delete(&deny, "a", &admin).await.status(),
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        world.put(&once, "a", &admin, b"1").await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        world.put(&once, "a", &admin, b"2").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        &body_bytes(world.get(&once, "a", &admin).await).await[..],
        b"1"
    );

    assert_eq!(
        world.put(&open, "a", &admin, b"1").await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        world.put(&open, "a", &admin, b"2").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        &body_bytes(world.get(&open, "a", &admin).await).await[..],
        b"2"
    );
}

#[tokio::test]
async fn a_path_is_a_file_or_a_directory_never_both() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);

    assert_eq!(
        world.put(&repo, "a", &admin, b"file").await.status(),
        StatusCode::CREATED
    );
    // `a` is a file: nothing may be created below it.
    let below = world.put(&repo, "a/b", &admin, b"x").await;
    assert_eq!(below.status(), StatusCode::CONFLICT);
    let deeper = world.put(&repo, "a/b/c/d", &admin, b"x").await;
    assert_eq!(deeper.status(), StatusCode::CONFLICT);

    assert_eq!(
        world.put(&repo, "d/x", &admin, b"x").await.status(),
        StatusCode::CREATED
    );
    // `d` is a directory prefix now: it cannot become a file.
    let over = world.put(&repo, "d", &admin, b"x").await;
    assert_eq!(over.status(), StatusCode::CONFLICT);
    let deep = world.put(&repo, "e/f/g", &admin, b"x").await;
    assert_eq!(deep.status(), StatusCode::CREATED);
    assert_eq!(
        world.put(&repo, "e/f", &admin, b"x").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        world.put(&repo, "e", &admin, b"x").await.status(),
        StatusCode::CONFLICT
    );

    // Siblings are fine, and an existing file can be overwritten.
    assert_eq!(
        world.put(&repo, "d/y", &admin, b"y").await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        world.put(&repo, "d/x", &admin, b"z").await.status(),
        StatusCode::OK
    );

    // Once the last file below a directory is gone the path is free again.
    assert_eq!(
        world.delete(&repo, "d/x", &admin).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world.put(&repo, "d", &admin, b"x").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        world.delete(&repo, "d/y", &admin).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world.put(&repo, "d", &admin, b"now a file").await.status(),
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn the_trailing_slash_never_turns_a_file_into_a_directory() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world.seed(&repo, "f.txt", b"file").await;
    world.seed(&repo, "dir/x.txt", b"x").await;

    // `f.txt/` addresses a directory that does not exist; `dir` addresses a file that does not.
    assert_eq!(
        world.get(&repo, "f.txt/", &admin).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.get(&repo, "dir", &admin).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.head_of(&repo, "dir", &admin).await.status(),
        StatusCode::NOT_FOUND
    );

    // A directory takes no write and no delete.
    for method in [Method::PUT, Method::DELETE, Method::POST] {
        let response = world
            .send(
                &repo,
                method.clone(),
                "dir/",
                &admin,
                axum_core::body::Body::empty(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
        assert_eq!(header_of(&response, "allow"), Some("GET, HEAD"));
    }
    let root_put = world.put(&repo, "", &admin, b"x").await;
    assert_eq!(root_put.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        world.get(&repo, "dir/x.txt", &admin).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn unsupported_methods_name_the_ones_that_work() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    for method in [Method::POST, Method::PATCH, Method::OPTIONS] {
        let response = world
            .send(
                &repo,
                method.clone(),
                "a.txt",
                &admin,
                axum_core::body::Body::empty(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
        assert_eq!(
            header_of(&response, "allow"),
            Some("GET, HEAD, PUT, DELETE")
        );
    }
}

#[tokio::test]
async fn must_not_resolve_traversal_in_any_form_and_authorization_never_sees_it() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world.seed(&repo, "ok.txt", b"x").await;
    world.rbac.clear_checks();

    let vectors = [
        "../x",
        "a/../b",
        "a/..",
        "%2e%2e/x",
        "%2E%2E%2Fx",
        "a/%2e%2e/b",
        ".%2e/x",
        "a/..%2fb",
        "..%5cx",
        "a%5cb",
        "a\\b",
        "a%00b",
        "a%0ab",
        "%252e%252e/x",
        "a//b",
        "//x",
        "a%2fb",
        "./a",
        "a/./b",
        "a%E2%80%AEb",
    ];
    for vector in vectors {
        for method in [Method::GET, Method::HEAD, Method::PUT, Method::DELETE] {
            let response = world
                .send(
                    &repo,
                    method.clone(),
                    vector,
                    &admin,
                    axum_core::body::Body::from("x"),
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{method} {vector}"
            );
        }
    }
    assert!(
        world.rbac.checks().is_empty(),
        "authorization saw a path that was refused: {:?}",
        world.rbac.checks()
    );
    // Nothing was written by any of them.
    assert_eq!(
        world.get(&repo, "ok.txt", &admin).await.status(),
        StatusCode::OK
    );
    let listing = world
        .get_with(&repo, "", &admin, &[("accept", "application/json")])
        .await;
    assert_eq!(entry_names(&body_json(listing).await), ["ok.txt"]);
}

#[tokio::test]
async fn refuses_overlong_and_too_deep_paths() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    let long_segment = "a".repeat(256);
    assert_eq!(
        world.get(&repo, &long_segment, &admin).await.status(),
        StatusCode::BAD_REQUEST
    );
    let deep = vec!["d"; 40].join("/");
    assert_eq!(
        world.put(&repo, &deep, &admin, b"x").await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn must_not_serve_stored_html_inline() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    let page = b"<html><script>alert(document.cookie)</script></html>";
    world
        .put_typed(&repo, "site/index.html", &admin, page, Some("text/html"))
        .await;

    let got = world.get(&repo, "site/index.html", &admin).await;
    assert_eq!(got.status(), StatusCode::OK);
    assert_eq!(header_of(&got, "content-type"), Some("text/html"));
    let disposition = header_of(&got, "content-disposition").expect("a disposition");
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(disposition.contains("index.html"));
    assert_eq!(header_of(&got, "x-content-type-options"), Some("nosniff"));
    assert!(header_of(&got, "content-security-policy").is_some_and(|v| v.contains("sandbox")));

    // The same holds for a ranged read, svg, xml and a script.
    let ranged = world
        .get_with(&repo, "site/index.html", &admin, &[("range", "bytes=0-5")])
        .await;
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    assert!(header_of(&ranged, "content-disposition").is_some());
    for (path, kind) in [
        ("img.svg", "image/svg+xml"),
        ("data.xml", "application/xml"),
        ("app.js", "text/javascript"),
        ("page.htm", "text/html"),
    ] {
        world
            .put_typed(&repo, path, &admin, b"<svg onload=alert(1)/>", Some(kind))
            .await;
        let response = world.get(&repo, path, &admin).await;
        assert!(
            header_of(&response, "content-disposition")
                .is_some_and(|v| v.starts_with("attachment")),
            "{path}"
        );
        assert_eq!(
            header_of(&response, "x-content-type-options"),
            Some("nosniff")
        );
    }
}

#[tokio::test]
async fn html_declared_for_a_file_without_an_html_name_is_not_stored_as_html() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world
        .put_typed(
            &repo,
            "notes.bin",
            &admin,
            b"<html></html>",
            Some("text/html"),
        )
        .await;
    let got = world.get(&repo, "notes.bin", &admin).await;
    assert_eq!(
        header_of(&got, "content-type"),
        Some("application/octet-stream")
    );
    assert!(header_of(&got, "content-disposition").is_none());
    assert_eq!(header_of(&got, "x-content-type-options"), Some("nosniff"));
}

#[tokio::test]
async fn an_inline_repository_renders_html_but_stays_sandboxed() {
    let world = World::new();
    let repo = world
        .hosted_with_config(
            "site",
            serde_json::json!({ "content_disposition": "inline" }),
        )
        .await;
    let admin = world.admin(&[&repo]);
    world
        .put_typed(
            &repo,
            "index.html",
            &admin,
            b"<html></html>",
            Some("text/html"),
        )
        .await;
    let got = world.get(&repo, "index.html", &admin).await;
    assert_eq!(header_of(&got, "content-type"), Some("text/html"));
    assert!(header_of(&got, "content-disposition").is_none());
    assert_eq!(header_of(&got, "x-content-type-options"), Some("nosniff"));
    assert!(header_of(&got, "content-security-policy").is_some_and(|v| v.contains("sandbox")));
}

#[tokio::test]
async fn a_specific_declared_content_type_is_kept_and_inert_types_get_no_disposition() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world
        .put_typed(
            &repo,
            "x.bin",
            &admin,
            b"{}",
            Some("application/vnd.acme+json"),
        )
        .await;
    let got = world.get(&repo, "x.bin", &admin).await;
    assert_eq!(
        header_of(&got, "content-type"),
        Some("application/vnd.acme+json")
    );
    assert!(header_of(&got, "content-disposition").is_none());
    world.put(&repo, "pkg.tar.gz", &admin, b"gz").await;
    let tarball = world.get(&repo, "pkg.tar.gz", &admin).await;
    assert_eq!(
        header_of(&tarball, "content-type"),
        Some("application/gzip")
    );
}

#[tokio::test]
async fn computes_checksum_side_files_from_the_stored_digests() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world
        .seed(&repo, "dist/app-1.0.tgz", b"archive bytes")
        .await;
    let file = world.get(&repo, "dist/app-1.0.tgz", &admin).await;
    let sha1 = header_of(&file, "x-checksum-sha1").unwrap().to_owned();
    let sha256 = header_of(&file, "x-checksum-sha256").unwrap().to_owned();
    let md5 = header_of(&file, "x-checksum-md5").unwrap().to_owned();

    for (suffix, expected) in [("sha1", &sha1), ("sha256", &sha256), ("md5", &md5)] {
        let path = format!("dist/app-1.0.tgz.{suffix}");
        let response = world.get(&repo, &path, &admin).await;
        assert_eq!(response.status(), StatusCode::OK, "{suffix}");
        assert_eq!(
            header_of(&response, "content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            header_of(&response, "x-content-type-options"),
            Some("nosniff")
        );
        assert!(header_of(&response, "etag").is_some());
        assert_eq!(&body_text(response).await, expected, "{suffix}");
    }

    let head = world
        .head_of(&repo, "dist/app-1.0.tgz.sha256", &admin)
        .await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(header_of(&head, "content-length"), Some("64"));
    assert!(body_bytes(head).await.is_empty());

    let first = world.get(&repo, "dist/app-1.0.tgz.sha1", &admin).await;
    let etag = header_of(&first, "etag").unwrap().to_owned();
    let not_modified = world
        .get_with(
            &repo,
            "dist/app-1.0.tgz.sha1",
            &admin,
            &[("if-none-match", &etag)],
        )
        .await;
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);

    // A missing base file has no side file, and the side file is not listed.
    assert_eq!(
        world
            .get(&repo, "dist/missing.tgz.sha1", &admin)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let listing = world
        .get_with(&repo, "dist/", &admin, &[("accept", "application/json")])
        .await;
    assert_eq!(entry_names(&body_json(listing).await), ["app-1.0.tgz"]);
}

#[tokio::test]
async fn an_uploaded_side_file_wins_over_the_computed_one() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world.seed(&repo, "app.tgz", b"bytes").await;
    world
        .seed(&repo, "app.tgz.sha256", b"deadbeef  app.tgz\n")
        .await;
    let response = world.get(&repo, "app.tgz.sha256", &admin).await;
    assert_eq!(body_text(response).await, "deadbeef  app.tgz\n");
}

#[tokio::test]
async fn must_not_reveal_the_digest_of_a_file_the_caller_may_not_read() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "private/app.tgz", b"bytes").await;
    world.seed(&repo, "public/app.tgz", b"bytes").await;
    world.grant_read("reader", &[&repo]);
    // The selector allows nothing below private/: neither the file nor its side files.
    world.rbac.deny_prefix("files", "private/");
    let reader = principal("reader");
    assert_eq!(
        world.get(&repo, "private/app.tgz", &reader).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        world
            .get(&repo, "public/app.tgz.sha256", &reader)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        world
            .get(&repo, "private/app.tgz.sha256", &reader)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn must_not_hand_out_a_side_file_when_the_base_file_is_closed() {
    // The engine authorizes the side file's own path, which a selector may allow while the base
    // file stays closed; the handler asks about the base file as well.
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "closed/app.tgz", b"bytes").await;
    world.seed(&repo, "open/app.tgz", b"bytes").await;
    world.grant_read("reader", &[&repo]);
    world.rbac.deny_exact("files", "closed/app.tgz");
    let reader = principal("reader");
    assert_eq!(
        world.get(&repo, "closed/app.tgz", &reader).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        world
            .get(&repo, "closed/app.tgz.sha1", &reader)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .get(&repo, "open/app.tgz.sha1", &reader)
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_failing_authorizer_fails_closed() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world.seed(&repo, "a.txt", b"x").await;
    world.rbac.set_failing(true);
    assert_eq!(
        world.get(&repo, "a.txt", &admin).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        world.put(&repo, "b.txt", &admin, b"y").await.status(),
        StatusCode::FORBIDDEN
    );
    world.rbac.set_failing(false);
    assert_eq!(
        world.get(&repo, "b.txt", &admin).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_offline_repository_answers_503() {
    let world = World::new();
    let mut repository = rustryx_testkit::fixtures::repository(
        "off",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repository.online = false;
    let repo = world.add(repository).await;
    let admin = world.admin(&[&repo]);
    assert_eq!(
        world.get(&repo, "a", &admin).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn errors_use_the_generic_envelope_without_internal_text() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    let response = world.get(&repo, "missing.txt", &admin).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        header_of(&response, "content-type"),
        Some("application/json")
    );
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "not_found");
    let bad = world.get(&repo, "a/../b", &admin).await;
    let text = body_text(bad).await;
    assert!(text.contains("invalid path"));
    assert!(!text.contains("traversal"));
}

#[tokio::test]
async fn uploads_and_deletes_are_audited() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world.put(&repo, "a.txt", &admin, b"x").await;
    world.delete(&repo, "a.txt", &admin).await;
    let actions: Vec<String> = world
        .audit
        .events()
        .iter()
        .map(|event| event.action.clone())
        .collect();
    assert!(actions.contains(&"asset.publish".to_owned()), "{actions:?}");
    assert!(actions.contains(&"asset.delete".to_owned()), "{actions:?}");
}
