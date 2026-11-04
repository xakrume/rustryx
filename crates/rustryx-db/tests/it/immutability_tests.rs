//! HB-24: immutability rules persist, a sixteenth rule of one scope is refused, and the
//! SQL function names the rule that covers a version.

use rustryx_core::ids::ImmutabilityRuleId;
use rustryx_core::lifecycle::{ImmutabilityRule, ItemKind, NameSelector, PolicyScope};
use rustryx_core::ports::{ImmutabilityRepo, PageRequest};
use rustryx_db::PgImmutabilityRepo;
use sqlx::postgres::PgDatabaseError;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::support::{TestDb, sqlstate};

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

fn rule(scope: PolicyScope, name: &str, glob: &str) -> ImmutabilityRule {
    ImmutabilityRule::new(
        ImmutabilityRuleId::generate(),
        scope,
        name,
        ItemKind::Version,
        NameSelector::matching(vec![glob.to_owned()]).unwrap(),
        OffsetDateTime::now_utc(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_matching_version_is_covered_and_a_sixteenth_rule_is_refused() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let repo_id = hosted(&pool, "libs").await;
    let scope = PolicyScope::Repository(repo_id.into());
    let store = PgImmutabilityRepo::new(pool.clone());
    let first = rule(scope, "releases", "1.*");
    store.create(&first).await.unwrap();
    let covered = store
        .covering_rule(repo_id.into(), ItemKind::Version, "1.2.0")
        .await
        .unwrap();
    assert_eq!(covered, Some(first.id));
    let missed = store
        .covering_rule(repo_id.into(), ItemKind::Version, "2.0.0")
        .await
        .unwrap();
    assert_eq!(missed, None);

    for n in 0..14 {
        store
            .create(&rule(scope, &format!("extra-{n}"), "9.*"))
            .await
            .unwrap();
    }
    let over = store.create(&rule(scope, "one-too-many", "9.*")).await;
    assert!(over.is_err(), "a scope stops at 15 rules");

    let page = store
        .list(
            scope,
            &PageRequest {
                limit: 100,
                after: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 15);
}

fn guard_detail(err: &sqlx::Error) -> Option<&str> {
    err.as_database_error()
        .and_then(|db| db.try_downcast_ref::<PgDatabaseError>())
        .and_then(PgDatabaseError::detail)
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_replace_or_delete_a_covered_asset() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let repo_id = hosted(&pool, "libs").await;
    let store_id: Uuid = sqlx::query_scalar("SELECT blob_store_id FROM repositories WHERE id = $1")
        .bind(repo_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let blob_id = Uuid::now_v7();
    let same_id = Uuid::now_v7();
    let other_id = Uuid::now_v7();
    let component_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let sha_same = "0".repeat(64);
    let sha_other = "1".repeat(64);
    for (id, key, sha) in [
        (blob_id, "k", sha_same.as_str()),
        (same_id, "k-same", sha_same.as_str()),
        (other_id, "k-other", sha_other.as_str()),
    ] {
        sqlx::query(
            "INSERT INTO blobs (id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at) \
             VALUES ($1, $2, $3, 1, $4, $5, $6, now())",
        )
        .bind(id)
        .bind(store_id)
        .bind(key)
        .bind("0".repeat(40))
        .bind(sha)
        .bind("0".repeat(32))
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO components (id, repository_id, format, group_id, name, version) \
         VALUES ($1, $2, 'maven2', NULL, 'c', '1.2.0')",
    )
    .bind(component_id)
    .bind(repo_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO assets (id, repository_id, component_id, path, blob_id, content_type) \
         VALUES ($1, $2, $3, 'p', $4, 'x/y')",
    )
    .bind(asset_id)
    .bind(repo_id)
    .bind(component_id)
    .bind(blob_id)
    .execute(&pool)
    .await
    .unwrap();

    let scope = PolicyScope::Repository(repo_id.into());
    let store = PgImmutabilityRepo::new(pool.clone());
    let rule = rule(scope, "releases", "1.*");
    store.create(&rule).await.unwrap();
    let rule_id = rule.id.as_uuid().to_string();

    // Idempotent re-push: a new blob row with the same sha256 is not a replacement.
    sqlx::query("UPDATE assets SET blob_id = $1 WHERE id = $2")
        .bind(same_id)
        .bind(asset_id)
        .execute(&pool)
        .await
        .unwrap();

    let replaced = sqlx::query("UPDATE assets SET blob_id = $1 WHERE id = $2")
        .bind(other_id)
        .bind(asset_id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&replaced), "RX004");
    assert_eq!(guard_detail(&replaced), Some(rule_id.as_str()));

    let deleted = sqlx::query("DELETE FROM assets WHERE id = $1")
        .bind(asset_id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&deleted), "RX004");
    assert_eq!(guard_detail(&deleted), Some(rule_id.as_str()));
}
