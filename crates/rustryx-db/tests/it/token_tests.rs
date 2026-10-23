use std::time::Duration;

use rustryx_core::StoreError;
use rustryx_core::identity::{ApiToken, NewUser, UserSource};
use rustryx_core::ids::{ApiTokenId, UserId};
use rustryx_core::ports::{TokenRepo, UserRepo};
use sha2::{Digest, Sha256};
use time::Duration as TimeDuration;

use crate::fixtures::{now, unique};
use crate::support::{TestDb, listen_rbac, next_payload};

fn hash_of(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn owner(store: &rustryx_db::PgStore) -> UserId {
    store
        .users
        .create(&NewUser {
            username: unique("owner"),
            source: UserSource::Ldap,
            issuer: None,
            subject: None,
            email: None,
            display_name: None,
            password_hash: None,
        })
        .await
        .unwrap()
        .id
}

fn token(user_id: UserId, secret: &str) -> ApiToken {
    ApiToken {
        id: ApiTokenId::generate(),
        user_id,
        name: "ci".to_owned(),
        token_hash: hash_of(secret),
        display_prefix: "ryx_ab".to_owned(),
        scopes: vec!["read".to_owned(), "repo:maven-*".to_owned()],
        expires_at: Some(now() + TimeDuration::days(30)),
        revoked_at: None,
        last_used_at: None,
        created_at: now(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn create_find_by_hash_and_list() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = owner(&store).await;
    let t = token(user, "ryx_secret-one");
    store.tokens.create(&t).await.unwrap();

    let found = store
        .tokens
        .find_by_hash(&hash_of("ryx_secret-one"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, t.id);
    assert_eq!(found.scopes, t.scopes);
    assert_eq!(found.expires_at, t.expires_at);
    assert_eq!(found.display_prefix, "ryx_ab");
    assert!(
        store
            .tokens
            .find_by_hash(&hash_of("ryx_other"))
            .await
            .unwrap()
            .is_none()
    );

    store
        .tokens
        .create(&token(user, "ryx_secret-two"))
        .await
        .unwrap();
    assert_eq!(store.tokens.list_for_user(user).await.unwrap().len(), 2);
    assert!(
        store
            .tokens
            .list_for_user(UserId::generate())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_store_the_token_itself() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = owner(&store).await;
    store
        .tokens
        .create(&token(user, "ryx_canary-token-value"))
        .await
        .unwrap();
    let dump: String = sqlx::query_scalar("SELECT row_to_json(t)::text FROM user_tokens t")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(!dump.contains("canary-token-value"), "{dump}");
}

#[tokio::test(flavor = "multi_thread")]
async fn hashes_must_be_unique_and_well_formed() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = owner(&store).await;
    let first = token(user, "ryx_dup");
    store.tokens.create(&first).await.unwrap();
    let mut clash = token(user, "ryx_dup");
    clash.id = ApiTokenId::generate();
    let err = store.tokens.create(&clash).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "token already exists"),
        "{err:?}"
    );

    for bad in ["", "ABCDEF", &"g".repeat(64), &hash_of("x").to_uppercase()] {
        let mut malformed = token(user, "ryx_x");
        malformed.id = ApiTokenId::generate();
        malformed.token_hash = bad.to_owned();
        assert!(
            matches!(
                store.tokens.create(&malformed).await.unwrap_err(),
                StoreError::Conflict(_)
            ),
            "{bad}"
        );
    }
    let orphan = token(UserId::generate(), "ryx_orphan");
    assert!(matches!(
        store.tokens.create(&orphan).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_keeps_the_first_time_notifies_and_reports_unknown_ids() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = owner(&store).await;
    let t = token(user, "ryx_to-revoke");
    store.tokens.create(&t).await.unwrap();
    let mut listener = listen_rbac(&db).await;

    let first = now();
    store.tokens.revoke(t.id, first).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, Duration::from_secs(5)).await,
        Some(format!("user:{user}"))
    );
    store
        .tokens
        .revoke(t.id, first + TimeDuration::hours(1))
        .await
        .unwrap();
    let found = store
        .tokens
        .find_by_hash(&t.token_hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found.revoked_at,
        Some(first),
        "a second revoke does not rewrite history"
    );
    assert!(matches!(
        store
            .tokens
            .revoke(ApiTokenId::generate(), first)
            .await
            .unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn last_used_writes_are_coalesced_within_a_minute() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = owner(&store).await;
    let t = token(user, "ryx_used");
    store.tokens.create(&t).await.unwrap();
    let read = || async {
        store
            .tokens
            .find_by_hash(&t.token_hash)
            .await
            .unwrap()
            .unwrap()
            .last_used_at
    };

    let t0 = now();
    store.tokens.touch_last_used(t.id, t0).await.unwrap();
    assert_eq!(read().await, Some(t0));
    store
        .tokens
        .touch_last_used(t.id, t0 + TimeDuration::seconds(10))
        .await
        .unwrap();
    assert_eq!(read().await, Some(t0), "within a minute: no write");
    let t2 = t0 + TimeDuration::seconds(120);
    store.tokens.touch_last_used(t.id, t2).await.unwrap();
    assert_eq!(read().await, Some(t2));
    store.tokens.touch_last_used(t.id, t0).await.unwrap();
    assert_eq!(read().await, Some(t2), "never backwards");
    store
        .tokens
        .touch_last_used(ApiTokenId::generate(), t0)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_owner_removes_the_tokens() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let user = owner(&store).await;
    let t = token(user, "ryx_gone");
    store.tokens.create(&t).await.unwrap();
    store.users.delete(user).await.unwrap();
    assert!(
        store
            .tokens
            .find_by_hash(&t.token_hash)
            .await
            .unwrap()
            .is_none()
    );
}
