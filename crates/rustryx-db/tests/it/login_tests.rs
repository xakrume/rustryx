use std::collections::HashSet;
use std::net::IpAddr;

use rustryx_core::ports::{LoginAttemptRepo, LoginKey};
use time::Duration;
use tokio::task::JoinSet;

use crate::fixtures::now;
use crate::support::TestDb;

fn key(account: &str, ip: Option<&str>) -> LoginKey {
    LoginKey {
        account: account.to_owned(),
        ip: ip.map(|s| s.parse::<IpAddr>().unwrap()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn counts_failures_per_key_and_resets() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let account = key("alice", None);
    let from_ip = key("alice", Some("203.0.113.7"));
    let v6 = key("alice", Some("2001:db8::1"));

    let empty = store.login_attempts.state(&account).await.unwrap();
    assert_eq!(empty.failures, 0);
    assert!(empty.last_failure_at.is_none());

    let t = now();
    for expected in 1..=3_u32 {
        let state = store
            .login_attempts
            .register_failure(&account, t)
            .await
            .unwrap();
        assert_eq!(state.failures, expected);
    }
    assert_eq!(
        store
            .login_attempts
            .register_failure(&from_ip, t)
            .await
            .unwrap()
            .failures,
        1
    );
    assert_eq!(
        store
            .login_attempts
            .register_failure(&v6, t)
            .await
            .unwrap()
            .failures,
        1
    );
    assert_eq!(
        store.login_attempts.state(&account).await.unwrap().failures,
        3
    );
    assert_eq!(
        store
            .login_attempts
            .state(&account)
            .await
            .unwrap()
            .last_failure_at,
        Some(t)
    );

    store.login_attempts.reset(&account).await.unwrap();
    assert_eq!(
        store.login_attempts.state(&account).await.unwrap().failures,
        0
    );
    assert_eq!(
        store.login_attempts.state(&from_ip).await.unwrap().failures,
        1,
        "the (account, ip) counter is separate"
    );
    store.login_attempts.reset(&account).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn last_failure_time_never_moves_backwards() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let k = key("bob", None);
    let late = now();
    store
        .login_attempts
        .register_failure(&k, late)
        .await
        .unwrap();
    let state = store
        .login_attempts
        .register_failure(&k, late - Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(state.last_failure_at, Some(late));
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_failures_are_counted_exactly_once_each() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let k = key("victim", Some("198.51.100.9"));
    let mut tasks = JoinSet::new();
    for _ in 0..60 {
        let repo = store.login_attempts.clone();
        let k = k.clone();
        tasks.spawn(async move { repo.register_failure(&k, now()).await.unwrap().failures });
    }
    let mut returned = HashSet::new();
    while let Some(count) = tasks.join_next().await {
        returned.insert(count.unwrap());
    }
    // A lost update would return the same count twice and end below 60.
    assert_eq!(returned, (1..=60_u32).collect::<HashSet<_>>());
    assert_eq!(store.login_attempts.state(&k).await.unwrap().failures, 60);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_counters_are_purged() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let old = key("old", None);
    let fresh = key("fresh", None);
    store
        .login_attempts
        .register_failure(&old, now() - Duration::days(3))
        .await
        .unwrap();
    store
        .login_attempts
        .register_failure(&fresh, now())
        .await
        .unwrap();
    let purged =
        rustryx_db::maintenance::purge_login_attempts(store.pool(), now() - Duration::days(1))
            .await
            .unwrap();
    assert_eq!(purged, 1);
    assert_eq!(store.login_attempts.state(&old).await.unwrap().failures, 0);
    assert_eq!(
        store.login_attempts.state(&fresh).await.unwrap().failures,
        1
    );
}
