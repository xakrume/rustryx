//! Notification subscriptions and the ten minute suppression of a repeated mail.

use std::time::Duration;

use rustryx_core::events::EventKind;
use rustryx_core::ids::{EventId, ProjectId, SubscriptionId};
use rustryx_core::notification::{EmailStatus, NotificationSubscription, Recipient};
use rustryx_core::ports::{NewEmail, NotificationRepo};
use rustryx_core::project::ProjectRole;
use rustryx_core::webhook::NotifyScope;
use time::OffsetDateTime;

use crate::fixtures::unique;
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_matches_its_scope_and_a_repeat_mail_is_suppressed() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let notes = rustryx_db::repos::PgNotificationRepo::new(pool.clone());
    let project = ProjectId::generate();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(project.as_uuid())
        .bind(unique("notes"))
        .execute(&pool)
        .await
        .unwrap();

    let mut bad = NotificationSubscription::new(
        SubscriptionId::generate(),
        NotifyScope::System,
        vec![EventKind::Push],
        vec![Recipient::ProjectRole {
            role: ProjectRole::Maintainer,
        }],
    );
    assert!(notes.upsert(&bad, None).await.is_err());
    bad.scope = NotifyScope::Project(project);
    bad.recipients = vec![Recipient::address("ops@example.test").unwrap()];
    notes.upsert(&bad, None).await.unwrap();
    assert!(notes.upsert(&bad, None).await.is_err());
    bad.enabled = false;
    notes.upsert(&bad, Some(1)).await.unwrap();

    let system = NotificationSubscription::new(
        SubscriptionId::generate(),
        NotifyScope::System,
        vec![EventKind::Push],
        vec![Recipient::address("all@example.test").unwrap()],
    );
    notes.upsert(&system, None).await.unwrap();
    let matched = notes
        .matching(EventKind::Push, Some(project), None)
        .await
        .unwrap();
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].id, system.id);

    let mail = NewEmail {
        subscription_id: Some(system.id),
        event_id: EventId::generate(),
        to: "all@example.test".to_owned(),
        kind: EventKind::Push,
        subject_key: "repo".to_owned(),
    };
    assert_eq!(
        notes
            .create_for_events(std::slice::from_ref(&mail), None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(notes.create_for_events(&[mail], None).await.unwrap(), 1);
    let claimed = notes
        .claim("worker", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.status, EmailStatus::Sending);
    assert!(
        notes
            .finish(claimed.id, "worker", EmailStatus::Sent, None, None)
            .await
            .unwrap()
    );
    assert_eq!(
        notes
            .count_sent_since(OffsetDateTime::UNIX_EPOCH)
            .await
            .unwrap(),
        1
    );
    let removed = notes
        .purge(OffsetDateTime::now_utc() + time::Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(removed, 2);
}
