use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::ports::{InvalidationBus, InvalidationEvent};
use rustryx_db::PgInvalidationBus;
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::TestDb;

const WAIT: Duration = Duration::from_secs(10);

async fn next(stream: &mut BoxStream<'static, InvalidationEvent>) -> InvalidationEvent {
    tokio::time::timeout(WAIT, stream.next())
        .await
        .expect("event in time")
        .expect("stream open")
}

/// Pid of the session that executed `LISTEN` in this test's database (the activity view is
/// cluster wide, and other tests listen too).
async fn listener_pid(pool: &PgPool) -> i32 {
    for _ in 0..50 {
        let pid: Option<i32> = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity WHERE query ILIKE 'listen%' AND pid <> pg_backend_pid() \
             AND datname = current_database() LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .unwrap();
        if let Some(pid) = pid {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no LISTEN session found");
}

#[tokio::test(flavor = "multi_thread")]
async fn events_round_trip_after_an_initial_reset() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let bus = PgInvalidationBus::new(pool, db.url.clone());
    let mut events = bus.subscribe();
    assert_eq!(
        next(&mut events).await,
        InvalidationEvent::Reset,
        "Reset comes first, after LISTEN is active"
    );

    let user = Uuid::now_v7();
    bus.publish(InvalidationEvent::Rbac).await.unwrap();
    assert_eq!(next(&mut events).await, InvalidationEvent::Rbac);
    bus.publish(InvalidationEvent::User(user)).await.unwrap();
    assert_eq!(next(&mut events).await, InvalidationEvent::User(user));
    bus.publish(InvalidationEvent::Reset).await.unwrap();
    assert_eq!(next(&mut events).await, InvalidationEvent::Reset);
}

#[tokio::test(flavor = "multi_thread")]
async fn every_subscriber_hears_every_event_including_the_publisher_node() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let bus = PgInvalidationBus::new(pool.clone(), db.url.clone());
    let other_node = PgInvalidationBus::new(pool, db.url.clone());
    let mut a = bus.subscribe();
    let mut b = other_node.subscribe();
    assert_eq!(next(&mut a).await, InvalidationEvent::Reset);
    assert_eq!(next(&mut b).await, InvalidationEvent::Reset);
    bus.publish(InvalidationEvent::Rbac).await.unwrap();
    assert_eq!(next(&mut a).await, InvalidationEvent::Rbac);
    assert_eq!(next(&mut b).await, InvalidationEvent::Rbac);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_listener_connection_yields_reset_first_after_resubscription() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let bus = PgInvalidationBus::new(pool.clone(), db.url.clone());
    let mut events = bus.subscribe();
    assert_eq!(next(&mut events).await, InvalidationEvent::Reset);

    let pid = listener_pid(&pool).await;
    sqlx::query("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();

    // Whatever was sent while the session was down is gone; the consumer is told so.
    assert_eq!(next(&mut events).await, InvalidationEvent::Reset);
    bus.publish(InvalidationEvent::Rbac).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        InvalidationEvent::Rbac,
        "listening again after the reset"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_payloads_reset_the_caches() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let bus = PgInvalidationBus::new(pool.clone(), db.url.clone());
    let mut events = bus.subscribe();
    assert_eq!(next(&mut events).await, InvalidationEvent::Reset);
    sqlx::query("SELECT pg_notify('rustryx_rbac', 'something-from-a-newer-release')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(next(&mut events).await, InvalidationEvent::Reset);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_that_is_down_keeps_the_stream_retrying_instead_of_ending() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let dead = rustryx_core::Secret::new("postgres://u:p@127.0.0.1:1/none");
    let bus = PgInvalidationBus::new(pool, dead);
    let mut events = bus.subscribe();
    let waited = tokio::time::timeout(Duration::from_millis(1500), events.next()).await;
    assert!(
        waited.is_err(),
        "no event and no end of stream while the listener cannot connect"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wave_2_events_round_trip_through_the_bus() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let bus = PgInvalidationBus::new(pool, db.url.clone());
    let mut events = bus.subscribe();
    assert_eq!(next(&mut events).await, InvalidationEvent::Reset);

    bus.publish(InvalidationEvent::Projects).await.unwrap();
    assert_eq!(next(&mut events).await, InvalidationEvent::Projects);
    bus.publish(InvalidationEvent::Config(42)).await.unwrap();
    assert_eq!(next(&mut events).await, InvalidationEvent::Config(42));
}
