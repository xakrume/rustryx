//! Project members: uniqueness, the last admin, and the RBAC read side.

use rustryx_core::StoreError;
use rustryx_core::identity::IdpProvider;
use rustryx_core::ids::{ProjectId, ProjectMemberId, UserId};
use rustryx_core::ports::{PageRequest, ProjectAccessRepo, ProjectMemberRepo};
use rustryx_core::project::{ProjectMember, ProjectRole, ProjectSubject};
use rustryx_db::repos::project_member::{PgProjectAccessRepo, PgProjectMemberRepo};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::support::TestDb;

fn page(limit: u32) -> PageRequest {
    PageRequest { limit, after: None }
}

async fn insert_project(pool: &PgPool, name: &str) -> ProjectId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(name)
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

fn user_member(project: ProjectId, user: UserId, role: ProjectRole) -> ProjectMember {
    ProjectMember::new(
        ProjectMemberId::generate(),
        project,
        ProjectSubject::User { user_id: user },
        role,
        OffsetDateTime::UNIX_EPOCH,
    )
}

fn group_member(project: ProjectId, group: &str, role: ProjectRole) -> ProjectMember {
    ProjectMember::new(
        ProjectMemberId::generate(),
        project,
        ProjectSubject::Group {
            provider: IdpProvider::Oidc,
            external_group: group.to_owned(),
        },
        role,
        OffsetDateTime::UNIX_EPOCH,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn members_are_unique_per_subject_and_listed_by_id() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let project = insert_project(&pool, "payments").await;
    let user = insert_user(&pool, "ada").await;
    let repo = PgProjectMemberRepo::new(pool);

    let direct = user_member(project, user, ProjectRole::Developer);
    repo.add(&direct).await.unwrap();
    let again = user_member(project, user, ProjectRole::Guest);
    let clash = repo.add(&again).await.unwrap_err();
    assert!(
        matches!(clash, StoreError::Conflict(ref text) if text.contains("already has a role")),
        "{clash}"
    );

    let orphan = user_member(ProjectId::generate(), user, ProjectRole::Guest);
    let missing = repo.add(&orphan).await.unwrap_err();
    assert!(matches!(missing, StoreError::Conflict(_)), "{missing}");

    let empty = group_member(project, "", ProjectRole::Guest);
    let empty_group = repo.add(&empty).await.unwrap_err();
    assert!(matches!(empty_group, StoreError::Conflict(_)), "{empty_group}");

    let group = group_member(project, "/dev", ProjectRole::Guest);
    repo.add(&group).await.unwrap();

    assert_eq!(
        repo.get(direct.id).await.unwrap().unwrap().role,
        ProjectRole::Developer
    );
    repo.set_role(direct.id, ProjectRole::Maintainer, false)
        .await
        .unwrap();
    assert_eq!(
        repo.get(direct.id).await.unwrap().unwrap().role,
        ProjectRole::Maintainer
    );

    let listed = repo.list(project, &page(1)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert!(listed.next.is_some());

    assert!(repo.get(ProjectMemberId::generate()).await.unwrap().is_none());
    assert!(matches!(
        repo.set_role(ProjectMemberId::generate(), ProjectRole::Guest, false)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        repo.remove(ProjectMemberId::generate(), false).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_project_admin_stays_unless_the_guard_is_turned_off() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let project = insert_project(&pool, "guarded").await;
    let user = insert_user(&pool, "admin").await;
    let repo = PgProjectMemberRepo::new(pool);

    let first = user_member(project, user, ProjectRole::ProjectAdmin);
    let second = group_member(project, "/admins", ProjectRole::ProjectAdmin);
    repo.add(&first).await.unwrap();
    repo.add(&second).await.unwrap();

    repo.remove(second.id, false).await.unwrap();
    let demote = repo
        .set_role(first.id, ProjectRole::Guest, false)
        .await
        .unwrap_err();
    assert!(matches!(demote, StoreError::Conflict(_)), "{demote}");
    let remove = repo.remove(first.id, false).await.unwrap_err();
    assert!(matches!(remove, StoreError::Conflict(_)), "{remove}");

    repo.set_role(first.id, ProjectRole::ProjectAdmin, false)
        .await
        .unwrap();
    repo.set_role(first.id, ProjectRole::Guest, true)
        .await
        .unwrap();
    repo.remove(first.id, true).await.unwrap();
    assert!(repo.get(first.id).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn access_lists_user_memberships_and_bounds_group_rows() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let first = insert_project(&pool, "access-one").await;
    let second = insert_project(&pool, "access-two").await;
    let user = insert_user(&pool, "member").await;
    let members = PgProjectMemberRepo::new(pool.clone());
    let access = PgProjectAccessRepo::new(pool);

    members
        .add(&user_member(first, user, ProjectRole::Guest))
        .await
        .unwrap();
    members
        .add(&user_member(second, user, ProjectRole::Maintainer))
        .await
        .unwrap();
    members
        .add(&group_member(first, "/b", ProjectRole::Developer))
        .await
        .unwrap();
    members
        .add(&group_member(second, "/a", ProjectRole::Guest))
        .await
        .unwrap();
    let mut ldap = group_member(second, "cn=ops", ProjectRole::Guest);
    ldap.subject = ProjectSubject::Group {
        provider: IdpProvider::Ldap,
        external_group: "cn=ops".to_owned(),
    };
    members.add(&ldap).await.unwrap();

    let memberships = access.memberships_of_user(user).await.unwrap();
    assert_eq!(memberships.len(), 2);
    assert!(memberships.contains(&(first, ProjectRole::Guest)));
    assert!(memberships.contains(&(second, ProjectRole::Maintainer)));

    let groups = access.group_mappings(IdpProvider::Oidc, 10).await.unwrap();
    let mut names: Vec<&str> = groups.iter().map(|(group, _, _)| group.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["/a", "/b"]);
    let ldap_rows = access.group_mappings(IdpProvider::Ldap, 10).await.unwrap();
    assert_eq!(ldap_rows.len(), 1);
    let too_many = access
        .group_mappings(IdpProvider::Oidc, 1)
        .await
        .unwrap_err();
    assert!(
        matches!(too_many, StoreError::Invalid(ref text) if text.contains("more than 1")),
        "{too_many}"
    );
}
