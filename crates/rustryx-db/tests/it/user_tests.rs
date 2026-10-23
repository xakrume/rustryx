use std::time::Duration;

use rustryx_core::identity::{NewUser, RoleOrigin, UserSource};
use rustryx_core::ids::{RoleId, UserId};
use rustryx_core::ports::{PageRequest, RoleRepo, UserRepo};
use rustryx_core::{Secret, StoreError};

use crate::fixtures::{now, role, unique};
use crate::support::{TestDb, listen_rbac, next_payload};

const ARGON2: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaGhhc2g";

fn local(username: &str) -> NewUser {
    NewUser {
        username: username.to_owned(),
        source: UserSource::Local,
        issuer: None,
        subject: None,
        email: Some(format!("{username}@example.org")),
        display_name: Some(username.to_owned()),
        password_hash: Some(Secret::new(ARGON2)),
    }
}

fn oidc(username: &str, issuer: &str, subject: &str) -> NewUser {
    NewUser {
        username: username.to_owned(),
        source: UserSource::Oidc,
        issuer: Some(issuer.to_owned()),
        subject: Some(subject.to_owned()),
        email: None,
        display_name: None,
        password_hash: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn create_and_read_back_a_local_user() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = store.users.create(&local("alice")).await.unwrap();
    assert_eq!(user.username, "alice");
    assert_eq!(user.source, UserSource::Local);
    assert!(user.enabled);
    assert!(user.tokens_valid_after.is_none());
    assert_eq!(user.password_hash.as_ref().unwrap().expose(), ARGON2);
    assert_eq!(store.users.get(user.id).await.unwrap().unwrap().id, user.id);
    assert!(store.users.get(UserId::generate()).await.unwrap().is_none());
    assert_eq!(
        store
            .users
            .find_by_username("alice")
            .await
            .unwrap()
            .unwrap()
            .id,
        user.id
    );
    assert!(store.users.find_by_username("bob").await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_print_the_password_hash_in_debug_output() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = store.users.create(&local("carol")).await.unwrap();
    assert!(!format!("{user:?}").contains("argon2id"));
}

#[tokio::test(flavor = "multi_thread")]
async fn username_is_unique_across_sources_and_case_insensitive() {
    let db = TestDb::new().await;
    let store = db.store().await;
    store.users.create(&local("Alice")).await.unwrap();

    let err = store
        .users
        .create(&oidc("alice", "https://kc/realms/r", "sub-1"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "username already exists"),
        "{err:?}"
    );
    let err = store.users.create(&local("ALICE")).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
    assert_eq!(
        store
            .users
            .find_by_username("aLiCe")
            .await
            .unwrap()
            .unwrap()
            .username,
        "Alice"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn external_identity_is_matched_by_source_issuer_and_subject_only() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let kc = "https://kc.example.org/realms/main";
    let user = store
        .users
        .create(&oidc("dave", kc, "sub-dave"))
        .await
        .unwrap();

    let found = store
        .users
        .find_external(UserSource::Oidc, Some(kc), Some("sub-dave"))
        .await
        .unwrap();
    assert_eq!(found.unwrap().id, user.id);
    // Same subject at another issuer is another person.
    assert!(
        store
            .users
            .find_external(
                UserSource::Oidc,
                Some("https://other/realms/x"),
                Some("sub-dave")
            )
            .await
            .unwrap()
            .is_none()
    );
    // Wrong source, missing subject.
    assert!(
        store
            .users
            .find_external(UserSource::Ldap, Some(kc), Some("sub-dave"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .users
            .find_external(UserSource::Oidc, Some(kc), None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_concatenate_issuer_and_subject() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let first = store.users.create(&oidc("one", "a|b", "c")).await.unwrap();
    let second = store.users.create(&oidc("two", "a", "b|c")).await.unwrap();
    assert_ne!(first.id, second.id);
    let found = store
        .users
        .find_external(UserSource::Oidc, Some("a|b"), Some("c"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, first.id);
    let found = store
        .users
        .find_external(UserSource::Oidc, Some("a"), Some("b|c"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, second.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_link_external_accounts_by_email() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut admin = local("admin-local");
    admin.email = Some("boss@example.org".to_owned());
    let local_admin = store.users.create(&admin).await.unwrap();
    let mut external = oidc("someone-else", "https://kc/realms/r", "sub-x");
    external.email = Some("boss@example.org".to_owned());
    let created = store.users.create(&external).await.unwrap();
    assert_ne!(created.id, local_admin.id);
    let found = store
        .users
        .find_external(UserSource::Oidc, Some("https://kc/realms/r"), Some("sub-x"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, created.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_external_identity_conflicts() {
    let db = TestDb::new().await;
    let store = db.store().await;
    store
        .users
        .create(&oidc("first", "https://kc/r", "same-sub"))
        .await
        .unwrap();
    let err = store
        .users
        .create(&oidc("second", "https://kc/r", "same-sub"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "external identity already exists"),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_store_a_plaintext_password() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut plain = local("eve");
    plain.password_hash = Some(Secret::new("hunter2-plaintext"));
    let err = store.users.create(&plain).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "password must be stored as a hash"),
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("hunter2"));
    assert!(store.users.find_by_username("eve").await.unwrap().is_none());

    let user = store.users.create(&local("frank")).await.unwrap();
    let err = store
        .users
        .set_password_hash(user.id, "hunter2-plaintext")
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
    assert_eq!(
        store
            .users
            .get(user.id)
            .await
            .unwrap()
            .unwrap()
            .password_hash
            .unwrap()
            .expose(),
        ARGON2,
        "the previous hash is untouched"
    );
    let bcrypt = "$2b$12$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234";
    store
        .users
        .set_password_hash(user.id, bcrypt)
        .await
        .unwrap();
    assert_eq!(
        store
            .users
            .get(user.id)
            .await
            .unwrap()
            .unwrap()
            .password_hash
            .unwrap()
            .expose(),
        bcrypt
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_local_accounts_carry_a_password_hash_or_lack_an_external_identity() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut with_hash = oidc("oidc-with-hash", "https://kc/r", "s1");
    with_hash.password_hash = Some(Secret::new(ARGON2));
    assert!(matches!(
        store.users.create(&with_hash).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    let mut no_subject = oidc("oidc-no-subject", "https://kc/r", "s2");
    no_subject.subject = None;
    assert!(matches!(
        store.users.create(&no_subject).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    let mut local_with_subject = local("local-with-subject");
    local_with_subject.subject = Some("x".to_owned());
    assert!(matches!(
        store.users.create(&local_with_subject).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    let ldap = NewUser {
        username: "ldap-user".to_owned(),
        source: UserSource::Ldap,
        issuer: None,
        subject: None,
        email: None,
        display_name: None,
        password_hash: None,
    };
    store.users.create(&ldap).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn enable_disable_and_token_cutoff_report_missing_users() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = store.users.create(&local("gina")).await.unwrap();
    store.users.set_enabled(user.id, false).await.unwrap();
    assert!(!store.users.get(user.id).await.unwrap().unwrap().enabled);
    store.users.set_enabled(user.id, true).await.unwrap();
    assert!(store.users.get(user.id).await.unwrap().unwrap().enabled);

    let cutoff = now();
    store
        .users
        .set_tokens_valid_after(user.id, cutoff)
        .await
        .unwrap();
    assert_eq!(
        store
            .users
            .get(user.id)
            .await
            .unwrap()
            .unwrap()
            .tokens_valid_after,
        Some(cutoff)
    );

    let ghost = UserId::generate();
    assert!(matches!(
        store.users.set_enabled(ghost, true).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert!(matches!(
        store
            .users
            .set_tokens_valid_after(ghost, cutoff)
            .await
            .unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert!(matches!(
        store
            .users
            .set_password_hash(ghost, ARGON2)
            .await
            .unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert!(matches!(
        store.users.delete(ghost).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_paginates_by_keyset() {
    let db = TestDb::new().await;
    let store = db.store().await;
    for i in 0..7 {
        store
            .users
            .create(&local(&format!("user{i}")))
            .await
            .unwrap();
    }
    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = store
            .users
            .list(&PageRequest {
                limit: 3,
                after: after.clone(),
            })
            .await
            .unwrap();
        pages += 1;
        seen.extend(page.items.iter().map(|u| u.id));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 7);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted);
    let err = store
        .users
        .list(&PageRequest {
            limit: 3,
            after: Some("garbage".to_owned()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
}

async fn make_roles(store: &rustryx_db::PgStore, names: &[&str]) -> Vec<RoleId> {
    let mut ids = Vec::new();
    for name in names {
        let r = role(&unique(name), vec![], vec![]);
        store.roles.create(&r).await.unwrap();
        ids.push(r.id);
    }
    ids
}

#[tokio::test(flavor = "multi_thread")]
async fn replace_roles_touches_only_rows_of_its_own_origin() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = store.users.create(&local("henry")).await.unwrap();
    let roles = make_roles(&store, &["a", "b", "c", "d"]).await;
    let (a, b, c, d) = (roles[0], roles[1], roles[2], roles[3]);

    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Manual, &[a])
        .await
        .unwrap();
    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Ldap, &[d])
        .await
        .unwrap();
    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Oidc, &[b, c])
        .await
        .unwrap();
    // A Keycloak group change: b is gone, d arrives.
    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Oidc, &[c, d])
        .await
        .unwrap();

    let held = store.users.roles_of(user.id).await.unwrap();
    let pairs: Vec<(RoleId, RoleOrigin)> = held.iter().map(|(r, o)| (r.id, *o)).collect();
    assert!(
        pairs.contains(&(a, RoleOrigin::Manual)),
        "the manual grant survives"
    );
    assert!(
        pairs.contains(&(d, RoleOrigin::Ldap)),
        "the LDAP row survives"
    );
    assert!(pairs.contains(&(c, RoleOrigin::Oidc)));
    assert!(pairs.contains(&(d, RoleOrigin::Oidc)));
    assert!(
        !pairs.contains(&(b, RoleOrigin::Oidc)),
        "the removed group's role is gone"
    );
    assert_eq!(pairs.len(), 4);

    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Oidc, &[])
        .await
        .unwrap();
    let pairs: Vec<RoleOrigin> = store
        .users
        .roles_of(user.id)
        .await
        .unwrap()
        .iter()
        .map(|(_, o)| *o)
        .collect();
    assert_eq!(pairs.len(), 2);
    assert!(!pairs.contains(&RoleOrigin::Oidc));
}

#[tokio::test(flavor = "multi_thread")]
async fn replace_roles_rolls_back_entirely_on_an_unknown_role() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = store.users.create(&local("irene")).await.unwrap();
    let roles = make_roles(&store, &["keep"]).await;
    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Oidc, &roles)
        .await
        .unwrap();

    let err = store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Oidc, &[RoleId::generate()])
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    let held = store.users.roles_of(user.id).await.unwrap();
    assert_eq!(
        held.len(),
        1,
        "the delete of the old rows was rolled back too"
    );

    let err = store
        .users
        .replace_roles_for_origin(UserId::generate(), RoleOrigin::Oidc, &roles)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn role_replacement_notifies_other_nodes_after_commit_only() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = store.users.create(&local("judy")).await.unwrap();
    let roles = make_roles(&store, &["n1"]).await;
    let mut listener = listen_rbac(&db).await;

    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Ldap, &roles)
        .await
        .unwrap();
    let payload = next_payload(&mut listener, Duration::from_secs(5)).await;
    assert_eq!(payload, Some(format!("user:{}", user.id)));

    // A failed replacement queues a notification inside its transaction; rollback drops it.
    let _ = store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Ldap, &[RoleId::generate()])
        .await
        .unwrap_err();
    assert_eq!(
        next_payload(&mut listener, Duration::from_millis(500)).await,
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn user_changes_notify_and_delete_cascades() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = store.users.create(&local("kate")).await.unwrap();
    let roles = make_roles(&store, &["r"]).await;
    store
        .users
        .replace_roles_for_origin(user.id, RoleOrigin::Manual, &roles)
        .await
        .unwrap();
    let mut listener = listen_rbac(&db).await;

    store
        .users
        .set_tokens_valid_after(user.id, now())
        .await
        .unwrap();
    assert_eq!(
        next_payload(&mut listener, Duration::from_secs(5)).await,
        Some(format!("user:{}", user.id))
    );
    store.users.set_enabled(user.id, false).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, Duration::from_secs(5)).await,
        Some(format!("user:{}", user.id))
    );

    store.users.delete(user.id).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, Duration::from_secs(5)).await,
        Some(format!("user:{}", user.id))
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM user_roles")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert!(store.users.get(user.id).await.unwrap().is_none());
}
