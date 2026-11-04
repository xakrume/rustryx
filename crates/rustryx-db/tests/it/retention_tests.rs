//! One retention policy per repository, replaced only with the expected version.

use rustryx_core::ids::RetentionPolicyId;
use rustryx_core::lifecycle::{
    NameSelector, PolicyScope, RetentionPolicy, RetentionRule, RetentionTemplate,
};
use rustryx_core::ports::RetentionPolicyRepo;
use rustryx_db::PgRetentionPolicyRepo;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::support::TestDb;

async fn hosted(pool: &sqlx::PgPool, name: &str) -> Uuid {
    let store_id = Uuid::now_v7();
    let repo_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO blob_stores (id, name, kind, config, quota_bytes, created_at) \
         VALUES ($1, $2, 'local', '{}', NULL, now())",
    )
    .bind(store_id)
    .bind(format!("store-{name}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO repositories (id, name, format, repo_type, blob_store_id, write_policy, \
         format_config) VALUES ($1, $2, 'maven2', 'hosted', $3, 'allow', '{}')",
    )
    .bind(repo_id)
    .bind(name)
    .bind(store_id)
    .execute(pool)
    .await
    .unwrap();
    repo_id
}

fn policy(scope: PolicyScope, name: &str) -> RetentionPolicy {
    RetentionPolicy::new(
        RetentionPolicyId::generate(),
        scope,
        name,
        vec![RetentionRule {
            template: RetentionTemplate::KeepLastPushed { count: 10 },
            repository: NameSelector::everything(),
            item: NameSelector::everything(),
            include_untagged: false,
        }],
        OffsetDateTime::now_utc(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scope_keeps_one_retention_policy_and_rejects_a_stale_write() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let repo_id = hosted(&pool, "libs").await;
    let scope = PolicyScope::Repository(repo_id.into());
    let store = PgRetentionPolicyRepo::new(pool);
    let first = policy(scope, "keep-ten");
    store.upsert(&first, None).await.unwrap();
    let again = store.upsert(&policy(scope, "other"), None).await;
    assert!(
        again.is_err(),
        "a second insert without a version is refused"
    );

    let mut next = first.clone();
    next.name = "keep-ten-renamed".to_owned();
    store.upsert(&next, Some(1)).await.unwrap();
    let stored = store.get_for_scope(scope).await.unwrap().unwrap();
    assert_eq!(stored.id, first.id);
    assert_eq!(stored.version, 2);
    assert_eq!(stored.name, "keep-ten-renamed");

    let stale = store.upsert(&next, Some(1)).await;
    assert!(stale.is_err(), "version 1 is no longer current");
}
