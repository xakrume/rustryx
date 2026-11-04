//! Labels: scope uniqueness, the project scope guard, and the 50 label cap.

use rustryx_core::StoreError;
use rustryx_core::ids::{ComponentId, LabelId, ProjectId, UserId};
use rustryx_core::label::{Label, LabelName, LabelScope};
use rustryx_core::ports::{ComponentRepo, LabelRepo, PageRequest};
use rustryx_db::repos::label::PgLabelRepo;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::fixtures::{new_component, seed_repository};
use crate::support::TestDb;

fn name(raw: &str) -> LabelName {
    LabelName::parse(raw).expect("name")
}

fn label(scope: LabelScope, raw: &str) -> Label {
    Label::new(LabelId::generate(), scope, name(raw), OffsetDateTime::UNIX_EPOCH)
}

fn page(limit: u32) -> PageRequest {
    PageRequest { limit, after: None }
}

async fn insert_project(pool: &PgPool, project_name: &str) -> ProjectId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(project_name)
        .execute(pool)
        .await
        .unwrap();
    ProjectId::from_uuid(id)
}

async fn insert_user(pool: &PgPool, username: &str) -> UserId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, source) VALUES ($1, $2, 'local')")
        .bind(id)
        .bind(username)
        .execute(pool)
        .await
        .unwrap();
    UserId::from_uuid(id)
}

#[tokio::test(flavor = "multi_thread")]
async fn system_and_project_labels_share_a_name_but_not_a_scope() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let project = insert_project(&pool, "payments").await;
    let repo = PgLabelRepo::new(pool);

    let system = label(LabelScope::System, "prod");
    let project_label = label(LabelScope::Project(project), "prod");
    repo.create(&system).await.unwrap();
    repo.create(&project_label).await.unwrap();

    let duplicate = label(LabelScope::System, "prod");
    let clash = repo.create(&duplicate).await.unwrap_err();
    assert!(
        matches!(clash, StoreError::Conflict(ref text) if text.contains("already")),
        "{clash}"
    );
    let missing = label(LabelScope::Project(ProjectId::generate()), "other");
    let fk = repo.create(&missing).await.unwrap_err();
    assert!(matches!(fk, StoreError::Conflict(_)), "{fk}");

    let mut renamed = project_label.clone();
    renamed.scope = LabelScope::System;
    renamed.name = name("renamed");
    renamed.color = "#112233".to_owned();
    repo.update(&renamed, 1).await.unwrap();
    let stored = repo.get(project_label.id).await.unwrap().unwrap();
    assert_eq!(stored.scope, LabelScope::Project(project));
    assert_eq!(stored.version, 2);
    assert_eq!(stored.name.as_str(), "renamed");
    assert_eq!(stored.color, "#112233");
    assert_eq!(stored.created_at, project_label.created_at);

    let stale = repo.update(&renamed, 1).await.unwrap_err();
    assert!(matches!(stale, StoreError::Conflict(_)), "{stale}");
    let absent = repo
        .update(&label(LabelScope::System, "gone"), 1)
        .await
        .unwrap_err();
    assert!(matches!(absent, StoreError::NotFound(_)), "{absent}");

    let mut bad = stored.clone();
    bad.color = "red".to_owned();
    let color = repo.update(&bad, 2).await.unwrap_err();
    assert!(matches!(color, StoreError::Conflict(_)), "{color}");

    let second = label(LabelScope::System, "aaa");
    repo.create(&second).await.unwrap();
    let first_page = repo.list(LabelScope::System, &page(1)).await.unwrap();
    assert_eq!(first_page.items.len(), 1);
    assert!(first_page.next.is_some());
    let mut next = page(1);
    next.after = first_page.next;
    let second_page = repo.list(LabelScope::System, &next).await.unwrap();
    assert_eq!(second_page.items.len(), 1);
    assert!(second_page.next.is_none());
    assert_ne!(first_page.items[0].id, second_page.items[0].id);
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_is_idempotent_and_delete_counts_the_link() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let project = insert_project(&pool, "libs").await;
    let user = insert_user(&pool, "ada").await;
    let other = insert_user(&pool, "bea").await;
    let (_, repo_row) = seed_repository(&store).await;
    let (_, outside) = seed_repository(&store).await;
    sqlx::query("UPDATE repositories SET project_id = $2 WHERE id = $1")
        .bind(repo_row.id.as_uuid())
        .bind(project.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let inside = store
        .components
        .find_or_create(&new_component(repo_row.id, None, "lib", Some("1")))
        .await
        .unwrap();
    let foreign = store
        .components
        .find_or_create(&new_component(outside.id, None, "lib", Some("1")))
        .await
        .unwrap();
    let labels = PgLabelRepo::new(pool.clone());
    let project_label = label(LabelScope::Project(project), "stable");
    labels.create(&project_label).await.unwrap();

    labels
        .apply(inside.id, project_label.id, Some(user))
        .await
        .unwrap();
    labels
        .apply(inside.id, project_label.id, Some(other))
        .await
        .unwrap();
    let applied_by: Uuid = sqlx::query_scalar(
        "SELECT applied_by FROM component_labels WHERE component_id = $1 AND label_id = $2",
    )
    .bind(inside.id.as_uuid())
    .bind(project_label.id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied_by, user.as_uuid());
    assert_eq!(labels.usage(project_label.id).await.unwrap(), 1);
    assert_eq!(labels.usage(LabelId::generate()).await.unwrap(), 0);
    let of = labels.labels_of(inside.id).await.unwrap();
    assert_eq!(of.len(), 1);
    assert_eq!(of[0].id, project_label.id);

    let scope = labels
        .apply(foreign.id, project_label.id, Some(user))
        .await
        .unwrap_err();
    assert!(matches!(scope, StoreError::Conflict(_)), "{scope}");
    let no_label = labels
        .apply(inside.id, LabelId::generate(), None)
        .await
        .unwrap_err();
    assert!(matches!(no_label, StoreError::NotFound(_)), "{no_label}");
    let no_component = labels
        .apply(ComponentId::generate(), project_label.id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(no_component, StoreError::NotFound(_)),
        "{no_component}"
    );

    labels.unapply(inside.id, project_label.id).await.unwrap();
    labels.unapply(inside.id, project_label.id).await.unwrap();
    assert!(labels.labels_of(inside.id).await.unwrap().is_empty());
    labels
        .apply(inside.id, project_label.id, Some(user))
        .await
        .unwrap();
    assert_eq!(labels.delete(project_label.id).await.unwrap(), 1);
    let again = labels.delete(project_label.id).await.unwrap_err();
    assert!(matches!(again, StoreError::NotFound(_)), "{again}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_51st_label_on_one_component_is_invalid() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repository) = seed_repository(&store).await;
    let component = store
        .components
        .find_or_create(&new_component(repository.id, None, "lib", Some("1")))
        .await
        .unwrap();
    let labels = PgLabelRepo::new(store.pool().clone());
    for index in 0..50 {
        let label = label(LabelScope::System, &format!("n{index}"));
        labels.create(&label).await.unwrap();
        labels.apply(component.id, label.id, None).await.unwrap();
    }
    let extra = label(LabelScope::System, "n50");
    labels.create(&extra).await.unwrap();
    let err = labels
        .apply(component.id, extra.id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Invalid(ref text) if text.contains("50")),
        "{err}"
    );
    let found = labels.labels_of(component.id).await.unwrap();
    assert_eq!(found.len(), 50);
    assert!(found.windows(2).all(|pair| pair[0].name <= pair[1].name));
}
