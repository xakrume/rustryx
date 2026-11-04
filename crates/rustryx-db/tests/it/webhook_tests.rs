//! Webhook rows: sealed secrets, the per-scope cap and delivery matching.

use rustryx_core::StoreError;
use rustryx_core::events::EventKind;
use rustryx_core::ids::WebhookId;
use rustryx_core::ports::WebhookRepo;
use rustryx_core::secret::Secret;
use rustryx_core::webhook::{NotifyScope, Webhook};
use time::OffsetDateTime;

use crate::fixtures::seed_repository;
use crate::support::{TestDb, test_sealer};

fn hook(scope: NotifyScope, name: &str) -> Webhook {
    Webhook::new(
        WebhookId::generate(),
        scope,
        name,
        "https://example.test/hook",
        Secret::new("signing-secret"),
        vec![EventKind::Push],
        OffsetDateTime::now_utc(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webhook_keeps_its_sealed_secret_and_a_scope_stops_at_twenty() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let project = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, 'hooks')")
        .bind(project)
        .execute(store.pool())
        .await
        .unwrap();
    let scope = NotifyScope::Project(project.into());
    let hooks = rustryx_db::repos::PgWebhookRepo::new(store.pool().clone(), test_sealer());
    let first = hook(scope, "ci");
    hooks.create(&first).await.unwrap();
    let stored = hooks.get(first.id).await.unwrap().unwrap();
    assert_eq!(stored.secret.expose(), "signing-secret");
    assert_ne!(
        sqlx::query_scalar::<_, String>("SELECT secret FROM webhooks WHERE id = $1")
            .bind(first.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap(),
        "signing-secret"
    );

    let mut edited = stored.clone();
    edited.url = "https://example.test/renamed".to_owned();
    edited.secret = Secret::new("replacement");
    hooks.update(&edited, 1).await.unwrap();
    assert_eq!(
        hooks.get(first.id).await.unwrap().unwrap().secret.expose(),
        "signing-secret"
    );
    let version = hooks
        .rotate_secret(first.id, &Secret::new("rotated"), 2)
        .await
        .unwrap();
    assert_eq!(version, 3);
    assert_eq!(
        hooks.get(first.id).await.unwrap().unwrap().secret.expose(),
        "rotated"
    );

    for index in 1..20 {
        hooks
            .create(&hook(scope, &format!("h{index}")))
            .await
            .unwrap();
    }
    let extra = hooks.create(&hook(scope, "one-too-many")).await.unwrap_err();
    assert!(matches!(extra, StoreError::Conflict(_)), "{extra:?}");

    let matched = hooks
        .matching(EventKind::Push, Some(project.into()), None)
        .await
        .unwrap();
    assert!(matched.iter().any(|hook| hook.id == first.id));
    assert!(!hooks.any_subscribes_to(EventKind::Delete).await.unwrap());

    hooks.record_outcome(first.id, false, 1).await.unwrap();
    assert!(
        hooks
            .get(first.id)
            .await
            .unwrap()
            .unwrap()
            .suspended_at
            .is_some()
    );
    hooks.delete(first.id).await.unwrap();
    assert!(hooks.get(first.id).await.unwrap().is_none());
    let missing = hooks.delete(first.id).await.unwrap_err();
    assert!(matches!(missing, StoreError::NotFound(_)));

    let (_bs, repo) = seed_repository(&store).await;
    let on_repo = hook(NotifyScope::Repository(repo.id), "repo");
    hooks.create(&on_repo).await.unwrap();
    let hits = hooks
        .matching(EventKind::Push, None, Some(repo.id))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
}
