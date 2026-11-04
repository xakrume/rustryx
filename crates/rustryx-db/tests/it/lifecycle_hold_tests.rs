//! A legal hold on a repository is visible to `rx_active_legal_hold` until it is released.

use rustryx_core::ids::LifecycleHoldId;
use rustryx_core::lifecycle::{HoldKind, HoldTarget, LifecycleHold};
use rustryx_core::ports::LifecycleHoldRepo;
use rustryx_db::repos::lifecycle_hold::PgLifecycleHoldRepo;
use sqlx::postgres::PgDatabaseError;
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

#[tokio::test(flavor = "multi_thread")]
async fn a_legal_hold_on_a_repository_is_active_until_released() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let repo_id = hosted(&pool, "libs").await;
    let component_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO components (id, repository_id, format, group_id, name, version) \
         VALUES ($1, $2, 'maven2', NULL, 'c', '1.0.0')",
    )
    .bind(component_id)
    .bind(repo_id)
    .execute(&pool)
    .await
    .unwrap();

    let hold = LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Legal,
        HoldTarget::Repository(repo_id.into()),
        "litigation",
        OffsetDateTime::now_utc(),
    );
    let store = PgLifecycleHoldRepo::new(pool.clone());
    store.create(&hold).await.unwrap();

    let found: Option<Uuid> = sqlx::query_scalar("SELECT rx_active_legal_hold($1, $2)")
        .bind(repo_id)
        .bind(component_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(found, Some(hold.id.as_uuid()));

    store
        .release(hold.id, None, OffsetDateTime::now_utc())
        .await
        .unwrap();
    let after: Option<Uuid> = sqlx::query_scalar("SELECT rx_active_legal_hold($1, $2)")
        .bind(repo_id)
        .bind(component_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, None);
}

fn guard_detail(err: &sqlx::Error) -> Option<&str> {
    err.as_database_error()
        .and_then(|db| db.try_downcast_ref::<PgDatabaseError>())
        .and_then(PgDatabaseError::detail)
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_replace_or_delete_a_held_asset() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let repo_id = hosted(&pool, "held").await;
    let store_id: Uuid = sqlx::query_scalar("SELECT blob_store_id FROM repositories WHERE id = $1")
        .bind(repo_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let blob_id = Uuid::now_v7();
    let other_id = Uuid::now_v7();
    let component_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    for (id, key, sha) in [(blob_id, "k", "0"), (other_id, "k2", "1")] {
        sqlx::query(
            "INSERT INTO blobs (id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at) \
             VALUES ($1, $2, $3, 1, $4, $5, $6, now())",
        )
        .bind(id)
        .bind(store_id)
        .bind(key)
        .bind("0".repeat(40))
        .bind(sha.repeat(64))
        .bind("0".repeat(32))
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO components (id, repository_id, format, group_id, name, version) \
         VALUES ($1, $2, 'maven2', NULL, 'c', '1.0.0')",
    )
    .bind(component_id)
    .bind(repo_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO assets (id, repository_id, component_id, path, blob_id, content_type) \
         VALUES ($1, $2, $3, 'p', $4, 'application/octet-stream')",
    )
    .bind(asset_id)
    .bind(repo_id)
    .bind(component_id)
    .bind(blob_id)
    .execute(&pool)
    .await
    .unwrap();

    let hold = LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Legal,
        HoldTarget::Repository(repo_id.into()),
        "litigation",
        OffsetDateTime::now_utc(),
    );
    PgLifecycleHoldRepo::new(pool.clone())
        .create(&hold)
        .await
        .unwrap();

    let replaced = sqlx::query("UPDATE assets SET blob_id = $2 WHERE id = $1")
        .bind(asset_id)
        .bind(other_id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        guard_detail(&replaced),
        Some(hold.id.as_uuid().to_string()).as_deref()
    );

    let deleted = sqlx::query("DELETE FROM assets WHERE id = $1")
        .bind(asset_id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        guard_detail(&deleted),
        Some(hold.id.as_uuid().to_string()).as_deref()
    );
}
