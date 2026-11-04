//! A project quota counts distinct blob bytes and leaves that much room.

use rustryx_core::audit::ActorRef;
use rustryx_core::ids::RepositoryId;
use rustryx_core::ports::QuotaRepo;
use rustryx_core::quota::QuotaScope;
use rustryx_db::repos::quota::PgQuotaRepo;
use uuid::Uuid;

use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn recount_sums_distinct_blobs_and_headroom_is_what_remains() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let project_id = Uuid::now_v7();
    let store_id = Uuid::now_v7();
    let repo_id = Uuid::now_v7();
    let blob_id = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, 'libs')")
        .bind(project_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO blob_stores (id, name, kind, config) VALUES ($1, 'q', 'local', '{}')")
        .bind(store_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO repositories (id, name, format, repo_type, blob_store_id, write_policy, \
         project_id, format_config) VALUES ($1, 'libs-hosted', 'maven2', 'hosted', $2, 'allow', $3, '{}')",
    )
    .bind(repo_id)
    .bind(store_id)
    .bind(project_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO blobs (id, blob_store_id, key, size_bytes, sha1, sha256, md5) \
         VALUES ($1, $2, 'k', 40, $3, $4, $5)",
    )
    .bind(blob_id)
    .bind(store_id)
    .bind("0".repeat(40))
    .bind("0".repeat(64))
    .bind("0".repeat(32))
    .execute(&pool)
    .await
    .unwrap();
    let component_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO components (id, repository_id, format, name, version) \
         VALUES ($1, $2, 'maven2', 'c', '1')",
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
    .bind(Uuid::now_v7())
    .bind(repo_id)
    .bind(component_id)
    .bind(blob_id)
    .execute(&pool)
    .await
    .unwrap();

    let store = PgQuotaRepo::new(pool);
    let scope = QuotaScope::Project(project_id.into());
    let (before, after) = store.recount(scope).await.unwrap();
    assert_eq!((before, after), (0, 40));
    store
        .set_limit(scope, Some(100), 80, Some(1), &ActorRef::system("test"))
        .await
        .unwrap();
    let room = store
        .headroom(RepositoryId::from_uuid(repo_id))
        .await
        .unwrap();
    assert_eq!(room.bytes, Some(60));
    assert_eq!(room.binding, Some(scope));
}
