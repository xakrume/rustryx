//! The outbox, durable job queues, revoked tokens, routing rules, the audit read side, the
//! settings store, task definitions and runs, the node registry and the trust store.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    clippy::assert_is_empty,
    reason = "each test is one scenario read top to bottom; filters and non exhaustive types are \
              filled field by field after `default()`, and `.is_empty()` reads better than a \
              comparison with an empty literal of an inferred type"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Ports, T0, env, hosted, page};
use futures::StreamExt;
use rustryx_core::audit::{ActorKind, ActorRef, AuditEvent, AuditKind};
use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventDraft, EventKind, OutboxCursor};
use rustryx_core::ids::{
    ProjectId, RoutingRuleId, TaskDefinitionId, TaskRunId, TrustedCertificateId, UserId,
};
use rustryx_core::model::{ProxyConfig, RepoFormat, RepoType};
use rustryx_core::node::NodeInfo;
use rustryx_core::ports::{
    AuditQuery, AuditSink, InvalidationBus, InvalidationEvent, NewJob, PageRequest, SecretView,
};
use rustryx_core::registry::{Registry, RegistryProvider};
use rustryx_core::routing::{RoutingMode, RoutingRule};
use rustryx_core::settings::TrustedCertificate;
use rustryx_core::tasks::{
    NewTaskRun, Schedule, TaskDefinition, TaskLogLine, TaskNotify, TaskOutcome, TaskRunStatus,
    TaskTrigger,
};
use rustryx_testkit::{MemAuditSink, MemInvalidationBus};
use uuid::Uuid;

fn actor() -> ActorRef {
    ActorRef::system("test")
}

fn draft(kind: EventKind) -> EventDraft {
    EventDraft::new(kind, actor(), serde_json::json!({}))
}

// outbox

#[tokio::test]
async fn the_outbox_hands_batches_to_one_consumer_through_a_compare_and_set_cursor() {
    let env = env();
    let outbox = env.store.outbox();
    outbox
        .enqueue(&[draft(EventKind::Push), draft(EventKind::Delete)])
        .await
        .unwrap();
    outbox.enqueue(&[draft(EventKind::Pull)]).await.unwrap();
    outbox.enqueue(&[]).await.unwrap();
    let batch = outbox.read_batch("webhooks", 2).await.unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.from, OutboxCursor::default());
    assert!(batch.to > batch.from);
    let kinds: Vec<EventKind> = batch.events.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [EventKind::Push, EventKind::Delete]);
    assert_eq!(batch.events[0].actor.name, "test");
    assert!(batch.events[0].seq < batch.events[1].seq);

    let advance = CursorAdvance {
        consumer: "webhooks",
        from: batch.from,
        to: batch.to,
    };
    assert!(outbox.advance(&advance).await.unwrap());
    assert!(
        !outbox.advance(&advance).await.unwrap(),
        "another instance moved it first"
    );
    let next = outbox.read_batch("webhooks", 10).await.unwrap();
    assert_eq!(next.events.len(), 1);
    assert_eq!(next.from, batch.to);
    // Another consumer has its own cursor.
    assert_eq!(
        outbox.read_batch("email", 10).await.unwrap().events.len(),
        3
    );
    let empty = outbox.read_batch("webhooks", 0).await.unwrap();
    assert!(empty.events.is_empty());
    assert_eq!(empty.from, empty.to, "an empty batch carries from == to");

    let huge = EventDraft::new(
        EventKind::Push,
        actor(),
        serde_json::json!({"x": "y".repeat(20_000)}),
    );
    assert!(matches!(
        outbox.enqueue(&[draft(EventKind::Push), huge]).await,
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        outbox.read_batch("probe", 100).await.unwrap().events.len(),
        3,
        "nothing of the refused call is written"
    );
}

#[tokio::test]
async fn the_outbox_reports_lag_and_purges_only_what_every_cursor_has_passed() {
    let env = env();
    let outbox = env.store.outbox();
    assert_eq!(outbox.lag("webhooks").await.unwrap(), Duration::ZERO);
    outbox.enqueue(&[draft(EventKind::Push)]).await.unwrap();
    env.clock.advance(Duration::from_secs(90));
    outbox.enqueue(&[draft(EventKind::Delete)]).await.unwrap();
    assert_eq!(
        outbox.lag("webhooks").await.unwrap(),
        Duration::from_secs(90)
    );
    env.clock.set(T0 - time::Duration::hours(1));
    assert_eq!(
        outbox.lag("webhooks").await.unwrap(),
        Duration::ZERO,
        "a clock behind the event reads as no lag"
    );
    env.clock.set(T0 + time::Duration::hours(1));

    // No cursor yet: no consumer has started, so nothing may be dropped.
    assert_eq!(outbox.purge(env.clock_now()).await.unwrap(), 0);
    let batch = outbox.read_batch("webhooks", 1).await.unwrap();
    outbox
        .advance(&CursorAdvance {
            consumer: "webhooks",
            from: batch.from,
            to: batch.to,
        })
        .await
        .unwrap();
    let removed = outbox.purge(env.clock_now()).await.unwrap();
    assert_eq!(removed, 1, "only the handled event goes");
    assert_eq!(
        outbox.read_batch("fresh", 10).await.unwrap().events.len(),
        1
    );
    assert_eq!(
        outbox.purge(T0 - time::Duration::days(1)).await.unwrap(),
        0,
        "nothing is that old"
    );
}

// revoked tokens

#[tokio::test]
async fn revoked_token_ids_are_idempotent_and_purged_after_their_expiry() {
    let env = env();
    let revoked = env.store.revoked();
    let (soon, later) = (Uuid::now_v7(), Uuid::now_v7());
    assert!(!revoked.is_revoked(soon).await.unwrap());
    revoked
        .revoke(soon, T0 + time::Duration::minutes(5))
        .await
        .unwrap();
    revoked
        .revoke(soon, T0 + time::Duration::days(5))
        .await
        .unwrap();
    revoked
        .revoke(later, T0 + time::Duration::days(1))
        .await
        .unwrap();
    assert!(revoked.is_revoked(soon).await.unwrap());
    assert_eq!(
        revoked
            .purge_expired(T0 + time::Duration::minutes(5))
            .await
            .unwrap(),
        0,
        "a row lives until after its expiry"
    );
    assert_eq!(
        revoked
            .purge_expired(T0 + time::Duration::minutes(6))
            .await
            .unwrap(),
        1,
        "the first expiry stayed"
    );
    assert!(!revoked.is_revoked(soon).await.unwrap());
    assert!(revoked.is_revoked(later).await.unwrap());
}

// durable queues

#[tokio::test]
async fn a_job_is_claimed_once_leased_completed_and_deduplicated_while_active() {
    let env = env();
    let jobs = env.store.jobs();
    let payload = serde_json::json!({"component": "c1"});
    assert!(
        jobs.enqueue("scan", Some("scan:c1"), &payload)
            .await
            .unwrap()
    );
    assert!(
        !jobs
            .enqueue("scan", Some("scan:c1"), &payload)
            .await
            .unwrap(),
        "queued twice"
    );
    assert!(jobs.enqueue("scan", None, &payload).await.unwrap());
    assert!(
        jobs.enqueue("scan", None, &payload).await.unwrap(),
        "no key, no dedupe"
    );
    assert!(
        jobs.enqueue("other", Some("scan:c1"), &payload)
            .await
            .unwrap(),
        "the key is per queue"
    );
    assert_eq!(jobs.depth("scan").await.unwrap(), 3);

    let lease = Duration::from_secs(60);
    let first = jobs.claim("scan", "w1", lease).await.unwrap().unwrap();
    assert_eq!((first.attempts, first.queue.as_str()), (1, "scan"));
    assert_eq!(first.payload, payload);
    assert_eq!(
        first.lease_expires_at,
        env.clock_now() + time::Duration::seconds(60)
    );
    assert!(
        !jobs
            .enqueue("scan", Some("scan:c1"), &payload)
            .await
            .unwrap(),
        "running counts as active"
    );
    assert!(jobs.heartbeat(first.id, "w1", lease).await.unwrap());
    assert!(!jobs.heartbeat(first.id, "intruder", lease).await.unwrap());
    assert!(!jobs.complete(first.id, "intruder").await.unwrap());
    assert!(jobs.complete(first.id, "w1").await.unwrap());
    assert!(
        !jobs.complete(first.id, "w1").await.unwrap(),
        "the lease is gone once done"
    );
    assert!(
        jobs.enqueue("scan", Some("scan:c1"), &payload)
            .await
            .unwrap(),
        "a done row does not block"
    );
    assert_eq!(jobs.depth("scan").await.unwrap(), 3);
    assert!(jobs.claim("empty", "w1", lease).await.unwrap().is_none());
}

#[tokio::test]
async fn a_job_whose_lease_ran_out_is_taken_over_and_the_old_owner_loses_it() {
    let env = env();
    let jobs = env.store.jobs();
    jobs.enqueue("q", None, &serde_json::json!({}))
        .await
        .unwrap();
    let lease = Duration::from_secs(30);
    let first = jobs.claim("q", "dead", lease).await.unwrap().unwrap();
    assert!(jobs.claim("q", "other", lease).await.unwrap().is_none());
    env.clock.advance(Duration::from_secs(31));
    let taken = jobs.claim("q", "other", lease).await.unwrap().unwrap();
    assert_eq!((taken.id, taken.attempts), (first.id, 2));
    assert!(!jobs.heartbeat(first.id, "dead", lease).await.unwrap());
    assert!(!jobs.fail(first.id, "dead", "late").await.unwrap());
    assert!(
        !jobs
            .release_for_retry(first.id, "dead", lease, "late")
            .await
            .unwrap()
    );
    assert!(jobs.heartbeat(taken.id, "other", lease).await.unwrap());
}

#[tokio::test]
async fn a_job_is_retried_after_a_delay_or_fails_for_good_and_old_rows_are_purged() {
    let env = env();
    let jobs = env.store.jobs();
    jobs.enqueue("q", Some("a"), &serde_json::json!({}))
        .await
        .unwrap();
    let lease = Duration::from_secs(30);
    let job = jobs.claim("q", "w", lease).await.unwrap().unwrap();
    assert!(
        jobs.release_for_retry(job.id, "w", Duration::from_secs(120), &"e".repeat(900))
            .await
            .unwrap()
    );
    assert!(
        jobs.claim("q", "w", lease).await.unwrap().is_none(),
        "not due yet"
    );
    env.clock.advance(Duration::from_secs(121));
    let again = jobs.claim("q", "w", lease).await.unwrap().unwrap();
    assert_eq!((again.id, again.attempts), (job.id, 2));
    assert!(jobs.fail(again.id, "w", "gave up").await.unwrap());
    assert_eq!(jobs.depth("q").await.unwrap(), 0);
    assert!(
        jobs.enqueue("q", Some("a"), &serde_json::json!({}))
            .await
            .unwrap(),
        "a failed row does not block either"
    );

    assert!(
        jobs.enqueue_after("q", None, &serde_json::json!({}), Duration::from_secs(600))
            .await
            .unwrap()
    );
    assert!(jobs.claim("q", "w", lease).await.unwrap().is_some());
    assert!(
        jobs.claim("q", "w", lease).await.unwrap().is_none(),
        "the delayed one is not available yet"
    );
    env.clock.advance(Duration::from_secs(3600));
    let finished_at = env.clock_now();
    let last = jobs.claim("q", "w", lease).await.unwrap().unwrap();
    assert!(jobs.complete(last.id, "w").await.unwrap());
    assert_eq!(
        jobs.purge_finished(finished_at - time::Duration::hours(2))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        jobs.purge_finished(env.clock_now() + time::Duration::seconds(1))
            .await
            .unwrap(),
        2,
        "done and failed rows"
    );
}

#[tokio::test]
async fn a_batch_of_jobs_and_its_cursor_move_together_or_not_at_all() {
    let env = env();
    let jobs = env.store.jobs();
    let job = |key: &str| NewJob {
        queue: "events".to_owned(),
        dedupe_key: Some(key.to_owned()),
        payload: serde_json::json!({}),
        delay: Duration::from_secs(5),
    };
    let advance = CursorAdvance {
        consumer: "dispatcher",
        from: OutboxCursor::default(),
        to: OutboxCursor { tx_id: 4, seq: 9 },
    };
    let inserted = jobs
        .enqueue_batch(&[job("a"), job("a"), job("b")], Some(&advance))
        .await
        .unwrap();
    assert_eq!(
        inserted, 2,
        "dedupe keys behave as in enqueue, inside the batch too"
    );
    let again = jobs
        .enqueue_batch(&[job("c")], Some(&advance))
        .await
        .unwrap();
    assert_eq!(again, 0, "the cursor moved meanwhile: nothing is inserted");
    assert_eq!(jobs.depth("events").await.unwrap(), 2);
    assert!(
        jobs.claim("events", "w", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none(),
        "the delay applies"
    );
    env.clock.advance(Duration::from_secs(6));
    assert!(
        jobs.claim("events", "w", Duration::from_secs(60))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(jobs.enqueue_batch(&[job("d")], None).await.unwrap(), 1);
    let cursor = env
        .store
        .outbox()
        .read_batch("dispatcher", 1)
        .await
        .unwrap();
    assert_eq!(cursor.from, OutboxCursor { tx_id: 4, seq: 9 });
}

// routing rules

fn routing(name: &str) -> RoutingRule {
    RoutingRule::new(
        RoutingRuleId::generate(),
        name,
        RoutingMode::Block,
        vec!["^com/acme/.*".to_owned()],
        T0,
    )
}

#[tokio::test]
async fn routing_rules_are_unique_versioned_and_protected_while_assigned() {
    let env = env();
    let rules = env.store.routing_rules();
    let first = routing("no-snapshots");
    rules.create(&first).await.unwrap();
    assert!(matches!(
        rules.create(&first).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        rules.create(&routing("no-snapshots")).await,
        Err(StoreError::Conflict(_))
    ));
    rules.create(&routing("a-first")).await.unwrap();
    let names: Vec<String> = rules
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, ["a-first", "no-snapshots"]);

    let mut edited = first.clone();
    edited.mode = RoutingMode::Allow;
    env.clock.advance(Duration::from_secs(5));
    rules.update(&edited, 1).await.unwrap();
    let stored = rules.get(first.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.version, stored.mode, stored.updated_at),
        (2, RoutingMode::Allow, env.clock_now())
    );
    assert_eq!(stored.created_at, first.created_at);
    assert!(matches!(
        rules.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut clash = edited.clone();
    clash.name = "a-first".to_owned();
    assert!(matches!(
        rules.update(&clash, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = edited.clone();
    ghost.id = RoutingRuleId::generate();
    assert!(matches!(
        rules.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));

    let mut proxy =
        rustryx_testkit::fixtures::repository("routed", RepoFormat::Npm, RepoType::Proxy);
    proxy.routing_rule_id = Some(first.id);
    env.store.repos().create(&proxy).await.unwrap();
    assert_eq!(rules.assigned_to(first.id).await.unwrap(), [proxy.id]);
    assert!(matches!(
        rules.delete(first.id).await,
        Err(StoreError::Conflict(_))
    ));
    env.store.repos().delete(proxy.id).await.unwrap();
    assert!(rules.assigned_to(first.id).await.unwrap().is_empty());
    rules.delete(first.id).await.unwrap();
    assert!(matches!(
        rules.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// the audit read side

fn audit_event(kind: AuditKind, name: &str, action: &str, at: time::OffsetDateTime) -> AuditEvent {
    let mut event = AuditEvent::new(kind, name, action, "success");
    event.at = at;
    event
}

#[tokio::test]
async fn the_audit_log_is_read_newest_first_filtered_and_exported_oldest_first() {
    let env = env();
    let created = ProjectId::generate();
    let log = env.store.audit_log();
    let sink = env.store.audit_sink();
    let user = UserId::generate();
    let mut first = audit_event(AuditKind::LoginSuccess, "alice", "login", T0);
    first.actor_id = Some(user);
    let mut second = audit_event(
        AuditKind::RbacChange,
        "bob",
        "role.update",
        T0 + time::Duration::minutes(1),
    );
    second.project_id = Some(created);
    second.target = Some("role:dev".to_owned());
    second.actor_kind = ActorKind::System;
    let mut third = audit_event(
        AuditKind::LoginFailure,
        "alice",
        "login",
        T0 + time::Duration::minutes(2),
    );
    third.outcome = "failure".to_owned();
    for event in [&first, &second, &third] {
        sink.record((*event).clone()).await.unwrap();
    }
    let everything = log.query(&AuditQuery::new(), &page(10)).await.unwrap();
    let order: Vec<&str> = everything
        .items
        .iter()
        .map(|e| e.event.action.as_str())
        .collect();
    assert_eq!(order, ["login", "role.update", "login"]);
    assert_eq!(
        everything.items[0].event.kind,
        AuditKind::LoginFailure,
        "newest first"
    );

    let select = |query: AuditQuery| {
        let log = env.store.audit_log();
        async move {
            log.query(&query, &page(10))
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|e| e.event.action)
                .collect::<Vec<_>>()
        }
    };
    let mut from_to = AuditQuery::new();
    from_to.from = Some(T0 + time::Duration::minutes(1));
    from_to.to = Some(T0 + time::Duration::minutes(2));
    assert_eq!(
        select(from_to).await,
        ["role.update"],
        "from is inclusive, to is exclusive"
    );
    let mut kinds = AuditQuery::new();
    kinds.kinds = vec![AuditKind::LoginSuccess, AuditKind::LoginFailure];
    assert_eq!(select(kinds).await.len(), 2);
    let mut by_actor = AuditQuery::new();
    by_actor.actor_id = Some(user);
    assert_eq!(select(by_actor).await, ["login"]);
    let mut by_name = AuditQuery::new();
    by_name.actor_name = Some("alice".to_owned());
    assert_eq!(select(by_name).await.len(), 2);
    let mut by_prefix = AuditQuery::new();
    by_prefix.action_prefix = Some("role.".to_owned());
    assert_eq!(select(by_prefix).await, ["role.update"]);
    let mut by_target = AuditQuery::new();
    by_target.target = Some("role:dev".to_owned());
    assert_eq!(select(by_target).await, ["role.update"]);
    let mut by_project = AuditQuery::new();
    by_project.project_id = Some(created);
    assert_eq!(select(by_project).await, ["role.update"]);
    let mut by_outcome = AuditQuery::new();
    by_outcome.outcome = Some("failure".to_owned());
    assert_eq!(select(by_outcome).await, ["login"]);

    let first_page = log.query(&AuditQuery::new(), &page(2)).await.unwrap();
    assert_eq!(first_page.items.len(), 2);
    let rest = log
        .query(
            &AuditQuery::new(),
            &PageRequest {
                limit: 2,
                after: first_page.next.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    assert!(rest.next.is_none());
    let bad = log
        .query(
            &AuditQuery::new(),
            &PageRequest {
                limit: 2,
                after: Some("junk".to_owned()),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(bad, StoreError::Invalid(_)));

    let exported: Vec<String> = log
        .export(&AuditQuery::new())
        .map(|row| row.unwrap().event.action)
        .collect()
        .await;
    assert_eq!(exported, ["login", "role.update", "login"]);
    let times: Vec<_> = log
        .export(&AuditQuery::new())
        .map(|row| row.unwrap().event.at)
        .collect()
        .await;
    assert!(
        times.windows(2).all(|pair| pair[0] <= pair[1]),
        "oldest first"
    );
}

#[tokio::test]
async fn the_sink_the_store_hands_out_and_a_standalone_sink_read_the_same_way() {
    let env = env();
    let shared = MemAuditSink::new();
    let store = env.store.clone().with_audit_sink(shared.clone());
    shared
        .record(audit_event(AuditKind::LoginSuccess, "a", "login", T0))
        .await
        .unwrap();
    store
        .audit_sink()
        .record(audit_event(
            AuditKind::LoginSuccess,
            "b",
            "login",
            T0 + time::Duration::seconds(1),
        ))
        .await
        .unwrap();
    // The store is an `AuditSink` itself, over the same log.
    let as_sink: Arc<dyn AuditSink> = Arc::new(store.clone());
    as_sink
        .record(audit_event(
            AuditKind::LoginSuccess,
            "c",
            "login",
            T0 + time::Duration::seconds(2),
        ))
        .await
        .unwrap();
    assert_eq!(shared.events().len(), 3);
    let read = store
        .audit_log()
        .query(&AuditQuery::new(), &page(10))
        .await
        .unwrap();
    assert_eq!(read.items.len(), 3);
    // `MemAuditSink` implements the read port by itself too.
    let direct = rustryx_core::ports::AuditReadRepo::query(&shared, &AuditQuery::new(), &page(10))
        .await
        .unwrap();
    assert_eq!(direct.items.len(), 3);
    let streamed = rustryx_core::ports::AuditReadRepo::export(&shared, &AuditQuery::new())
        .count()
        .await;
    assert_eq!(streamed, 3);
    store.set_unavailable(true);
    let down = as_sink
        .record(audit_event(AuditKind::LoginSuccess, "d", "login", T0))
        .await;
    assert!(matches!(down, Err(StoreError::Unavailable(_))));
    assert!(matches!(
        store.audit_log().query(&AuditQuery::new(), &page(1)).await,
        Err(StoreError::Unavailable(_))
    ));
    let failed: Vec<_> = store.audit_log().export(&AuditQuery::new()).collect().await;
    assert!(matches!(
        failed.as_slice(),
        [Err(StoreError::Unavailable(_))]
    ));
}

// settings

#[tokio::test]
async fn settings_are_written_with_a_revision_and_sealed_fields_never_leave_in_a_masked_read() {
    let env = env();
    let bus = MemInvalidationBus::new();
    let store = env.store.clone().with_invalidation_bus(bus.clone());
    let mut events = bus.subscribe();
    let config = store.config();
    assert_eq!(config.current_revision().await.unwrap(), 0);
    let value =
        serde_json::json!({"host": "smtp.example.test", "port": 587, "password": "hunter2"});
    let written = config
        .put(
            "settings.smtp",
            &value,
            &["/password"],
            None,
            Some(UserId::generate()),
        )
        .await
        .unwrap();
    assert_eq!(written.revision, 1);
    assert_eq!(
        written.value["password"],
        serde_json::Value::Null,
        "the masked entry has no password"
    );
    assert_eq!(written.value["_sealed"], serde_json::json!(["/password"]));
    assert_eq!(events.next().await, Some(InvalidationEvent::Config(1)));

    let masked = config
        .get("settings.smtp", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert!(masked.value.get("password").is_none());
    assert!(!format!("{masked:?}").contains("hunter2"));
    let revealed = config
        .get("settings.smtp", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.value["password"], "hunter2");
    assert!(revealed.value.get("_sealed").is_none());

    // A sealed field that is absent keeps its value, a null clears it.
    let without = serde_json::json!({"host": "smtp2.example.test", "port": 25});
    config
        .put("settings.smtp", &without, &["/password"], Some(1), None)
        .await
        .unwrap();
    let kept = config
        .get("settings.smtp", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (kept.value["host"].clone(), kept.value["password"].clone()),
        (
            serde_json::json!("smtp2.example.test"),
            serde_json::json!("hunter2")
        )
    );
    let cleared = serde_json::json!({"host": "h", "password": null});
    config
        .put("settings.smtp", &cleared, &["/password"], None, None)
        .await
        .unwrap();
    let after = config
        .get("settings.smtp", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert!(after.value.get("_sealed").is_none());
    assert!(
        config
            .get("settings.smtp", SecretView::Reveal)
            .await
            .unwrap()
            .unwrap()
            .value
            .get("password")
            .is_none()
    );
}

#[tokio::test]
async fn settings_writes_check_the_revision_the_key_and_the_sealed_field_type() {
    let env = env();
    let config = env.store.config();
    let value = serde_json::json!({"a": 1});
    config
        .put("settings.one", &value, &[], None, None)
        .await
        .unwrap();
    assert!(matches!(
        config
            .put("settings.one", &value, &[], Some(99), None)
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        config
            .put("settings.fresh", &value, &[], Some(1), None)
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        config.put("", &value, &[], None, None).await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        config.put(&"k".repeat(201), &value, &[], None, None).await,
        Err(StoreError::Invalid(_))
    ));
    let number = serde_json::json!({"password": 5});
    assert!(matches!(
        config
            .put("settings.bad", &number, &["/password"], None, None)
            .await,
        Err(StoreError::Invalid(_))
    ));
    // Nested pointers create the objects on the way and are removed from the stored value.
    let nested = serde_json::json!({"auth": {"token": "t0k3n", "user": "u"}});
    config
        .put("settings.nested", &nested, &["/auth/token"], None, None)
        .await
        .unwrap();
    let masked = config
        .get("settings.nested", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(masked.value["auth"], serde_json::json!({"user": "u"}));
    let back = config
        .get("settings.nested", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.value["auth"]["token"], "t0k3n");
    let escaped = serde_json::json!({"a/b": {"c~d": "secret"}});
    config
        .put("settings.escaped", &escaped, &["/a~1b/c~0d"], None, None)
        .await
        .unwrap();
    let revealed = config
        .get("settings.escaped", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.value["a/b"]["c~d"], "secret");

    let listed = config
        .list_prefix("settings.", SecretView::Mask)
        .await
        .unwrap();
    assert_eq!(
        listed.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        ["settings.escaped", "settings.nested", "settings.one"]
    );
    assert!(
        config
            .list_prefix("system.", SecretView::Mask)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        config
            .get("settings.absent", SecretView::Mask)
            .await
            .unwrap()
            .is_none()
    );
    let revision = config.current_revision().await.unwrap();
    assert_eq!(revision, 3);
    config.delete("settings.one", None).await.unwrap();
    assert!(matches!(
        config.delete("settings.one", None).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        config.current_revision().await.unwrap(),
        3,
        "the highest remaining row"
    );
}

#[tokio::test]
async fn read_only_mode_is_the_system_read_only_row() {
    let env = env();
    let config = env.store.config();
    env.store.set_read_only(true, "backup in progress").unwrap();
    let row = config
        .get("system.read_only", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.value["enabled"], true);
    assert_eq!(row.value["reason"], "backup in progress");
    assert!(!row.value["set_at"].is_null());
    assert_eq!(config.current_revision().await.unwrap(), row.revision);
    env.store.set_read_only(false, "").unwrap();
    let off = config
        .get("system.read_only", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(off.value["enabled"], false);
    assert!(off.value["set_at"].is_null());
    // The same row written through the port has the same effect on the guards.
    let value = serde_json::json!({"enabled": true, "reason": "x", "set_by": null, "set_at": null});
    config
        .put("system.read_only", &value, &[], None, None)
        .await
        .unwrap();
    let repo = hosted(&env.store, "frozen").await;
    let refused = env
        .store
        .assets()
        .publish_with(
            &common::asset(repo.id, "f", b"x"),
            &rustryx_core::model::WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        refused,
        rustryx_core::MutationError::Violation(rustryx_core::PolicyViolation::ReadOnly)
    ));
    // A row that does not say `enabled: true` is off.
    config
        .put(
            "system.read_only",
            &serde_json::json!({"enabled": "yes"}),
            &[],
            None,
            None,
        )
        .await
        .unwrap();
    env.store
        .assets()
        .publish_with(
            &common::asset(repo.id, "f", b"x"),
            &rustryx_core::model::WriteOptions::new(),
        )
        .await
        .unwrap();
}

// task definitions

fn definition(name: &str) -> TaskDefinition {
    TaskDefinition::new(
        TaskDefinitionId::generate(),
        "blob_gc",
        name,
        Schedule::Every { secs: 3600 },
        T0,
    )
}

#[tokio::test]
async fn task_definitions_are_validated_versioned_and_system_tasks_stay() {
    let env = env();
    let tasks = env.store.task_definitions();
    let first = definition("nightly");
    tasks.create(&first).await.unwrap();
    assert!(matches!(
        tasks.create(&first).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        tasks.create(&definition("nightly")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut bad_kind = definition("bad");
    bad_kind.kind = "Bad Kind".to_owned();
    assert!(matches!(
        tasks.create(&bad_kind).await,
        Err(StoreError::Conflict(_))
    ));
    let mut other = definition("other");
    other.kind = "cleanup".to_owned();
    tasks.create(&other).await.unwrap();
    assert_eq!(tasks.list(None).await.unwrap().len(), 2);
    assert_eq!(tasks.list(Some("cleanup")).await.unwrap().len(), 1);
    assert_eq!(
        tasks.get(first.id).await.unwrap().map(|d| d.name),
        Some("nightly".to_owned())
    );

    let mut edited = first.clone();
    edited.name = "renamed".to_owned();
    edited.enabled = false;
    edited.schedule = Schedule::Manual;
    edited.notify = TaskNotify {
        on_failure: true,
        on_success: false,
    };
    edited.kind = "other_kind".to_owned();
    edited.next_run_at = Some(T0);
    edited.params = serde_json::json!({"x": 1});
    tasks.update(&edited, 1).await.unwrap();
    let stored = tasks.get(first.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.version, stored.name.as_str(), stored.enabled),
        (2, "renamed", false)
    );
    assert_eq!(
        (stored.kind.as_str(), stored.next_run_at),
        ("blob_gc", None),
        "kind and schedule progress are not editable"
    );
    assert_eq!(stored.schedule, Schedule::Manual);
    assert_eq!(stored.params, serde_json::json!({"x": 1}));
    assert!(matches!(
        tasks.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut clash = edited.clone();
    clash.name = "other".to_owned();
    assert!(matches!(
        tasks.update(&clash, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = edited.clone();
    ghost.id = TaskDefinitionId::generate();
    assert!(matches!(
        tasks.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));

    let mut system = definition("system-gc");
    system.system = true;
    tasks.create(&system).await.unwrap();
    let mut system_edit = system.clone();
    system_edit.name = "renamed-system".to_owned();
    system_edit.params = serde_json::json!({"x": 2});
    system_edit.enabled = false;
    tasks.update(&system_edit, 1).await.unwrap();
    let kept = tasks.get(system.id).await.unwrap().unwrap();
    assert_eq!(
        (kept.name.as_str(), kept.enabled),
        ("system-gc", false),
        "a system task keeps its name and params"
    );
    assert_eq!(kept.params, serde_json::json!({}));
    assert!(matches!(
        tasks.delete(system.id).await,
        Err(StoreError::Conflict(_))
    ));
    tasks.delete(first.id).await.unwrap();
    assert!(matches!(
        tasks.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn seeding_adds_only_the_missing_system_definitions_and_the_schedule_is_a_compare_and_set() {
    let env = env();
    let tasks = env.store.task_definitions();
    let mut system = definition("gc");
    system.system = true;
    let mut edited_by_operator = system.clone();
    edited_by_operator.schedule = Schedule::Every { secs: 60 };
    assert_eq!(tasks.seed(std::slice::from_ref(&system)).await.unwrap(), 1);
    assert_eq!(
        tasks
            .seed(std::slice::from_ref(&edited_by_operator))
            .await
            .unwrap(),
        0,
        "existing rows are left alone"
    );
    let mut broken = definition("broken");
    broken.kind = "NOPE".to_owned();
    assert!(matches!(
        tasks.seed(&[broken]).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(tasks.get(system.id).await.unwrap().is_some());

    let now = env.clock_now();
    assert!(
        tasks.due(now, 10).await.unwrap().is_empty(),
        "no next run yet"
    );
    assert!(tasks.advance(system.id, None, Some(now)).await.unwrap());
    assert!(
        !tasks.advance(system.id, None, Some(now)).await.unwrap(),
        "exactly one node wins a due time"
    );
    assert_eq!(tasks.due(now, 10).await.unwrap().len(), 1);
    assert!(
        tasks
            .due(now - time::Duration::seconds(1), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(tasks.due(now, 0).await.unwrap().len(), 0);
    let next = now + time::Duration::hours(1);
    assert!(
        tasks
            .advance(system.id, Some(now), Some(next))
            .await
            .unwrap()
    );
    assert!(tasks.due(now, 10).await.unwrap().is_empty());
    assert!(
        !tasks
            .advance(TaskDefinitionId::generate(), None, None)
            .await
            .unwrap(),
        "a deleted definition is a lost race"
    );
    let mut disabled = definition("off");
    disabled.enabled = false;
    disabled.next_run_at = Some(T0);
    tasks.create(&disabled).await.unwrap();
    assert!(
        tasks.due(env.clock_now(), 10).await.unwrap().is_empty(),
        "a disabled task is never due"
    );
}

// task runs

fn new_run(kind: &str, scope: &str) -> NewTaskRun {
    NewTaskRun {
        task_id: None,
        kind: kind.to_owned(),
        scope_key: scope.to_owned(),
        trigger: TaskTrigger::Manual,
        dry_run: false,
        params: serde_json::json!({"k": 1}),
    }
}

#[tokio::test]
async fn a_run_is_unique_per_kind_and_scope_while_active_and_claimed_oldest_first() {
    let env = env();
    let runs = env.store.task_runs();
    let first = runs
        .enqueue(&new_run("blob_gc", "store-a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.status, TaskRunStatus::Queued);
    assert_eq!(first.params, serde_json::json!({"k": 1}));
    assert!(
        runs.enqueue(&new_run("blob_gc", "store-a"))
            .await
            .unwrap()
            .is_none(),
        "the unique index"
    );
    env.clock.advance(Duration::from_secs(1));
    let second = runs
        .enqueue(&new_run("blob_gc", "store-b"))
        .await
        .unwrap()
        .unwrap();
    let other_kind = runs
        .enqueue(&new_run("cleanup", "store-a"))
        .await
        .unwrap()
        .unwrap();

    let lease = Duration::from_secs(60);
    assert!(
        runs.claim("node-1", &["nothing"], lease)
            .await
            .unwrap()
            .is_none()
    );
    let claimed = runs
        .claim("node-1", &["blob_gc"], lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (claimed.id, claimed.status),
        (first.id, TaskRunStatus::Running)
    );
    assert_eq!(
        (claimed.node_id.as_deref(), claimed.started_at),
        (Some("node-1"), Some(env.clock_now()))
    );
    assert!(
        runs.enqueue(&new_run("blob_gc", "store-a"))
            .await
            .unwrap()
            .is_none(),
        "running counts as active"
    );
    let next = runs
        .claim("node-2", &["blob_gc", "cleanup"], lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.id, second.id);
    let last = runs
        .claim("node-2", &["cleanup"], lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.id, other_kind.id);
    assert!(
        runs.claim("node-2", &["cleanup"], lease)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        runs.get(first.id).await.unwrap().map(|r| r.id),
        Some(first.id)
    );
    assert!(runs.get(TaskRunId::generate()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_run_handed_back_waits_its_delay_and_is_not_counted_as_an_attempt() {
    let env = env();
    let runs = env.store.task_runs();
    let queued = runs
        .enqueue(&new_run("blob_gc", "s"))
        .await
        .unwrap()
        .unwrap();
    let lease = Duration::from_secs(60);
    runs.claim("n1", &["blob_gc"], lease)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !runs
            .release(queued.id, "intruder", Duration::from_secs(30))
            .await
            .unwrap()
    );
    assert!(
        runs.release(queued.id, "n1", Duration::from_secs(30))
            .await
            .unwrap()
    );
    assert!(
        !runs
            .release(queued.id, "n1", Duration::from_secs(30))
            .await
            .unwrap()
    );
    assert!(
        runs.claim("n1", &["blob_gc"], lease)
            .await
            .unwrap()
            .is_none()
    );
    env.clock.advance(Duration::from_secs(31));
    let again = runs
        .claim("n1", &["blob_gc"], lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.id, queued.id);
    // Three real claims and an expiry: a resumable run is requeued twice, then abandoned.
    for round in 0..3 {
        env.clock.advance(Duration::from_secs(61));
        let reaped = runs
            .reap_expired(env.clock_now(), &["blob_gc"])
            .await
            .unwrap();
        if round < 2 {
            assert_eq!((reaped.requeued, reaped.abandoned), (1, 0), "round {round}");
            runs.claim("n1", &["blob_gc"], lease)
                .await
                .unwrap()
                .unwrap();
        } else {
            assert_eq!((reaped.requeued, reaped.abandoned), (0, 1));
        }
    }
    let abandoned = runs.get(queued.id).await.unwrap().unwrap();
    assert_eq!(abandoned.status, TaskRunStatus::Abandoned);
    assert_eq!(abandoned.error.as_deref(), Some("lease_expired"));
    assert!(abandoned.finished_at.is_some());
}

#[tokio::test]
async fn reaping_abandons_runs_of_kinds_that_cannot_resume_and_spares_live_leases() {
    let env = env();
    let runs = env.store.task_runs();
    let live = runs
        .enqueue(&new_run("cleanup", "a"))
        .await
        .unwrap()
        .unwrap();
    let dead = runs
        .enqueue(&new_run("retention", "b"))
        .await
        .unwrap()
        .unwrap();
    runs.claim("n", &["cleanup", "retention"], Duration::from_secs(600))
        .await
        .unwrap()
        .unwrap();
    runs.claim("n", &["cleanup", "retention"], Duration::from_secs(10))
        .await
        .unwrap()
        .unwrap();
    env.clock.advance(Duration::from_secs(11));
    let reaped = runs.reap_expired(env.clock_now(), &[]).await.unwrap();
    assert_eq!((reaped.requeued, reaped.abandoned), (0, 1));
    assert_eq!(
        runs.get(live.id).await.unwrap().unwrap().status,
        TaskRunStatus::Running
    );
    assert_eq!(
        runs.get(dead.id).await.unwrap().unwrap().status,
        TaskRunStatus::Abandoned
    );
}

#[tokio::test]
async fn heartbeats_report_ownership_and_stop_requests_and_finishing_writes_the_task_event() {
    let env = env();
    let tasks = env.store.task_definitions();
    let mut def = definition("notifying");
    def.notify = TaskNotify {
        on_failure: true,
        on_success: false,
    };
    tasks.create(&def).await.unwrap();
    let runs = env.store.task_runs();
    let mut request = new_run("blob_gc", "s");
    request.task_id = Some(def.id);
    let queued = runs.enqueue(&request).await.unwrap().unwrap();
    assert_eq!(
        tasks.get(def.id).await.unwrap().unwrap().last_run_id,
        Some(queued.id)
    );
    let lease = Duration::from_secs(60);
    runs.claim("n1", &["blob_gc"], lease)
        .await
        .unwrap()
        .unwrap();
    let beat = runs.heartbeat(queued.id, "n1", lease).await.unwrap();
    assert!(beat.owned && !beat.stop_requested);
    assert!(
        !runs
            .heartbeat(queued.id, "intruder", lease)
            .await
            .unwrap()
            .owned
    );
    assert!(runs.request_stop(queued.id).await.unwrap());
    assert!(
        runs.heartbeat(queued.id, "n1", lease)
            .await
            .unwrap()
            .stop_requested
    );

    let bad = TaskOutcome {
        status: TaskRunStatus::Running,
        summary: serde_json::json!({}),
        error: None,
    };
    assert!(matches!(
        runs.finish(queued.id, "n1", &bad).await,
        Err(StoreError::Invalid(_))
    ));
    let outcome = TaskOutcome {
        status: TaskRunStatus::Failed,
        summary: serde_json::json!({"deleted": 3}),
        error: Some("e".repeat(900)),
    };
    assert!(!runs.finish(queued.id, "intruder", &outcome).await.unwrap());
    assert!(runs.finish(queued.id, "n1", &outcome).await.unwrap());
    assert!(
        !runs.finish(queued.id, "n1", &outcome).await.unwrap(),
        "the lease is gone"
    );
    let done = runs.get(queued.id).await.unwrap().unwrap();
    assert_eq!(done.summary, serde_json::json!({"deleted": 3}));
    assert_eq!(done.error.as_deref().map(|e| e.chars().count()), Some(500));
    assert!(
        !runs.request_stop(queued.id).await.unwrap(),
        "a finished run cannot be stopped"
    );
    assert!(matches!(
        runs.request_stop(TaskRunId::generate()).await,
        Err(StoreError::NotFound(_))
    ));
    let events = env
        .store
        .outbox()
        .read_batch("mail", 10)
        .await
        .unwrap()
        .events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::TaskFinished);
    assert_eq!(events[0].data["status"], "failed");

    // A success the definition did not ask about writes no event.
    request.scope_key = "t".to_owned();
    let second = runs.enqueue(&request).await.unwrap().unwrap();
    runs.claim("n1", &["blob_gc"], lease)
        .await
        .unwrap()
        .unwrap();
    let success = TaskOutcome {
        status: TaskRunStatus::Succeeded,
        summary: serde_json::json!({}),
        error: None,
    };
    assert!(runs.finish(second.id, "n1", &success).await.unwrap());
    assert_eq!(
        env.store
            .outbox()
            .read_batch("mail-2", 10)
            .await
            .unwrap()
            .events
            .len(),
        1
    );
}

#[tokio::test]
async fn runs_are_listed_newest_first_logged_with_a_cap_and_purged_when_old() {
    let env = env();
    let runs = env.store.task_runs();
    let def = definition("listed");
    env.store.task_definitions().create(&def).await.unwrap();
    let mut with_task = new_run("blob_gc", "a");
    with_task.task_id = Some(def.id);
    let first = runs.enqueue(&with_task).await.unwrap().unwrap();
    let second = runs
        .enqueue(&new_run("blob_gc", "b"))
        .await
        .unwrap()
        .unwrap();
    let all = runs.list(None, &page(10)).await.unwrap();
    assert_eq!(
        all.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        [second.id, first.id]
    );
    let of_task = runs.list(Some(def.id), &page(10)).await.unwrap();
    assert_eq!(of_task.items.len(), 1);
    assert!(runs.list(None, &page(1)).await.unwrap().next.is_some());

    let line = |seq: i32, text: &str| TaskLogLine {
        seq,
        at: T0,
        level: 1,
        message: text.to_owned(),
    };
    runs.append_log(first.id, &[line(1, "one"), line(2, &"x".repeat(2000))])
        .await
        .unwrap();
    let read = runs.read_log(first.id, 0, 10).await.unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[1].message.chars().count(), 1024);
    assert_eq!(runs.read_log(first.id, 1, 10).await.unwrap().len(), 1);
    assert_eq!(runs.read_log(first.id, 0, 1).await.unwrap().len(), 1);
    // Past 5000 lines the rest is dropped and one truncation line stays.
    let many: Vec<TaskLogLine> = (3..6000).map(|seq| line(seq, "bulk")).collect();
    runs.append_log(first.id, &many).await.unwrap();
    runs.append_log(first.id, &[line(7000, "after the cut")])
        .await
        .unwrap();
    let all_lines = runs.read_log(first.id, 0, u32::MAX).await.unwrap();
    assert_eq!(all_lines.len(), 5001);
    assert_eq!(
        all_lines.last().map(|l| l.message.as_str()),
        Some("log truncated")
    );
    let ghost = TaskRunId::generate();
    assert!(matches!(
        runs.append_log(ghost, &[]).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        runs.read_log(ghost, 0, 1).await,
        Err(StoreError::NotFound(_))
    ));

    let lease = Duration::from_secs(60);
    runs.claim("n", &["blob_gc"], lease).await.unwrap().unwrap();
    let outcome = TaskOutcome {
        status: TaskRunStatus::Succeeded,
        summary: serde_json::json!({}),
        error: None,
    };
    assert!(runs.finish(first.id, "n", &outcome).await.unwrap());
    env.clock.advance(Duration::from_secs(60));
    assert_eq!(runs.purge(env.clock_now()).await.unwrap(), 1);
    assert!(runs.get(first.id).await.unwrap().is_none());
    assert!(matches!(
        runs.read_log(first.id, 0, 1).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        runs.get(second.id).await.unwrap().is_some(),
        "an unfinished run is never purged"
    );
}

// nodes

#[tokio::test]
async fn nodes_are_live_while_they_keep_sending_heartbeats() {
    let env = env();
    let nodes = env.store.nodes();
    let node =
        |id: &str, level: u32| NodeInfo::new(id, "0.2.0", level, T0, T0 - time::Duration::days(1));
    nodes.heartbeat(&node("b-node", 2)).await.unwrap();
    nodes.heartbeat(&node("a-node", 1)).await.unwrap();
    let max_age = Duration::from_secs(90);
    let live = nodes.list_live(max_age).await.unwrap();
    assert_eq!(
        live.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>(),
        ["a-node", "b-node"]
    );
    assert_eq!(
        live[0].seen_at,
        env.clock_now(),
        "the store sets the time, not the node"
    );
    env.clock.advance(Duration::from_secs(60));
    nodes.heartbeat(&node("b-node", 2)).await.unwrap();
    env.clock.advance(Duration::from_secs(60));
    let live = nodes.list_live(max_age).await.unwrap();
    assert_eq!(
        live.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>(),
        ["b-node"],
        "a stopped node drops out by itself"
    );
    assert!(matches!(
        nodes.heartbeat(&node("", 2)).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        nodes.heartbeat(&node(&"n".repeat(201), 2)).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        nodes.heartbeat(&node("zero", 0)).await,
        Err(StoreError::Conflict(_))
    ));
    let all_gone = nodes.list_live(Duration::ZERO).await.unwrap();
    assert!(all_gone.is_empty());
}

// the managed trust store

fn certificate(name: &str, seed: char) -> TrustedCertificate {
    TrustedCertificate::new(
        TrustedCertificateId::generate(),
        name,
        "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
        seed.to_string().repeat(64),
        T0 - time::Duration::days(1),
        T0 + time::Duration::days(365),
        T0,
    )
}

#[tokio::test]
async fn trusted_certificates_are_unique_and_protected_while_something_lists_them() {
    let env = env();
    let certificates = env.store.certificates();
    let ca = certificate("corporate-ca", 'a');
    certificates.create(&ca).await.unwrap();
    assert!(matches!(
        certificates.create(&ca).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        certificates.create(&certificate("corporate-ca", 'b')).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        certificates.create(&certificate("other", 'a')).await,
        Err(StoreError::Conflict(_))
    ));
    let mut leaf = certificate("leaf", 'c');
    leaf.is_ca = false;
    assert!(
        matches!(
            certificates.create(&leaf).await,
            Err(StoreError::Conflict(_))
        ),
        "a leaf needs allow_leaf"
    );
    leaf.allow_leaf = true;
    certificates.create(&leaf).await.unwrap();
    let names: Vec<String> = certificates
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["corporate-ca", "leaf"]);
    assert!(certificates.get(ca.id).await.unwrap().is_some());

    let mut proxy =
        rustryx_testkit::fixtures::repository("tls-proxy", RepoFormat::Npm, RepoType::Proxy);
    let mut config = ProxyConfig::new("https://upstream.example.test");
    config.ca_certificate_ids = vec![ca.id];
    proxy.proxy = Some(config);
    env.store.repos().create(&proxy).await.unwrap();
    let mut registry = Registry::new(
        rustryx_core::ids::RegistryId::generate(),
        "tls-registry",
        RegistryProvider::Oci,
        "https://registry.example.test",
        T0,
    );
    registry.ca_certificate_ids = vec![ca.id];
    env.store.registries().create(&registry).await.unwrap();
    let mut scanner = rustryx_core::scan::ScannerRecord::new(
        rustryx_core::ids::ScannerId::generate(),
        "tls-scanner",
        rustryx_core::scan::ScannerKind::Http,
        T0,
    );
    scanner.endpoint = Some("https://scanner.example.test".to_owned());
    scanner.ca_certificate_ids = vec![ca.id];
    env.store.scanners().create(&scanner).await.unwrap();

    let dependents = certificates.dependents(ca.id).await.unwrap();
    assert_eq!(dependents.repositories, [proxy.id]);
    assert_eq!(dependents.registries, [registry.id]);
    assert_eq!(dependents.scanners, [scanner.id]);
    assert!(matches!(
        certificates.delete(ca.id).await,
        Err(StoreError::Conflict(_))
    ));
    env.store.repos().delete(proxy.id).await.unwrap();
    env.store.registries().delete(registry.id).await.unwrap();
    assert!(matches!(
        certificates.delete(ca.id).await,
        Err(StoreError::Conflict(_))
    ));
    env.store.scanners().delete(scanner.id).await.unwrap();
    certificates.delete(ca.id).await.unwrap();
    assert!(matches!(
        certificates.delete(ca.id).await,
        Err(StoreError::NotFound(_))
    ));
    let none = certificates.dependents(leaf.id).await.unwrap();
    assert!(none.repositories.is_empty() && none.registries.is_empty() && none.scanners.is_empty());
}
