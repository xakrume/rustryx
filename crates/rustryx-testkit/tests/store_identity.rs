//! `MemStore`: users, roles, privileges, selectors, tokens, mappings, login counters.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure signal"
)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use futures::StreamExt;
use rustryx_core::StoreError;
use rustryx_core::identity::{
    Action, ApiToken, ContentSelector, IdpGroupMapping, IdpProvider, Privilege, PrivilegeGrant,
    ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER, Role, RoleOrigin, User, UserSource,
};
use rustryx_core::ids::{
    ApiTokenId, ContentSelectorId, IdpGroupMappingId, PrivilegeId, RoleId, UserId,
};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{
    Clock, IdpMappingRepo, InvalidationBus, InvalidationEvent, LoginAttemptRepo, LoginKey,
    PageRequest, PrivilegeRepo, RoleRepo, SelectorRepo, TokenRepo, UserRepo,
};
use rustryx_testkit::fixtures::user;
use rustryx_testkit::{FixedClock, MemInvalidationBus, MemStore, builtin_role_id};
use time::OffsetDateTime;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn role(name: &str) -> Role {
    Role {
        id: RoleId::generate(),
        name: name.to_owned(),
        description: String::new(),
        builtin: false,
        privilege_ids: Vec::new(),
        nested_role_ids: Vec::new(),
    }
}

fn privilege(name: &str) -> Privilege {
    Privilege {
        id: PrivilegeId::generate(),
        name: name.to_owned(),
        description: String::new(),
        builtin: false,
        grant: PrivilegeGrant::RepositoryView {
            format: Some(RepoFormat::Npm),
            repository: "*".to_owned(),
        },
        actions: vec![Action::Read],
    }
}

fn selector(name: &str) -> ContentSelector {
    ContentSelector {
        id: ContentSelectorId::generate(),
        name: name.to_owned(),
        description: String::new(),
        expression: "path =^ \"/a/\"".to_owned(),
        updated_at: T0,
    }
}

fn token_for(owner: UserId, hash: &str) -> ApiToken {
    ApiToken {
        id: ApiTokenId::generate(),
        user_id: owner,
        name: "ci".to_owned(),
        token_hash: hash.to_owned(),
        display_prefix: "ryx_ab".to_owned(),
        scopes: Vec::new(),
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
        created_at: T0,
    }
}

fn mapping(group: &str, role_id: RoleId, priority: i32, provider: IdpProvider) -> IdpGroupMapping {
    IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        provider,
        external_group: group.to_owned(),
        role_id,
        priority,
    }
}

async fn make_user(store: &MemStore, name: &str) -> User {
    let users: &dyn UserRepo = store;
    users.create(&user(name, UserSource::Local)).await.unwrap()
}

async fn make_role(store: &MemStore, name: &str) -> Role {
    let roles: &dyn RoleRepo = store;
    let new = role(name);
    roles.create(&new).await.unwrap();
    new
}

fn role_names(
    roles: &[(rustryx_core::identity::RoleRef, RoleOrigin)],
) -> Vec<(String, RoleOrigin)> {
    roles.iter().map(|(r, o)| (r.name.clone(), *o)).collect()
}

// users

#[tokio::test]
async fn user_create_applies_defaults_and_is_found_every_way() {
    let clock = FixedClock::default();
    let store = MemStore::new().with_clock(Arc::new(clock.clone()));
    let users: &dyn UserRepo = &store;
    let created = users
        .create(&user("alice", UserSource::Local))
        .await
        .unwrap();
    assert!(created.enabled);
    assert!(created.tokens_valid_after.is_none());
    assert_eq!(created.created_at, clock.now());
    assert_eq!(
        users.get(created.id).await.unwrap().unwrap().username,
        "alice"
    );
    assert_eq!(
        users.find_by_username("alice").await.unwrap().unwrap().id,
        created.id
    );
    assert!(users.find_by_username("Alice").await.unwrap().is_none());
    assert!(users.get(UserId::generate()).await.unwrap().is_none());
}

#[tokio::test]
async fn username_is_unique_across_identity_sources() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    users
        .create(&user("shared", UserSource::Local))
        .await
        .unwrap();
    for source in [UserSource::Ldap, UserSource::Oidc, UserSource::Local] {
        let result = users.create(&user("shared", source)).await;
        assert!(matches!(result, Err(StoreError::Conflict(_))), "{source:?}");
    }
}

#[tokio::test]
async fn external_identity_is_unique_per_source_issuer_and_subject() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    users
        .create(&user("first", UserSource::Oidc))
        .await
        .unwrap();
    let mut twin = user("second", UserSource::Oidc);
    twin.subject = Some("sub-first".to_owned());
    assert!(matches!(
        users.create(&twin).await,
        Err(StoreError::Conflict(_))
    ));
    let mut other_issuer = twin.clone();
    other_issuer.issuer = Some("https://other.example.test".to_owned());
    users.create(&other_issuer).await.unwrap();
}

#[tokio::test]
async fn find_external_matches_source_issuer_and_subject_exactly() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    let oidc = user("carol", UserSource::Oidc);
    let issuer = oidc.issuer.clone().unwrap();
    let created = users.create(&oidc).await.unwrap();
    let found = users
        .find_external(UserSource::Oidc, Some(&issuer), Some("sub-carol"))
        .await
        .unwrap();
    assert_eq!(found.unwrap().id, created.id);
    for (source, issuer, subject) in [
        (UserSource::Ldap, Some(issuer.as_str()), Some("sub-carol")),
        (
            UserSource::Oidc,
            Some("https://other.example.test"),
            Some("sub-carol"),
        ),
        (UserSource::Oidc, None, Some("sub-carol")),
        (UserSource::Oidc, Some(issuer.as_str()), Some("sub-other")),
    ] {
        let result = users.find_external(source, issuer, subject).await.unwrap();
        assert!(result.is_none(), "{source:?} {issuer:?} {subject:?}");
    }
}

#[tokio::test]
async fn must_not_find_an_external_account_by_username_or_email() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    let mut local = user("dave", UserSource::Local);
    local.email = Some("dave@example.test".to_owned());
    users.create(&local).await.unwrap();
    let by_missing_subject = users
        .find_external(UserSource::Local, None, None)
        .await
        .unwrap();
    assert!(by_missing_subject.is_none());
    let by_username = users
        .find_external(UserSource::Local, None, Some("dave"))
        .await
        .unwrap();
    assert!(by_username.is_none());
    let by_email = users
        .find_external(UserSource::Local, None, Some("dave@example.test"))
        .await
        .unwrap();
    assert!(by_email.is_none());
}

#[tokio::test]
async fn user_list_is_ordered_by_username_and_paginated() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    for name in ["delta", "alpha", "charlie", "bravo", "echo"] {
        make_user(&store, name).await;
    }
    let first = users
        .list(&PageRequest {
            limit: 2,
            after: None,
        })
        .await
        .unwrap();
    let second = users
        .list(&PageRequest {
            limit: 2,
            after: first.next.clone(),
        })
        .await
        .unwrap();
    let third = users
        .list(&PageRequest {
            limit: 2,
            after: second.next.clone(),
        })
        .await
        .unwrap();
    let names: Vec<String> = [first.items, second.items, third.items]
        .concat()
        .into_iter()
        .map(|u| u.username)
        .collect();
    assert_eq!(names, ["alpha", "bravo", "charlie", "delta", "echo"]);
    assert!(third.next.is_none());
}

#[tokio::test]
async fn user_mutators_update_the_row_and_report_unknown_ids() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    let created = make_user(&store, "erin").await;
    assert!(created.password_hash.is_none());

    users
        .set_password_hash(created.id, "$argon2id$hash")
        .await
        .unwrap();
    users.set_enabled(created.id, false).await.unwrap();
    let stamp = T0 + time::Duration::days(3);
    users
        .set_tokens_valid_after(created.id, stamp)
        .await
        .unwrap();
    let stored = users.get(created.id).await.unwrap().unwrap();
    assert_eq!(stored.password_hash.unwrap().expose(), "$argon2id$hash");
    assert!(!stored.enabled);
    assert_eq!(stored.tokens_valid_after, Some(stamp));

    let ghost = UserId::generate();
    assert!(matches!(
        users.set_password_hash(ghost, "h").await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        users.set_enabled(ghost, true).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        users.set_tokens_valid_after(ghost, stamp).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        users.delete(ghost).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn must_not_touch_roles_of_other_origins_when_replacing_one_origin() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    let alice = make_user(&store, "alice").await;
    let (admin, viewer, editor, auditor) = (
        make_role(&store, "admin").await,
        make_role(&store, "viewer").await,
        make_role(&store, "editor").await,
        make_role(&store, "auditor").await,
    );
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Manual, &[admin.id])
        .await
        .unwrap();
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Oidc, &[viewer.id, editor.id])
        .await
        .unwrap();
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Ldap, &[auditor.id])
        .await
        .unwrap();

    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Oidc, &[editor.id])
        .await
        .unwrap();
    assert_eq!(
        role_names(&users.roles_of(alice.id).await.unwrap()),
        [
            ("admin".to_owned(), RoleOrigin::Manual),
            ("auditor".to_owned(), RoleOrigin::Ldap),
            ("editor".to_owned(), RoleOrigin::Oidc),
        ]
    );

    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Oidc, &[])
        .await
        .unwrap();
    assert_eq!(
        role_names(&users.roles_of(alice.id).await.unwrap()),
        [
            ("admin".to_owned(), RoleOrigin::Manual),
            ("auditor".to_owned(), RoleOrigin::Ldap)
        ]
    );
}

#[tokio::test]
async fn one_role_may_be_held_under_two_origins_and_input_is_deduplicated() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    let alice = make_user(&store, "alice").await;
    let shared = make_role(&store, "shared").await;
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Manual, &[shared.id, shared.id])
        .await
        .unwrap();
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Ldap, &[shared.id])
        .await
        .unwrap();
    assert_eq!(
        role_names(&users.roles_of(alice.id).await.unwrap()),
        [
            ("shared".to_owned(), RoleOrigin::Manual),
            ("shared".to_owned(), RoleOrigin::Ldap)
        ]
    );
}

#[tokio::test]
async fn must_not_change_roles_when_the_replacement_names_an_unknown_role() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    let alice = make_user(&store, "alice").await;
    let kept = make_role(&store, "kept").await;
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Oidc, &[kept.id])
        .await
        .unwrap();
    let result = users
        .replace_roles_for_origin(alice.id, RoleOrigin::Oidc, &[RoleId::generate()])
        .await;
    assert!(matches!(result, Err(StoreError::NotFound(_))));
    assert_eq!(
        role_names(&users.roles_of(alice.id).await.unwrap()),
        [("kept".to_owned(), RoleOrigin::Oidc)]
    );
    let unknown_user = users
        .replace_roles_for_origin(UserId::generate(), RoleOrigin::Oidc, &[])
        .await;
    assert!(matches!(unknown_user, Err(StoreError::NotFound(_))));
    assert_eq!(users.roles_of(UserId::generate()).await.unwrap().len(), 0);
}

#[tokio::test]
async fn deleting_a_user_removes_its_tokens_and_role_assignments() {
    let store = MemStore::new();
    let users: &dyn UserRepo = &store;
    let tokens: &dyn TokenRepo = &store;
    let alice = make_user(&store, "alice").await;
    let bob = make_user(&store, "bob").await;
    let viewer = make_role(&store, "viewer").await;
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Manual, &[viewer.id])
        .await
        .unwrap();
    tokens.create(&token_for(alice.id, "hash-a")).await.unwrap();
    tokens.create(&token_for(bob.id, "hash-b")).await.unwrap();

    users.delete(alice.id).await.unwrap();

    assert!(users.get(alice.id).await.unwrap().is_none());
    assert_eq!(users.roles_of(alice.id).await.unwrap().len(), 0);
    assert!(tokens.find_by_hash("hash-a").await.unwrap().is_none());
    assert!(tokens.find_by_hash("hash-b").await.unwrap().is_some());
}

// roles

#[tokio::test]
async fn role_crud_roundtrip() {
    let store = MemStore::new();
    let roles: &dyn RoleRepo = &store;
    let zed = make_role(&store, "zed").await;
    make_role(&store, "alpha").await;
    assert_eq!(roles.get(zed.id).await.unwrap().unwrap().name, "zed");
    assert_eq!(
        roles.get_by_name("alpha").await.unwrap().unwrap().name,
        "alpha"
    );
    assert!(roles.get_by_name("missing").await.unwrap().is_none());
    assert!(roles.get(RoleId::generate()).await.unwrap().is_none());
    let names: Vec<String> = roles
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, ["alpha", "zed"]);

    let mut edited = zed.clone();
    edited.description = "changed".to_owned();
    edited.builtin = true;
    roles.update(&edited).await.unwrap();
    let stored = roles.get(zed.id).await.unwrap().unwrap();
    assert_eq!(stored.description, "changed");
    assert!(
        !stored.builtin,
        "the built-in flag cannot be set through update"
    );

    roles.delete(zed.id).await.unwrap();
    assert!(roles.get(zed.id).await.unwrap().is_none());
}

#[tokio::test]
async fn role_write_conflicts_and_not_found() {
    let store = MemStore::new();
    let roles: &dyn RoleRepo = &store;
    let first = make_role(&store, "first").await;
    let mut second = make_role(&store, "second").await;
    assert!(matches!(
        roles.create(&role("first")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut same_id = role("other");
    same_id.id = first.id;
    assert!(matches!(
        roles.create(&same_id).await,
        Err(StoreError::Conflict(_))
    ));
    second.name = "first".to_owned();
    assert!(matches!(
        roles.update(&second).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        roles.update(&role("ghost")).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        roles.delete(RoleId::generate()).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn builtin_roles_cannot_be_edited_or_deleted() {
    let store = MemStore::with_builtin_roles();
    let roles: &dyn RoleRepo = &store;
    let admin = roles.get_by_name(ROLE_NX_ADMIN).await.unwrap().unwrap();
    assert!(admin.builtin);
    let mut edited = admin.clone();
    edited.description = "hijacked".to_owned();
    assert!(matches!(
        roles.update(&edited).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        roles.delete(admin.id).await,
        Err(StoreError::Conflict(_))
    ));
    assert_ne!(
        roles.get(admin.id).await.unwrap().unwrap().description,
        "hijacked"
    );
}

#[tokio::test]
async fn must_not_accept_a_role_nesting_cycle() {
    let store = MemStore::new();
    let roles: &dyn RoleRepo = &store;

    let mut selfish = role("selfish");
    selfish.nested_role_ids = vec![selfish.id];
    assert!(matches!(
        roles.create(&selfish).await,
        Err(StoreError::Conflict(_))
    ));

    let c = make_role(&store, "c").await;
    let mut b = role("b");
    b.nested_role_ids = vec![c.id];
    roles.create(&b).await.unwrap();
    let mut a = role("a");
    a.nested_role_ids = vec![b.id];
    roles.create(&a).await.unwrap();

    let mut c_cycle = c.clone();
    c_cycle.nested_role_ids = vec![a.id];
    assert!(matches!(
        roles.update(&c_cycle).await,
        Err(StoreError::Conflict(_))
    ));
    let mut a_self = a.clone();
    a_self.nested_role_ids = vec![a.id];
    assert!(matches!(
        roles.update(&a_self).await,
        Err(StoreError::Conflict(_))
    ));
    let untouched = roles.get(c.id).await.unwrap().unwrap();
    assert_eq!(untouched.nested_role_ids.len(), 0);
}

#[tokio::test]
async fn updating_a_role_with_unchanged_nesting_is_not_mistaken_for_a_cycle() {
    let store = MemStore::new();
    let roles: &dyn RoleRepo = &store;
    let child = make_role(&store, "child").await;
    let mut parent = role("parent");
    parent.nested_role_ids = vec![child.id];
    roles.create(&parent).await.unwrap();
    parent.description = "touched".to_owned();
    roles.update(&parent).await.unwrap();
    let mut child_moves = child.clone();
    child_moves.description = "also touched".to_owned();
    roles.update(&child_moves).await.unwrap();
}

#[tokio::test]
async fn effective_privileges_expand_nested_roles_once_and_sorted() {
    let store = MemStore::new();
    let roles: &dyn RoleRepo = &store;
    let privileges: &dyn PrivilegeRepo = &store;
    let (read, write, delete) = (
        privilege("p-read"),
        privilege("p-write"),
        privilege("p-delete"),
    );
    for p in [&read, &write, &delete] {
        privileges.create(p).await.unwrap();
    }
    let mut base = role("base");
    base.privilege_ids = vec![write.id, read.id];
    roles.create(&base).await.unwrap();
    let mut left = role("left");
    left.nested_role_ids = vec![base.id];
    left.privilege_ids = vec![delete.id, PrivilegeId::generate()];
    roles.create(&left).await.unwrap();
    let mut right = role("right");
    right.nested_role_ids = vec![base.id, RoleId::generate()];
    roles.create(&right).await.unwrap();

    let effective = roles
        .effective_privileges(&[left.id, right.id, RoleId::generate()])
        .await
        .unwrap();
    let names: Vec<&str> = effective.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["p-delete", "p-read", "p-write"]);
    assert!(roles.effective_privileges(&[]).await.unwrap().is_empty());
    let only_right = roles.effective_privileges(&[right.id]).await.unwrap();
    assert_eq!(only_right.len(), 2);
}

#[tokio::test]
async fn deleting_a_role_removes_it_from_users_nesting_and_mappings() {
    let store = MemStore::new();
    let roles: &dyn RoleRepo = &store;
    let users: &dyn UserRepo = &store;
    let mappings: &dyn IdpMappingRepo = &store;
    let doomed = make_role(&store, "doomed").await;
    let mut holder = role("holder");
    holder.nested_role_ids = vec![doomed.id];
    roles.create(&holder).await.unwrap();
    let alice = make_user(&store, "alice").await;
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Manual, &[doomed.id, holder.id])
        .await
        .unwrap();
    mappings
        .upsert(&mapping("/g", doomed.id, 1, IdpProvider::Oidc))
        .await
        .unwrap();
    mappings
        .upsert(&mapping("/h", holder.id, 2, IdpProvider::Oidc))
        .await
        .unwrap();

    roles.delete(doomed.id).await.unwrap();

    let holder_after = roles.get(holder.id).await.unwrap().unwrap();
    assert_eq!(holder_after.nested_role_ids.len(), 0);
    assert_eq!(
        role_names(&users.roles_of(alice.id).await.unwrap()),
        [("holder".to_owned(), RoleOrigin::Manual)]
    );
    let left = mappings.list(IdpProvider::Oidc).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].external_group, "/h");
}

// privileges

#[tokio::test]
async fn privilege_crud_conflicts_and_builtin_protection() {
    let store = MemStore::with_builtin_roles();
    let privileges: &dyn PrivilegeRepo = &store;
    let custom = privilege("custom");
    privileges.create(&custom).await.unwrap();
    assert!(matches!(
        privileges.create(&privilege("custom")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut same_id = privilege("other");
    same_id.id = custom.id;
    assert!(matches!(
        privileges.create(&same_id).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        privileges.get(custom.id).await.unwrap().unwrap().name,
        "custom"
    );
    assert!(
        privileges
            .get(PrivilegeId::generate())
            .await
            .unwrap()
            .is_none()
    );

    let mut renamed = custom.clone();
    renamed.name = "renamed".to_owned();
    renamed.builtin = true;
    privileges.update(&renamed).await.unwrap();
    let stored = privileges.get(custom.id).await.unwrap().unwrap();
    assert_eq!((stored.name.as_str(), stored.builtin), ("renamed", false));
    assert!(matches!(
        privileges.update(&privilege("ghost")).await,
        Err(StoreError::NotFound(_))
    ));

    let other = privilege("taken");
    privileges.create(&other).await.unwrap();
    let mut clash = other.clone();
    clash.name = "renamed".to_owned();
    assert!(matches!(
        privileges.update(&clash).await,
        Err(StoreError::Conflict(_))
    ));

    let builtin = privileges
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.builtin)
        .unwrap();
    assert!(matches!(
        privileges.update(&builtin).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        privileges.delete(builtin.id).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        privileges.delete(PrivilegeId::generate()).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn deleting_a_privilege_detaches_it_from_roles() {
    let store = MemStore::new();
    let roles: &dyn RoleRepo = &store;
    let privileges: &dyn PrivilegeRepo = &store;
    let (keep, drop_me) = (privilege("keep"), privilege("drop"));
    privileges.create(&keep).await.unwrap();
    privileges.create(&drop_me).await.unwrap();
    let mut holder = role("holder");
    holder.privilege_ids = vec![keep.id, drop_me.id];
    roles.create(&holder).await.unwrap();
    privileges.delete(drop_me.id).await.unwrap();
    assert_eq!(
        roles.get(holder.id).await.unwrap().unwrap().privilege_ids,
        [keep.id]
    );
    let effective = roles.effective_privileges(&[holder.id]).await.unwrap();
    assert_eq!(effective.len(), 1);
}

// selectors

#[tokio::test]
async fn selector_crud_and_conflicts() {
    let store = MemStore::new();
    let selectors: &dyn SelectorRepo = &store;
    let first = selector("first");
    selectors.create(&first).await.unwrap();
    assert!(matches!(
        selectors.create(&selector("first")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut same_id = selector("other");
    same_id.id = first.id;
    assert!(matches!(
        selectors.create(&same_id).await,
        Err(StoreError::Conflict(_))
    ));
    let second = selector("second");
    selectors.create(&second).await.unwrap();

    let mut edited = first.clone();
    edited.expression = "true".to_owned();
    edited.updated_at = T0 + time::Duration::days(1);
    selectors.update(&edited).await.unwrap();
    let stored = selectors.get(first.id).await.unwrap().unwrap();
    assert_eq!(stored.expression, "true");
    assert_eq!(stored.updated_at, edited.updated_at);
    let mut clash = second.clone();
    clash.name = "first".to_owned();
    assert!(matches!(
        selectors.update(&clash).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        selectors.update(&selector("ghost")).await,
        Err(StoreError::NotFound(_))
    ));
    let names: Vec<String> = selectors
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["first", "second"]);
    assert!(
        selectors
            .get(ContentSelectorId::generate())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn must_not_delete_a_selector_while_a_privilege_references_it() {
    let store = MemStore::new();
    let selectors: &dyn SelectorRepo = &store;
    let privileges: &dyn PrivilegeRepo = &store;
    let used = selector("used");
    selectors.create(&used).await.unwrap();
    let mut referencing = privilege("by-selector");
    referencing.grant = PrivilegeGrant::RepositoryContentSelector {
        repository: "*".to_owned(),
        selector_id: used.id,
    };
    privileges.create(&referencing).await.unwrap();

    assert!(matches!(
        selectors.delete(used.id).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(selectors.get(used.id).await.unwrap().is_some());

    privileges.delete(referencing.id).await.unwrap();
    selectors.delete(used.id).await.unwrap();
    assert!(matches!(
        selectors.delete(used.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// tokens

#[tokio::test]
async fn token_create_find_and_list() {
    let store = MemStore::new();
    let tokens: &dyn TokenRepo = &store;
    let owner = UserId::generate();
    let mut older = token_for(owner, "hash-1");
    older.created_at = T0;
    let mut newer = token_for(owner, "hash-2");
    newer.created_at = T0 + time::Duration::hours(1);
    tokens.create(&newer).await.unwrap();
    tokens.create(&older).await.unwrap();
    tokens
        .create(&token_for(UserId::generate(), "hash-3"))
        .await
        .unwrap();

    assert_eq!(
        tokens.find_by_hash("hash-2").await.unwrap().unwrap().id,
        newer.id
    );
    assert!(tokens.find_by_hash("nope").await.unwrap().is_none());
    let listed: Vec<ApiTokenId> = tokens
        .list_for_user(owner)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(listed, [older.id, newer.id]);

    assert!(matches!(
        tokens.create(&token_for(owner, "hash-1")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut same_id = token_for(owner, "hash-4");
    same_id.id = older.id;
    assert!(matches!(
        tokens.create(&same_id).await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn token_revoke_keeps_the_first_time_and_touch_only_moves_forward() {
    let store = MemStore::new();
    let tokens: &dyn TokenRepo = &store;
    let record = token_for(UserId::generate(), "hash");
    tokens.create(&record).await.unwrap();

    let first = T0 + time::Duration::days(1);
    tokens.revoke(record.id, first).await.unwrap();
    tokens
        .revoke(record.id, first + time::Duration::days(1))
        .await
        .unwrap();
    tokens.touch_last_used(record.id, first).await.unwrap();
    tokens.touch_last_used(record.id, T0).await.unwrap();
    let stored = tokens.find_by_hash("hash").await.unwrap().unwrap();
    assert_eq!(stored.revoked_at, Some(first));
    assert_eq!(stored.last_used_at, Some(first));

    let ghost = ApiTokenId::generate();
    assert!(matches!(
        tokens.revoke(ghost, T0).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        tokens.touch_last_used(ghost, T0).await,
        Err(StoreError::NotFound(_))
    ));
}

// idp mappings

#[tokio::test]
async fn mappings_are_listed_per_provider_by_priority() {
    let store = MemStore::new();
    let mappings: &dyn IdpMappingRepo = &store;
    let role_id = RoleId::generate();
    mappings
        .upsert(&mapping("/b", role_id, 5, IdpProvider::Oidc))
        .await
        .unwrap();
    mappings
        .upsert(&mapping("/a", role_id, 5, IdpProvider::Oidc))
        .await
        .unwrap();
    mappings
        .upsert(&mapping("/first", role_id, 1, IdpProvider::Oidc))
        .await
        .unwrap();
    mappings
        .upsert(&mapping("cn=ops", role_id, 0, IdpProvider::Ldap))
        .await
        .unwrap();
    let oidc: Vec<String> = mappings
        .list(IdpProvider::Oidc)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.external_group)
        .collect();
    assert_eq!(oidc, ["/first", "/a", "/b"]);
    assert_eq!(mappings.list(IdpProvider::Ldap).await.unwrap().len(), 1);
}

#[tokio::test]
async fn upsert_replaces_by_provider_and_group_keeping_the_row_id() {
    let store = MemStore::new();
    let mappings: &dyn IdpMappingRepo = &store;
    let (first_role, second_role) = (RoleId::generate(), RoleId::generate());
    let original = mapping("/team", first_role, 1, IdpProvider::Oidc);
    mappings.upsert(&original).await.unwrap();
    let replacement = mapping("/team", second_role, 9, IdpProvider::Oidc);
    mappings.upsert(&replacement).await.unwrap();
    mappings
        .upsert(&mapping("/team", first_role, 1, IdpProvider::Ldap))
        .await
        .unwrap();

    let oidc = mappings.list(IdpProvider::Oidc).await.unwrap();
    assert_eq!(oidc.len(), 1);
    assert_eq!(oidc[0].id, original.id);
    assert_eq!((oidc[0].role_id, oidc[0].priority), (second_role, 9));
    assert_eq!(mappings.list(IdpProvider::Ldap).await.unwrap().len(), 1);

    let mut id_clash = mapping("/another", first_role, 1, IdpProvider::Oidc);
    id_clash.id = original.id;
    assert!(matches!(
        mappings.upsert(&id_clash).await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn mapping_delete_and_not_found() {
    let store = MemStore::new();
    let mappings: &dyn IdpMappingRepo = &store;
    let record = mapping("/gone", RoleId::generate(), 1, IdpProvider::Oidc);
    mappings.upsert(&record).await.unwrap();
    mappings.delete(record.id).await.unwrap();
    assert!(mappings.list(IdpProvider::Oidc).await.unwrap().is_empty());
    assert!(matches!(
        mappings.delete(record.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// login attempts

#[tokio::test]
async fn login_counters_increment_per_key_and_reset() {
    let store = MemStore::new();
    let attempts: &dyn LoginAttemptRepo = &store;
    let by_account = LoginKey {
        account: "alice".to_owned(),
        ip: None,
    };
    let by_ip = LoginKey {
        account: "alice".to_owned(),
        ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
    };
    assert_eq!(attempts.state(&by_account).await.unwrap().failures, 0);
    assert!(
        attempts
            .state(&by_account)
            .await
            .unwrap()
            .last_failure_at
            .is_none()
    );

    attempts.register_failure(&by_account, T0).await.unwrap();
    let after_two = attempts
        .register_failure(&by_account, T0 + time::Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(after_two.failures, 2);
    assert_eq!(
        after_two.last_failure_at,
        Some(T0 + time::Duration::seconds(5))
    );
    assert_eq!(attempts.state(&by_ip).await.unwrap().failures, 0);
    assert_eq!(
        attempts
            .register_failure(&by_ip, T0)
            .await
            .unwrap()
            .failures,
        1
    );

    attempts.reset(&by_account).await.unwrap();
    assert_eq!(attempts.state(&by_account).await.unwrap().failures, 0);
    assert_eq!(attempts.state(&by_ip).await.unwrap().failures, 1);
    attempts.reset(&by_account).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_failures_are_all_counted() {
    let store = MemStore::new();
    let key = LoginKey {
        account: "race".to_owned(),
        ip: None,
    };
    let mut tasks = Vec::new();
    for _ in 0..50 {
        let (store, key) = (store.clone(), key.clone());
        tasks.push(tokio::spawn(async move {
            store.register_failure(&key, T0).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(
        LoginAttemptRepo::state(&store, &key)
            .await
            .unwrap()
            .failures,
        50
    );
}

#[tokio::test]
async fn unavailable_store_fails_login_counters_and_identity_ports() {
    let store = MemStore::with_builtin_roles();
    store.set_unavailable(true);
    let key = LoginKey {
        account: "alice".to_owned(),
        ip: None,
    };
    let attempts: &dyn LoginAttemptRepo = &store;
    assert!(matches!(
        attempts.state(&key).await,
        Err(StoreError::Unavailable(_))
    ));
    assert!(matches!(
        attempts.register_failure(&key, T0).await,
        Err(StoreError::Unavailable(_))
    ));
    assert!(matches!(
        attempts.reset(&key).await,
        Err(StoreError::Unavailable(_))
    ));
    let roles: &dyn RoleRepo = &store;
    assert!(matches!(
        roles.list().await,
        Err(StoreError::Unavailable(_))
    ));
    let users: &dyn UserRepo = &store;
    assert!(matches!(
        users.create(&user("alice", UserSource::Local)).await,
        Err(StoreError::Unavailable(_))
    ));
    let clone = store.clone();
    clone.set_unavailable(false);
    assert!(
        attempts.state(&key).await.is_ok(),
        "clones share the availability switch"
    );
}

// invalidation

#[tokio::test]
async fn rbac_relevant_writes_publish_invalidation_events() {
    let bus = MemInvalidationBus::new();
    let mut events = bus.subscribe();
    let store = MemStore::new().with_invalidation_bus(bus);
    let users: &dyn UserRepo = &store;
    let roles: &dyn RoleRepo = &store;
    let tokens: &dyn TokenRepo = &store;

    let viewer = role("viewer");
    roles.create(&viewer).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::Rbac));

    let alice = users
        .create(&user("alice", UserSource::Local))
        .await
        .unwrap();
    let uuid = alice.id.as_uuid();
    users
        .replace_roles_for_origin(alice.id, RoleOrigin::Oidc, &[viewer.id])
        .await
        .unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::User(uuid)));
    users.set_enabled(alice.id, false).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::User(uuid)));
    users.set_tokens_valid_after(alice.id, T0).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::User(uuid)));

    let record = token_for(alice.id, "hash");
    tokens.create(&record).await.unwrap();
    tokens.revoke(record.id, T0).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::User(uuid)));

    roles.delete(viewer.id).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::Rbac));
    users.delete(alice.id).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::User(uuid)));
}

#[tokio::test]
async fn privilege_selector_and_mapping_writes_publish_rbac_events() {
    let bus = MemInvalidationBus::new();
    let mut events = bus.subscribe();
    let store = MemStore::new().with_invalidation_bus(bus);
    let privileges: &dyn PrivilegeRepo = &store;
    let selectors: &dyn SelectorRepo = &store;
    let mappings: &dyn IdpMappingRepo = &store;

    let p = privilege("p");
    privileges.create(&p).await.unwrap();
    privileges.update(&p).await.unwrap();
    privileges.delete(p.id).await.unwrap();
    let s = selector("s");
    selectors.create(&s).await.unwrap();
    selectors.update(&s).await.unwrap();
    selectors.delete(s.id).await.unwrap();
    let m = mapping("/g", RoleId::generate(), 1, IdpProvider::Oidc);
    mappings.upsert(&m).await.unwrap();
    mappings.delete(m.id).await.unwrap();
    for _ in 0..8 {
        assert_eq!(events.next().await, Some(InvalidationEvent::Rbac));
    }
}

#[tokio::test]
async fn failed_writes_publish_nothing() {
    let bus = MemInvalidationBus::new();
    let mut events = bus.subscribe();
    let store = MemStore::new().with_invalidation_bus(bus.clone());
    let roles: &dyn RoleRepo = &store;
    assert!(roles.delete(RoleId::generate()).await.is_err());
    bus.emit(InvalidationEvent::Reset);
    assert_eq!(events.next().await, Some(InvalidationEvent::Reset));
}

// built-in roles

#[tokio::test]
async fn new_store_is_empty_and_builtin_store_has_the_three_roles() {
    let empty = MemStore::new();
    let roles: &dyn RoleRepo = &empty;
    assert!(roles.list().await.unwrap().is_empty());
    let privileges: &dyn PrivilegeRepo = &empty;
    assert!(privileges.list().await.unwrap().is_empty());

    let seeded = MemStore::with_builtin_roles();
    let roles: &dyn RoleRepo = &seeded;
    let names: Vec<String> = roles
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, [ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER]);
    for name in names {
        let stored = roles.get_by_name(&name).await.unwrap().unwrap();
        assert!(stored.builtin);
        assert_eq!(Some(stored.id), builtin_role_id(&name));
    }
    assert!(builtin_role_id("custom").is_none());
}

#[tokio::test]
async fn builtin_roles_carry_the_documented_privileges() {
    let store = MemStore::with_builtin_roles();
    let roles: &dyn RoleRepo = &store;
    let view_any = |actions: &[Action]| {
        (
            PrivilegeGrant::RepositoryView {
                format: None,
                repository: "*".to_owned(),
            },
            actions.to_vec(),
        )
    };
    let expectations = [
        (
            ROLE_NX_ADMIN,
            (
                PrivilegeGrant::Admin,
                vec![Action::Browse, Action::Read, Action::Write, Action::Delete],
            ),
        ),
        (
            ROLE_NX_DEVELOPER,
            view_any(&[Action::Browse, Action::Read, Action::Write]),
        ),
        (ROLE_NX_ANONYMOUS, view_any(&[Action::Browse, Action::Read])),
    ];
    for (name, (grant, actions)) in expectations {
        let id = builtin_role_id(name).unwrap();
        let effective = roles.effective_privileges(&[id]).await.unwrap();
        assert_eq!(effective.len(), 1, "{name}");
        assert_eq!(effective[0].grant, grant, "{name}");
        assert_eq!(effective[0].actions, actions, "{name}");
        assert!(effective[0].builtin, "{name}");
    }
}

#[tokio::test]
async fn clones_share_state_and_seeded_ids_are_stable_across_stores() {
    let store = MemStore::with_builtin_roles();
    let alias = store.clone();
    make_user(&alias, "shared-user").await;
    let users: &dyn UserRepo = &store;
    assert!(
        users
            .find_by_username("shared-user")
            .await
            .unwrap()
            .is_some()
    );
    let other = MemStore::with_builtin_roles();
    let (a, b): (&dyn RoleRepo, &dyn RoleRepo) = (&store, &other);
    let first = a.get_by_name(ROLE_NX_ADMIN).await.unwrap().unwrap().id;
    let second = b.get_by_name(ROLE_NX_ADMIN).await.unwrap().unwrap().id;
    assert_eq!(first, second);
    assert!(format!("{store:?}").starts_with("MemStore"));
}
