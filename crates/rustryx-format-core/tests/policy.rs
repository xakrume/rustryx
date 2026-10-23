//! Policy plumbing of the hosted pipeline: the hook, the typed refusals of the ports
//! (`PolicyViolation`, `MutationError`), create-only publishes and where blobs live.
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

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderValue, Method, StatusCode, header};
use rustryx_core::audit::{ActorKind, AuditKind};
use rustryx_core::error::PolicyViolation;
use rustryx_core::ids::{BlobStoreId, ImmutabilityRuleId, LifecycleHoldId};
use rustryx_core::model::{
    NewAssetLink, PublishMode, RepoFormat, RepoType, Repository, WriteOptions, WritePolicy,
};
use rustryx_core::ports::{AssetRepo, BlobKey};
use rustryx_core::quota::{QuotaExceeded, QuotaScope};
use rustryx_format_core::{FormatError, OverwriteRule, UploadSpec};
use rustryx_testkit::{MemBlobStore, fixtures};
use support::doubles::{AssetSpy, GroupLikeStore, MapRegistry, ScriptedPolicy, Verdict};
use support::recorder::TestRecorder;
use support::{
    World, body_bytes, body_text, channel_body, eventually, header_of, principal, tracked_body,
};

const CONTENT: &[u8] = b"hosted artifact bytes";

fn hosted_repo(name: &str, policy: WritePolicy) -> Repository {
    let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Hosted);
    repository.write_policy = policy;
    repository
}

fn immutable() -> PolicyViolation {
    PolicyViolation::Immutable {
        rule: ImmutabilityRuleId::generate(),
    }
}

fn held() -> PolicyViolation {
    PolicyViolation::Held {
        hold: LifecycleHoldId::generate(),
    }
}

fn quota() -> PolicyViolation {
    PolicyViolation::QuotaExceeded(QuotaExceeded {
        scope: QuotaScope::Repository(rustryx_core::ids::RepositoryId::generate()),
        limit_bytes: 10,
        used_bytes: 9,
        requested_bytes: 5,
    })
}

async fn world_with_policy(
    policy: Arc<ScriptedPolicy>,
    write_policy: WritePolicy,
) -> (World, Arc<Repository>) {
    let world = World::with_ports(|ports| ports.policy = policy);
    let repo = world.add(hosted_repo("files", write_policy)).await;
    world.allow_all("alice", &[&repo]);
    (world, repo)
}

fn put_head(world: &World, path: &str, length: usize) -> rustryx_format_core::RequestHead {
    let mut head = world.head(Method::PUT, path, &principal("alice"));
    head.headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    head
}

#[tokio::test]
async fn must_not_read_the_body_when_a_policy_hook_refuses() {
    let policy = ScriptedPolicy::new(Verdict::Refuse(immutable()), Verdict::Allow);
    let (world, repo) = world_with_policy(policy.clone(), WritePolicy::Allow).await;
    let (polled, body) = tracked_body(CONTENT);
    let response = world
        .send(&repo, Method::PUT, "pkg/a.bin", &principal("alice"), body)
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_text(response).await, "RAW-ERROR immutable");
    assert!(
        !polled.load(Ordering::SeqCst),
        "a refusal must cost no byte of the body"
    );
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
    assert_eq!(policy.writes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_hook_sees_the_write_it_is_asked_about() {
    let policy = ScriptedPolicy::allowing();
    let (world, repo) = world_with_policy(policy.clone(), WritePolicy::Allow).await;
    let head = put_head(&world, "pkg/a.bin", CONTENT.len());
    let response = world
        .send_with(&repo, head, Body::from(Bytes::from_static(CONTENT)))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let head = put_head(&world, "pkg/a.bin", 3);
    let response = world
        .send_with(&repo, head, Body::from(Bytes::from_static(b"new")))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let seen = policy.writes.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].repository, "files");
    assert_eq!(seen[0].principal, "alice");
    assert_eq!(seen[0].path, "pkg/a.bin");
    assert_eq!(seen[0].declared_length, Some(CONTENT.len() as u64));
    assert!(!seen[0].existing, "the first upload replaces nothing");
    assert_eq!(seen[0].component.as_deref(), Some("pkg"));
    assert!(seen[1].existing, "the second one replaces an asset");
    assert_eq!(seen[1].declared_length, Some(3));
}

#[tokio::test]
async fn a_policy_hook_that_refuses_a_delete_keeps_the_asset() {
    let policy = ScriptedPolicy::allowing();
    let (world, repo) = world_with_policy(policy.clone(), WritePolicy::Allow).await;
    let alice = principal("alice");
    world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    policy.set_delete(Verdict::Refuse(held()));
    let response = world
        .send(&repo, Method::DELETE, "pkg/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_text(response).await, "RAW-ERROR held");
    assert_eq!(world.get(&repo, "pkg/a.bin", &alice).await.status(), 200);
    assert_eq!(world.events.orphaned(), 0, "nothing was deleted");
    let deletes = policy.deletes.lock().unwrap().clone();
    assert_eq!(deletes, [("pkg/a.bin".to_owned(), CONTENT.len() as u64)]);
}

#[tokio::test]
async fn must_not_allow_a_write_when_the_policy_hook_cannot_decide() {
    let policy = ScriptedPolicy::new(Verdict::CannotDecide, Verdict::Allow);
    let (world, repo) = world_with_policy(policy, WritePolicy::Allow).await;
    let (polled, body) = tracked_body(CONTENT);
    let response = world
        .send(&repo, Method::PUT, "pkg/a.bin", &principal("alice"), body)
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().get(header::RETRY_AFTER).is_some());
    assert!(!polled.load(Ordering::SeqCst));
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn must_not_delete_when_the_policy_hook_cannot_decide() {
    let policy = ScriptedPolicy::allowing();
    let (world, repo) = world_with_policy(policy.clone(), WritePolicy::Allow).await;
    let alice = principal("alice");
    world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    policy.set_delete(Verdict::CannotDecide);
    let response = world
        .send(&repo, Method::DELETE, "pkg/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(world.get(&repo, "pkg/a.bin", &alice).await.status(), 200);
}

#[tokio::test]
async fn each_policy_refusal_reaches_the_client_with_its_status_and_code() {
    let cases: Vec<(PolicyViolation, u16, &str)> = vec![
        (immutable(), 409, "immutable"),
        (held(), 409, "held"),
        (quota(), 507, "quota_exceeded"),
        (PolicyViolation::ReadOnly, 503, "read_only_mode"),
        (PolicyViolation::ApprovalRequired, 409, "approval_required"),
        (
            PolicyViolation::WritePolicy(WritePolicy::Deny),
            403,
            "forbidden",
        ),
        (
            PolicyViolation::WritePolicy(WritePolicy::AllowOnce),
            409,
            "conflict",
        ),
        (PolicyViolation::AlreadyExists, 409, "conflict"),
    ];
    for (violation, status, code) in cases {
        let policy = ScriptedPolicy::new(Verdict::Refuse(violation.clone()), Verdict::Allow);
        let (world, repo) = world_with_policy(policy, WritePolicy::Allow).await;
        let response = world
            .put(&repo, "pkg/a.bin", &principal("alice"), CONTENT)
            .await;
        assert_eq!(response.status().as_u16(), status, "{violation:?}");
        if violation == PolicyViolation::ReadOnly {
            assert_eq!(header_of(response.headers(), "retry-after"), Some("30"));
        }
        assert_eq!(
            body_text(response).await,
            format!("RAW-ERROR {code}"),
            "{violation:?}"
        );
    }
}

#[tokio::test]
async fn a_refusal_by_the_transaction_removes_the_committed_blob_and_is_counted_and_audited() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let spy = AssetSpy::new();
    let world = World::with_ports(|ports| spy.install(ports));
    let repo = world.add(hosted_repo("files", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&repo]);
    *spy.refuse_publish.lock().unwrap() = Some(immutable());

    let response = world
        .put(&repo, "pkg/a.bin", &principal("alice"), CONTENT)
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_text(response).await, "RAW-ERROR immutable");
    assert_eq!(
        world.blobs.len(),
        0,
        "the object was committed before the transaction said no: it must be deleted"
    );
    assert_eq!(world.blobs.staging_count(), 0);
    assert_eq!(
        recorder.counter(
            "policy_violations_total",
            &[("format", "raw"), ("reason", "immutable")]
        ),
        1
    );
    let events = world.audit.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].action, "asset.rejected");
    assert_eq!(events[0].kind, AuditKind::SecurityDecision);
    assert_eq!(events[0].detail["reason"], "immutable");
    assert!(
        !events.iter().any(|e| e.action == "asset.publish"),
        "a refused write is not a publish"
    );
}

#[tokio::test]
async fn a_hook_refusal_is_counted_and_audited_but_a_hook_outage_is_not_a_refusal() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let policy = ScriptedPolicy::new(Verdict::Refuse(quota()), Verdict::Allow);
    let (world, repo) = world_with_policy(policy.clone(), WritePolicy::Allow).await;
    let alice = principal("alice");
    let response = world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(
        recorder.counter(
            "policy_violations_total",
            &[("format", "raw"), ("reason", "quota_exceeded")]
        ),
        1
    );
    assert_eq!(world.audit.events().len(), 1);

    policy.set_write(Verdict::CannotDecide);
    let response = world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        recorder.counter_total("policy_violations_total"),
        1,
        "a hook that could not decide refused nothing"
    );
    assert_eq!(world.audit.events().len(), 1);
}

#[tokio::test]
async fn publish_with_receives_the_actor_the_request_id_and_the_mode() {
    let spy = AssetSpy::new();
    let world = World::with_ports(|ports| spy.install(ports));
    let once = world.add(hosted_repo("once", WritePolicy::AllowOnce)).await;
    let open = world.add(hosted_repo("open", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&once, &open]);
    let mut head = world.head(Method::PUT, "a/b.bin", &principal("alice"));
    head.client.request_id = Some("req-42".into());
    assert!(
        world
            .send_with(&once, head, Body::from(Bytes::from_static(CONTENT)))
            .await
            .status()
            .is_success()
    );
    assert!(
        world
            .put(&open, "a/b.bin", &principal("alice"), CONTENT)
            .await
            .status()
            .is_success()
    );

    let options = spy.publish_options.lock().unwrap().clone();
    assert_eq!(options.len(), 2);
    assert_eq!(
        options[0].mode,
        PublishMode::CreateOnly,
        "an allow_once path must be written create-only"
    );
    assert_eq!(options[1].mode, PublishMode::Upsert);
    for option in &options {
        let actor = option.actor.as_ref().expect("the uploader acts");
        assert_eq!(actor.name, "alice");
        assert_eq!(actor.kind, ActorKind::User);
        assert!(!option.override_immutable, "never set from a request");
    }
    assert_eq!(options[0].request_id.as_deref(), Some("req-42"));
    assert_eq!(options[1].request_id, None);
}

#[tokio::test]
async fn must_not_overwrite_an_allow_once_path_published_by_another_node() {
    let world = World::new();
    let repo = world.add(hosted_repo("once", WritePolicy::AllowOnce)).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");

    // This node's upload is staged and its path is reserved in this process.
    let (tx, body) = channel_body();
    let local = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::PUT, "race/x.bin", &alice);
        async move { engine.serve(repo, head, body).await }
    });
    eventually("local upload staged", || world.blobs.staging_count() == 1).await;
    tx.send(Ok(Bytes::from_static(b"local bytes")))
        .await
        .unwrap();

    // Another node publishes the same path now. The reservation of this process cannot see it.
    let key = "22222222-2222-7222-8222-222222222222";
    world
        .blobs
        .put(&BlobKey::new(key), Bytes::from_static(b"remote bytes"));
    let mut remote = fixtures::blob_record(key, b"remote bytes");
    remote.blob_store_id = repo.blob_store_id;
    world
        .store
        .publish_with(
            &rustryx_core::model::NewAsset::new(
                repo.id,
                "race/x.bin",
                "application/octet-stream",
                remote,
            ),
            &WriteOptions::new().with_mode(PublishMode::CreateOnly),
        )
        .await
        .unwrap();

    drop(tx);
    let response = local.await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "the create-only publish of the late upload must be refused by the database"
    );
    let stored = world
        .store
        .get_by_path(repo.id, "race/x.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.size_bytes,
        b"remote bytes".len() as u64,
        "the other node's content must survive"
    );
    world.settle().await;
    assert_eq!(
        world.blobs.len(),
        1,
        "the late upload's committed object is deleted, only the published one remains"
    );
}

#[tokio::test]
async fn must_not_replace_a_never_overwrite_path_published_by_another_node() {
    // The repository itself allows overwrites; the FORMAT says this path is immutable (an npm
    // version, a release). Only the create-only mode can make that hold across nodes: the
    // repository policy of the database knows nothing about it.
    let world = World::new();
    let repo = world.add(hosted_repo("open", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&repo]);
    let head = world.head(Method::PUT, "v/1.0.bin", &principal("alice"));
    let cx = world.engine.context(repo.clone(), &head);
    let path = rustryx_format_core::NormalizedPath::parse("v/1.0.bin").unwrap();

    let (tx, body) = channel_body();
    let local = tokio::spawn({
        let cx = cx.clone();
        let path = path.clone();
        async move {
            let mut spec = UploadSpec::new(path);
            spec.overwrite = OverwriteRule::Never;
            cx.hosted().upload(spec, body).await
        }
    });
    eventually("local upload staged", || world.blobs.staging_count() == 1).await;
    tx.send(Ok(Bytes::from_static(b"local"))).await.unwrap();

    let key = "33333333-3333-7333-8333-333333333333";
    world
        .blobs
        .put(&BlobKey::new(key), Bytes::from_static(b"remote bytes"));
    let mut remote = fixtures::blob_record(key, b"remote bytes");
    remote.blob_store_id = repo.blob_store_id;
    world
        .store
        .publish_with(
            &rustryx_core::model::NewAsset::new(
                repo.id,
                "v/1.0.bin",
                "application/octet-stream",
                remote,
            ),
            &WriteOptions::new(),
        )
        .await
        .unwrap();

    drop(tx);
    let error = local.await.unwrap().unwrap_err();
    assert!(matches!(error, FormatError::Conflict(_)), "{error:?}");
    let stored = world
        .store
        .get_by_path(repo.id, "v/1.0.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.size_bytes, b"remote bytes".len() as u64);
    world.settle().await;
    assert_eq!(world.blobs.len(), 1);
}

#[tokio::test]
async fn an_overwrite_rule_of_never_is_enforced_by_the_transaction_on_an_open_repository() {
    let spy = AssetSpy::new();
    let world = World::with_ports(|ports| spy.install(ports));
    let repo = world.add(hosted_repo("open", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    let head = world.head(Method::PUT, "v/1.0.bin", &alice);
    let cx = world.engine.context(repo.clone(), &head);
    let spec = || {
        let mut spec =
            UploadSpec::new(rustryx_format_core::NormalizedPath::parse("v/1.0.bin").unwrap());
        spec.overwrite = OverwriteRule::Never;
        spec
    };
    cx.hosted()
        .upload(spec(), Body::from(Bytes::from_static(b"one")))
        .await
        .unwrap();
    let error = cx
        .hosted()
        .upload(spec(), Body::from(Bytes::from_static(b"two")))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::Conflict(_)), "{error:?}");
    let options = spy.publish_options.lock().unwrap().clone();
    assert_eq!(
        options.len(),
        1,
        "the second upload is refused before the port is asked"
    );
    assert_eq!(options[0].mode, PublishMode::CreateOnly);
    // And if the pre check missed it (a race), the port answers AlreadyExists.
    *spy.refuse_publish.lock().unwrap() = Some(PolicyViolation::AlreadyExists);
    let fresh = rustryx_format_core::NormalizedPath::parse("v/2.0.bin").unwrap();
    let mut late = UploadSpec::new(fresh);
    late.overwrite = OverwriteRule::Never;
    let error = cx
        .hosted()
        .upload(late, Body::from(Bytes::from_static(b"late")))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::Conflict(_)), "{error:?}");
    assert_eq!(world.blobs.len(), 1, "the late object was deleted");
}

#[tokio::test]
async fn a_concurrent_second_upload_is_refused_without_reading_its_body() {
    let world = World::new();
    let repo = world.add(hosted_repo("once", WritePolicy::AllowOnce)).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    let (tx, body) = channel_body();
    let first = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::PUT, "race/y.bin", &alice);
        async move { engine.serve(repo, head, body).await.status() }
    });
    eventually("first upload staged", || world.blobs.staging_count() == 1).await;
    let (polled, second_body) = tracked_body(b"other");
    let second = world
        .send(&repo, Method::PUT, "race/y.bin", &alice, second_body)
        .await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert!(
        !polled.load(Ordering::SeqCst),
        "the reservation is an optimisation: it saves the body of the loser"
    );
    tx.send(Ok(Bytes::from_static(b"abc"))).await.unwrap();
    drop(tx);
    assert_eq!(first.await.unwrap(), StatusCode::CREATED);
}

#[tokio::test]
async fn delete_goes_through_the_checked_port_and_reports_what_it_returned() {
    let spy = AssetSpy::new();
    let world = World::with_ports(|ports| spy.install(ports));
    let repo = world.add(hosted_repo("files", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    let response = world
        .send(&repo, Method::DELETE, "pkg/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let options = spy.delete_options.lock().unwrap().clone();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].actor.as_ref().unwrap().name, "alice");
    assert_eq!(world.events.orphaned(), 1);
}

#[tokio::test]
async fn deleting_an_asset_whose_blob_is_still_linked_announces_no_orphan() {
    let world = World::new();
    let repo = world.add(hosted_repo("files", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    let original = world
        .store
        .get_by_path(repo.id, "pkg/a.bin")
        .await
        .unwrap()
        .unwrap();
    // A second path points at the same blob (a promotion or a mount does that).
    world
        .store
        .link(
            &NewAssetLink::new(
                repo.id,
                "copy/a.bin",
                "application/octet-stream",
                original.blob_id,
            ),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
    let response = world
        .send(&repo, Method::DELETE, "pkg/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "the asset existed, so the delete succeeded"
    );
    assert_eq!(
        world.events.orphaned(),
        0,
        "the blob is still referenced by the copy: nothing is orphaned"
    );
    assert_eq!(world.get(&repo, "copy/a.bin", &alice).await.status(), 200);
}

#[tokio::test]
async fn a_delete_refused_by_the_transaction_is_a_policy_error() {
    let spy = AssetSpy::new();
    let world = World::with_ports(|ports| spy.install(ports));
    let repo = world.add(hosted_repo("files", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    *spy.refuse_delete.lock().unwrap() = Some(PolicyViolation::ReadOnly);
    let response = world
        .send(&repo, Method::DELETE, "pkg/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header_of(response.headers(), "retry-after"), Some("30"));
    assert_eq!(world.get(&repo, "pkg/a.bin", &alice).await.status(), 200);
    assert_eq!(world.events.orphaned(), 0);
}

#[tokio::test]
async fn a_deny_repository_refuses_a_delete_before_asking_anything() {
    let policy = ScriptedPolicy::allowing();
    let (world, repo) = world_with_policy(policy.clone(), WritePolicy::Deny).await;
    let response = world
        .send(
            &repo,
            Method::DELETE,
            "a/b",
            &principal("alice"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(policy.deletes.lock().unwrap().is_empty());
}

fn group_world() -> (
    World,
    MemBlobStore,
    Arc<GroupLikeStore>,
    Arc<MapRegistry>,
    BlobStoreId,
    BlobStoreId,
) {
    let member = MemBlobStore::new();
    let member_id = BlobStoreId::generate();
    let group_id = BlobStoreId::generate();
    let group = GroupLikeStore::new(member.clone(), member_id);
    let registry = MapRegistry::new(vec![
        (group_id, group.clone()),
        (member_id, Arc::new(member.clone())),
    ]);
    let world = World::with_registry(member.clone(), registry.clone());
    (world, member, group, registry, group_id, member_id)
}

#[tokio::test]
async fn blob_rows_name_the_member_that_took_the_bytes_and_reads_go_there() {
    let (world, member, group, registry, group_id, member_id) = group_world();
    let mut repository = hosted_repo("grouped", WritePolicy::Allow);
    repository.blob_store_id = group_id;
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");

    let response = world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let asset = world
        .store
        .get_by_path(repo.id, "pkg/a.bin")
        .await
        .unwrap()
        .unwrap();
    let row = rustryx_core::ports::BlobRepo::get(&world.store, asset.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.blob_store_id, member_id,
        "the row must name the concrete member, not the group"
    );
    assert_eq!(member.len(), 1);

    registry.requested.lock().unwrap().clear();
    let response = world.get(&repo, "pkg/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, Bytes::from_static(CONTENT));
    assert_eq!(
        registry.requested(),
        vec![member_id],
        "a read resolves the store of the blob row and never touches the group"
    );
    assert_eq!(
        group.refused_calls.load(Ordering::SeqCst),
        0,
        "must_not_read_through_the_group_adapter"
    );
}

#[tokio::test]
async fn must_not_delete_an_unpublished_object_through_the_group_adapter() {
    let (world, member, group, _registry, group_id, _member_id) = group_world();
    let mut repository = hosted_repo("grouped", WritePolicy::Allow);
    repository.blob_store_id = group_id;
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    // A digest mismatch is a refusal after the commit: the object is on the member and must be
    // removed from the member.
    let head = world.head(Method::PUT, "pkg/a.bin", &principal("alice"));
    let cx = world.engine.context(repo.clone(), &head);
    let mut spec =
        UploadSpec::new(rustryx_format_core::NormalizedPath::parse("pkg/a.bin").unwrap());
    spec.expected = Some(
        rustryx_format_core::ExpectedChecksum::new(
            rustryx_format_core::HashAlgo::Sha256,
            &"0".repeat(64),
        )
        .unwrap(),
    );
    let error = cx
        .hosted()
        .upload(spec, Body::from(Bytes::from_static(CONTENT)))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::DigestMismatch), "{error:?}");
    assert_eq!(
        member.len(),
        0,
        "the committed object is deleted from the member"
    );
    assert_eq!(
        group.refused_calls.load(Ordering::SeqCst),
        0,
        "the group adapter refuses deletes; asking it would leak the object"
    );
}
