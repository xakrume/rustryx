use std::collections::HashSet;
use std::time::Duration;

use rustryx_core::StoreError;
use rustryx_core::identity::{
    Action, Privilege, PrivilegeGrant, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER,
};
use rustryx_core::ids::{PrivilegeId, RoleId};
use rustryx_core::ports::{PrivilegeRepo, RoleRepo};

use crate::fixtures::{role, unique, view_privilege};
use crate::support::{TestDb, sqlstate};

#[tokio::test(flavor = "multi_thread")]
async fn builtin_roles_and_privileges_are_seeded_without_any_user() {
    let db = TestDb::new().await;
    let store = db.store().await;
    for name in [ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER] {
        let role = store
            .roles
            .get_by_name(name)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{name} missing"));
        assert!(role.builtin, "{name}");
        assert_eq!(role.privilege_ids.len(), 1, "{name}");
    }
    let admin = store
        .roles
        .get_by_name(ROLE_NX_ADMIN)
        .await
        .unwrap()
        .unwrap();
    let effective = store.roles.effective_privileges(&[admin.id]).await.unwrap();
    assert_eq!(effective.len(), 1);
    assert_eq!(effective[0].grant, PrivilegeGrant::Admin);
    assert!(effective[0].builtin);

    let anonymous = store
        .roles
        .get_by_name(ROLE_NX_ANONYMOUS)
        .await
        .unwrap()
        .unwrap();
    let anon_privileges = store
        .roles
        .effective_privileges(&[anonymous.id])
        .await
        .unwrap();
    assert!(
        anon_privileges
            .iter()
            .all(|p| !p.actions.contains(&Action::Write) && !p.actions.contains(&Action::Delete))
    );
    let developer = store
        .roles
        .get_by_name(ROLE_NX_DEVELOPER)
        .await
        .unwrap()
        .unwrap();
    let dev = store
        .roles
        .effective_privileges(&[developer.id])
        .await
        .unwrap();
    assert!(dev[0].actions.contains(&Action::Write));
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_seed_a_default_user_or_password() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(users, 0);
    let hashes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE password_hash IS NOT NULL")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(hashes, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn role_round_trip_with_privileges_and_nesting() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let p1 = view_privilege(&unique("p1"), "*", vec![Action::Read]);
    let p2 = view_privilege(&unique("p2"), "maven-*", vec![Action::Read, Action::Write]);
    store.privileges.create(&p1).await.unwrap();
    store.privileges.create(&p2).await.unwrap();
    let child = role(&unique("child"), vec![p2.id], vec![]);
    store.roles.create(&child).await.unwrap();
    let mut parent = role(
        &unique("parent"),
        vec![p1.id, p1.id],
        vec![child.id, child.id],
    );
    parent.description = "parent role".to_owned();
    store.roles.create(&parent).await.unwrap();

    let back = store.roles.get(parent.id).await.unwrap().unwrap();
    assert_eq!(back.name, parent.name);
    assert_eq!(back.description, "parent role");
    assert_eq!(back.privilege_ids, vec![p1.id], "duplicates collapse");
    assert_eq!(back.nested_role_ids, vec![child.id]);
    assert!(!back.builtin);
    assert_eq!(
        store
            .roles
            .get_by_name(&parent.name)
            .await
            .unwrap()
            .unwrap()
            .id,
        parent.id
    );
    assert!(store.roles.get(RoleId::generate()).await.unwrap().is_none());
    assert!(store.roles.list().await.unwrap().len() >= 5);

    let dup = role(&parent.name, vec![], vec![]);
    let err = store.roles.create(&dup).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "role name already exists"),
        "{err:?}"
    );

    let mut edited = back.clone();
    edited.description = "changed".to_owned();
    edited.privilege_ids = vec![];
    edited.nested_role_ids = vec![];
    store.roles.update(&edited).await.unwrap();
    let after = store.roles.get(parent.id).await.unwrap().unwrap();
    assert_eq!(after.description, "changed");
    assert!(after.privilege_ids.is_empty() && after.nested_role_ids.is_empty());

    store.roles.delete(parent.id).await.unwrap();
    assert!(matches!(
        store.roles.delete(parent.id).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert!(matches!(
        store
            .roles
            .update(&role(&unique("ghost"), vec![], vec![]))
            .await
            .unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn role_with_unknown_member_is_refused_whole() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let bad = role(&unique("bad"), vec![PrivilegeId::generate()], vec![]);
    assert!(matches!(
        store.roles.create(&bad).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
    assert!(
        store.roles.get(bad.id).await.unwrap().is_none(),
        "no half-created role"
    );
    let bad_nested = role(&unique("bad2"), vec![], vec![RoleId::generate()]);
    assert!(matches!(
        store.roles.create(&bad_nested).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_edit_or_delete_builtin_roles_and_privileges() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let admin = store
        .roles
        .get_by_name(ROLE_NX_ADMIN)
        .await
        .unwrap()
        .unwrap();

    let mut edited = admin.clone();
    edited.description = "hijacked".to_owned();
    let err = store.roles.update(&edited).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "built-in entity is read-only"),
        "{err:?}"
    );
    assert!(matches!(
        store.roles.delete(admin.id).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    // Attaching a privilege to a built-in role changes it without touching its row.
    let extra = view_privilege(&unique("extra"), "*", vec![Action::Delete]);
    store.privileges.create(&extra).await.unwrap();
    let anonymous = store
        .roles
        .get_by_name(ROLE_NX_ANONYMOUS)
        .await
        .unwrap()
        .unwrap();
    let err = sqlx::query("INSERT INTO role_privileges (role_id, privilege_id) VALUES ($1, $2)")
        .bind(anonymous.id.as_uuid())
        .bind(extra.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&err), "RX002");
    let err = sqlx::query("DELETE FROM role_privileges WHERE role_id = $1")
        .bind(anonymous.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&err), "RX002");
    let err = sqlx::query("INSERT INTO role_roles (parent_role_id, child_role_id) VALUES ($1, $2)")
        .bind(anonymous.id.as_uuid())
        .bind(admin.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&err), "RX002");

    let builtin_privilege = store
        .roles
        .effective_privileges(&[admin.id])
        .await
        .unwrap()
        .remove(0);
    let mut changed = builtin_privilege.clone();
    changed.actions = vec![Action::Read];
    assert!(matches!(
        store.privileges.update(&changed).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
    assert!(matches!(
        store
            .privileges
            .delete(builtin_privilege.id)
            .await
            .unwrap_err(),
        StoreError::Conflict(_)
    ));

    // The seed is intact.
    let admin_after = store
        .roles
        .get_by_name(ROLE_NX_ADMIN)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admin_after.privilege_ids, admin.privilege_ids);
}

#[tokio::test(flavor = "multi_thread")]
async fn created_roles_and_privileges_are_never_builtin() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut sneaky = role(&unique("sneaky"), vec![], vec![]);
    sneaky.builtin = true;
    store.roles.create(&sneaky).await.unwrap();
    assert!(!store.roles.get(sneaky.id).await.unwrap().unwrap().builtin);
    let mut privilege = view_privilege(&unique("sneaky"), "*", vec![Action::Read]);
    privilege.builtin = true;
    store.privileges.create(&privilege).await.unwrap();
    assert!(
        !store
            .privileges
            .get(privilege.id)
            .await
            .unwrap()
            .unwrap()
            .builtin
    );
    store.privileges.delete(privilege.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn role_nesting_cycles_are_rejected_on_write() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let c = role(&unique("c"), vec![], vec![]);
    store.roles.create(&c).await.unwrap();
    let b = role(&unique("b"), vec![], vec![c.id]);
    store.roles.create(&b).await.unwrap();
    let a = role(&unique("a"), vec![], vec![b.id]);
    store.roles.create(&a).await.unwrap();

    // c -> a would close a -> b -> c -> a.
    let mut closing = c.clone();
    closing.nested_role_ids = vec![a.id];
    let err = store.roles.update(&closing).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "role nesting cycle"),
        "{err:?}"
    );
    let mut selfish = a.clone();
    selfish.nested_role_ids = vec![a.id];
    assert!(matches!(
        store.roles.update(&selfish).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    // The failed updates changed nothing.
    assert!(
        store
            .roles
            .get(c.id)
            .await
            .unwrap()
            .unwrap()
            .nested_role_ids
            .is_empty()
    );
    assert_eq!(
        store
            .roles
            .get(a.id)
            .await
            .unwrap()
            .unwrap()
            .nested_role_ids,
        vec![b.id]
    );

    // A diamond is not a cycle.
    let mut diamond = a.clone();
    diamond.nested_role_ids = vec![b.id, c.id];
    store.roles.update(&diamond).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_opposite_nesting_cannot_create_a_cycle() {
    let db = TestDb::new().await;
    let store = db.store().await;
    for round in 0..20 {
        let x = role(&unique(&format!("x{round}")), vec![], vec![]);
        let y = role(&unique(&format!("y{round}")), vec![], vec![]);
        store.roles.create(&x).await.unwrap();
        store.roles.create(&y).await.unwrap();
        let mut x_over_y = x.clone();
        x_over_y.nested_role_ids = vec![y.id];
        let mut y_over_x = y.clone();
        y_over_x.nested_role_ids = vec![x.id];
        let (r1, r2) = tokio::join!(store.roles.update(&x_over_y), store.roles.update(&y_over_x));
        let successes = usize::from(r1.is_ok()) + usize::from(r2.is_ok());
        assert_eq!(successes, 1, "round {round}: {r1:?} {r2:?}");
        for failure in [r1, r2].into_iter().filter_map(Result::err) {
            assert!(matches!(failure, StoreError::Conflict(_)), "{failure:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn effective_privileges_follow_nesting_recursively() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut privileges = Vec::new();
    for i in 0..4 {
        let p = view_privilege(&unique(&format!("p{i}")), "*", vec![Action::Read]);
        store.privileges.create(&p).await.unwrap();
        privileges.push(p);
    }
    let c = role(&unique("c"), vec![privileges[2].id], vec![]);
    store.roles.create(&c).await.unwrap();
    let b = role(&unique("b"), vec![privileges[1].id], vec![c.id]);
    store.roles.create(&b).await.unwrap();
    let a = role(
        &unique("a"),
        vec![privileges[0].id, privileges[1].id],
        vec![b.id],
    );
    store.roles.create(&a).await.unwrap();
    let lone = role(&unique("lone"), vec![privileges[3].id], vec![]);
    store.roles.create(&lone).await.unwrap();

    let ids = |list: Vec<Privilege>| list.into_iter().map(|p| p.id).collect::<HashSet<_>>();
    let from_a = ids(store.roles.effective_privileges(&[a.id]).await.unwrap());
    assert_eq!(from_a, privileges[..3].iter().map(|p| p.id).collect());
    let from_c = ids(store.roles.effective_privileges(&[c.id]).await.unwrap());
    assert_eq!(from_c, HashSet::from([privileges[2].id]));
    let from_both = ids(store
        .roles
        .effective_privileges(&[b.id, lone.id])
        .await
        .unwrap());
    assert_eq!(
        from_both,
        HashSet::from([privileges[1].id, privileges[2].id, privileges[3].id])
    );
    assert!(
        store
            .roles
            .effective_privileges(&[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .roles
            .effective_privileges(&[RoleId::generate()])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn effective_privileges_terminate_even_on_a_cycle_made_by_hand() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let p = view_privilege(&unique("p"), "*", vec![Action::Read]);
    store.privileges.create(&p).await.unwrap();
    let a = role(&unique("a"), vec![p.id], vec![]);
    let b = role(&unique("b"), vec![], vec![]);
    store.roles.create(&a).await.unwrap();
    store.roles.create(&b).await.unwrap();
    // Bypass the guard the way a bad restore or a manual repair could.
    let mut conn = store.pool().acquire().await.unwrap();
    sqlx::query("ALTER TABLE role_roles DISABLE TRIGGER role_roles_no_cycle")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO role_roles (parent_role_id, child_role_id) VALUES ($1, $2), ($2, $1)")
        .bind(a.id.as_uuid())
        .bind(b.id.as_uuid())
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE role_roles ENABLE TRIGGER role_roles_no_cycle")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        store.roles.effective_privileges(&[b.id]),
    )
    .await;
    let privileges = result.expect("terminates").unwrap();
    assert_eq!(privileges.len(), 1);
}
