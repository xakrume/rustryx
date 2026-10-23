use rustryx_core::ports::RevokedTokenRepo;
use time::Duration;
use uuid::Uuid;

use crate::fixtures::now;
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_id_is_revoked_and_revoking_twice_is_fine() {
    let db = TestDb::new().await;
    let tokens = db.store().await.revoked_tokens;
    let tid = Uuid::now_v7();
    assert!(!tokens.is_revoked(tid).await.unwrap());
    tokens
        .revoke(tid, now() + Duration::hours(1))
        .await
        .unwrap();
    tokens
        .revoke(tid, now() + Duration::hours(1))
        .await
        .unwrap();
    assert!(tokens.is_revoked(tid).await.unwrap());
    assert!(
        !tokens.is_revoked(Uuid::now_v7()).await.unwrap(),
        "only the revoked id"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_purge_a_revoked_id_before_its_expiry() {
    let db = TestDb::new().await;
    let tokens = db.store().await.revoked_tokens;
    let expiry = now() + Duration::hours(1);
    let live = Uuid::now_v7();
    let expired = Uuid::now_v7();
    let edge = Uuid::now_v7();
    tokens.revoke(live, expiry).await.unwrap();
    tokens
        .revoke(expired, now() - Duration::minutes(1))
        .await
        .unwrap();
    tokens
        .revoke(edge, expiry - Duration::hours(2) + Duration::hours(2))
        .await
        .unwrap();

    // Only the row whose expiry lies strictly before `now` goes.
    assert_eq!(tokens.purge_expired(now()).await.unwrap(), 1);
    assert!(!tokens.is_revoked(expired).await.unwrap());
    assert!(
        tokens.is_revoked(live).await.unwrap(),
        "a live token stays revoked"
    );
    assert!(tokens.is_revoked(edge).await.unwrap());

    // Exactly at the expiry the row is kept; one microsecond later it goes.
    assert_eq!(tokens.purge_expired(expiry).await.unwrap(), 0);
    assert!(tokens.is_revoked(live).await.unwrap());
    assert_eq!(
        tokens
            .purge_expired(expiry + Duration::microseconds(1))
            .await
            .unwrap(),
        2
    );
    assert!(!tokens.is_revoked(live).await.unwrap());
    assert_eq!(
        tokens
            .purge_expired(now() + Duration::days(1))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_shorten_the_expiry_of_an_already_revoked_id() {
    let db = TestDb::new().await;
    let tokens = db.store().await.revoked_tokens;
    let tid = Uuid::now_v7();
    let long = now() + Duration::hours(10);
    tokens.revoke(tid, long).await.unwrap();
    // A second revocation with an earlier expiry must not make the row purgeable early.
    tokens
        .revoke(tid, now() + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(
        tokens
            .purge_expired(now() + Duration::hours(1))
            .await
            .unwrap(),
        0
    );
    assert!(tokens.is_revoked(tid).await.unwrap());
    // A later one extends it.
    let longer = long + Duration::hours(5);
    tokens.revoke(tid, longer).await.unwrap();
    assert_eq!(
        tokens
            .purge_expired(long + Duration::hours(1))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_is_shared_by_every_store_handle() {
    // Two handles over one database stand for two nodes: the point of the table.
    let db = TestDb::new().await;
    let node_a = db.store().await.revoked_tokens;
    let node_b = db.store().await.revoked_tokens;
    let tid = Uuid::now_v7();
    node_a
        .revoke(tid, now() + Duration::hours(1))
        .await
        .unwrap();
    assert!(node_b.is_revoked(tid).await.unwrap());
}
