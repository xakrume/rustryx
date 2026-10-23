use std::time::Duration;

use rustryx_core::StoreError;
use rustryx_core::identity::{
    Action, ContentSelector, IdpGroupMapping, IdpProvider, Privilege, PrivilegeGrant, ROLE_NX_ADMIN,
};
use rustryx_core::ids::{ContentSelectorId, IdpGroupMappingId, PrivilegeId, RoleId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{IdpMappingRepo, PrivilegeRepo, RoleRepo, SelectorRepo};
use tokio::task::JoinSet;

use crate::fixtures::{now, role, unique, view_privilege};
use crate::support::{TestDb, listen_rbac, next_payload};

#[tokio::test(flavor = "multi_thread")]
async fn privilege_grants_round_trip_in_every_shape() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let selector = ContentSelector {
        id: ContentSelectorId::generate(),
        name: unique("sel"),
        description: "only releases".to_owned(),
        expression: "format == \"maven2\" && path.startsWith(\"/com/acme/\")".to_owned(),
        updated_at: now(),
    };
    store.selectors.create(&selector).await.unwrap();

    let shapes = [
        PrivilegeGrant::Admin,
        PrivilegeGrant::RepositoryView {
            format: None,
            repository: "*".to_owned(),
        },
        PrivilegeGrant::RepositoryView {
            format: Some(RepoFormat::Npm),
            repository: "npm-hosted".to_owned(),
        },
        PrivilegeGrant::RepositoryContentSelector {
            repository: "maven-releases".to_owned(),
            selector_id: selector.id,
        },
    ];
    for grant in shapes {
        let privilege = Privilege {
            id: PrivilegeId::generate(),
            name: unique("priv"),
            description: "d".to_owned(),
            builtin: false,
            grant: grant.clone(),
            actions: vec![Action::Write, Action::Read, Action::Read],
        };
        store.privileges.create(&privilege).await.unwrap();
        let back = store.privileges.get(privilege.id).await.unwrap().unwrap();
        assert_eq!(back.grant, grant);
        assert_eq!(
            back.actions,
            vec![Action::Read, Action::Write],
            "sorted and deduplicated"
        );
        assert_eq!(back.description, "d");
    }
    assert!(store.privileges.list().await.unwrap().len() >= 7);
    assert!(
        store
            .privileges
            .get(PrivilegeId::generate())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn privilege_update_delete_and_constraints() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut privilege = view_privilege(&unique("p"), "repo", vec![Action::Browse]);
    store.privileges.create(&privilege).await.unwrap();
    assert!(matches!(
        store.privileges.create(&privilege).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    privilege.actions = vec![Action::Delete];
    privilege.grant = PrivilegeGrant::RepositoryView {
        format: Some(RepoFormat::Raw),
        repository: "r".to_owned(),
    };
    store.privileges.update(&privilege).await.unwrap();
    let back = store.privileges.get(privilege.id).await.unwrap().unwrap();
    assert_eq!(back.actions, vec![Action::Delete]);

    let mut empty_actions = view_privilege(&unique("e"), "repo", vec![]);
    assert!(matches!(
        store.privileges.create(&empty_actions).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
    empty_actions.actions = vec![Action::Read];
    empty_actions.grant = PrivilegeGrant::RepositoryContentSelector {
        repository: "r".to_owned(),
        selector_id: ContentSelectorId::generate(),
    };
    assert!(
        matches!(
            store.privileges.create(&empty_actions).await.unwrap_err(),
            StoreError::Conflict(_)
        ),
        "unknown selector"
    );

    // Deleting a privilege takes it out of the roles that hold it.
    let holder = role(&unique("holder"), vec![privilege.id], vec![]);
    store.roles.create(&holder).await.unwrap();
    store.privileges.delete(privilege.id).await.unwrap();
    assert!(
        store
            .roles
            .get(holder.id)
            .await
            .unwrap()
            .unwrap()
            .privilege_ids
            .is_empty()
    );
    assert!(matches!(
        store.privileges.delete(privilege.id).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert!(matches!(
        store.privileges.update(&privilege).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn selector_lifecycle_and_updated_at_always_advances() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut selector = ContentSelector {
        id: ContentSelectorId::generate(),
        name: unique("s"),
        description: String::new(),
        expression: "path.startsWith(\"/a\")".to_owned(),
        updated_at: now(),
    };
    store.selectors.create(&selector).await.unwrap();
    assert!(matches!(
        store.selectors.create(&selector).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
    let first = store.selectors.get(selector.id).await.unwrap().unwrap();
    assert_eq!(first.updated_at, selector.updated_at);

    // The caller repeats the same timestamp: the cache key must still change.
    selector.expression = "path.startsWith(\"/b\")".to_owned();
    store.selectors.update(&selector).await.unwrap();
    let second = store.selectors.get(selector.id).await.unwrap().unwrap();
    assert_eq!(second.expression, selector.expression);
    assert!(second.updated_at > first.updated_at);
    store.selectors.update(&selector).await.unwrap();
    let third = store.selectors.get(selector.id).await.unwrap().unwrap();
    assert!(third.updated_at > second.updated_at);

    assert_eq!(store.selectors.list().await.unwrap().len(), 1);
    store.selectors.delete(selector.id).await.unwrap();
    assert!(matches!(
        store.selectors.delete(selector.id).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert!(matches!(
        store.selectors.update(&selector).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert!(store.selectors.get(selector.id).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_selector_in_use_cannot_be_deleted_and_the_expression_is_bounded() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let selector = ContentSelector {
        id: ContentSelectorId::generate(),
        name: unique("s"),
        description: String::new(),
        expression: "true".to_owned(),
        updated_at: now(),
    };
    store.selectors.create(&selector).await.unwrap();
    let privilege = Privilege {
        id: PrivilegeId::generate(),
        name: unique("p"),
        description: String::new(),
        builtin: false,
        grant: PrivilegeGrant::RepositoryContentSelector {
            repository: "*".to_owned(),
            selector_id: selector.id,
        },
        actions: vec![Action::Read],
    };
    store.privileges.create(&privilege).await.unwrap();
    let err = store.selectors.delete(selector.id).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "content selector is still referenced by a privilege"),
        "{err:?}"
    );
    store.privileges.delete(privilege.id).await.unwrap();
    store.selectors.delete(selector.id).await.unwrap();

    let huge = ContentSelector {
        id: ContentSelectorId::generate(),
        name: unique("huge"),
        description: String::new(),
        expression: "a".repeat(1025),
        updated_at: now(),
    };
    assert!(matches!(
        store.selectors.create(&huge).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn group_mappings_upsert_replace_list_and_delete() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let r1 = role(&unique("r1"), vec![], vec![]);
    let r2 = role(&unique("r2"), vec![], vec![]);
    store.roles.create(&r1).await.unwrap();
    store.roles.create(&r2).await.unwrap();

    let first = IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        provider: IdpProvider::Oidc,
        external_group: "/rustryx/readers".to_owned(),
        role_id: r1.id,
        priority: 20,
    };
    store.idp_mappings.upsert(&first).await.unwrap();
    let second = IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        provider: IdpProvider::Oidc,
        external_group: "/rustryx/admins".to_owned(),
        role_id: r2.id,
        priority: 10,
    };
    store.idp_mappings.upsert(&second).await.unwrap();
    let ldap = IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        provider: IdpProvider::Ldap,
        external_group: "/rustryx/readers".to_owned(),
        role_id: r1.id,
        priority: 0,
    };
    store.idp_mappings.upsert(&ldap).await.unwrap();

    let oidc = store.idp_mappings.list(IdpProvider::Oidc).await.unwrap();
    assert_eq!(oidc.len(), 2);
    assert_eq!(
        oidc[0].external_group, "/rustryx/admins",
        "ordered by priority"
    );
    assert_eq!(
        store
            .idp_mappings
            .list(IdpProvider::Ldap)
            .await
            .unwrap()
            .len(),
        1
    );

    // Same (provider, group) again: replaced, id included.
    let replacement = IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        role_id: r2.id,
        priority: 5,
        ..first.clone()
    };
    store.idp_mappings.upsert(&replacement).await.unwrap();
    let oidc = store.idp_mappings.list(IdpProvider::Oidc).await.unwrap();
    assert_eq!(oidc.len(), 2);
    let replaced = oidc
        .iter()
        .find(|m| m.external_group == "/rustryx/readers")
        .unwrap();
    assert_eq!(replaced.id, replacement.id);
    assert_eq!(replaced.role_id, r2.id);

    // Group ids are exact: a different case or a missing slash is another group.
    let lookalike = IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        external_group: "/Rustryx/Admins".to_owned(),
        ..second.clone()
    };
    store.idp_mappings.upsert(&lookalike).await.unwrap();
    assert_eq!(
        store
            .idp_mappings
            .list(IdpProvider::Oidc)
            .await
            .unwrap()
            .len(),
        3
    );

    // A mapped role cannot be deleted from under the mapping.
    assert!(matches!(
        store.roles.delete(r2.id).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
    store.idp_mappings.delete(replacement.id).await.unwrap();
    assert!(matches!(
        store.idp_mappings.delete(replacement.id).await.unwrap_err(),
        StoreError::NotFound(_)
    ));

    let unknown_role = IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        role_id: RoleId::generate(),
        external_group: "/x".to_owned(),
        ..first
    };
    assert!(matches!(
        store.idp_mappings.upsert(&unknown_role).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn rbac_writes_notify_the_channel() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut listener = listen_rbac(&db).await;
    let wait = Duration::from_secs(5);

    let p = view_privilege(&unique("p"), "*", vec![Action::Read]);
    store.privileges.create(&p).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, wait).await.as_deref(),
        Some("rbac")
    );
    let r = role(&unique("r"), vec![p.id], vec![]);
    store.roles.create(&r).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, wait).await.as_deref(),
        Some("rbac")
    );
    store.roles.update(&r).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, wait).await.as_deref(),
        Some("rbac")
    );
    store.roles.delete(r.id).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, wait).await.as_deref(),
        Some("rbac")
    );
    store.privileges.delete(p.id).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, wait).await.as_deref(),
        Some("rbac")
    );

    // A refused change must not announce anything.
    let admin = store
        .roles
        .get_by_name(ROLE_NX_ADMIN)
        .await
        .unwrap()
        .unwrap();
    let _ = store.roles.delete(admin.id).await.unwrap_err();
    assert_eq!(
        next_payload(&mut listener, Duration::from_millis(500)).await,
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_role_creation_with_one_name_yields_one_winner() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let name = unique("same");
    let mut tasks = JoinSet::new();
    for _ in 0..8 {
        let roles = store.roles.clone();
        let role = role(&name, vec![], vec![]);
        tasks.spawn(async move { roles.create(&role).await });
    }
    let mut ok = 0;
    while let Some(joined) = tasks.join_next().await {
        match joined.unwrap() {
            Ok(()) => ok += 1,
            Err(e) => assert!(matches!(e, StoreError::Conflict(_)), "{e:?}"),
        }
    }
    assert_eq!(ok, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn renaming_works_and_a_name_clash_is_a_conflict() {
    let db = TestDb::new().await;
    let store = db.store().await;

    let mut first = role(&unique("first"), vec![], vec![]);
    let second = role(&unique("second"), vec![], vec![]);
    store.roles.create(&first).await.unwrap();
    store.roles.create(&second).await.unwrap();
    first.name = unique("renamed");
    store.roles.update(&first).await.unwrap();
    assert_eq!(
        store.roles.get(first.id).await.unwrap().unwrap().name,
        first.name
    );
    first.name = second.name.clone();
    assert!(matches!(
        store.roles.update(&first).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    let mut privilege = view_privilege(&unique("p"), "*", vec![Action::Read]);
    let other = view_privilege(&unique("q"), "*", vec![Action::Read]);
    store.privileges.create(&privilege).await.unwrap();
    store.privileges.create(&other).await.unwrap();
    privilege.name = unique("p-renamed");
    store.privileges.update(&privilege).await.unwrap();
    assert_eq!(
        store
            .privileges
            .get(privilege.id)
            .await
            .unwrap()
            .unwrap()
            .name,
        privilege.name
    );
    privilege.name = other.name.clone();
    assert!(matches!(
        store.privileges.update(&privilege).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    let mut selector = ContentSelector {
        id: ContentSelectorId::generate(),
        name: unique("s"),
        description: String::new(),
        expression: "true".to_owned(),
        updated_at: now(),
    };
    store.selectors.create(&selector).await.unwrap();
    selector.name = unique("s-renamed");
    store.selectors.update(&selector).await.unwrap();
    assert_eq!(
        store
            .selectors
            .get(selector.id)
            .await
            .unwrap()
            .unwrap()
            .name,
        selector.name
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_a_role_does_not_block_assigning_it_to_users() {
    use rustryx_core::identity::{NewUser, RoleOrigin, UserSource};
    use rustryx_core::ports::UserRepo;

    let db = TestDb::new().await;
    let store = db.store().await;
    let target = role(&unique("busy"), vec![], vec![]);
    store.roles.create(&target).await.unwrap();
    let user = store
        .users
        .create(&NewUser {
            username: unique("u"),
            source: UserSource::Ldap,
            issuer: None,
            subject: None,
            email: None,
            display_name: None,
            password_hash: None,
        })
        .await
        .unwrap();

    // Hold a role edit open (row updated, transaction not committed) and assign the same role
    // meanwhile. A KEY-strength lock from the edit would make the assignment wait for it.
    let mut tx = store.pool().begin().await.unwrap();
    sqlx::query("UPDATE roles SET description = 'edited', updated_at = now() WHERE id = $1")
        .bind(target.id.as_uuid())
        .execute(&mut *tx)
        .await
        .unwrap();
    let wanted = [target.id];
    let assign = store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Oidc, &wanted);
    tokio::time::timeout(Duration::from_secs(5), assign)
        .await
        .expect("the assignment does not wait for the open role edit")
        .unwrap();
    tx.commit().await.unwrap();
}
