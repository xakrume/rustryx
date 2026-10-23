//! The wake-up that rides with outbox events: delivered after commit, never for a rollback.

use std::time::Duration;

use rustryx_core::audit::ActorRef;
use rustryx_core::events::{EventDraft, EventKind};
use rustryx_core::model::{PublishMode, WriteOptions};
use rustryx_core::ports::AssetRepo;
use rustryx_db::outbox::OUTBOX_CHANNEL;
use sqlx::postgres::PgListener;

use crate::fixtures::{blob, new_asset, seed_repository};
use crate::support::TestDb;

async fn listen(db: &TestDb) -> PgListener {
    let mut listener = PgListener::connect(db.url.expose()).await.unwrap();
    listener.listen(OUTBOX_CHANNEL).await.unwrap();
    listener
}

async fn next(listener: &mut PgListener, wait: Duration) -> Option<String> {
    match tokio::time::timeout(wait, listener.recv()).await {
        Ok(Ok(n)) => Some(n.payload().to_owned()),
        _ => None,
    }
}

fn draft() -> EventDraft {
    EventDraft::new(
        EventKind::Push,
        ActorRef::system("test"),
        serde_json::json!({}),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn consumers_are_woken_once_per_committed_write_with_events() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let mut listener = listen(&db).await;

    // No events, no wake-up.
    store
        .assets
        .publish(&new_asset(repo.id, "quiet", blob(bs.id, 1)))
        .await
        .unwrap();
    assert_eq!(next(&mut listener, Duration::from_millis(400)).await, None);

    // Three events in one write are one wake-up with an empty payload.
    let options = WriteOptions::new().with_events(vec![draft(), draft(), draft()]);
    store
        .assets
        .publish_with(&new_asset(repo.id, "loud", blob(bs.id, 1)), &options)
        .await
        .unwrap();
    assert_eq!(
        next(&mut listener, Duration::from_secs(5)).await.as_deref(),
        Some("")
    );
    assert_eq!(next(&mut listener, Duration::from_millis(400)).await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_wake_consumers_for_a_write_that_was_refused() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    store
        .assets
        .publish(&new_asset(repo.id, "taken", blob(bs.id, 1)))
        .await
        .unwrap();
    let mut listener = listen(&db).await;

    let options = WriteOptions::new()
        .with_mode(PublishMode::CreateOnly)
        .with_event(draft());
    let refused = store
        .assets
        .publish_with(&new_asset(repo.id, "taken", blob(bs.id, 2)), &options)
        .await;
    assert!(refused.is_err());
    assert_eq!(
        next(&mut listener, Duration::from_millis(600)).await,
        None,
        "the notification is queued in the transaction and dies with it"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
