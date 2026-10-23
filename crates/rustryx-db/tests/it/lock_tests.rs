use std::time::Duration;

use rustryx_core::Secret;
use rustryx_core::StoreError;
use rustryx_core::ports::LockBackend;
use rustryx_db::PgLockBackend;
use sqlx::PgPool;

use crate::fixtures::unique;
use crate::support::TestDb;

const TTL: Duration = Duration::from_secs(30);

fn backend(db: &TestDb) -> PgLockBackend {
    PgLockBackend::new(&db.url).unwrap()
}

/// Backend pid that holds an advisory lock, found the way an operator would. `pg_locks` is
/// cluster wide while every test owns one database of a shared server, so the lookup is
/// limited to this database; without that it can pick (and kill) another test's session.
async fn holder_pid(pool: &PgPool) -> i32 {
    sqlx::query_scalar(
        "SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND granted \
         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_lock_excludes_every_other_taker_until_released() {
    let db = TestDb::new().await;
    let first = backend(&db);
    let second = backend(&db);
    let key = unique("job");

    let guard = first
        .try_acquire(&key, TTL)
        .await
        .unwrap()
        .expect("first taker wins");
    assert!(
        second.try_acquire(&key, TTL).await.unwrap().is_none(),
        "second connection is excluded"
    );
    assert!(
        first.try_acquire(&key, TTL).await.unwrap().is_none(),
        "even the same backend is excluded"
    );
    let other = second.try_acquire(&unique("other-job"), TTL).await.unwrap();
    assert!(other.is_some(), "different keys do not interfere");

    guard.release().await.unwrap();
    let again = second.try_acquire(&key, TTL).await.unwrap();
    assert!(again.is_some(), "free after release");
}

#[tokio::test(flavor = "multi_thread")]
async fn renew_confirms_the_lock_and_extends_the_ttl() {
    let db = TestDb::new().await;
    let lock = backend(&db);
    let mut guard = lock
        .try_acquire(&unique("job"), Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(guard.renew().await.unwrap(), "renewed in time");
        assert!(!guard.is_lost());
    }
    // 2.5 s have passed, more than the original TTL, and the lock is still ours.
    guard.release().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn loss_of_the_connection_is_detected_by_renew() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let lock = backend(&db);
    let key = unique("job");
    let mut guard = lock.try_acquire(&key, TTL).await.unwrap().unwrap();
    assert!(guard.renew().await.unwrap());

    // The node loses its session (failover, network reset, an operator's kill).
    let pid = holder_pid(&pool).await;
    sqlx::query("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();

    assert!(!guard.renew().await.unwrap(), "renew reports the loss");
    assert!(guard.is_lost());
    assert!(!guard.renew().await.unwrap(), "and keeps reporting it");
    let taken = lock.try_acquire(&key, TTL).await.unwrap();
    assert!(
        taken.is_some(),
        "the server released the lock with the session"
    );
    guard.release().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_holder_that_misses_its_renewals_loses_the_lock_like_a_expired_ttl() {
    let db = TestDb::new().await;
    let lock = backend(&db);
    let key = unique("job");
    let mut guard = lock
        .try_acquire(&key, Duration::from_millis(300))
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(guard.is_lost(), "the deadline passed without a renewal");
    assert!(!guard.renew().await.unwrap());
    // Giving the lock up on expiry is what lets another node take over.
    let taken = lock.try_acquire(&key, TTL).await.unwrap();
    assert!(taken.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_guard_without_release_frees_the_lock() {
    let db = TestDb::new().await;
    let lock = backend(&db);
    let key = unique("job");
    let guard = lock.try_acquire(&key, TTL).await.unwrap().unwrap();
    drop(guard);
    let mut freed = None;
    for _ in 0..50 {
        if let Some(g) = lock.try_acquire(&key, TTL).await.unwrap() {
            freed = Some(g);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(freed.is_some(), "the session ended with the guard");
}

#[tokio::test(flavor = "multi_thread")]
async fn many_contenders_for_one_key_produce_exactly_one_holder() {
    let db = TestDb::new().await;
    let lock = backend(&db);
    let key = unique("singleton");
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let lock = lock.clone();
        let key = key.clone();
        tasks.spawn(async move { lock.try_acquire(&key, TTL).await.unwrap() });
    }
    let mut holders = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        if let Some(guard) = joined.unwrap() {
            holders.push(guard);
        }
    }
    assert_eq!(holders.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_ttl_and_unreachable_servers_are_reported() {
    let db = TestDb::new().await;
    let lock = backend(&db);
    let err = lock.try_acquire("k", Duration::ZERO).await.err().unwrap();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");

    let unreachable = PgLockBackend::new(&Secret::new("postgres://u:p@127.0.0.1:1/none")).unwrap();
    let err = unreachable.try_acquire("k", TTL).await.err().unwrap();
    assert!(matches!(err, StoreError::Unavailable(_)), "{err:?}");
    assert!(PgLockBackend::new(&Secret::new("not a url")).is_err());
}
