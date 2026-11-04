//! Webhook deliveries: one row per event, a claim, and a purge.

use std::time::Duration;

use rustryx_core::events::EventKind;
use rustryx_core::ids::{EventId, ProjectId, WebhookId};
use rustryx_core::ports::{DeliveryOutcome, WebhookDeliveryRepo, WebhookRepo};
use rustryx_core::secret::Secret;
use rustryx_core::webhook::{DeliveryStatus, NotifyScope, Webhook};
use time::OffsetDateTime;

use crate::fixtures::unique;
use crate::support::{TestDb, test_sealer};

#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_is_created_once_claimed_and_purged() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let project = ProjectId::generate();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(project.as_uuid())
        .bind(unique("hooks"))
        .execute(&pool)
        .await
        .unwrap();
    let hooks = rustryx_db::repos::PgWebhookRepo::new(pool.clone(), test_sealer());
    let hook = Webhook::new(
        WebhookId::generate(),
        NotifyScope::Project(project),
        "ci",
        "https://example.test/hook",
        Secret::new("signing-secret"),
        vec![EventKind::Push],
        OffsetDateTime::now_utc(),
    );
    hooks.create(&hook).await.unwrap();
    let deliveries = rustryx_db::repos::PgWebhookDeliveryRepo::new(pool);
    let event = EventId::generate();
    let rows = [(hook.id, event, EventKind::Push)];
    assert_eq!(deliveries.create_for_events(&rows, None).await.unwrap(), 1);
    assert_eq!(deliveries.create_for_events(&rows, None).await.unwrap(), 0);
    let missing = [(WebhookId::generate(), EventId::generate(), EventKind::Push)];
    assert_eq!(deliveries.create_for_events(&missing, None).await.unwrap(), 0);
    assert_eq!(deliveries.pending_count().await.unwrap(), 1);

    let claimed = deliveries
        .claim("worker", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.attempt, 1);
    assert_eq!(claimed.status, DeliveryStatus::Delivering);
    let outcome = DeliveryOutcome {
        status: DeliveryStatus::Succeeded,
        status_code: Some(204),
        latency_ms: Some(12),
        error: None,
        excerpt: Some("ok".to_owned()),
        next_attempt_at: None,
    };
    assert!(deliveries.finish(claimed.id, "worker", &outcome).await.unwrap());
    assert!(!deliveries.finish(claimed.id, "worker", &outcome).await.unwrap());
    assert_eq!(deliveries.pending_count().await.unwrap(), 0);

    let removed = deliveries
        .purge(OffsetDateTime::now_utc() + time::Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(removed, 1);
}
