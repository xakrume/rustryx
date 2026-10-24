//! Directory listings through the real engine: shape, paging, authorization, escaping.
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

use std::sync::Arc;

use http::StatusCode;
use rustryx_core::model::Repository;
use rustryx_core::ports::AssetRepo;
use rustryx_format_raw::RawHandler;
use rustryx_format_raw::listing::MAX_SCANNED;
use rustryx_testkit::fixtures;
use support::{World, anonymous, body_json, body_text, entry_names, header_of, principal};

const JSON: [(&str, &str); 1] = [("accept", "application/json")];

async fn json_listing(world: &World, repo: &Arc<Repository>, path: &str) -> serde_json::Value {
    let admin = principal("admin");
    let response = world.get_with(repo, path, &admin, &JSON).await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    body_json(response).await
}

/// Follows every continuation token of a listing and returns the pages.
async fn all_pages(
    world: &World,
    repo: &Arc<Repository>,
    path: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    let admin = principal("admin");
    let mut pages = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..200 {
        let query = match &token {
            Some(token) => format!("{path}?limit={limit}&token={token}"),
            None => format!("{path}?limit={limit}"),
        };
        let head = world
            .head(http::Method::GET, &query, &admin)
            .with_headers(support::header_map(&JSON));
        // The harness hands the path and the query to the engine separately.
        let (raw_path, raw_query) = query.split_once('?').unwrap();
        let mut head = head;
        head.path = raw_path.to_owned();
        head.query = Some(raw_query.to_owned());
        let response = world
            .send_head(repo, head, axum_core::body::Body::empty())
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{query}");
        let page = body_json(response).await;
        token = page["continuationToken"].as_str().map(str::to_owned);
        pages.push(page);
        if token.is_none() {
            return pages;
        }
    }
    panic!("the listing never ended");
}

#[tokio::test]
async fn lists_the_immediate_children_of_a_directory() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    for (path, data) in [
        ("top.txt", &b"t"[..]),
        ("docs/a.md", b"a"),
        ("docs/b.md", b"bb"),
        ("docs/img/logo.png", b"png"),
        ("docs/img/small.png", b"png"),
        ("src/main.rs", b"fn main() {}"),
    ] {
        let data: &'static [u8] = Box::leak(data.to_vec().into_boxed_slice());
        assert!(
            world
                .put(&repo, path, &admin, data)
                .await
                .status()
                .is_success()
        );
    }

    let root = json_listing(&world, &repo, "").await;
    assert_eq!(entry_names(&root), ["docs/", "src/", "top.txt"]);
    assert_eq!(root["path"], "");
    assert_eq!(root["repository"], "files");
    assert!(root["continuationToken"].is_null());

    let docs = json_listing(&world, &repo, "docs/").await;
    assert_eq!(entry_names(&docs), ["a.md", "b.md", "img/"]);
    assert_eq!(docs["path"], "docs/");
    let file = &docs["items"][1];
    assert_eq!(file["size"], 2);
    assert_eq!(file["contentType"], "text/markdown");
    assert_eq!(file["sha256"].as_str().unwrap().len(), 64);
    assert!(file["lastModified"].as_str().unwrap().ends_with('Z'));

    let img = json_listing(&world, &repo, "docs/img/").await;
    assert_eq!(entry_names(&img), ["logo.png", "small.png"]);
}

#[tokio::test]
async fn an_empty_repository_has_an_empty_root_and_no_other_directory() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    let root = json_listing(&world, &repo, "").await;
    assert!(root["items"].as_array().unwrap().is_empty());
    assert_eq!(
        world
            .get_with(&repo, "nothing/", &admin, &JSON)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn html_is_the_default_and_json_is_negotiated() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "docs/a.txt", b"a").await;
    let admin = world.admin(&[&repo]);

    let html = world.get(&repo, "docs/", &admin).await;
    assert_eq!(html.status(), StatusCode::OK);
    assert_eq!(
        header_of(&html, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(header_of(&html, "x-content-type-options"), Some("nosniff"));
    assert_eq!(header_of(&html, "cache-control"), Some("no-cache"));
    assert_eq!(header_of(&html, "vary"), Some("Accept"));
    let csp = header_of(&html, "content-security-policy")
        .unwrap()
        .to_owned();
    assert!(csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"));
    assert!(!csp.contains("script-src"));
    let text = body_text(html).await;
    assert!(text.contains("Index of /repository/files/docs/"));
    assert!(text.contains("href=\"./a.txt\""));
    assert!(text.contains("href=\"../\""));

    let json = world.get_with(&repo, "docs/", &admin, &JSON).await;
    assert_eq!(header_of(&json, "content-type"), Some("application/json"));

    let browser = world
        .get_with(
            &repo,
            "docs/",
            &admin,
            &[(
                "accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )],
        )
        .await;
    assert_eq!(
        header_of(&browser, "content-type"),
        Some("text/html; charset=utf-8")
    );

    let head = world.head_of(&repo, "docs/", &admin).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert!(header_of(&head, "content-length").is_some());
    assert!(support::body_bytes(head).await.is_empty());
}

#[tokio::test]
async fn must_not_let_a_file_name_turn_the_listing_into_markup_or_a_script_link() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    for name in [
        "%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E.txt",
        "javascript%3Aalert(1)",
        "%22%3E%3Cscript%3Ealert(1)%3C",
        "a%26b%27c.txt",
    ] {
        let response = world.put(&repo, name, &admin, b"x").await;
        assert_eq!(response.status(), StatusCode::CREATED, "{name}");
    }
    let html = body_text(world.get(&repo, "", &admin).await).await;
    assert!(!html.contains("<img"), "{html}");
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("href=\"javascript:"));
    assert!(!html.contains("href=\"./javascript:"));
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;.txt"));
    assert!(html.contains("a&amp;b&#39;c.txt"));
    assert!(html.contains("./javascript%3Aalert%281%29"));
}

#[tokio::test]
async fn pages_cover_every_entry_once_and_follow_the_token() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    let mut expected = Vec::new();
    for dir in ["a", "b", "c"] {
        for n in 0..7 {
            let path = format!("{dir}/f{n}.txt");
            assert!(
                world
                    .put(&repo, &path, &admin, b"x")
                    .await
                    .status()
                    .is_success()
            );
        }
        expected.push(format!("{dir}/"));
    }
    for n in 0..5 {
        let path = format!("top{n}.txt");
        assert!(
            world
                .put(&repo, &path, &admin, b"x")
                .await
                .status()
                .is_success()
        );
        expected.push(path);
    }
    expected.sort();

    for limit in [1, 2, 3, 4, 10, 100] {
        let pages = all_pages(&world, &repo, "", limit).await;
        let mut seen: Vec<String> = pages.iter().flat_map(entry_names).collect();
        seen.sort();
        assert_eq!(seen, expected, "limit {limit}: every entry exactly once");
        for page in &pages[..pages.len() - 1] {
            let count = page["items"].as_array().unwrap().len();
            assert!(
                count >= limit.min(expected.len()) && count < 2 * limit,
                "limit {limit}: a page holds {count}"
            );
        }
    }
    // A directory is paged the same way.
    let pages = all_pages(&world, &repo, "a/", 3).await;
    let names: Vec<String> = pages.iter().flat_map(entry_names).collect();
    assert_eq!(names.len(), 7);
}

#[tokio::test]
async fn a_directory_spanning_two_requests_is_listed_once() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world.put(&repo, "a0.txt", &admin, b"x").await;
    for n in 0..12 {
        let path = format!("big/f{n:02}");
        world.put(&repo, &path, &admin, b"x").await;
    }
    world.put(&repo, "zzz.txt", &admin, b"x").await;
    // limit 2: the first request ends inside `big/`, the carried state keeps the rest of its
    // files from producing a second entry.
    let pages = all_pages(&world, &repo, "", 2).await;
    let names: Vec<String> = pages.iter().flat_map(entry_names).collect();
    assert_eq!(names, ["a0.txt", "big/", "zzz.txt"]);
    assert!(pages.len() > 1, "the scenario must span requests");
}

#[tokio::test]
async fn refuses_a_bad_limit_or_token() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a/x", b"x").await;
    world.seed(&repo, "b/y", b"y").await;
    let admin = world.admin(&[&repo]);
    for query in [
        "limit=0",
        "limit=1001",
        "limit=-1",
        "limit=abc",
        "limit=",
        "token=!!!",
        "token=e30",
    ] {
        let mut head = world.head(http::Method::GET, "", &admin);
        head.query = Some(query.to_owned());
        let response = world
            .send_head(&repo, head, axum_core::body::Body::empty())
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
}

#[tokio::test]
async fn a_token_belongs_to_the_directory_it_was_issued_for() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a/x", b"x").await;
    world.seed(&repo, "b/y", b"y").await;
    let admin = world.admin(&[&repo]);
    let mut head = world.head(http::Method::GET, "", &admin);
    head.query = Some("limit=1".to_owned());
    head.headers = support::header_map(&JSON);
    let page = body_json(
        world
            .send_head(&repo, head, axum_core::body::Body::empty())
            .await,
    )
    .await;
    let token = page["continuationToken"]
        .as_str()
        .expect("a second page")
        .to_owned();
    let mut head = world.head(http::Method::GET, "a/", &admin);
    head.query = Some(format!("token={token}"));
    let response = world
        .send_head(&repo, head, axum_core::body::Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn must_not_list_what_the_caller_may_not_read() {
    let world = World::new();
    let repo = world.hosted("files").await;
    for path in [
        "pub/a.txt",
        "pub/b.txt",
        "secret/x.txt",
        "secret/y.txt",
        "mixed/ok.txt",
        "mixed/hidden.txt",
        "readme.txt",
        "hidden-top.txt",
    ] {
        world.seed(&repo, path, b"x").await;
    }
    world.grant_read("reader", &[&repo]);
    world.rbac.deny_prefix("files", "secret/");
    world.rbac.deny_exact("files", "mixed/hidden.txt");
    world.rbac.deny_exact("files", "hidden-top.txt");
    let reader = principal("reader");

    let root = world.get_with(&repo, "", &reader, &JSON).await;
    assert_eq!(root.status(), StatusCode::OK);
    assert_eq!(
        entry_names(&body_json(root).await),
        ["mixed/", "pub/", "readme.txt"]
    );
    let mixed = world.get_with(&repo, "mixed/", &reader, &JSON).await;
    assert_eq!(entry_names(&body_json(mixed).await), ["ok.txt"]);
    // A directory with nothing readable is a 404, the same as one that does not exist.
    let secret = world.get_with(&repo, "secret/", &reader, &JSON).await;
    assert_eq!(
        secret.status(),
        StatusCode::NOT_FOUND,
        "nothing readable below it"
    );
    let html = body_text(world.get(&repo, "", &reader).await).await;
    assert!(!html.contains("secret"), "{html}");
    assert!(!html.contains("hidden-top"));
}

#[tokio::test]
async fn a_directory_whose_files_are_all_closed_looks_like_a_missing_one() {
    // The caller may browse the directory itself (the selector allows its path), but every
    // file inside is closed: the answer must equal the answer for a directory that is not there.
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "vault/a.txt", b"x").await;
    world.seed(&repo, "vault/b.txt", b"x").await;
    world.grant_read("reader", &[&repo]);
    world.rbac.deny_exact("files", "vault/a.txt");
    world.rbac.deny_exact("files", "vault/b.txt");
    let reader = principal("reader");
    let closed = world.get_with(&repo, "vault/", &reader, &JSON).await;
    let missing = world.get_with(&repo, "nothing/", &reader, &JSON).await;
    assert_eq!(closed.status(), StatusCode::NOT_FOUND);
    assert_eq!(closed.status(), missing.status());
    assert_eq!(body_text(closed).await, body_text(missing).await);
}

#[tokio::test]
async fn browse_without_read_lists_nothing() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a.txt", b"x").await;
    world.rbac.grant(
        "browser",
        "files",
        &[rustryx_core::identity::Action::Browse],
    );
    let browser = principal("browser");
    let root = world.get_with(&repo, "", &browser, &JSON).await;
    assert_eq!(root.status(), StatusCode::OK);
    assert!(
        body_json(root).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn must_not_scan_more_than_the_bound_for_one_request() {
    const BOUND: usize = 300;
    let world = World::with_handler(RawHandler::new().with_scan_limit(BOUND));
    let repo = world.hosted("files").await;
    let assets: Arc<dyn AssetRepo> = Arc::new(world.store.clone());
    // More files than one request may look at, all of them closed to the caller.
    let total = BOUND + 100;
    for n in 0..total {
        let new = fixtures::new_asset(repo.id, &format!("closed/f{n:05}"), b"x");
        assets.publish(&new).await.unwrap();
    }
    let new = fixtures::new_asset(repo.id, "zz-open.txt", b"x");
    assets.publish(&new).await.unwrap();
    world.grant_read("reader", &[&repo]);
    world.rbac.deny_prefix("files", "closed/");
    let reader = principal("reader");
    world.rbac.clear_checks();

    let mut head = world.head(http::Method::GET, "", &reader);
    head.query = Some("limit=50".to_owned());
    head.headers = support::header_map(&JSON);
    let first = world
        .send_head(&repo, head, axum_core::body::Body::empty())
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = body_json(first).await;
    // The scan stopped at the bound, handing back a token instead of working on.
    assert!(first["items"].as_array().unwrap().is_empty());
    assert!(first["continuationToken"].is_string());
    let reads = world
        .rbac
        .checks()
        .iter()
        .filter(|(_, action, path)| {
            *action == rustryx_core::identity::Action::Read && path.starts_with("closed/")
        })
        .count();
    assert_eq!(reads, BOUND, "one request, at most the bound in lookups");

    // Following the token reaches the open file.
    let mut token = first["continuationToken"].as_str().unwrap().to_owned();
    let mut names = Vec::new();
    for _ in 0..10 {
        let mut head = world.head(http::Method::GET, "", &reader);
        head.query = Some(format!("limit=50&token={token}"));
        head.headers = support::header_map(&JSON);
        let page = body_json(
            world
                .send_head(&repo, head, axum_core::body::Body::empty())
                .await,
        )
        .await;
        names.extend(entry_names(&page));
        match page["continuationToken"].as_str() {
            Some(next) => next.clone_into(&mut token),
            None => break,
        }
    }
    assert_eq!(names, ["zz-open.txt"]);
}

#[tokio::test]
async fn the_default_scan_bound_is_documented_and_the_handler_clamps_a_zero_bound() {
    assert_eq!(MAX_SCANNED, 5000);
    // A bound of zero would make every listing empty: it is raised to one file.
    let world = World::with_handler(RawHandler::new().with_scan_limit(0));
    let repo = world.hosted("files").await;
    let admin = world.admin(&[&repo]);
    world.put(&repo, "a.txt", &admin, b"x").await;
    world.put(&repo, "b.txt", &admin, b"x").await;
    let mut names = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..10 {
        let mut head = world.head(http::Method::GET, "", &admin);
        head.query = Some(match &token {
            Some(token) => format!("limit=1&token={token}"),
            None => "limit=1".to_owned(),
        });
        head.headers = support::header_map(&JSON);
        let page = body_json(
            world
                .send_head(&repo, head, axum_core::body::Body::empty())
                .await,
        )
        .await;
        names.extend(entry_names(&page));
        token = page["continuationToken"].as_str().map(str::to_owned);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(names, ["a.txt", "b.txt"]);
}

#[tokio::test]
async fn anonymous_and_unknown_callers_get_the_usual_refusals() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a.txt", b"x").await;
    assert_eq!(
        world.get(&repo, "", &anonymous()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        world.get(&repo, "", &principal("stranger")).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_failing_read_check_stops_instead_of_walking_every_file() {
    // Browse is allowed, so the handler itself lists. The first `read` lookup fails. The
    // answer is one refusal: the provider error is not in the body, and the remaining files
    // are not checked (that would be one failure line per file).
    let world = World::new();
    let repo = world.hosted("files").await;
    for n in 0..8 {
        world.seed(&repo, &format!("f{n}.txt"), b"x").await;
    }
    world.grant_read("reader", &[&repo]);
    world.rbac.set_fail_reads(true);
    world.rbac.clear_checks();
    let response = world.get_with(&repo, "", &principal("reader"), &JSON).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let text = body_text(response).await;
    assert!(!text.contains("provider down"), "{text}");
    let reads = world
        .rbac
        .checks()
        .iter()
        .filter(|(_, action, _)| *action == rustryx_core::identity::Action::Read)
        .count();
    assert_eq!(
        reads, 1,
        "the listing must not continue after the authorizer fails"
    );
}

#[tokio::test]
async fn a_failing_authorizer_makes_the_listing_fail_closed() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.seed(&repo, "a.txt", b"x").await;
    let admin = world.admin(&[&repo]);
    world.rbac.set_failing(true);
    let response = world.get(&repo, "", &admin).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
