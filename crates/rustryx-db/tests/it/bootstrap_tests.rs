use rustryx_core::identity::{ROLE_NX_ADMIN, RoleOrigin};
use rustryx_core::ports::UserRepo;
use rustryx_core::{Secret, StoreError};
use rustryx_db::bootstrap_admin;
use tokio::task::JoinSet;

use crate::support::TestDb;

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaGhhc2g";

#[tokio::test(flavor = "multi_thread")]
async fn creates_the_admin_once_and_only_when_none_exists() {
    let db = TestDb::new().await;
    let store = db.store().await;
    assert!(
        bootstrap_admin(store.pool(), "root", &Secret::new(HASH))
            .await
            .unwrap()
    );

    let admin = store.users.find_by_username("root").await.unwrap().unwrap();
    assert!(admin.enabled);
    assert_eq!(admin.password_hash.as_ref().unwrap().expose(), HASH);
    let roles = store.users.roles_of(admin.id).await.unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].0.name, ROLE_NX_ADMIN);
    assert_eq!(roles[0].1, RoleOrigin::Manual);

    // A restart, or a second node: nothing changes, not even with other arguments.
    assert!(
        !bootstrap_admin(store.pool(), "root", &Secret::new(HASH))
            .await
            .unwrap()
    );
    assert!(
        !bootstrap_admin(store.pool(), "someone-else", &Secret::new(HASH))
            .await
            .unwrap()
    );
    assert!(
        store
            .users
            .find_by_username("someone-else")
            .await
            .unwrap()
            .is_none()
    );
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(users, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_nodes_create_exactly_one_admin() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let mut tasks = JoinSet::new();
    for i in 0..8 {
        let pool = pool.clone();
        tasks.spawn(async move {
            bootstrap_admin(&pool, &format!("root{i}"), &Secret::new(HASH)).await
        });
    }
    let mut created = 0;
    while let Some(joined) = tasks.join_next().await {
        if joined.unwrap().unwrap() {
            created += 1;
        }
    }
    assert_eq!(created, 1);
    let admins: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE r.name = 'nx-admin'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(admins, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_store_a_plaintext_password_as_the_admin_credential() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let err = bootstrap_admin(store.pool(), "root", &Secret::new("admin123"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "password must be stored as a hash"),
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("admin123"));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(users, 0, "nothing was committed");
    // The failure left the door open for the correct call.
    assert!(
        bootstrap_admin(store.pool(), "root", &Secret::new(HASH))
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_taken_username_is_a_conflict_not_a_second_admin() {
    let db = TestDb::new().await;
    let store = db.store().await;
    store
        .users
        .create(&rustryx_core::identity::NewUser {
            username: "root".to_owned(),
            source: rustryx_core::identity::UserSource::Ldap,
            issuer: None,
            subject: None,
            email: None,
            display_name: None,
            password_hash: None,
        })
        .await
        .unwrap();
    let err = bootstrap_admin(store.pool(), "ROOT", &Secret::new(HASH))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_schema_without_the_admin_role_commits_nothing() {
    let db = TestDb::new().await;
    let store = db.store().await;
    // Simulate a restore that lost the built-in role (the guard has to be switched off first).
    sqlx::query("ALTER TABLE roles DISABLE TRIGGER roles_builtin_guard")
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM roles WHERE name = 'nx-admin'")
        .execute(store.pool())
        .await
        .unwrap();

    let err = bootstrap_admin(store.pool(), "root", &Secret::new(HASH))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(
        users, 0,
        "the user insert was rolled back with the failed grant"
    );
}
