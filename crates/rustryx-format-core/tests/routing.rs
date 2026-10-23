//! Routing rules through the engine: allow and block rules on proxy and group repositories.
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
use std::sync::atomic::Ordering;
use std::time::Duration;

use http::StatusCode;
use rustryx_core::ids::RoutingRuleId;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::routing::{RoutingMode, RoutingRule};
use rustryx_format_core::RuleRoutingGate;
use rustryx_format_core::testing::Reply;
use rustryx_testkit::fixtures;
use support::doubles::FakeRules;
use support::{World, body_bytes, principal};
use time::OffsetDateTime;

const CONTENT: &[u8] = b"routed bytes";

fn rule(mode: RoutingMode, patterns: &[&str]) -> RoutingRule {
    RoutingRule::new(
        RoutingRuleId::generate(),
        "rule",
        mode,
        patterns.iter().map(|p| (*p).to_owned()).collect(),
        OffsetDateTime::UNIX_EPOCH,
    )
}

fn world_with_rules(ttl: Duration) -> (World, Arc<FakeRules>) {
    let rules = FakeRules::new();
    let gate = Arc::new(RuleRoutingGate::with_ttl(rules.clone(), ttl));
    let world = World::with_ports(|ports| ports.routing = gate);
    (world, rules)
}

async fn proxy_with(world: &World, name: &str, rule: Option<&RoutingRule>) -> Arc<Repository> {
    let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Proxy);
    repository.routing_rule_id = rule.map(|r| r.id);
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    repo
}

#[tokio::test]
async fn a_block_rule_answers_404_without_reaching_the_upstream() {
    let (world, rules) = world_with_rules(Duration::from_secs(60));
    let block = rule(RoutingMode::Block, &["^private/"]);
    rules.put(&block);
    let repo = proxy_with(&world, "mirror", Some(&block)).await;
    world
        .upstream
        .route("/private/a.bin", vec![Reply::ok(CONTENT)]);
    world
        .upstream
        .route("/public/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");

    assert_eq!(
        world.get(&repo, "private/a.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.upstream.calls(),
        0,
        "a blocked path never leaves the node"
    );
    let allowed = world.get(&repo, "public/a.bin", &alice).await;
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(&body_bytes(allowed).await[..], CONTENT);
}

#[tokio::test]
async fn an_allow_rule_blocks_every_path_that_matches_nothing() {
    let (world, rules) = world_with_rules(Duration::from_secs(60));
    let allow = rule(RoutingMode::Allow, &["^com/acme/", r"\.pom$"]);
    rules.put(&allow);
    let repo = proxy_with(&world, "mirror", Some(&allow)).await;
    for path in ["com/acme/lib/1.0/lib.jar", "org/other/x.pom"] {
        world
            .upstream
            .route(&format!("/{path}"), vec![Reply::ok(CONTENT)]);
    }
    world
        .upstream
        .route("/org/other/x.jar", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");

    assert_eq!(
        world
            .get(&repo, "com/acme/lib/1.0/lib.jar", &alice)
            .await
            .status(),
        200
    );
    assert_eq!(
        world.get(&repo, "org/other/x.pom", &alice).await.status(),
        200,
        "any pattern of an allow rule is enough"
    );
    assert_eq!(
        world.get(&repo, "org/other/x.jar", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(world.upstream.calls_for("/org/other/x.jar"), 0);
}

#[tokio::test]
async fn patterns_are_unanchored_unless_the_author_anchors_them() {
    let (world, rules) = world_with_rules(Duration::from_secs(60));
    let loose = rule(RoutingMode::Block, &["secret"]);
    let anchored = rule(RoutingMode::Block, &["^secret$"]);
    rules.put(&loose);
    rules.put(&anchored);
    let by_loose = proxy_with(&world, "loose", Some(&loose)).await;
    let by_anchor = proxy_with(&world, "anchored", Some(&anchored)).await;
    world
        .upstream
        .route("/a/secret/b", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    assert_eq!(
        world.get(&by_loose, "a/secret/b", &alice).await.status(),
        404
    );
    assert_eq!(
        world.get(&by_anchor, "a/secret/b", &alice).await.status(),
        200
    );
}

#[tokio::test]
async fn a_repository_without_a_rule_is_not_gated_and_neither_is_a_hosted_one() {
    let (world, rules) = world_with_rules(Duration::from_secs(60));
    let block_all = rule(RoutingMode::Block, &[".*"]);
    rules.put(&block_all);
    let plain = proxy_with(&world, "plain", None).await;
    world.upstream.route("/x", vec![Reply::ok(CONTENT)]);
    assert_eq!(
        world.get(&plain, "x", &principal("alice")).await.status(),
        200
    );
    assert_eq!(rules.reads(), 0, "no rule attached, nothing to read");

    // A rule that somehow sits on a hosted repository is ignored: hosted content is written by
    // its own users and must stay readable by them.
    let mut hosted = fixtures::repository("files", RepoFormat::Raw, RepoType::Hosted);
    hosted.routing_rule_id = Some(block_all.id);
    let hosted = world.add(hosted).await;
    world.allow_all("alice", &[&hosted]);
    world.put_as_admin(&hosted, "a/b.bin", CONTENT).await;
    assert_eq!(
        world
            .get(&hosted, "a/b.bin", &principal("alice"))
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn rules_are_cumulative_across_a_group_and_a_refusal_always_wins() {
    let (world, rules) = world_with_rules(Duration::from_secs(60));
    let member_block = rule(RoutingMode::Block, &["^internal/"]);
    let group_allow = rule(RoutingMode::Allow, &["^open/", "^internal/"]);
    rules.put(&member_block);
    rules.put(&group_allow);
    let hosted = world.hosted("local").await;
    let mirror = proxy_with(&world, "mirror", Some(&member_block)).await;
    let mut group = fixtures::repository("all", RepoFormat::Raw, RepoType::Group);
    group.routing_rule_id = Some(group_allow.id);
    group.group = Some(rustryx_core::model::GroupConfig {
        members: vec![hosted.name.clone(), mirror.name.clone()],
    });
    let group = world.add(group).await;
    world.allow_all("alice", &[&hosted, &group]);
    world.put_as_admin(&hosted, "libs/own.bin", b"hosted").await;
    for path in ["/internal/tool.bin", "/other/y.bin", "/open/x.bin"] {
        world.upstream.route(path, vec![Reply::ok(CONTENT)]);
    }
    let alice = principal("alice");

    // The group's allow list lets `internal/` through, the member's block list refuses it:
    // the proxy member is never asked.
    assert_eq!(
        world
            .get(&group, "internal/tool.bin", &alice)
            .await
            .status(),
        404
    );
    assert_eq!(world.upstream.calls_for("/internal/tool.bin"), 0);
    // The member would serve `other/`, the group's allow list refuses it before any member
    // is consulted.
    assert_eq!(world.get(&group, "other/y.bin", &alice).await.status(), 404);
    assert_eq!(world.upstream.calls_for("/other/y.bin"), 0);
    // Both let this one through.
    assert_eq!(world.get(&group, "open/x.bin", &alice).await.status(), 200);
    assert_eq!(world.upstream.calls_for("/open/x.bin"), 1);
}

#[tokio::test]
async fn must_not_serve_a_path_when_the_rule_cannot_be_read() {
    let (world, rules) = world_with_rules(Duration::ZERO);
    let block = rule(RoutingMode::Block, &["^nothing-matches$"]);
    rules.put(&block);
    let repo = proxy_with(&world, "mirror", Some(&block)).await;
    world.upstream.route("/a.bin", vec![Reply::ok(CONTENT)]);
    rules.fail.store(true, Ordering::SeqCst);
    let response = world.get(&repo, "a.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().get("retry-after").is_some());
    assert_eq!(
        world.upstream.calls(),
        0,
        "an unreadable rule is not an allow"
    );
    let text = support::body_text(response).await;
    assert!(
        !text.contains("rules down"),
        "the store's own text must stay in the log: {text}"
    );
    // And the gate recovers when the store does.
    rules.fail.store(false, Ordering::SeqCst);
    assert_eq!(
        world
            .get(&repo, "a.bin", &principal("alice"))
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn must_not_serve_a_path_when_the_rule_is_missing_or_invalid() {
    let (world, rules) = world_with_rules(Duration::ZERO);
    let missing = rule(RoutingMode::Block, &["^x$"]);
    let repo = proxy_with(&world, "mirror", Some(&missing)).await;
    world.upstream.route("/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    let response = world.get(&repo, "a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        world.upstream.calls(),
        0,
        "a rule that does not resolve is not an allow"
    );

    // A row written around the service: a pattern that does not compile.
    let broken = rule(RoutingMode::Block, &["("]);
    rules.put(&broken);
    let repo = proxy_with(&world, "broken", Some(&broken)).await;
    assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 500);
    // And one without any pattern, which the service would refuse to save.
    let empty = rule(RoutingMode::Allow, &[]);
    rules.put(&empty);
    let repo = proxy_with(&world, "empty", Some(&empty)).await;
    assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 500);
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn a_member_whose_rule_cannot_be_read_is_skipped_by_the_group() {
    let (world, rules) = world_with_rules(Duration::ZERO);
    let block = rule(RoutingMode::Block, &["^nothing-matches$"]);
    rules.put(&block);
    let hosted = world.hosted("local").await;
    let mirror = proxy_with(&world, "mirror", Some(&block)).await;
    let group = world.group("all", &[&hosted, &mirror]).await;
    world.allow_all("alice", &[&hosted, &group]);
    world.put_as_admin(&hosted, "pkg/a.bin", b"hosted").await;
    rules.fail.store(true, Ordering::SeqCst);
    world
        .upstream
        .route("/other/b.bin", vec![Reply::ok(CONTENT)]);
    // The member whose rule is unreadable is skipped (fail closed), the hosted one answers.
    assert_eq!(
        &body_bytes(world.get(&group, "pkg/a.bin", &principal("alice")).await).await[..],
        b"hosted"
    );
    // For a path only the proxy could serve the group does not pretend it does not exist.
    let response = world.get(&group, "other/b.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn a_block_rule_applies_to_content_that_is_already_cached() {
    let (world, rules) = world_with_rules(Duration::ZERO);
    let block = rule(RoutingMode::Block, &["^cached/"]);
    let mut open = block.clone();
    open.patterns = vec!["^nothing-matches$".into()];
    rules.put(&open);
    let repo = proxy_with(&world, "mirror", Some(&open)).await;
    world
        .upstream
        .route("/cached/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    body_bytes(world.get(&repo, "cached/a.bin", &alice).await).await;
    world.settle().await;
    assert_eq!(world.get(&repo, "cached/a.bin", &alice).await.status(), 200);
    // The operator adds the block: the cached copy must disappear at once, not at expiry.
    rules.put(&block);
    assert_eq!(world.get(&repo, "cached/a.bin", &alice).await.status(), 404);
    assert_eq!(world.upstream.calls(), 1);
}

#[tokio::test]
async fn compiled_rules_are_reused_until_they_expire_or_are_invalidated() {
    let rules = FakeRules::new();
    let gate = Arc::new(RuleRoutingGate::with_ttl(
        rules.clone(),
        Duration::from_secs(300),
    ));
    let world = World::with_ports(|ports| ports.routing = gate.clone());
    let open = rule(RoutingMode::Block, &["^nothing-matches$"]);
    rules.put(&open);
    let repo = proxy_with(&world, "mirror", Some(&open)).await;
    world.upstream.route("/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    for _ in 0..5 {
        assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 200);
    }
    assert_eq!(
        rules.reads(),
        1,
        "one read serves every request of the interval"
    );

    // A change is not seen until the node is told, or the entry expires.
    let mut closed = open.clone();
    closed.patterns = vec![".*".into()];
    rules.put(&closed);
    assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 200);
    gate.invalidate(open.id).await;
    assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 404);
    assert_eq!(rules.reads(), 2);
    gate.invalidate_all().await;
    assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 404);
    assert_eq!(rules.reads(), 3);
}

#[tokio::test]
async fn a_rule_is_read_on_every_request_when_the_cache_is_off() {
    let (world, rules) = world_with_rules(Duration::ZERO);
    let open = rule(RoutingMode::Block, &["^nothing-matches$"]);
    rules.put(&open);
    let repo = proxy_with(&world, "mirror", Some(&open)).await;
    world.upstream.route("/a.bin", vec![Reply::ok(CONTENT)]);
    for _ in 0..3 {
        world.get(&repo, "a.bin", &principal("alice")).await;
    }
    assert_eq!(rules.reads(), 3);
}

#[tokio::test]
async fn concurrent_requests_share_one_rule_load_and_a_failed_load_is_not_cached() {
    let rules = FakeRules::new();
    let gate = Arc::new(RuleRoutingGate::with_ttl(
        rules.clone(),
        Duration::from_secs(300),
    ));
    let world = World::with_ports(|ports| ports.routing = gate);
    let open = rule(RoutingMode::Block, &["^nothing-matches$"]);
    rules.put(&open);
    let repo = proxy_with(&world, "mirror", Some(&open)).await;
    world.upstream.route("/a.bin", vec![Reply::ok(CONTENT)]);
    *rules.get_delay.lock().unwrap() = Duration::from_millis(80);
    let alice = principal("alice");
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let engine = world.engine.clone();
            let repo = repo.clone();
            let head = world.head(http::Method::GET, "a.bin", &alice);
            tokio::spawn(async move {
                engine
                    .serve(repo, head, axum_core::body::Body::empty())
                    .await
                    .status()
            })
        })
        .collect();
    for request in requests {
        assert_eq!(request.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(rules.reads(), 1, "eight concurrent misses are one load");

    // A failure is answered, not remembered.
    let gate = Arc::new(RuleRoutingGate::with_ttl(
        rules.clone(),
        Duration::from_secs(300),
    ));
    let world = World::with_ports(|ports| ports.routing = gate);
    let repo = proxy_with(&world, "again", Some(&open)).await;
    world.upstream.route("/a.bin", vec![Reply::ok(CONTENT)]);
    *rules.get_delay.lock().unwrap() = Duration::ZERO;
    rules.fail.store(true, Ordering::SeqCst);
    assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 503);
    rules.fail.store(false, Ordering::SeqCst);
    assert_eq!(world.get(&repo, "a.bin", &alice).await.status(), 200);
}
