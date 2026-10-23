//! Group pipeline: ordered members, per-member authorization, merging, name claims.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::duration_suboptimal_units,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::similar_names,
    clippy::match_wild_err_arm,
    clippy::manual_assert,
    clippy::semicolon_if_nothing_returned,
    clippy::match_single_binding,
    clippy::large_futures,
    reason = "test scenarios read best as straight-line code with local helpers"
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use http::{HeaderValue, Method, StatusCode, header};
use rustryx_core::error::StoreError;
use rustryx_core::model::Repository;
use rustryx_core::ports::{ComponentFilter, ComponentRepo, Page, PageRequest};
use rustryx_format_core::Limits;
use rustryx_format_core::testing::Reply;
use rustryx_testkit::fixtures;
use support::{BlockRouting, World, anonymous, body_bytes, body_text, header_of, principal};

const SECRET: &[u8] = b"internal secret payload";

struct Fixture {
    world: World,
    internal: Arc<Repository>,
    public: Arc<Repository>,
    group: Arc<Repository>,
}

/// Hosted `internal` first, proxy `public` second, group `all` over both.
async fn fixture() -> Fixture {
    let world = World::new();
    let internal = world.hosted("internal").await;
    let public = world.proxy("public").await;
    let group = world.group("all", &[&internal, &public]).await;
    world.allow_all("alice", &[&internal, &public, &group]);
    Fixture {
        world,
        internal,
        public,
        group,
    }
}

#[tokio::test]
async fn first_member_that_has_the_artifact_answers() {
    let Fixture {
        world,
        internal,
        group,
        ..
    } = fixture().await;
    let alice = principal("alice");
    world.put(&internal, "lib/a.bin", &alice, SECRET).await;
    let response = world.get(&group, "lib/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], SECRET);
    assert_eq!(
        world.upstream.calls(),
        0,
        "the proxy member is not asked when the hosted one has it"
    );
}

#[tokio::test]
async fn a_member_404_is_discarded_and_the_next_member_is_tried() {
    let Fixture { world, group, .. } = fixture().await;
    world.upstream.route(
        "/lib/only-public.bin",
        vec![Reply::ok(&b"from upstream"[..])],
    );
    let response = world
        .get(&group, "lib/only-public.bin", &principal("alice"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], b"from upstream");
    world.settle().await;
    assert_eq!(
        world.blobs.len(),
        1,
        "the proxy member cached what it fetched"
    );
}

#[tokio::test]
async fn nothing_anywhere_is_404() {
    let Fixture { world, group, .. } = fixture().await;
    world
        .upstream
        .route("/lib/none.bin", vec![Reply::status(404)]);
    let response = world.get(&group, "lib/none.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn must_not_leak_data_through_group_without_member_rights() {
    let Fixture {
        world,
        internal,
        group,
        ..
    } = fixture().await;
    world
        .put(&internal, "team/secret.bin", &principal("alice"), SECRET)
        .await;
    // Bob may use the group and the proxy, but has no rights on the hosted member.
    world.rbac.grant_read("bob", "all");
    world.rbac.grant_read("bob", "public");
    world
        .upstream
        .route("/team/secret.bin", vec![Reply::status(404)]);
    let response = world
        .get(&group, "team/secret.bin", &principal("bob"))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a hidden member is indistinguishable from a missing artifact"
    );
    let text = body_text(response).await;
    assert!(!text.contains("secret payload"));
    // The member was never asked: its authorization was checked and denied.
    let checks = world.rbac.checks();
    assert!(
        checks
            .iter()
            .any(|(user, repo, _, _)| user == "bob" && repo == "internal")
    );
    // Alice has rights on the member and gets the data through the very same group.
    let response = world
        .get(&group, "team/secret.bin", &principal("alice"))
        .await;
    assert_eq!(&body_bytes(response).await[..], SECRET);
}

#[tokio::test]
async fn rights_on_a_member_alone_do_not_open_the_group() {
    let Fixture {
        world,
        internal,
        group,
        ..
    } = fixture().await;
    world
        .put(&internal, "team/secret.bin", &principal("alice"), SECRET)
        .await;
    world.rbac.grant_read("carol", "internal");
    let response = world
        .get(&group, "team/secret.bin", &principal("carol"))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "no rights on the group itself"
    );
}

#[tokio::test]
async fn must_not_serve_a_member_when_its_authorization_lookup_fails() {
    let Fixture {
        world,
        internal,
        group,
        ..
    } = fixture().await;
    world
        .put(&internal, "team/secret.bin", &principal("alice"), SECRET)
        .await;
    world
        .upstream
        .route("/team/secret.bin", vec![Reply::status(404)]);
    world.rbac.fail_repository("internal");
    let response = world
        .get(&group, "team/secret.bin", &principal("alice"))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "fail closed, never 'no groups'"
    );
    assert!(!body_text(response).await.contains("secret payload"));
}

#[tokio::test]
async fn anonymous_callers_only_reach_members_that_allow_anonymous_access() {
    let world = World::new();
    let mut open = fixtures::repository(
        "open",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    open.allow_anonymous = true;
    let open = world.add(open).await;
    let closed = world.hosted("closed").await;
    let mut group = fixtures::repository(
        "pub",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Group,
    );
    group.allow_anonymous = true;
    group.group = Some(rustryx_core::model::GroupConfig {
        members: vec![closed.name.clone(), open.name.clone()],
    });
    let group = world.add(group).await;
    world.put_as_admin(&open, "x/open.bin", b"open data").await;
    world
        .put_as_admin(&closed, "x/closed.bin", b"closed data")
        .await;
    for name in ["pub", "open", "closed"] {
        world.rbac.grant_read("anonymous", name);
    }
    let response = world.get(&group, "x/open.bin", &anonymous()).await;
    assert_eq!(&body_bytes(response).await[..], b"open data");
    let response = world.get(&group, "x/closed.bin", &anonymous()).await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "the closed member is skipped for anonymous callers"
    );
}

#[tokio::test]
async fn a_failing_member_is_skipped_but_remembered() {
    let Fixture {
        world,
        internal,
        public,
        group,
    } = fixture().await;
    let alice = principal("alice");
    // Order: proxy first, hosted second.
    let swapped = world.group("swapped", &[&public, &internal]).await;
    world.allow_all("alice", &[&swapped]);
    world.upstream.route("/lib/x.bin", vec![Reply::status(500)]);
    world.put(&internal, "lib/x.bin", &alice, SECRET).await;

    // (a) the hosted member has it: served, with a marker that a member was skipped.
    let response = world.get(&swapped, "lib/x.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "x-rustryx-group-skipped"),
        Some("1")
    );
    assert_eq!(&body_bytes(response).await[..], SECRET);

    // (b) nobody has it and a member failed: not 404, because the failed member may have had it.
    world
        .upstream
        .route("/other/y.bin", vec![Reply::status(500)]);
    let response = world.get(&group, "other/y.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_ne!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn must_not_forward_a_member_auth_failure_through_the_group() {
    let Fixture { world, group, .. } = fixture().await;
    world.upstream.route("/lib/z.bin", vec![Reply::status(401)]);
    let response = world.get(&group, "lib/z.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
    world.upstream.route("/lib/w.bin", vec![Reply::status(403)]);
    let response = world.get(&group, "lib/w.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_has() {
    let Fixture {
        world,
        internal,
        group,
        ..
    } = fixture().await;
    let alice = principal("alice");
    world
        .put(&internal, "internal-lib/1.0.bin", &alice, SECRET)
        .await;
    // The attacker published a higher version of the same name on the public registry.
    world
        .upstream
        .route("/internal-lib/99.0.bin", vec![Reply::ok(&b"malicious"[..])]);
    let response = world.get(&group, "internal-lib/99.0.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        world.upstream.calls(),
        0,
        "the claimed name must never reach the public member"
    );
    // A name the hosted member does not have still comes from the proxy.
    world
        .upstream
        .route("/public-lib/1.0.bin", vec![Reply::ok(&b"public"[..])]);
    let response = world.get(&group, "public-lib/1.0.bin", &alice).await;
    assert_eq!(&body_bytes(response).await[..], b"public");
}

#[tokio::test]
async fn the_claim_holds_even_when_the_caller_cannot_read_the_claiming_member() {
    let Fixture {
        world,
        internal,
        group,
        public,
    } = fixture().await;
    world
        .put(
            &internal,
            "internal-lib/1.0.bin",
            &principal("alice"),
            SECRET,
        )
        .await;
    world
        .upstream
        .route("/internal-lib/99.0.bin", vec![Reply::ok(&b"malicious"[..])]);
    world.rbac.grant_read("bob", group.name.as_str());
    world.rbac.grant_read("bob", public.name.as_str());
    let response = world
        .get(&group, "internal-lib/99.0.bin", &principal("bob"))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        world.upstream.calls(),
        0,
        "bob must not be exposed to the look-alike either"
    );
}

#[tokio::test]
async fn a_failing_claim_lookup_fails_closed() {
    struct FailingComponents(rustryx_testkit::MemStore);

    #[async_trait]
    impl ComponentRepo for FailingComponents {
        async fn find_or_create(
            &self,
            new: &rustryx_core::model::NewComponent,
        ) -> Result<rustryx_core::model::Component, StoreError> {
            self.0.find_or_create(new).await
        }
        async fn get(
            &self,
            id: rustryx_core::ids::ComponentId,
        ) -> Result<Option<rustryx_core::model::Component>, StoreError> {
            ComponentRepo::get(&self.0, id).await
        }
        async fn find(
            &self,
            new: &rustryx_core::model::NewComponent,
        ) -> Result<Option<rustryx_core::model::Component>, StoreError> {
            self.0.find(new).await
        }
        async fn list(
            &self,
            _filter: &ComponentFilter,
            _page: &PageRequest,
        ) -> Result<Page<rustryx_core::model::Component>, StoreError> {
            Err(StoreError::Unavailable("components down".into()))
        }
        async fn delete(&self, id: rustryx_core::ids::ComponentId) -> Result<(), StoreError> {
            ComponentRepo::delete(&self.0, id).await
        }
        async fn delete_checked(
            &self,
            id: rustryx_core::ids::ComponentId,
            options: &rustryx_core::model::WriteOptions,
        ) -> Result<(), rustryx_core::error::MutationError> {
            ComponentRepo::delete_checked(&self.0, id, options).await
        }
    }

    // The wrapper only has to fail `list`; the store behind it is irrelevant here.
    let world = World::with_ports(|ports| {
        ports.components = Arc::new(FailingComponents(rustryx_testkit::MemStore::new()));
    });
    let internal = world.hosted("internal").await;
    let public = world.proxy("public").await;
    let group = world.group("all", &[&internal, &public]).await;
    world.allow_all("alice", &[&internal, &public, &group]);
    world
        .upstream
        .route("/some/lib.bin", vec![Reply::ok(&b"public copy"[..])]);
    let response = world.get(&group, "some/lib.bin", &principal("alice")).await;
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "when claims cannot be evaluated the proxy member must be skipped"
    );
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn proxy_members_before_the_hosted_member_are_not_affected_by_its_claim() {
    let Fixture {
        world,
        internal,
        public,
        ..
    } = fixture().await;
    let alice = principal("alice");
    let proxy_first = world.group("proxy-first", &[&public, &internal]).await;
    world.allow_all("alice", &[&proxy_first]);
    world
        .put(&internal, "internal-lib/1.0.bin", &alice, SECRET)
        .await;
    world.upstream.route(
        "/internal-lib/99.0.bin",
        vec![Reply::ok(&b"from proxy"[..])],
    );
    let response = world
        .get(&proxy_first, "internal-lib/99.0.bin", &alice)
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "claims bind later members only: put hosted members first"
    );
}

#[tokio::test]
async fn index_documents_of_all_members_are_merged_in_group_order() {
    let world = World::new();
    let one = world
        .proxy_at("one", "https://upstream.example.test/one/")
        .await;
    let two = world
        .proxy_at("two", "https://upstream.example.test/two/")
        .await;
    let group = world.group("both", &[&one, &two]).await;
    world.allow_all("alice", &[&one, &two, &group]);
    world
        .upstream
        .route("/one/pkg/list.index", vec![Reply::ok(&b"a\nb"[..])]);
    world
        .upstream
        .route("/two/pkg/list.index", vec![Reply::ok(&b"b\nc"[..])]);
    let alice = principal("alice");
    let response = world.get(&group, "pkg/list.index", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "content-type"),
        Some("text/plain")
    );
    assert_eq!(header_of(response.headers(), "content-length"), Some("5"));
    assert!(response.headers().contains_key("etag"));
    assert_eq!(&body_bytes(response).await[..], b"a\nb\nc");

    // Reversing the group order reverses the precedence.
    let reversed = world.group("reversed", &[&two, &one]).await;
    world.allow_all("alice", &[&reversed]);
    assert_eq!(
        &body_bytes(world.get(&reversed, "pkg/list.index", &alice).await).await[..],
        b"b\nc\na"
    );
}

#[tokio::test]
async fn merged_index_supports_head_and_if_none_match() {
    let world = World::new();
    let one = world
        .proxy_at("one", "https://upstream.example.test/one/")
        .await;
    let two = world
        .proxy_at("two", "https://upstream.example.test/two/")
        .await;
    let group = world.group("both", &[&one, &two]).await;
    world.allow_all("alice", &[&one, &two, &group]);
    world
        .upstream
        .route("/one/pkg/list.index", vec![Reply::ok(&b"a"[..])]);
    world
        .upstream
        .route("/two/pkg/list.index", vec![Reply::ok(&b"b"[..])]);
    let alice = principal("alice");
    let response = world.get(&group, "pkg/list.index", &alice).await;
    let etag = header_of(response.headers(), "etag").unwrap().to_owned();

    let mut head = world.head(Method::GET, "pkg/list.index", &alice);
    head.headers
        .insert(header::IF_NONE_MATCH, HeaderValue::from_str(&etag).unwrap());
    let response = world.send_with(&group, head, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    // A validator of one member's document must not make the merged answer a 304.
    let mut head = world.head(Method::GET, "pkg/list.index", &alice);
    head.headers.insert(
        header::IF_NONE_MATCH,
        HeaderValue::from_static("\"member-etag\""),
    );
    head.headers
        .insert(header::RANGE, HeaderValue::from_static("bytes=0-0"));
    let response = world.send_with(&group, head, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], b"a\nb");

    let response = world
        .send(
            &group,
            Method::HEAD,
            "pkg/list.index",
            &alice,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_of(response.headers(), "content-length"), Some("3"));
    assert!(body_bytes(response).await.is_empty());
}

#[tokio::test]
async fn merge_skips_failed_members_and_marks_the_result_incomplete() {
    let world = World::new();
    let one = world
        .proxy_at("one", "https://upstream.example.test/one/")
        .await;
    let two = world
        .proxy_at("two", "https://upstream.example.test/two/")
        .await;
    let group = world.group("both", &[&one, &two]).await;
    world.allow_all("alice", &[&one, &two, &group]);
    world
        .upstream
        .route("/one/pkg/list.index", vec![Reply::status(500)]);
    world
        .upstream
        .route("/two/pkg/list.index", vec![Reply::ok(&b"b\nc"[..])]);
    let response = world
        .get(&group, "pkg/list.index", &principal("alice"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "x-rustryx-group-skipped"),
        Some("1")
    );
    assert_eq!(
        header_of(response.headers(), "cache-control"),
        Some("no-store")
    );
    assert_eq!(&body_bytes(response).await[..], b"b\nc");
}

#[tokio::test]
async fn merge_only_includes_members_the_caller_may_read() {
    let world = World::new();
    let hosted_a = world.hosted("hosted-a").await;
    let hosted_b = world.hosted("hosted-b").await;
    let group = world.group("both", &[&hosted_a, &hosted_b]).await;
    world.allow_all("alice", &[&hosted_a, &hosted_b, &group]);
    let alice = principal("alice");
    world
        .put(&hosted_a, "pkg/list.index", &alice, b"public-line")
        .await;
    world
        .put(&hosted_b, "pkg/list.index", &alice, b"secret-line")
        .await;
    world.rbac.grant_read("bob", "both");
    world.rbac.grant_read("bob", "hosted-a");
    let merged = body_text(world.get(&group, "pkg/list.index", &principal("bob")).await).await;
    assert_eq!(merged, "public-line");
    assert!(
        !merged.contains("secret-line"),
        "must_not_leak_data_through_group_without_member_rights"
    );
    let merged = body_text(world.get(&group, "pkg/list.index", &alice).await).await;
    assert_eq!(merged, "public-line\nsecret-line");
}

#[tokio::test]
async fn merge_applies_name_claims_to_proxy_members() {
    let Fixture {
        world,
        internal,
        group,
        ..
    } = fixture().await;
    let alice = principal("alice");
    world
        .put(&internal, "internal-lib/list.index", &alice, b"1.0")
        .await;
    world
        .upstream
        .route("/internal-lib/list.index", vec![Reply::ok(&b"99.0"[..])]);
    let response = world.get(&group, "internal-lib/list.index", &alice).await;
    assert_eq!(
        &body_bytes(response).await[..],
        b"1.0",
        "only the claiming hosted member contributes"
    );
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn must_not_buffer_more_than_the_merge_cap() {
    let limits = Limits {
        group_merge_cap_bytes: 12,
        ..Limits::default()
    };
    let world = World::with(limits);
    let one = world
        .proxy_at("one", "https://upstream.example.test/one/")
        .await;
    let two = world
        .proxy_at("two", "https://upstream.example.test/two/")
        .await;
    let group = world.group("both", &[&one, &two]).await;
    world.allow_all("alice", &[&one, &two, &group]);
    world
        .upstream
        .route("/one/pkg/list.index", vec![Reply::ok(vec![b'a'; 8])]);
    world
        .upstream
        .route("/two/pkg/list.index", vec![Reply::ok(vec![b'b'; 8])]);
    let response = world
        .get(&group, "pkg/list.index", &principal("alice"))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(body_text(response).await, "RAW-ERROR index_too_large");
}

#[tokio::test]
async fn artifacts_are_streamed_through_the_group_not_buffered() {
    let limits = Limits {
        group_merge_cap_bytes: 4,
        max_document_bytes: 4,
        ..Limits::default()
    };
    let world = World::with(limits);
    let internal = world.hosted("internal").await;
    let group = world.group("all", &[&internal]).await;
    world.allow_all("alice", &[&internal, &group]);
    let alice = principal("alice");
    world.put(&internal, "lib/big.bin", &alice, SECRET).await;
    let response = world.get(&group, "lib/big.bin", &alice).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the merge cap does not apply to artifact bodies"
    );
    assert_eq!(&body_bytes(response).await[..], SECRET);
}

#[tokio::test]
async fn a_cyclic_group_does_not_recurse_forever() {
    let world = World::new();
    let hosted = world.hosted("h").await;
    let g1 = world.group("g1", &[&hosted]).await;
    let g2 = world.group("g2", &[&g1]).await;
    world.allow_all("alice", &[&hosted, &g1, &g2]);
    // g1 now also lists g2: g1 -> g2 -> g1 -> ...
    let mut cyclic = (*g1).clone();
    cyclic.group = Some(rustryx_core::model::GroupConfig {
        members: vec![g2.name.clone(), hosted.name.clone()],
    });
    rustryx_core::ports::RepositoryRepo::update(&world.store, &cyclic)
        .await
        .unwrap();
    let alice = principal("alice");
    world.put(&hosted, "x/a.bin", &alice, SECRET).await;
    let response = tokio::time::timeout(Duration::from_secs(5), world.get(&g2, "x/a.bin", &alice))
        .await
        .expect("a cycle must not hang");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the cycle is skipped and the hosted member still answers"
    );
    let missing =
        tokio::time::timeout(Duration::from_secs(5), world.get(&g2, "x/none.bin", &alice))
            .await
            .expect("a cycle must not hang");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_group_that_lists_itself_skips_itself() {
    let world = World::new();
    let hosted = world.hosted("h").await;
    let group = world.group("self", &[&hosted]).await;
    let mut looped = (*group).clone();
    looped.group = Some(rustryx_core::model::GroupConfig {
        members: vec![looped.name.clone(), hosted.name.clone()],
    });
    rustryx_core::ports::RepositoryRepo::update(&world.store, &looped)
        .await
        .unwrap();
    world.allow_all("alice", &[&hosted, &group]);
    let alice = principal("alice");
    world.put(&hosted, "x/a.bin", &alice, SECRET).await;
    assert_eq!(
        world.get(&group, "x/a.bin", &alice).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn nesting_beyond_the_depth_limit_is_refused_not_followed() {
    let limits = Limits {
        max_group_depth: 2,
        ..Limits::default()
    };
    let world = World::with(limits);
    let hosted = world.hosted("h").await;
    let g1 = world.group("g1", &[&hosted]).await;
    let g2 = world.group("g2", &[&g1]).await;
    let g3 = world.group("g3", &[&g2]).await;
    world.allow_all("alice", &[&hosted, &g1, &g2, &g3]);
    let alice = principal("alice");
    world.put(&hosted, "x/a.bin", &alice, SECRET).await;
    assert_eq!(
        world.get(&g2, "x/a.bin", &alice).await.status(),
        StatusCode::OK
    );
    let response = world.get(&g3, "x/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn unknown_offline_and_mismatched_members_are_skipped() {
    let world = World::new();
    let hosted = world.hosted("h").await;
    let mut offline = fixtures::repository(
        "off",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    offline.online = false;
    let offline = world.add(offline).await;
    let other_format = world
        .add(fixtures::repository(
            "npm-h",
            rustryx_core::model::RepoFormat::Npm,
            rustryx_core::model::RepoType::Hosted,
        ))
        .await;
    let mut group = fixtures::repository(
        "mixed",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Group,
    );
    group.group = Some(rustryx_core::model::GroupConfig {
        members: vec![
            rustryx_core::names::RepoName::parse("does-not-exist").unwrap(),
            offline.name.clone(),
            other_format.name.clone(),
            hosted.name.clone(),
        ],
    });
    let group = world.add(group).await;
    world.allow_all("alice", &[&hosted, &group]);
    let alice = principal("alice");
    world.put(&hosted, "x/a.bin", &alice, SECRET).await;
    assert_eq!(
        world.get(&group, "x/a.bin", &alice).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn routing_rules_block_members_and_proxy_repositories() {
    let world = World::with_ports(|ports| {
        ports.routing = Arc::new(BlockRouting {
            blocked: vec![
                ("internal".into(), "blocked/".into()),
                ("public".into(), "blocked/".into()),
            ],
        });
    });
    let internal = world.hosted("internal").await;
    let public = world.proxy("public").await;
    let group = world.group("all", &[&internal, &public]).await;
    world.allow_all("alice", &[&internal, &public, &group]);
    let alice = principal("alice");
    world.put(&internal, "blocked/a.bin", &alice, SECRET).await;
    world
        .upstream
        .route("/blocked/a.bin", vec![Reply::ok(&b"upstream"[..])]);
    // The proxy repository itself refuses the path, and so does the group member.
    assert_eq!(
        world.get(&public, "blocked/a.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.get(&group, "blocked/a.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(world.upstream.calls(), 0);
    world
        .upstream
        .route("/fine/a.bin", vec![Reply::ok(&b"ok"[..])]);
    assert_eq!(
        world.get(&group, "fine/a.bin", &alice).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn writes_to_a_group_are_refused_before_the_body_is_read() {
    let Fixture { world, group, .. } = fixture().await;
    let (polled, body) = support::tracked_body(SECRET);
    let response = world
        .send(&group, Method::PUT, "x/a.bin", &principal("alice"), body)
        .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
    let delete = world
        .send(
            &group,
            Method::DELETE,
            "x/a.bin",
            &principal("alice"),
            Body::empty(),
        )
        .await;
    assert_eq!(delete.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn members_build_urls_for_the_repository_the_client_addressed() {
    let Fixture {
        world,
        group,
        internal,
        ..
    } = fixture().await;
    let alice = principal("alice");
    let direct = body_text(world.get(&internal, "__whoami", &alice).await).await;
    assert_eq!(direct, "origin=internal repo=internal");
    let through_group = body_text(world.get(&group, "__whoami", &alice).await).await;
    assert_eq!(
        through_group, "origin=all repo=internal",
        "the origin stays the group while a member answers"
    );
}

#[tokio::test]
async fn a_client_error_from_the_first_member_that_has_the_artifact_ends_the_search() {
    let Fixture {
        world,
        internal,
        group,
        ..
    } = fixture().await;
    let alice = principal("alice");
    world.put(&internal, "lib/r.bin", &alice, SECRET).await;
    world
        .upstream
        .route("/lib/r.bin", vec![Reply::ok(&b"other copy"[..])]);
    let mut head = world.head(Method::GET, "lib/r.bin", &alice);
    head.headers
        .insert(header::RANGE, HeaderValue::from_static("bytes=999-"));
    let response = world.send_with(&group, head, Body::empty()).await;
    assert_eq!(
        response.status(),
        StatusCode::RANGE_NOT_SATISFIABLE,
        "the range is wrong for every member, so it is answered at once"
    );
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn a_routing_lookup_that_fails_skips_the_member_and_fails_closed() {
    struct FailsForInternal;
    #[async_trait]
    impl rustryx_format_core::RoutingGate for FailsForInternal {
        async fn check(
            &self,
            repository: &Repository,
            _request: &rustryx_format_core::ArtifactRequest,
        ) -> Result<rustryx_format_core::RoutingDecision, rustryx_format_core::FormatError>
        {
            if repository.name.as_str() == "internal" {
                Err(rustryx_format_core::FormatError::internal(
                    "routing store down",
                ))
            } else {
                Ok(rustryx_format_core::RoutingDecision::Allow)
            }
        }
    }
    let world = World::with_ports(|ports| ports.routing = Arc::new(FailsForInternal));
    let internal = world.hosted("internal").await;
    let second = world.hosted("second").await;
    let group = world.group("all", &[&internal, &second]).await;
    world.allow_all("alice", &[&internal, &second, &group]);
    let alice = principal("alice");
    world.put(&internal, "x/a.bin", &alice, SECRET).await;
    // The member whose rules cannot be read is not served; the other member is unaffected.
    assert_eq!(
        world.get(&group, "x/a.bin", &alice).await.status(),
        StatusCode::FORBIDDEN
    );
    world.put(&second, "x/b.bin", &alice, b"second").await;
    assert_eq!(
        &body_bytes(world.get(&group, "x/b.bin", &alice).await).await[..],
        b"second"
    );
}

#[tokio::test]
async fn the_group_pipeline_refuses_unsafe_methods_when_called_directly() {
    let Fixture { world, group, .. } = fixture().await;
    let head = world.head(Method::POST, "x/a.bin", &principal("alice"));
    let request = rustryx_format_core::ArtifactRequest::new(
        &head,
        rustryx_format_core::NormalizedPath::parse("x/a.bin").unwrap(),
        rustryx_core::identity::Action::Read,
    );
    let cx = world.engine.context(group, &head);
    let error = cx.group().resolve(&request).await.unwrap_err();
    assert!(matches!(
        error,
        rustryx_format_core::FormatError::MethodNotAllowed { .. }
    ));
}

#[tokio::test]
async fn a_member_error_that_is_a_server_failure_is_remembered_as_the_groups_failure() {
    let world = World::new();
    let public = world.proxy("public").await;
    let group = world.group("all", &[&public]).await;
    world.allow_all("alice", &[&public, &group]);
    world.upstream.route("/slow/a.bin", vec![Reply::hang()]);
    let response = world.get(&group, "slow/a.bin", &principal("alice")).await;
    assert_eq!(
        response.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "the member's timeout is the group's answer, not a 404"
    );
}
