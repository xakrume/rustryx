//! `AssetRepo::publish_with`: write policy, create only mode, quota, outbox events and the
//! options that reach the SQL guards.

use rustryx_core::StoreError;
use rustryx_core::audit::ActorRef;
use rustryx_core::error::{MutationError, PolicyViolation};
use rustryx_core::events::{EventDraft, EventKind};
use rustryx_core::model::{ProxyConfig, PublishMode, RepoType, WriteOptions, WritePolicy};
use rustryx_core::ports::{AssetRepo, BlobRepo, QuotaRepo, RepositoryRepo};
use rustryx_core::quota::QuotaScope;
use tokio::task::JoinSet;

use crate::fixtures::{blob, hosted_repository, new_asset, seed_repository, unique};
use crate::support::TestDb;

type EventRow = (
    String,
    String,
    String,
    Option<uuid::Uuid>,
    Option<String>,
    serde_json::Value,
);

fn create_only() -> WriteOptions {
    WriteOptions::new().with_mode(PublishMode::CreateOnly)
}

fn violation(err: MutationError) -> PolicyViolation {
    match err {
        MutationError::Violation(v) => v,
        other => panic!("expected a violation, got {other:?}"),
    }
}

async fn count(store: &rustryx_db::PgStore, table: &'static str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(store.pool())
        .await
        .unwrap()
}

fn push_event(repo: rustryx_core::ids::RepositoryId, path: &str) -> EventDraft {
    let mut draft = EventDraft::new(
        EventKind::Push,
        ActorRef::system("test:publisher"),
        serde_json::json!({ "path": path }),
    );
    draft.repository_id = Some(repo);
    draft.request_id = Some("req-7".to_owned());
    draft
}

#[tokio::test(flavor = "multi_thread")]
async fn creates_then_replaces_and_reports_both_in_the_outcome() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let uploader = crate::fixtures::user(&store).await;

    let first = blob(bs.id, 10);
    let mut new = new_asset(repo.id, "a/b", first.clone());
    new.created_by = Some(uploader);
    let created = store
        .assets
        .publish_with(&new, &WriteOptions::new())
        .await
        .unwrap();
    assert!(created.created && created.replaced.is_none());
    assert_eq!(created.asset.created_by, Some(uploader));

    let second = blob(bs.id, 20);
    let replaced = store
        .assets
        .publish_with(
            &new_asset(repo.id, "a/b", second.clone()),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
    assert!(!replaced.created);
    assert_eq!(replaced.replaced.unwrap().id, first.id);
    assert_eq!(replaced.asset.id, created.asset.id, "same asset row");
    assert_eq!(replaced.asset.blob_id, second.id);
    assert_eq!(
        replaced.asset.created_by, None,
        "the uploader follows the content"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_replace_an_existing_path_in_create_only_mode() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let winner = blob(bs.id, 1);
    store
        .assets
        .publish(&new_asset(repo.id, "p", winner.clone()))
        .await
        .unwrap();

    let loser = blob(bs.id, 2);
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "p", loser.clone()),
            &create_only().with_event(push_event(repo.id, "p")),
        )
        .await
        .unwrap_err();
    assert_eq!(violation(err), PolicyViolation::AlreadyExists);

    let asset = store
        .assets
        .get_by_path(repo.id, "p")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        asset.blob_id, winner.id,
        "the path still serves the first upload"
    );
    assert!(
        store.blobs.get(loser.id).await.unwrap().is_none(),
        "no blob row left"
    );
    assert_eq!(
        count(&store, "event_outbox").await,
        0,
        "no event of a refused write"
    );

    // A new path is fine in the same mode.
    let fresh = store
        .assets
        .publish_with(&new_asset(repo.id, "q", blob(bs.id, 3)), &create_only())
        .await
        .unwrap();
    assert!(fresh.created);
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_let_two_create_only_publishers_both_win_the_same_path() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let mut tasks = JoinSet::new();
    for _ in 0..12 {
        let assets = store.assets.clone();
        let new = new_asset(repo.id, "contended", blob(bs.id, 5));
        tasks.spawn(async move { assets.publish_with(&new, &create_only()).await });
    }
    let mut wins = 0;
    let mut refused = 0;
    while let Some(done) = tasks.join_next().await {
        match done.unwrap() {
            Ok(outcome) => {
                assert!(outcome.created && outcome.replaced.is_none());
                wins += 1;
            }
            Err(err) => {
                assert_eq!(violation(err), PolicyViolation::AlreadyExists);
                refused += 1;
            }
        }
    }
    assert_eq!((wins, refused), (1, 11));
    assert_eq!(count(&store, "assets").await, 1);
    assert_eq!(
        count(&store, "blobs").await,
        1,
        "the losers left no blob row"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_write_policy_is_enforced_in_the_transaction() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, mut repo) = seed_repository(&store).await;

    repo.write_policy = WritePolicy::AllowOnce;
    store.repositories.update(&repo).await.unwrap();
    store
        .assets
        .publish(&new_asset(repo.id, "once", blob(bs.id, 1)))
        .await
        .unwrap();
    let second = blob(bs.id, 2);
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "once", second.clone()),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        violation(err),
        PolicyViolation::WritePolicy(WritePolicy::AllowOnce)
    );
    assert!(store.blobs.get(second.id).await.unwrap().is_none());
    // The flattened phase 1 call reads the same refusal as a Conflict.
    let err = store
        .assets
        .publish(&new_asset(repo.id, "once", blob(bs.id, 3)))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "write policy forbids this write"),
        "{err:?}"
    );
    // A new path is allowed under allow_once.
    store
        .assets
        .publish(&new_asset(repo.id, "other", blob(bs.id, 4)))
        .await
        .unwrap();

    repo.write_policy = WritePolicy::Deny;
    store.repositories.update(&repo).await.unwrap();
    let denied = blob(bs.id, 5);
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "brand-new", denied.clone()),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        violation(err),
        PolicyViolation::WritePolicy(WritePolicy::Deny)
    );
    assert!(
        store.blobs.get(denied.id).await.unwrap().is_none(),
        "refused before any write"
    );

    // A proxy is created with deny so clients cannot upload. The cache fill still publishes.
    let mut proxy = hosted_repository(&unique("proxy"), bs.id);
    proxy.repo_type = RepoType::Proxy;
    proxy.write_policy = WritePolicy::Deny;
    proxy.proxy = Some(ProxyConfig::new("https://packages.example/rpm"));
    store.repositories.create(&proxy).await.unwrap();
    store
        .assets
        .publish_with(
            &new_asset(proxy.id, "gitlab-ee-1-1.x86_64.rpm", blob(bs.id, 6)),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_once_holds_across_concurrent_publishers() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, mut repo) = seed_repository(&store).await;
    repo.write_policy = WritePolicy::AllowOnce;
    store.repositories.update(&repo).await.unwrap();
    let mut tasks = JoinSet::new();
    for _ in 0..10 {
        let assets = store.assets.clone();
        let new = new_asset(repo.id, "single", blob(bs.id, 5));
        tasks.spawn(async move { assets.publish_with(&new, &WriteOptions::new()).await });
    }
    let mut wins = 0;
    while let Some(done) = tasks.join_next().await {
        match done.unwrap() {
            Ok(_) => wins += 1,
            Err(err) => assert_eq!(
                violation(err),
                PolicyViolation::WritePolicy(WritePolicy::AllowOnce)
            ),
        }
    }
    assert_eq!(wins, 1);
    assert_eq!(count(&store, "blobs").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_repository_is_not_found() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _repo) = seed_repository(&store).await;
    let err = store
        .assets
        .publish_with(
            &new_asset(
                rustryx_core::ids::RepositoryId::generate(),
                "x",
                blob(bs.id, 1),
            ),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, MutationError::Store(StoreError::NotFound(_))),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_repository_quota_is_checked_on_the_actual_size_inside_the_transaction() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, mut repo) = seed_repository(&store).await;
    repo.quota_bytes = Some(100);
    store.repositories.update(&repo).await.unwrap();

    store
        .assets
        .publish(&new_asset(repo.id, "a", blob(bs.id, 60)))
        .await
        .unwrap();
    let refused = blob(bs.id, 50);
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "b", refused.clone()),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    let PolicyViolation::QuotaExceeded(exceeded) = violation(err) else {
        panic!("quota expected");
    };
    assert_eq!(exceeded.scope, QuotaScope::Repository(repo.id));
    assert_eq!(
        (
            exceeded.limit_bytes,
            exceeded.used_bytes,
            exceeded.requested_bytes
        ),
        (100, 60, 50)
    );
    assert!(
        store.blobs.get(refused.id).await.unwrap().is_none(),
        "nothing left behind"
    );
    assert!(
        store
            .assets
            .get_by_path(repo.id, "b")
            .await
            .unwrap()
            .is_none()
    );

    // Replacing a blob counts the difference: 60 -> 100 fits exactly, 100 -> 101 does not.
    store
        .assets
        .publish(&new_asset(repo.id, "a", blob(bs.id, 100)))
        .await
        .unwrap();
    let err = store
        .assets
        .publish(&new_asset(repo.id, "a", blob(bs.id, 101)))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "quota exceeded"),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_publish_past_a_project_quota() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let project_id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, 'libs')")
        .bind(project_id)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET project_id = $2 WHERE id = $1")
        .bind(repo.id.as_uuid())
        .bind(project_id)
        .execute(store.pool())
        .await
        .unwrap();
    rustryx_db::repos::quota::PgQuotaRepo::new(store.pool().clone())
        .set_limit(
            QuotaScope::Project(project_id.into()),
            Some(100),
            80,
            None,
            &ActorRef::system("test"),
        )
        .await
        .unwrap();

    store
        .assets
        .publish_with(
            &new_asset(repo.id, "a", blob(bs.id, 60)),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "b", blob(bs.id, 50)),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    let PolicyViolation::QuotaExceeded(exceeded) = violation(err) else {
        panic!("project quota expected");
    };
    assert_eq!(exceeded.scope, QuotaScope::Project(project_id.into()));
    assert_eq!(
        (
            exceeded.limit_bytes,
            exceeded.used_bytes,
            exceeded.requested_bytes
        ),
        (100, 60, 50)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_that_does_not_grow_the_repository_is_never_refused_by_the_quota() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, mut repo) = seed_repository(&store).await;
    store
        .assets
        .publish(&new_asset(repo.id, "big", blob(bs.id, 90)))
        .await
        .unwrap();
    // The limit drops below the current usage (an operator lowered it).
    repo.quota_bytes = Some(50);
    store.repositories.update(&repo).await.unwrap();
    // Shrinking works, so an overquota repository can be brought back under its limit...
    store
        .assets
        .publish(&new_asset(repo.id, "big", blob(bs.id, 60)))
        .await
        .unwrap();
    store
        .assets
        .publish(&new_asset(repo.id, "big", blob(bs.id, 40)))
        .await
        .unwrap();
    // ...and a same size replacement is not refused either.
    store
        .assets
        .publish(&new_asset(repo.id, "big", blob(bs.id, 40)))
        .await
        .unwrap();
    // Growing again is.
    let err = store
        .assets
        .publish(&new_asset(repo.id, "big", blob(bs.id, 51)))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_quota_is_exact_under_concurrent_publishers() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, mut repo) = seed_repository(&store).await;
    repo.quota_bytes = Some(100);
    store.repositories.update(&repo).await.unwrap();
    let mut tasks = JoinSet::new();
    for i in 0..20 {
        let assets = store.assets.clone();
        let new = new_asset(repo.id, &format!("p{i}"), blob(bs.id, 10));
        tasks.spawn(async move { assets.publish_with(&new, &WriteOptions::new()).await });
    }
    let mut wins = 0;
    while let Some(done) = tasks.join_next().await {
        match done.unwrap() {
            Ok(_) => wins += 1,
            Err(err) => assert!(matches!(violation(err), PolicyViolation::QuotaExceeded(_))),
        }
    }
    assert_eq!(wins, 10, "the check and the write are one critical section");
    assert_eq!(
        store.blobs.used_bytes_by_repository(repo.id).await.unwrap(),
        100
    );
    assert_eq!(
        count(&store, "blobs").await,
        10,
        "refused publishes left no blob row"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn events_are_written_in_the_transaction_of_the_publish_in_order() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let options = WriteOptions::new()
        .with_event(push_event(repo.id, "one"))
        .with_event(push_event(repo.id, "two"));
    store
        .assets
        .publish_with(&new_asset(repo.id, "a", blob(bs.id, 1)), &options)
        .await
        .unwrap();

    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT kind, actor_name, actor_kind, repository_id, request_id, data \
             FROM event_outbox ORDER BY seq",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "push");
    assert_eq!(rows[0].1, "test:publisher");
    assert_eq!(rows[0].2, "system");
    assert_eq!(rows[0].3, Some(repo.id.as_uuid()));
    assert_eq!(rows[0].4.as_deref(), Some("req-7"));
    assert_eq!(rows[0].5["path"], "one");
    assert_eq!(
        rows[1].5["path"], "two",
        "sequence follows the order of the drafts"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_event_is_invalid_input_and_nothing_is_written() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let mut draft = push_event(repo.id, "x");
    draft.data = serde_json::json!({ "blob": "x".repeat(17 * 1024) });
    let record = blob(bs.id, 1);
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "a", record.clone()),
            &WriteOptions::new().with_event(draft),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, MutationError::Store(StoreError::Invalid(_))),
        "{err:?}"
    );
    assert!(store.blobs.get(record.id).await.unwrap().is_none());
    assert_eq!(count(&store, "event_outbox").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_maintenance_job_does_not_fail_the_publish() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    // Every partition is gone, as if the maintenance job had never run.
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
         WHERE i.inhparent = 'event_outbox'::regclass",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    for name in names {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {name}")))
            .execute(store.pool())
            .await
            .unwrap();
    }
    let options = WriteOptions::new().with_event(push_event(repo.id, "x"));
    store
        .assets
        .publish_with(&new_asset(repo.id, "a", blob(bs.id, 1)), &options)
        .await
        .unwrap();
    assert_eq!(
        count(&store, "event_outbox").await,
        1,
        "partition created, event kept"
    );
    assert_eq!(count(&store, "assets").await, 1);
}
