//! Projects, members, project access, labels, robot tokens and the token expiry notice.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    clippy::assert_is_empty,
    reason = "each test is one scenario read top to bottom; filters and non exhaustive types are \
              filled field by field after `default()`, and `.is_empty()` reads better than a \
              comparison with an empty literal of an inferred type"
)]

mod common;

use std::time::Duration;

use common::{Ports, T0, env, hosted, page, project, public_project};
use futures::StreamExt;
use rustryx_core::error::StoreError;
use rustryx_core::identity::{ApiToken, IdpProvider};
use rustryx_core::ids::{ApiTokenId, ComponentId, LabelId, ProjectMemberId, RobotTokenId, UserId};
use rustryx_core::label::{Label, LabelName, LabelScope};
use rustryx_core::model::{NewComponent, RepoFormat};
use rustryx_core::ports::{
    InvalidationBus, InvalidationEvent, PageRequest, ProjectVisibilityFilter, RobotFilter,
    RobotState,
};
use rustryx_core::project::{
    ProjectMember, ProjectName, ProjectRole, ProjectSubject, ProjectVisibility,
};
use rustryx_core::robot::RobotToken;
use rustryx_testkit::MemInvalidationBus;

fn user_member(
    project: rustryx_core::ids::ProjectId,
    user: UserId,
    role: ProjectRole,
) -> ProjectMember {
    ProjectMember::new(
        ProjectMemberId::generate(),
        project,
        ProjectSubject::User { user_id: user },
        role,
        T0,
    )
}

fn group_member(
    project: rustryx_core::ids::ProjectId,
    group: &str,
    role: ProjectRole,
) -> ProjectMember {
    ProjectMember::new(
        ProjectMemberId::generate(),
        project,
        ProjectSubject::Group {
            provider: IdpProvider::Oidc,
            external_group: group.to_owned(),
        },
        role,
        T0,
    )
}

fn label(scope: LabelScope, name: &str) -> Label {
    Label::new(
        LabelId::generate(),
        scope,
        LabelName::parse(name).unwrap(),
        T0,
    )
}

fn robot(name: &str, hash: &str) -> RobotToken {
    RobotToken::new(
        RobotTokenId::generate(),
        name.to_owned(),
        name.to_owned(),
        vec!["repo:*:browse".to_owned(), "repo:*:read".to_owned()],
        hash.to_owned(),
        "rx_pfx".to_owned(),
        T0,
    )
}

// projects

#[tokio::test]
async fn a_project_is_created_with_its_first_admin_and_found_by_id_and_name() {
    let env = env();
    let admin = UserId::generate();
    let created = rustryx_core::project::Project::new(
        rustryx_core::ids::ProjectId::generate(),
        ProjectName::parse("payments").unwrap(),
        T0,
    );
    env.store
        .projects()
        .create(&created, Some(admin))
        .await
        .unwrap();
    assert_eq!(
        env.store.access().memberships_of_user(admin).await.unwrap(),
        [(created.id, ProjectRole::ProjectAdmin)]
    );
    assert!(
        env.store
            .projects()
            .get(created.id)
            .await
            .unwrap()
            .is_some()
    );
    let by_name = env
        .store
        .projects()
        .get_by_name(&ProjectName::parse("payments").unwrap())
        .await
        .unwrap();
    assert_eq!(by_name.map(|p| p.id), Some(created.id));
    let clash = env
        .store
        .projects()
        .create(&created, None)
        .await
        .unwrap_err();
    assert!(matches!(clash, StoreError::Conflict(_)));
    let mut same_name = created.clone();
    same_name.id = rustryx_core::ids::ProjectId::generate();
    assert!(matches!(
        env.store.projects().create(&same_name, None).await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn updating_a_project_needs_the_current_version_and_rewrites_its_repositories() {
    let env = env();
    let bus = MemInvalidationBus::new();
    let store = env.store.clone().with_invalidation_bus(bus.clone());
    let mut events = bus.subscribe();
    let created = project(&store, "visibility").await;
    let repo = hosted(&store, "member-repo").await;
    store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::Projects));

    let mut edited = created.clone();
    edited.visibility = ProjectVisibility::Public;
    edited.description = "now public".to_owned();
    edited.name = ProjectName::parse("renamed").unwrap();
    store.projects().update(&edited, 1).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::Projects));
    let stored = store.projects().get(created.id).await.unwrap().unwrap();
    assert_eq!(stored.version, 2);
    assert_eq!(
        stored.name.as_str(),
        "visibility",
        "the name is not editable"
    );
    assert_eq!(stored.description, "now public");
    let attached = store.repos().get(repo.id).await.unwrap().unwrap();
    assert!(attached.allow_anonymous);
    assert_eq!(
        attached.project.as_ref().map(|p| p.visibility),
        Some(ProjectVisibility::Public)
    );

    let stale = store.projects().update(&edited, 1).await.unwrap_err();
    assert!(matches!(stale, StoreError::Conflict(_)));
    let mut ghost = created.clone();
    ghost.id = rustryx_core::ids::ProjectId::generate();
    assert!(matches!(
        store.projects().update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));
    // Back to private: the repository stops being public with it.
    let mut private = stored.clone();
    private.visibility = ProjectVisibility::Private;
    store.projects().update(&private, 2).await.unwrap();
    assert!(
        !store
            .repos()
            .get(repo.id)
            .await
            .unwrap()
            .unwrap()
            .allow_anonymous
    );
}

#[tokio::test]
async fn must_not_delete_a_project_that_still_has_repositories() {
    let env = env();
    let created = project(&env.store, "busy").await;
    let repo = hosted(&env.store, "attached").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    let error = env.store.projects().delete(created.id).await.unwrap_err();
    assert!(matches!(error, StoreError::Conflict(_)));
    let dependents = env.store.projects().dependents(created.id).await.unwrap();
    assert_eq!(dependents.repositories, [repo.id]);
    assert_eq!((dependents.robots, dependents.webhooks), (0, 0));

    env.store
        .projects()
        .detach_repository(repo.id)
        .await
        .unwrap();
    env.store.projects().delete(created.id).await.unwrap();
    assert!(
        env.store
            .projects()
            .get(created.id)
            .await
            .unwrap()
            .is_none()
    );
    let again = env.store.projects().delete(created.id).await.unwrap_err();
    assert!(matches!(again, StoreError::NotFound(_)));
}

#[tokio::test]
async fn deleting_a_project_removes_what_hangs_off_it() {
    let env = env();
    let created = project(&env.store, "cascade").await;
    env.store
        .members()
        .add(&user_member(
            created.id,
            UserId::generate(),
            ProjectRole::Developer,
        ))
        .await
        .unwrap();
    let mut scoped_robot = robot("scoped", "h-scoped");
    scoped_robot.project_id = Some(created.id);
    env.store.robots().create(&scoped_robot).await.unwrap();
    let scoped_label = label(LabelScope::Project(created.id), "team");
    env.store.labels().create(&scoped_label).await.unwrap();
    let dependents = env.store.projects().dependents(created.id).await.unwrap();
    assert_eq!(dependents.robots, 1);

    env.store.projects().delete(created.id).await.unwrap();
    assert!(
        env.store
            .robots()
            .get(scoped_robot.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .labels()
            .get(scoped_label.id)
            .await
            .unwrap()
            .is_none()
    );
    let members = env
        .store
        .members()
        .list(created.id, &page(10))
        .await
        .unwrap();
    assert!(members.items.is_empty());
}

#[tokio::test]
async fn visible_projects_follow_the_filter_the_checker_produced() {
    let env = env();
    let private_one = project(&env.store, "alpha").await;
    let private_two = project(&env.store, "beta").await;
    public_project(&env.store, "gamma").await;
    let user = UserId::generate();
    env.store
        .members()
        .add(&user_member(private_one.id, user, ProjectRole::Guest))
        .await
        .unwrap();
    env.store
        .members()
        .add(&group_member(
            private_two.id,
            "/teams/ops",
            ProjectRole::Developer,
        ))
        .await
        .unwrap();
    let repo = hosted(&env.store, "alpha-repo").await;
    env.store
        .projects()
        .attach_repository(private_one.id, repo.id)
        .await
        .unwrap();

    let names = |filter: ProjectVisibilityFilter, query: Option<&'static str>| {
        let store = env.store.clone();
        async move {
            store
                .projects()
                .list_visible(&filter, query, &page(10))
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|s| s.project.name.as_str().to_owned())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        names(ProjectVisibilityFilter::All, None).await,
        ["alpha", "beta", "gamma"]
    );
    assert_eq!(
        names(ProjectVisibilityFilter::PublicOnly, None).await,
        ["gamma"]
    );
    let member = ProjectVisibilityFilter::Member {
        user_id: Some(user),
        provider: Some(IdpProvider::Oidc),
        groups: vec!["/teams/ops".to_owned()],
    };
    assert_eq!(names(member, None).await, ["alpha", "beta", "gamma"]);
    let wrong_provider = ProjectVisibilityFilter::Member {
        user_id: None,
        provider: Some(IdpProvider::Ldap),
        groups: vec!["/teams/ops".to_owned()],
    };
    assert_eq!(
        names(wrong_provider, None).await,
        ["gamma"],
        "the provider must match"
    );
    let prefix_only = ProjectVisibilityFilter::Member {
        user_id: None,
        provider: Some(IdpProvider::Oidc),
        groups: vec!["/teams".to_owned()],
    };
    assert_eq!(
        names(prefix_only, None).await,
        ["gamma"],
        "groups match exactly, never by prefix"
    );
    assert_eq!(
        names(ProjectVisibilityFilter::All, Some(" ALP ")).await,
        ["alpha"]
    );

    let listed = env
        .store
        .projects()
        .list_visible(&ProjectVisibilityFilter::All, None, &page(1))
        .await
        .unwrap();
    assert_eq!(listed.items[0].repository_count, 1);
    assert_eq!(listed.items[0].member_count, 1);
    let next = listed.next.clone().unwrap();
    let second = env
        .store
        .projects()
        .list_visible(
            &ProjectVisibilityFilter::All,
            None,
            &PageRequest {
                limit: 5,
                after: Some(next),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 2);
    let bad = env
        .store
        .projects()
        .list_visible(
            &ProjectVisibilityFilter::All,
            None,
            &PageRequest {
                limit: 5,
                after: Some("junk".to_owned()),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(bad, StoreError::Invalid(_)));
}

#[tokio::test]
async fn attach_and_detach_keep_the_visibility_invariant_and_move_label_links() {
    let env = env();
    let public = public_project(&env.store, "front").await;
    let other = project(&env.store, "back").await;
    let repo = hosted(&env.store, "moving").await;
    let mut new = NewComponent::new(repo.id, RepoFormat::Raw, "pkg");
    new.version = Some("1".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();

    env.store
        .projects()
        .attach_repository(public.id, repo.id)
        .await
        .unwrap();
    assert!(
        env.store
            .repos()
            .get(repo.id)
            .await
            .unwrap()
            .unwrap()
            .allow_anonymous
    );
    // Attaching again to the same project is a no-op, to another project a conflict.
    env.store
        .projects()
        .attach_repository(public.id, repo.id)
        .await
        .unwrap();
    let elsewhere = env
        .store
        .projects()
        .attach_repository(other.id, repo.id)
        .await
        .unwrap_err();
    assert!(matches!(elsewhere, StoreError::Conflict(_)));
    for missing in [
        env.store
            .projects()
            .attach_repository(rustryx_core::ids::ProjectId::generate(), repo.id)
            .await,
        env.store
            .projects()
            .attach_repository(public.id, rustryx_core::ids::RepositoryId::generate())
            .await,
        env.store
            .projects()
            .detach_repository(rustryx_core::ids::RepositoryId::generate())
            .await,
    ] {
        assert!(matches!(missing, Err(StoreError::NotFound(_))));
    }

    let project_label = label(LabelScope::Project(public.id), "front-only");
    let system_label = label(LabelScope::System, "everywhere");
    env.store.labels().create(&project_label).await.unwrap();
    env.store.labels().create(&system_label).await.unwrap();
    env.store
        .labels()
        .apply(component.id, project_label.id, None)
        .await
        .unwrap();
    env.store
        .labels()
        .apply(component.id, system_label.id, None)
        .await
        .unwrap();

    env.store
        .projects()
        .detach_repository(repo.id)
        .await
        .unwrap();
    let detached = env.store.repos().get(repo.id).await.unwrap().unwrap();
    assert!(
        !detached.allow_anonymous,
        "a detached repository is never public by accident"
    );
    assert!(detached.project.is_none());

    // Moving into another project drops the links of project labels of the old one.
    env.store
        .projects()
        .attach_repository(other.id, repo.id)
        .await
        .unwrap();
    let remaining = env.store.labels().labels_of(component.id).await.unwrap();
    assert_eq!(
        remaining.iter().map(|l| l.id).collect::<Vec<_>>(),
        [system_label.id]
    );
}

// members

#[tokio::test]
async fn members_are_unique_per_subject_and_events_name_the_user_or_the_projects() {
    let env = env();
    let bus = MemInvalidationBus::new();
    let store = env.store.clone().with_invalidation_bus(bus.clone());
    let mut events = bus.subscribe();
    let created = project(&store, "team").await;
    let user = UserId::generate();
    let direct = user_member(created.id, user, ProjectRole::Developer);
    store.members().add(&direct).await.unwrap();
    assert_eq!(
        events.next().await,
        Some(InvalidationEvent::User(user.as_uuid()))
    );
    let group = group_member(created.id, "/dev", ProjectRole::Guest);
    store.members().add(&group).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::Projects));

    let twice = user_member(created.id, user, ProjectRole::Guest);
    assert!(matches!(
        store.members().add(&twice).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        store
            .members()
            .add(&group_member(created.id, "/dev", ProjectRole::Guest))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let empty_group = group_member(created.id, "", ProjectRole::Guest);
    assert!(matches!(
        store.members().add(&empty_group).await,
        Err(StoreError::Conflict(_))
    ));
    let orphan = user_member(
        rustryx_core::ids::ProjectId::generate(),
        user,
        ProjectRole::Guest,
    );
    assert!(matches!(
        store.members().add(&orphan).await,
        Err(StoreError::Conflict(_))
    ));

    assert_eq!(
        store.members().get(direct.id).await.unwrap().unwrap().role,
        ProjectRole::Developer
    );
    store
        .members()
        .set_role(direct.id, ProjectRole::Maintainer, false)
        .await
        .unwrap();
    assert_eq!(
        store.members().get(direct.id).await.unwrap().unwrap().role,
        ProjectRole::Maintainer
    );
    let listed = store.members().list(created.id, &page(1)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert!(listed.next.is_some());
    assert!(
        store
            .members()
            .get(ProjectMemberId::generate())
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store
            .members()
            .set_role(ProjectMemberId::generate(), ProjectRole::Guest, false)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store
            .members()
            .remove(ProjectMemberId::generate(), false)
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn must_not_leave_a_project_without_an_admin_unless_an_admin_of_the_system_says_so() {
    let env = env();
    let created = project(&env.store, "guarded").await;
    let first = user_member(created.id, UserId::generate(), ProjectRole::ProjectAdmin);
    let second = group_member(created.id, "/admins", ProjectRole::ProjectAdmin);
    env.store.members().add(&first).await.unwrap();
    env.store.members().add(&second).await.unwrap();
    // Two admins (one direct, one by group): either may go, not both.
    env.store.members().remove(second.id, false).await.unwrap();
    let demote = env
        .store
        .members()
        .set_role(first.id, ProjectRole::Guest, false)
        .await
        .unwrap_err();
    assert!(matches!(demote, StoreError::Conflict(_)));
    let remove = env
        .store
        .members()
        .remove(first.id, false)
        .await
        .unwrap_err();
    assert!(matches!(remove, StoreError::Conflict(_)));
    // Promoting to the same role is not a demotion.
    env.store
        .members()
        .set_role(first.id, ProjectRole::ProjectAdmin, false)
        .await
        .unwrap();
    // `nx-admin` may.
    env.store
        .members()
        .set_role(first.id, ProjectRole::Guest, true)
        .await
        .unwrap();
    env.store.members().remove(first.id, true).await.unwrap();
}

#[tokio::test]
async fn project_access_lists_memberships_and_bounded_group_rows() {
    let env = env();
    let first = project(&env.store, "access-one").await;
    let second = project(&env.store, "access-two").await;
    let user = UserId::generate();
    env.store
        .members()
        .add(&user_member(first.id, user, ProjectRole::Guest))
        .await
        .unwrap();
    env.store
        .members()
        .add(&user_member(second.id, user, ProjectRole::Maintainer))
        .await
        .unwrap();
    env.store
        .members()
        .add(&group_member(first.id, "/b", ProjectRole::Developer))
        .await
        .unwrap();
    env.store
        .members()
        .add(&group_member(second.id, "/a", ProjectRole::Guest))
        .await
        .unwrap();
    let mut other_provider = group_member(second.id, "cn=ops", ProjectRole::Guest);
    other_provider.subject = ProjectSubject::Group {
        provider: IdpProvider::Ldap,
        external_group: "cn=ops".to_owned(),
    };
    env.store.members().add(&other_provider).await.unwrap();

    let memberships = env.store.access().memberships_of_user(user).await.unwrap();
    assert_eq!(memberships.len(), 2);
    assert!(memberships.contains(&(first.id, ProjectRole::Guest)));
    assert!(memberships.contains(&(second.id, ProjectRole::Maintainer)));
    let groups = env
        .store
        .access()
        .group_mappings(IdpProvider::Oidc, 10)
        .await
        .unwrap();
    let names: Vec<&str> = groups.iter().map(|(g, _, _)| g.as_str()).collect();
    assert_eq!(names, ["/a", "/b"]);
    let ldap = env
        .store
        .access()
        .group_mappings(IdpProvider::Ldap, 10)
        .await
        .unwrap();
    assert_eq!(ldap.len(), 1);
    // A runaway table fails closed instead of loading without bound.
    let too_many = env
        .store
        .access()
        .group_mappings(IdpProvider::Oidc, 1)
        .await
        .unwrap_err();
    assert!(matches!(too_many, StoreError::Invalid(_)));
}

#[tokio::test]
async fn deleting_a_user_removes_their_project_roles_but_not_a_group_row() {
    let env = env();
    let created = project(&env.store, "accounts").await;
    let new = rustryx_testkit::fixtures::user("leaver", rustryx_core::identity::UserSource::Local);
    let user = env.store.users().create(&new).await.unwrap();
    env.store
        .members()
        .add(&user_member(created.id, user.id, ProjectRole::Developer))
        .await
        .unwrap();
    env.store
        .members()
        .add(&group_member(created.id, "/stay", ProjectRole::Guest))
        .await
        .unwrap();
    env.store.users().delete(user.id).await.unwrap();
    assert!(
        env.store
            .access()
            .memberships_of_user(user.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        env.store
            .members()
            .list(created.id, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}

// labels

#[tokio::test]
async fn labels_are_unique_per_scope_and_versioned() {
    let env = env();
    let created = project(&env.store, "labelled").await;
    let other = project(&env.store, "labelled-too").await;
    let system = label(LabelScope::System, "prod");
    env.store.labels().create(&system).await.unwrap();
    let same_name_other_scope = label(LabelScope::Project(created.id), "prod");
    env.store
        .labels()
        .create(&same_name_other_scope)
        .await
        .unwrap();
    assert!(matches!(
        env.store
            .labels()
            .create(&label(LabelScope::System, "prod"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store.labels().create(&system).await,
        Err(StoreError::Conflict(_))
    ));
    let ghost_project = label(
        LabelScope::Project(rustryx_core::ids::ProjectId::generate()),
        "x",
    );
    assert!(matches!(
        env.store.labels().create(&ghost_project).await,
        Err(StoreError::Conflict(_))
    ));
    let mut bad_color = label(LabelScope::System, "colour");
    bad_color.color = "red".to_owned();
    assert!(matches!(
        env.store.labels().create(&bad_color).await,
        Err(StoreError::Conflict(_))
    ));

    let system_page = env
        .store
        .labels()
        .list(LabelScope::System, &page(10))
        .await
        .unwrap();
    assert_eq!(system_page.items.len(), 1);
    let project_page = env
        .store
        .labels()
        .list(LabelScope::Project(created.id), &page(10))
        .await
        .unwrap();
    assert_eq!(project_page.items.len(), 1);
    assert!(
        env.store
            .labels()
            .list(LabelScope::Project(other.id), &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );

    let mut edited = same_name_other_scope.clone();
    edited.description = "edited".to_owned();
    edited.scope = LabelScope::System;
    edited.name = LabelName::parse("renamed").unwrap();
    env.store.labels().update(&edited, 1).await.unwrap();
    let stored = env.store.labels().get(edited.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.version, stored.scope),
        (2, LabelScope::Project(created.id)),
        "scope is fixed"
    );
    assert_eq!(stored.name.as_str(), "renamed");
    assert!(matches!(
        env.store.labels().update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut clash = stored.clone();
    clash.name = LabelName::parse("prod").unwrap();
    clash.scope = LabelScope::System;
    // Same scope, free name: fine; a name that exists in the label's own scope: conflict.
    let second = label(LabelScope::Project(created.id), "second");
    env.store.labels().create(&second).await.unwrap();
    let mut collide = second.clone();
    collide.name = LabelName::parse("renamed").unwrap();
    assert!(matches!(
        env.store.labels().update(&collide, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut missing = system.clone();
    missing.id = LabelId::generate();
    assert!(matches!(
        env.store.labels().update(&missing, 1).await,
        Err(StoreError::NotFound(_))
    ));
    let mut invalid = stored.clone();
    invalid.color = "nope".to_owned();
    assert!(matches!(
        env.store.labels().update(&invalid, 2).await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn applying_a_label_follows_the_scope_guard_and_is_idempotent() {
    let env = env();
    let (inside, outside) = (
        project(&env.store, "in").await,
        project(&env.store, "out").await,
    );
    let repo = hosted(&env.store, "labelled-repo").await;
    env.store
        .projects()
        .attach_repository(inside.id, repo.id)
        .await
        .unwrap();
    let loose = hosted(&env.store, "loose-repo").await;
    let make = |repo: &rustryx_core::model::Repository, name: &'static str| {
        let store = env.store.clone();
        let repo_id = repo.id;
        async move {
            let mut new = NewComponent::new(repo_id, RepoFormat::Raw, name);
            new.version = Some("1".to_owned());
            store.components().find_or_create(&new).await.unwrap()
        }
    };
    let component = make(&repo, "pkg").await;
    let stranger = make(&loose, "other").await;
    let mine = label(LabelScope::Project(inside.id), "mine");
    let foreign = label(LabelScope::Project(outside.id), "foreign");
    let system = label(LabelScope::System, "sys");
    for l in [&mine, &foreign, &system] {
        env.store.labels().create(l).await.unwrap();
    }
    let user = Some(UserId::generate());
    env.store
        .labels()
        .apply(component.id, mine.id, user)
        .await
        .unwrap();
    env.store
        .labels()
        .apply(component.id, mine.id, None)
        .await
        .unwrap();
    env.store
        .labels()
        .apply(stranger.id, system.id, None)
        .await
        .unwrap();
    // A project label sits only on components of its own project.
    for (target, wrong) in [(component.id, foreign.id), (stranger.id, mine.id)] {
        let error = env
            .store
            .labels()
            .apply(target, wrong, None)
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::Conflict(_)));
    }
    assert!(matches!(
        env.store
            .labels()
            .apply(ComponentId::generate(), mine.id, None)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        env.store
            .labels()
            .apply(component.id, LabelId::generate(), None)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(env.store.labels().usage(mine.id).await.unwrap(), 1);
    assert_eq!(
        env.store
            .labels()
            .labels_of(component.id)
            .await
            .unwrap()
            .len(),
        1
    );
    env.store
        .labels()
        .unapply(component.id, mine.id)
        .await
        .unwrap();
    env.store
        .labels()
        .unapply(component.id, mine.id)
        .await
        .unwrap();
    assert_eq!(env.store.labels().usage(mine.id).await.unwrap(), 0);
    env.store
        .labels()
        .apply(component.id, mine.id, None)
        .await
        .unwrap();
    assert_eq!(
        env.store.labels().delete(mine.id).await.unwrap(),
        1,
        "components that lost it"
    );
    assert!(matches!(
        env.store.labels().delete(mine.id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn must_not_put_more_than_fifty_labels_on_one_component() {
    let env = env();
    let repo = hosted(&env.store, "many").await;
    let mut new = NewComponent::new(repo.id, RepoFormat::Raw, "pkg");
    new.version = Some("1".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();
    for index in 0..50 {
        let l = label(LabelScope::System, &format!("l{index}"));
        env.store.labels().create(&l).await.unwrap();
        env.store
            .labels()
            .apply(component.id, l.id, None)
            .await
            .unwrap();
    }
    let extra = label(LabelScope::System, "one-too-many");
    env.store.labels().create(&extra).await.unwrap();
    let error = env
        .store
        .labels()
        .apply(component.id, extra.id, None)
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Invalid(_)));
    // Deleting the component takes its links with it.
    env.store.components().delete(component.id).await.unwrap();
    assert_eq!(env.store.labels().usage(extra.id).await.unwrap(), 0);
}

// robot tokens

#[tokio::test]
async fn robot_tokens_are_unique_by_name_and_hash_and_found_by_hash() {
    let env = env();
    let created = project(&env.store, "robots").await;
    let first = robot("ci", "hash-1");
    env.store.robots().create(&first).await.unwrap();
    for clash in [robot("ci", "hash-2"), robot("other", "hash-1")] {
        assert!(matches!(
            env.store.robots().create(&clash).await,
            Err(StoreError::Conflict(_))
        ));
    }
    let mut orphan = robot("orphan", "hash-3");
    orphan.project_id = Some(rustryx_core::ids::ProjectId::generate());
    assert!(matches!(
        env.store.robots().create(&orphan).await,
        Err(StoreError::Conflict(_))
    ));
    let mut scoped = robot("scoped", "hash-4");
    scoped.project_id = Some(created.id);
    env.store.robots().create(&scoped).await.unwrap();
    assert_eq!(
        env.store
            .robots()
            .find_by_hash("hash-1")
            .await
            .unwrap()
            .map(|r| r.id),
        Some(first.id)
    );
    assert!(
        env.store
            .robots()
            .find_by_hash("nope")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        env.store
            .robots()
            .get(scoped.id)
            .await
            .unwrap()
            .map(|r| r.name),
        Some("scoped".to_owned())
    );
}

#[tokio::test]
async fn robot_listing_filters_by_project_system_and_derived_state() {
    let env = env();
    let created = project(&env.store, "robot-list").await;
    let now = env.clock_now();
    let active = robot("a-active", "h1");
    let mut scoped = robot("b-scoped", "h2");
    scoped.project_id = Some(created.id);
    let mut disabled = robot("c-disabled", "h3");
    disabled.disabled_at = Some(now);
    let mut revoked = robot("d-revoked", "h4");
    revoked.revoked_at = Some(now);
    revoked.disabled_at = Some(now);
    let mut expired = robot("e-expired", "h5");
    expired.expires_at = Some(now - time::Duration::days(1));
    for r in [&active, &scoped, &disabled, &revoked, &expired] {
        env.store.robots().create(r).await.unwrap();
    }
    let names = |filter: RobotFilter| {
        let store = env.store.clone();
        async move {
            store
                .robots()
                .list(&filter, &page(10))
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|r| r.name)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(names(RobotFilter::default()).await.len(), 5);
    let only_project = RobotFilter {
        project: Some(created.id),
        ..RobotFilter::default()
    };
    assert_eq!(names(only_project).await, ["b-scoped"]);
    let system = RobotFilter {
        system_only: true,
        ..RobotFilter::default()
    };
    assert_eq!(names(system).await.len(), 4);
    for (state, expected) in [
        (RobotState::Active, "a-active"),
        (RobotState::Disabled, "c-disabled"),
        (RobotState::Revoked, "d-revoked"),
        (RobotState::Expired, "e-expired"),
    ] {
        let filter = RobotFilter {
            state: Some(state),
            ..RobotFilter::default()
        };
        let found = names(filter).await;
        assert!(found.contains(&expected.to_owned()), "{state:?}: {found:?}");
    }
}

#[tokio::test]
async fn rotating_a_robot_replaces_the_secret_at_once_and_never_re_enables_it() {
    let env = env();
    let mut token = robot("rotating", "old-hash");
    token.disabled_at = Some(T0);
    env.store.robots().create(&token).await.unwrap();
    env.store
        .robots()
        .create(&robot("neighbour", "taken"))
        .await
        .unwrap();
    let at = env.clock_now();
    env.store
        .robots()
        .rotate(token.id, "new-hash", "rx_new", at, 1)
        .await
        .unwrap();
    assert!(
        env.store
            .robots()
            .find_by_hash("old-hash")
            .await
            .unwrap()
            .is_none()
    );
    let stored = env
        .store
        .robots()
        .find_by_hash("new-hash")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            stored.version,
            stored.rotated_at,
            stored.display_prefix.as_str()
        ),
        (2, Some(at), "rx_new")
    );
    assert!(stored.disabled_at.is_some(), "rotation never re-enables");
    assert!(matches!(
        env.store.robots().rotate(token.id, "x", "p", at, 1).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .robots()
            .rotate(token.id, "taken", "p", at, 2)
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .robots()
            .rotate(RobotTokenId::generate(), "x", "p", at, 1)
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn disabling_revoking_deleting_and_touching_a_robot() {
    let env = env();
    let token = robot("lifecycle", "h");
    env.store.robots().create(&token).await.unwrap();
    let at = env.clock_now();
    env.store
        .robots()
        .set_disabled(token.id, true, at, 1)
        .await
        .unwrap();
    assert_eq!(
        env.store
            .robots()
            .get(token.id)
            .await
            .unwrap()
            .unwrap()
            .disabled_at,
        Some(at)
    );
    env.store
        .robots()
        .set_disabled(token.id, false, at, 2)
        .await
        .unwrap();
    let stored = env.store.robots().get(token.id).await.unwrap().unwrap();
    assert_eq!((stored.disabled_at, stored.version), (None, 3));
    assert!(matches!(
        env.store.robots().set_disabled(token.id, true, at, 1).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .robots()
            .set_disabled(RobotTokenId::generate(), true, at, 1)
            .await,
        Err(StoreError::NotFound(_))
    ));

    env.store.robots().revoke(token.id, at).await.unwrap();
    env.store
        .robots()
        .revoke(token.id, at + time::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(
        env.store
            .robots()
            .get(token.id)
            .await
            .unwrap()
            .unwrap()
            .revoked_at,
        Some(at),
        "the first time stays"
    );
    assert!(matches!(
        env.store
            .robots()
            .revoke(RobotTokenId::generate(), at)
            .await,
        Err(StoreError::NotFound(_))
    ));

    env.store
        .robots()
        .touch_last_used(token.id, at)
        .await
        .unwrap();
    env.store
        .robots()
        .touch_last_used(token.id, T0)
        .await
        .unwrap();
    assert_eq!(
        env.store
            .robots()
            .get(token.id)
            .await
            .unwrap()
            .unwrap()
            .last_used_at,
        Some(at)
    );
    env.store
        .robots()
        .touch_last_used(RobotTokenId::generate(), at)
        .await
        .unwrap();

    env.store.robots().delete(token.id).await.unwrap();
    assert!(matches!(
        env.store.robots().delete(token.id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn expired_and_revoked_robots_are_purged_and_expiring_ones_get_one_notice() {
    let env = env();
    let now = env.clock_now();
    let mut expired = robot("expired", "e");
    expired.expires_at = Some(now - time::Duration::days(10));
    let mut revoked = robot("revoked", "r");
    revoked.revoked_at = Some(now - time::Duration::days(10));
    let mut soon = robot("soon", "s");
    soon.expires_at = Some(now + time::Duration::days(2));
    let mut later = robot("later", "l");
    later.expires_at = Some(now + time::Duration::days(30));
    let mut gone = robot("gone", "g");
    gone.expires_at = Some(now + time::Duration::days(1));
    gone.revoked_at = Some(now);
    for r in [&expired, &revoked, &soon, &later, &gone] {
        env.store.robots().create(r).await.unwrap();
    }
    let week = Duration::from_hours(168);
    let due = env
        .store
        .robots()
        .list_expiring(week, now, 10)
        .await
        .unwrap();
    assert_eq!(
        due.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["soon"]
    );
    env.store
        .robots()
        .mark_expiry_notified(soon.id, now)
        .await
        .unwrap();
    env.store
        .robots()
        .mark_expiry_notified(soon.id, now)
        .await
        .unwrap();
    env.store
        .robots()
        .mark_expiry_notified(RobotTokenId::generate(), now)
        .await
        .unwrap();
    assert!(
        env.store
            .robots()
            .list_expiring(week, now, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let limited = env
        .store
        .robots()
        .list_expiring(Duration::from_hours(960), now, 1)
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);

    let purged = env.store.robots().purge_expired(now).await.unwrap();
    assert_eq!(purged, 2);
    assert!(env.store.robots().get(expired.id).await.unwrap().is_none());
    assert!(env.store.robots().get(revoked.id).await.unwrap().is_none());
    assert!(env.store.robots().get(soon.id).await.unwrap().is_some());
}

// personal token expiry

fn api_token(user: UserId, hash: &str, expires_at: Option<time::OffsetDateTime>) -> ApiToken {
    ApiToken {
        id: ApiTokenId::generate(),
        user_id: user,
        name: hash.to_owned(),
        token_hash: hash.to_owned(),
        display_prefix: "rx".to_owned(),
        scopes: Vec::new(),
        expires_at,
        revoked_at: None,
        last_used_at: None,
        created_at: T0,
    }
}

#[tokio::test]
async fn personal_tokens_report_expiry_once_and_skip_the_expired_and_the_revoked() {
    let env = env();
    let now = env.clock_now();
    let user = UserId::generate();
    let soon = api_token(user, "soon", Some(now + time::Duration::days(3)));
    let never = api_token(user, "never", None);
    let past = api_token(user, "past", Some(now - time::Duration::days(1)));
    let mut revoked = api_token(user, "revoked", Some(now + time::Duration::days(1)));
    revoked.revoked_at = Some(now);
    for t in [&soon, &never, &past, &revoked] {
        env.store.tokens().create(t).await.unwrap();
    }
    let week = Duration::from_hours(168);
    let due = env
        .store
        .tokens()
        .list_expiring(week, now, 10)
        .await
        .unwrap();
    assert_eq!(due.iter().map(|t| t.id).collect::<Vec<_>>(), [soon.id]);
    assert_eq!(
        env.store
            .tokens()
            .list_expiring(week, now, 0)
            .await
            .unwrap()
            .len(),
        0
    );
    env.store
        .tokens()
        .mark_expiry_notified(soon.id, now)
        .await
        .unwrap();
    env.store
        .tokens()
        .mark_expiry_notified(ApiTokenId::generate(), now)
        .await
        .unwrap();
    assert!(
        env.store
            .tokens()
            .list_expiring(week, now, 10)
            .await
            .unwrap()
            .is_empty()
    );
}
