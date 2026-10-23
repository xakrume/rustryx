use rustryx_core::audit::{AuditEvent, AuditKind};
use rustryx_core::ids::UserId;
use rustryx_core::ports::AuditSink;
use rustryx_db::maintenance::{
    AuditMaintenance, drop_expired_audit_partitions, ensure_audit_partitions, run_audit_maintenance,
};
use sqlx::Row;
use time::Duration;

use crate::fixtures::now;
use crate::support::{TestDb, sqlstate};

fn event(kind: AuditKind, at: time::OffsetDateTime) -> AuditEvent {
    let mut event = AuditEvent::new(kind, "alice", "role.update", "success");
    event.at = at;
    event.actor_id = Some(UserId::generate());
    event.client_ip = Some("203.0.113.5".to_owned());
    event.request_id = Some("req-1".to_owned());
    event.target = Some("nx-developer".to_owned());
    event.detail = serde_json::json!({ "changed": ["description"] });
    event
}

async fn partitions(store: &rustryx_db::PgStore) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
         WHERE i.inhparent = 'audit_events'::regclass ORDER BY 1",
    )
    .fetch_all(store.pool())
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_schema_starts_with_the_current_month_and_two_ahead() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let names = partitions(&store).await;
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.iter().all(|n| n.starts_with("audit_events_y")));
}

#[tokio::test(flavor = "multi_thread")]
async fn records_every_kind_with_all_fields() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let kinds = [
        AuditKind::LoginSuccess,
        AuditKind::LoginFailure,
        AuditKind::RbacChange,
        AuditKind::TokenChange,
        AuditKind::ConfigChange,
        AuditKind::ContentChange,
    ];
    for kind in kinds {
        store.audit.record(event(kind, now())).await.unwrap();
    }
    let stored: i64 = sqlx::query_scalar("SELECT count(DISTINCT kind) FROM audit_events")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(stored, 6);

    let row = sqlx::query(
        "SELECT kind, actor_name, client_ip, request_id, action, target, outcome, detail \
         FROM audit_events WHERE kind = 'rbac_change'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("kind"), "rbac_change");
    assert_eq!(row.get::<String, _>("actor_name"), "alice");
    assert_eq!(
        row.get::<Option<String>, _>("client_ip").as_deref(),
        Some("203.0.113.5")
    );
    assert_eq!(
        row.get::<Option<String>, _>("request_id").as_deref(),
        Some("req-1")
    );
    assert_eq!(row.get::<String, _>("action"), "role.update");
    assert_eq!(
        row.get::<Option<String>, _>("target").as_deref(),
        Some("nx-developer")
    );
    assert_eq!(row.get::<String, _>("outcome"), "success");
    assert_eq!(
        row.get::<serde_json::Value, _>("detail"),
        serde_json::json!({ "changed": ["description"] })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn detail_that_is_not_an_object_is_wrapped() {
    let db = TestDb::new().await;
    let store = db.store().await;
    for (detail, expected) in [
        (serde_json::Value::Null, serde_json::json!({})),
        (
            serde_json::json!("text"),
            serde_json::json!({ "value": "text" }),
        ),
        (
            serde_json::json!([1, 2]),
            serde_json::json!({ "value": [1, 2] }),
        ),
    ] {
        let mut e = event(AuditKind::ConfigChange, now());
        e.detail = detail;
        e.actor_id = None;
        e.client_ip = None;
        e.request_id = None;
        e.target = None;
        store.audit.record(e).await.unwrap();
        let stored: serde_json::Value = sqlx::query_scalar(
            "SELECT detail FROM audit_events ORDER BY event_time DESC, id DESC LIMIT 1",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(stored, expected);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ensure_partitions_is_idempotent_and_creates_the_lookahead() {
    let db = TestDb::new().await;
    let store = db.store().await;
    assert_eq!(
        ensure_audit_partitions(store.pool(), now(), 2)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        ensure_audit_partitions(store.pool(), now(), 5)
            .await
            .unwrap(),
        3
    );
    assert_eq!(partitions(&store).await.len(), 6);
    assert!(
        ensure_audit_partitions(store.pool(), now(), 99)
            .await
            .is_err(),
        "lookahead is bounded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_partition_is_created_on_demand_for_a_late_event() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let far = now() + Duration::days(400);
    store
        .audit
        .record(event(AuditKind::ContentChange, far))
        .await
        .unwrap();
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE event_time = $1")
        .bind(far)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(stored, 1);
    assert_eq!(partitions(&store).await.len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_drops_only_whole_months_older_than_the_limit() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let old = now() - Duration::days(200);
    let recent = now() - Duration::days(20);
    store
        .audit
        .record(event(AuditKind::LoginFailure, old))
        .await
        .unwrap();
    store
        .audit
        .record(event(AuditKind::LoginSuccess, recent))
        .await
        .unwrap();
    store
        .audit
        .record(event(AuditKind::LoginSuccess, now()))
        .await
        .unwrap();
    let before = partitions(&store).await.len();

    let dropped = drop_expired_audit_partitions(store.pool(), now(), 90)
        .await
        .unwrap();
    assert_eq!(dropped, 1);
    assert_eq!(partitions(&store).await.len(), before - 1);
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(
        remaining, 2,
        "the event inside the retention window is kept"
    );
    assert_eq!(
        drop_expired_audit_partitions(store.pool(), now(), 90)
            .await
            .unwrap(),
        0
    );
    assert!(
        drop_expired_audit_partitions(store.pool(), now(), 0)
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_pass_creates_first_then_applies_retention() {
    let db = TestDb::new().await;
    let store = db.store().await;
    store
        .audit
        .record(event(AuditKind::LoginFailure, now() - Duration::days(300)))
        .await
        .unwrap();
    let report = run_audit_maintenance(
        store.pool(),
        AuditMaintenance {
            lookahead_months: 4,
            retention_days: 90,
        },
    )
    .await
    .unwrap();
    assert_eq!(report.created, 2);
    assert_eq!(report.dropped, 1);
    let defaults = AuditMaintenance::default();
    assert_eq!(
        (defaults.lookahead_months, defaults.retention_days),
        (2, 90)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_drop_a_partition_that_was_attached_by_hand() {
    let db = TestDb::new().await;
    let store = db.store().await;
    sqlx::query(
        "CREATE TABLE audit_events_legal_hold PARTITION OF audit_events \
         FOR VALUES FROM ('2019-01-01 00:00:00+00') TO ('2019-02-01 00:00:00+00')",
    )
    .execute(store.pool())
    .await
    .unwrap();
    let dropped = drop_expired_audit_partitions(store.pool(), now(), 1)
        .await
        .unwrap();
    assert_eq!(dropped, 0, "only generated names are candidates");
    assert!(
        partitions(&store)
            .await
            .contains(&"audit_events_legal_hold".to_owned())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_update_delete_or_truncate_audit_events() {
    let db = TestDb::new().await;
    let store = db.store().await;
    store
        .audit
        .record(event(AuditKind::LoginSuccess, now()))
        .await
        .unwrap();

    let err = sqlx::query("UPDATE audit_events SET outcome = 'forged'")
        .execute(store.pool())
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&err), "RX003");
    let err = sqlx::query("DELETE FROM audit_events")
        .execute(store.pool())
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&err), "RX003");
    let err = sqlx::query("TRUNCATE audit_events")
        .execute(store.pool())
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&err), "RX003");
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM audit_events")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(outcome, "success");
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_partition_maintenance_is_safe() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..6 {
        let pool = store.pool().clone();
        tasks.spawn(
            async move { ensure_audit_partitions(&pool, now() + Duration::days(200), 3).await },
        );
    }
    let mut created = 0;
    while let Some(joined) = tasks.join_next().await {
        created += joined.unwrap().unwrap();
    }
    assert!((1..=4).contains(&created), "{created}");
    let names = partitions(&store).await;
    let unique: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(names.len(), unique.len());
}
