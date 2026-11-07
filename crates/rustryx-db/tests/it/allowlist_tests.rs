//! CVE exceptions: one id per scope, a project snapshot, and the ids a component may ignore.

use rustryx_core::ids::{ComponentId, CveAllowlistEntryId, ProjectId};
use rustryx_core::ports::{BlobStoreRepo, CveAllowlistRepo, PageRequest, RepositoryRepo};
use rustryx_core::scan::{AllowlistMode, AllowlistScope, CveAllowlistEntry};
use time::OffsetDateTime;

use crate::fixtures::{blob_store, hosted_repository, unique};
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn an_exception_is_unique_in_its_scope_and_applies_to_a_component() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let allow = rustryx_db::repos::PgCveAllowlistRepo::new(pool.clone());
    let project = ProjectId::generate();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(project.as_uuid())
        .bind(unique("cve"))
        .execute(&pool)
        .await
        .unwrap();
    let def = blob_store(&unique("blobs"));
    store.blob_stores.create(&def).await.unwrap();
    let repo = hosted_repository(&unique("lib"), def.id);
    store.repositories.create(&repo).await.unwrap();
    sqlx::query("UPDATE repositories SET project_id = $2 WHERE id = $1")
        .bind(repo.id.as_uuid())
        .bind(project.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let component = ComponentId::generate();
    sqlx::query(
        "INSERT INTO components (id, repository_id, format, name) VALUES ($1,$2,'maven2','left')",
    )
    .bind(component.as_uuid())
    .bind(repo.id.as_uuid())
    .execute(&pool)
    .await
    .unwrap();

    let system = CveAllowlistEntry::new(
        CveAllowlistEntryId::generate(),
        AllowlistScope::System,
        "CVE-2024-1",
        "accepted upstream",
        OffsetDateTime::now_utc(),
    );
    allow.create(&system).await.unwrap();
    assert!(allow.create(&system).await.is_err());
    let copied = allow.copy_system_to_project(project, None).await.unwrap();
    assert_eq!(copied, 1);
    assert_eq!(
        allow.copy_system_to_project(project, None).await.unwrap(),
        0
    );

    let mut own = CveAllowlistEntry::new(
        CveAllowlistEntryId::generate(),
        AllowlistScope::Component(component),
        "CVE-2024-2",
        "not reachable",
        OffsetDateTime::now_utc(),
    );
    own.expires_at = Some(OffsetDateTime::now_utc() + time::Duration::hours(1));
    allow.create(&own).await.unwrap();
    let ids = allow
        .active_ids(component, AllowlistMode::Project)
        .await
        .unwrap();
    assert_eq!(ids, ["CVE-2024-1", "CVE-2024-2"]);

    own.expires_at = Some(OffsetDateTime::UNIX_EPOCH);
    allow.update(&own, 1).await.unwrap();
    let page = allow
        .list(
            AllowlistScope::Component(component),
            false,
            &PageRequest {
                limit: 10,
                after: None,
            },
        )
        .await
        .unwrap();
    assert!(page.items.is_empty());
    let due = allow
        .take_expired_unaudited(OffsetDateTime::now_utc(), 10)
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert!(
        allow
            .take_expired_unaudited(OffsetDateTime::now_utc(), 10)
            .await
            .unwrap()
            .is_empty()
    );
}
