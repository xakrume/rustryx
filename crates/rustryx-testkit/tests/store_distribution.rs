//! Registries, replication, webhooks and their deliveries, e-mail notifications and
//! promotion.

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

use std::time::Duration;

use common::{Ports, T0, asset, env, hosted, page, project};
use rustryx_core::audit::ActorRef;
use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventDraft, EventKind, OutboxCursor};
use rustryx_core::ids::{
    ComponentId, EventId, ProjectId, PromotionId, RegistryId, ReplicationExecutionId,
    ReplicationRuleId, ReplicationTaskId, RepositoryId, SubscriptionId, UserId, WebhookId,
};
use rustryx_core::model::{
    NewComponent, ProxyConfig, RepoFormat, RepoType, UpstreamAuth, WriteOptions,
};
use rustryx_core::notification::{EmailStatus, NotificationSubscription, Recipient};
use rustryx_core::ports::{DeliveryOutcome, NewEmail};
use rustryx_core::project::ProjectRole;
use rustryx_core::promotion::{
    PromotionCheck, PromotionDecision, PromotionItemStatus, PromotionMode, PromotionRequest,
    PromotionSelector, PromotionStatus, PromotionTarget,
};
use rustryx_core::registry::{Registry, RegistryAuth, RegistryProvider, RegistryTest};
use rustryx_core::replication::{
    ExecutionStatus, ReplicationDirection, ReplicationExecution, ReplicationRule, ReplicationScope,
    ReplicationTask, ReplicationTaskKind, ReplicationTaskStatus,
};
use rustryx_core::secret::Secret;
use rustryx_core::tasks::{TaskLogLine, TaskTrigger};
use rustryx_core::webhook::{DeliveryStatus, NotifyScope, Webhook};
use rustryx_testkit::MemStore;

fn registry(name: &str) -> Registry {
    Registry::new(
        RegistryId::generate(),
        name,
        RegistryProvider::Oci,
        "https://registry.example.test",
        T0,
    )
}

fn actor() -> ActorRef {
    ActorRef::system("test")
}

// registries

#[tokio::test]
async fn registries_are_unique_validated_versioned_and_tested() {
    let env = env();
    let registries = env.store.registries();
    let first = registry("origin");
    registries.create(&first).await.unwrap();
    assert!(matches!(
        registries.create(&first).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        registries.create(&registry("origin")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut with_path = registry("pathy");
    with_path.url = "https://registry.example.test/v2".to_owned();
    assert!(matches!(
        registries.create(&with_path).await,
        Err(StoreError::Conflict(_))
    ));
    registries.create(&registry("alpha")).await.unwrap();
    let names: Vec<String> = registries
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, ["alpha", "origin"]);

    let test = RegistryTest {
        at: T0,
        ok: true,
        protocol_version: Some(1),
        message: "ok".to_owned(),
    };
    registries.record_test(first.id, &test).await.unwrap();
    assert!(matches!(
        registries.record_test(RegistryId::generate(), &test).await,
        Err(StoreError::NotFound(_))
    ));
    let mut edited = first.clone();
    edited.url = "https://moved.example.test".to_owned();
    registries.update(&edited, 1).await.unwrap();
    let stored = registries.get(first.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.version, stored.url.as_str()),
        (2, "https://moved.example.test")
    );
    assert!(
        stored.last_test.is_some(),
        "an edit does not erase the last test"
    );
    assert!(matches!(
        registries.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut clash = edited.clone();
    clash.name = "alpha".to_owned();
    assert!(matches!(
        registries.update(&clash, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut invalid = edited.clone();
    invalid.url = "ftp://nope".to_owned();
    assert!(matches!(
        registries.update(&invalid, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = edited.clone();
    ghost.id = RegistryId::generate();
    assert!(matches!(
        registries.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));
    registries.delete(first.id).await.unwrap();
    assert!(matches!(
        registries.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn updating_a_registry_rewrites_the_inline_copy_of_its_proxy_repositories() {
    let env = env();
    let mut record = registry("upstream");
    record.auth = RegistryAuth::Basic {
        username: "robot".to_owned(),
        password: Secret::new("pw-1"),
    };
    env.store.registries().create(&record).await.unwrap();
    let mut proxy =
        rustryx_testkit::fixtures::repository("mirror", RepoFormat::Docker, RepoType::Proxy);
    let mut config = ProxyConfig::new("https://stale.example.test");
    config.registry_id = Some(record.id);
    proxy.proxy = Some(config);
    env.store.repos().create(&proxy).await.unwrap();
    let unrelated =
        rustryx_testkit::fixtures::repository("direct", RepoFormat::Docker, RepoType::Proxy);
    env.store.repos().create(&unrelated).await.unwrap();

    env.clock.advance(Duration::from_secs(10));
    let mut edited = record.clone();
    edited.url = "https://new.example.test".to_owned();
    edited.auth = RegistryAuth::Basic {
        username: "robot".to_owned(),
        password: Secret::new("pw-2"),
    };
    env.store.registries().update(&edited, 1).await.unwrap();
    let bound = env.store.repos().get(proxy.id).await.unwrap().unwrap();
    let config = bound.proxy.unwrap();
    assert_eq!(config.remote_url, "https://new.example.test");
    let UpstreamAuth { username, password } = config.auth.unwrap();
    assert_eq!((username.as_str(), password.expose()), ("robot", "pw-2"));
    assert_eq!(bound.updated_at, env.clock_now());
    let other = env.store.repos().get(unrelated.id).await.unwrap().unwrap();
    assert_eq!(
        other.proxy.unwrap().remote_url,
        "https://upstream.example.test/",
        "unbound proxies are untouched"
    );

    // Bearer credentials cannot be carried by a proxy repository, which reads basic auth only.
    let mut bearer = edited.clone();
    bearer.auth = RegistryAuth::Bearer {
        token: Secret::new("t"),
    };
    env.store.registries().update(&bearer, 2).await.unwrap();
    let bound = env.store.repos().get(proxy.id).await.unwrap().unwrap();
    assert!(bound.proxy.unwrap().auth.is_none());
}

#[tokio::test]
async fn must_not_delete_a_registry_that_a_proxy_or_a_rule_still_uses() {
    let env = env();
    let record = registry("busy");
    env.store.registries().create(&record).await.unwrap();
    let mut proxy =
        rustryx_testkit::fixtures::repository("bound", RepoFormat::Docker, RepoType::Proxy);
    let mut config = ProxyConfig::new("https://x.example.test");
    config.registry_id = Some(record.id);
    proxy.proxy = Some(config);
    env.store.repos().create(&proxy).await.unwrap();
    let local = hosted(&env.store, "local").await;
    let rule = ReplicationRule::new(
        ReplicationRuleId::generate(),
        "push",
        ReplicationDirection::Push,
        record.id,
        ReplicationScope::Repository(local.id),
        T0,
    );
    env.store.replication().create_rule(&rule).await.unwrap();
    let dependents = env.store.registries().dependents(record.id).await.unwrap();
    assert_eq!(dependents.proxy_repositories, [proxy.id]);
    assert_eq!(dependents.replication_rules, [rule.id]);
    assert!(matches!(
        env.store.registries().delete(record.id).await,
        Err(StoreError::Conflict(_))
    ));
    env.store.repos().delete(proxy.id).await.unwrap();
    assert!(matches!(
        env.store.registries().delete(record.id).await,
        Err(StoreError::Conflict(_))
    ));
    env.store.replication().delete_rule(rule.id).await.unwrap();
    env.store.registries().delete(record.id).await.unwrap();
}

// replication

async fn rule_fixture(store: &MemStore, name: &str) -> (ReplicationRule, RepositoryId) {
    let record = registry(&format!("registry-{name}"));
    store.registries().create(&record).await.unwrap();
    let local = hosted(store, &format!("repo-{name}")).await;
    let rule = ReplicationRule::new(
        ReplicationRuleId::generate(),
        name,
        ReplicationDirection::Push,
        record.id,
        ReplicationScope::Repository(local.id),
        T0,
    );
    store.replication().create_rule(&rule).await.unwrap();
    (rule, local.id)
}

fn task(
    execution: ReplicationExecutionId,
    image: &str,
    at: time::OffsetDateTime,
) -> ReplicationTask {
    ReplicationTask::new(
        ReplicationTaskId::generate(),
        execution,
        ReplicationTaskKind::Copy,
        "local",
        image,
        "latest",
        at,
    )
}

#[tokio::test]
async fn replication_rules_are_validated_unique_and_frozen_while_a_run_is_active() {
    let env = env();
    let (rule, _) = rule_fixture(&env.store, "daily").await;
    let replication = env.store.replication();
    assert!(matches!(
        replication.create_rule(&rule).await,
        Err(StoreError::Conflict(_))
    ));
    let mut same_name = rule.clone();
    same_name.id = ReplicationRuleId::generate();
    assert!(matches!(
        replication.create_rule(&same_name).await,
        Err(StoreError::Conflict(_))
    ));
    let mut no_registry = rule.clone();
    no_registry.id = ReplicationRuleId::generate();
    no_registry.name = "orphan".to_owned();
    no_registry.registry_id = RegistryId::generate();
    assert!(matches!(
        replication.create_rule(&no_registry).await,
        Err(StoreError::Conflict(_))
    ));
    let mut invalid = rule.clone();
    invalid.id = ReplicationRuleId::generate();
    invalid.name = "invalid".to_owned();
    invalid.flatten_levels = 9;
    assert!(matches!(
        replication.create_rule(&invalid).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(replication.list_rules().await.unwrap().len(), 1);
    assert!(replication.get_rule(rule.id).await.unwrap().is_some());

    let mut edited = rule.clone();
    edited.copy_referrers = false;
    replication.update_rule(&edited, 1).await.unwrap();
    assert_eq!(
        replication
            .get_rule(rule.id)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    assert!(matches!(
        replication.update_rule(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = rule.clone();
    ghost.id = ReplicationRuleId::generate();
    assert!(matches!(
        replication.update_rule(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));
    let mut bad_scope = edited.clone();
    bad_scope.local = ReplicationScope::Repository(RepositoryId::generate());
    assert!(matches!(
        replication.update_rule(&bad_scope, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut clash = edited.clone();
    let second = rule_fixture(&env.store, "weekly").await.0;
    clash.name = second.name.clone();
    assert!(matches!(
        replication.update_rule(&clash, 2).await,
        Err(StoreError::Conflict(_))
    ));

    let run = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        T0,
    );
    replication.start_execution(&run).await.unwrap().unwrap();
    assert!(
        matches!(
            replication.update_rule(&edited, 2).await,
            Err(StoreError::Conflict(_))
        ),
        "the rule cannot change under a run"
    );
}

#[tokio::test]
async fn one_execution_per_rule_runs_its_tasks_and_settles_with_a_final_status() {
    let env = env();
    let (rule, _) = rule_fixture(&env.store, "settle").await;
    let replication = env.store.replication();
    let execution = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    let started = replication
        .start_execution(&execution)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(started.status, ExecutionStatus::Queued);
    let second = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    assert!(
        replication
            .start_execution(&second)
            .await
            .unwrap()
            .is_none(),
        "one active run per rule"
    );
    let stranger = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        ReplicationRuleId::generate(),
        TaskTrigger::Manual,
        T0,
    );
    assert!(matches!(
        replication.start_execution(&stranger).await,
        Err(StoreError::Conflict(_))
    ));

    let tasks: Vec<ReplicationTask> = ["a", "b", "c"]
        .iter()
        .map(|i| task(execution.id, i, env.clock_now()))
        .collect();
    replication.add_tasks(&tasks).await.unwrap();
    assert!(matches!(
        replication.add_tasks(&tasks[..1]).await,
        Err(StoreError::Conflict(_))
    ));
    let orphan = task(ReplicationExecutionId::generate(), "x", T0);
    assert!(matches!(
        replication.add_tasks(&[orphan]).await,
        Err(StoreError::Conflict(_))
    ));
    let too_many: Vec<ReplicationTask> = (0..1001)
        .map(|i| task(execution.id, &format!("i{i}"), T0))
        .collect();
    assert!(matches!(
        replication.add_tasks(&too_many).await,
        Err(StoreError::Invalid(_))
    ));
    let counts = replication
        .get_execution(execution.id)
        .await
        .unwrap()
        .unwrap()
        .counts;
    assert_eq!((counts.total, counts.in_progress), (3, 3));
    assert_eq!(
        replication
            .tasks(execution.id, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        3
    );

    let lease = Duration::from_secs(60);
    let first = replication
        .claim_task("worker-1", lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.attempts, 1);
    assert_eq!(
        replication
            .get_execution(execution.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ExecutionStatus::Running
    );
    assert!(
        replication
            .heartbeat_task(first.id, "worker-1", lease)
            .await
            .unwrap()
    );
    assert!(
        !replication
            .heartbeat_task(first.id, "intruder", lease)
            .await
            .unwrap()
    );
    // A finished task needs a terminal status, or a retry delay.
    assert!(matches!(
        replication
            .finish_task(
                first.id,
                "worker-1",
                ReplicationTaskStatus::Running,
                0,
                None,
                None,
                &[]
            )
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(
        !replication
            .finish_task(
                first.id,
                "intruder",
                ReplicationTaskStatus::Succeeded,
                5,
                None,
                None,
                &[]
            )
            .await
            .unwrap()
    );
    let event = EventDraft::new(EventKind::Push, actor(), serde_json::json!({}));
    assert!(
        replication
            .finish_task(
                first.id,
                "worker-1",
                ReplicationTaskStatus::Succeeded,
                42,
                None,
                None,
                &[event]
            )
            .await
            .unwrap()
    );
    assert!(
        !replication
            .heartbeat_task(first.id, "worker-1", lease)
            .await
            .unwrap()
    );

    // A retry puts the task back as pending after the delay.
    let second = replication
        .claim_task("worker-1", lease)
        .await
        .unwrap()
        .unwrap();
    assert!(
        replication
            .finish_task(
                second.id,
                "worker-1",
                ReplicationTaskStatus::Failed,
                0,
                Some(&"e".repeat(100)),
                Some(Duration::from_secs(30)),
                &[]
            )
            .await
            .unwrap()
    );
    let third = replication
        .claim_task("worker-2", lease)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(third.id, second.id, "the retried task waits for its delay");
    assert!(
        replication
            .finish_task(
                third.id,
                "worker-2",
                ReplicationTaskStatus::Skipped,
                0,
                None,
                None,
                &[]
            )
            .await
            .unwrap()
    );
    assert!(
        replication
            .claim_task("worker-1", lease)
            .await
            .unwrap()
            .is_none()
    );
    env.clock.advance(Duration::from_secs(31));
    let retried = replication
        .claim_task("worker-1", lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((retried.id, retried.attempts), (second.id, 2));
    assert_eq!(
        retried.error.as_deref().map(str::len),
        Some(64),
        "an error is a code, cut to 64"
    );
    assert!(
        replication
            .finish_task(
                retried.id,
                "worker-1",
                ReplicationTaskStatus::Failed,
                0,
                Some("timeout"),
                None,
                &[]
            )
            .await
            .unwrap()
    );

    assert_eq!(
        replication
            .settle_executions(env.clock_now())
            .await
            .unwrap(),
        1
    );
    let done = replication
        .get_execution(execution.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, ExecutionStatus::PartiallyFailed);
    assert_eq!(
        (
            done.counts.succeeded,
            done.counts.failed,
            done.counts.skipped
        ),
        (1, 1, 1)
    );
    assert!(done.finished_at.is_some());
    let finished = env
        .store
        .outbox()
        .read_batch("probe", 10)
        .await
        .unwrap()
        .events;
    assert!(
        finished
            .iter()
            .any(|e| e.kind == EventKind::ReplicationFinished)
    );
    // The rule is free again.
    let next = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    assert!(replication.start_execution(&next).await.unwrap().is_some());
}

#[tokio::test]
async fn a_stopped_execution_ends_stopped_and_an_empty_one_waits_out_its_grace_period() {
    let env = env();
    let (rule, _) = rule_fixture(&env.store, "stop").await;
    let replication = env.store.replication();
    let execution = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    replication
        .start_execution(&execution)
        .await
        .unwrap()
        .unwrap();
    // Between `start_execution` and `add_tasks` the execution is empty and must not settle.
    assert_eq!(
        replication
            .settle_executions(env.clock_now())
            .await
            .unwrap(),
        0
    );
    replication
        .add_tasks(&[
            task(execution.id, "a", env.clock_now()),
            task(execution.id, "b", env.clock_now()),
        ])
        .await
        .unwrap();
    assert!(replication.request_stop(execution.id).await.unwrap());
    assert!(
        replication
            .claim_task("w", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none(),
        "a stopped execution hands out no tasks"
    );
    assert_eq!(
        replication
            .settle_executions(env.clock_now())
            .await
            .unwrap(),
        1
    );
    let stopped = replication
        .get_execution(execution.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stopped.status, ExecutionStatus::Stopped);
    assert!(
        !replication.request_stop(execution.id).await.unwrap(),
        "a finished execution cannot be stopped"
    );
    assert!(matches!(
        replication
            .request_stop(ReplicationExecutionId::generate())
            .await,
        Err(StoreError::NotFound(_))
    ));

    let empty = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    replication.start_execution(&empty).await.unwrap().unwrap();
    env.clock.advance(Duration::from_secs(61));
    assert_eq!(
        replication
            .settle_executions(env.clock_now())
            .await
            .unwrap(),
        1
    );
    let settled = replication.get_execution(empty.id).await.unwrap().unwrap();
    assert_eq!(settled.status, ExecutionStatus::Succeeded);

    let all_failed = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    replication
        .start_execution(&all_failed)
        .await
        .unwrap()
        .unwrap();
    replication
        .add_tasks(&[task(all_failed.id, "x", env.clock_now())])
        .await
        .unwrap();
    let claimed = replication
        .claim_task("w", Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    replication
        .finish_task(
            claimed.id,
            "w",
            ReplicationTaskStatus::Failed,
            0,
            Some("boom"),
            None,
            &[],
        )
        .await
        .unwrap();
    replication
        .settle_executions(env.clock_now())
        .await
        .unwrap();
    assert_eq!(
        replication
            .get_execution(all_failed.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ExecutionStatus::Failed
    );
}

#[tokio::test]
async fn a_task_whose_worker_died_comes_back_after_its_lease() {
    let env = env();
    let (rule, _) = rule_fixture(&env.store, "lease").await;
    let replication = env.store.replication();
    let execution = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    replication
        .start_execution(&execution)
        .await
        .unwrap()
        .unwrap();
    replication
        .add_tasks(&[task(execution.id, "a", env.clock_now())])
        .await
        .unwrap();
    let lease = Duration::from_secs(30);
    let first = replication
        .claim_task("dead-worker", lease)
        .await
        .unwrap()
        .unwrap();
    assert!(
        replication
            .claim_task("other", lease)
            .await
            .unwrap()
            .is_none()
    );
    env.clock.advance(Duration::from_secs(31));
    let again = replication
        .claim_task("other", lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((again.id, again.attempts), (first.id, 2));
    assert!(
        !replication
            .heartbeat_task(first.id, "dead-worker", lease)
            .await
            .unwrap(),
        "the lease was taken over"
    );
}

#[tokio::test]
async fn event_tasks_join_the_open_execution_and_move_the_cursor_once() {
    let env = env();
    let (rule, _) = rule_fixture(&env.store, "events").await;
    let replication = env.store.replication();
    let cursor = |tx: u64| advance("replication", tx, tx + 1);
    let created = replication
        .create_event_tasks(
            rule.id,
            &[task(ReplicationExecutionId::generate(), "a", T0)],
            Some(&cursor(0)),
        )
        .await
        .unwrap();
    assert_eq!(created, 1);
    let executions = replication
        .list_executions(Some(rule.id), &page(10))
        .await
        .unwrap();
    assert_eq!(executions.items.len(), 1);
    assert_eq!(executions.items[0].trigger, TaskTrigger::Event);
    let execution = executions.items[0].id;
    // A duplicate consumer whose compare and set loses writes nothing.
    let lost = replication
        .create_event_tasks(
            rule.id,
            &[task(ReplicationExecutionId::generate(), "b", T0)],
            Some(&cursor(0)),
        )
        .await
        .unwrap();
    assert_eq!(lost, 0);
    // The next batch joins the same open execution.
    let joined = replication
        .create_event_tasks(
            rule.id,
            &[task(ReplicationExecutionId::generate(), "c", T0)],
            Some(&cursor(1)),
        )
        .await
        .unwrap();
    assert_eq!(joined, 1);
    assert_eq!(
        replication
            .tasks(execution, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    assert!(matches!(
        replication
            .create_event_tasks(ReplicationRuleId::generate(), &[], None)
            .await,
        Err(StoreError::NotFound(_))
    ));
    let too_many: Vec<ReplicationTask> = (0..1001)
        .map(|i| task(execution, &format!("i{i}"), T0))
        .collect();
    assert!(matches!(
        replication
            .create_event_tasks(rule.id, &too_many, None)
            .await,
        Err(StoreError::Invalid(_))
    ));
    let listed = replication.list_executions(None, &page(10)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
}

#[tokio::test]
async fn task_logs_are_appended_clipped_and_read_after_a_sequence() {
    let env = env();
    let (rule, _) = rule_fixture(&env.store, "logs").await;
    let replication = env.store.replication();
    let execution = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        T0,
    );
    replication
        .start_execution(&execution)
        .await
        .unwrap()
        .unwrap();
    let item = task(execution.id, "a", T0);
    replication
        .add_tasks(std::slice::from_ref(&item))
        .await
        .unwrap();
    let line = |seq: i32, text: &str| TaskLogLine {
        seq,
        at: T0,
        level: 1,
        message: text.to_owned(),
    };
    replication
        .append_task_log(item.id, &[line(2, "second"), line(1, &"x".repeat(2000))])
        .await
        .unwrap();
    let all = replication.read_task_log(item.id, 0, 10).await.unwrap();
    assert_eq!(all.iter().map(|l| l.seq).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(all[0].message.chars().count(), 1024);
    assert_eq!(
        replication
            .read_task_log(item.id, 1, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        replication
            .read_task_log(item.id, 0, 1)
            .await
            .unwrap()
            .len(),
        1
    );
    let ghost = ReplicationTaskId::generate();
    assert!(matches!(
        replication.append_task_log(ghost, &[]).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        replication.read_task_log(ghost, 0, 1).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn deleting_or_purging_replication_history_takes_the_tasks_and_logs_along() {
    let env = env();
    let (rule, _) = rule_fixture(&env.store, "purge").await;
    let replication = env.store.replication();
    let execution = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        T0,
    );
    replication
        .start_execution(&execution)
        .await
        .unwrap()
        .unwrap();
    let item = task(execution.id, "a", T0);
    replication
        .add_tasks(std::slice::from_ref(&item))
        .await
        .unwrap();
    replication
        .append_task_log(
            item.id,
            &[TaskLogLine {
                seq: 1,
                at: T0,
                level: 1,
                message: "m".to_owned(),
            }],
        )
        .await
        .unwrap();
    let claimed = replication
        .claim_task("w", Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    replication
        .finish_task(
            claimed.id,
            "w",
            ReplicationTaskStatus::Succeeded,
            1,
            None,
            None,
            &[],
        )
        .await
        .unwrap();
    replication
        .settle_executions(env.clock_now())
        .await
        .unwrap();
    env.clock.advance(Duration::from_secs(60));
    assert_eq!(replication.purge(env.clock_now()).await.unwrap(), 1);
    assert!(
        replication
            .get_execution(execution.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        replication
            .tasks(execution.id, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        replication.read_task_log(item.id, 0, 1).await,
        Err(StoreError::NotFound(_))
    ));

    let again = ReplicationExecution::new(
        ReplicationExecutionId::generate(),
        rule.id,
        TaskTrigger::Manual,
        env.clock_now(),
    );
    replication.start_execution(&again).await.unwrap().unwrap();
    replication
        .add_tasks(&[task(again.id, "b", env.clock_now())])
        .await
        .unwrap();
    replication.delete_rule(rule.id).await.unwrap();
    assert!(replication.get_execution(again.id).await.unwrap().is_none());
    assert!(matches!(
        replication.delete_rule(rule.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// webhooks

fn webhook(scope: NotifyScope, name: &str, events: &[EventKind]) -> Webhook {
    Webhook::new(
        WebhookId::generate(),
        scope,
        name,
        "https://hooks.example.test/rustryx",
        Secret::new("signing-secret-0123456789"),
        events.to_vec(),
        T0,
    )
}

#[tokio::test]
async fn webhooks_are_validated_capped_per_scope_and_keep_their_secret_on_update() {
    let env = env();
    let created = project(&env.store, "hooked").await;
    let hooks = env.store.webhooks();
    let first = webhook(NotifyScope::Project(created.id), "ci", &[EventKind::Push]);
    hooks.create(&first).await.unwrap();
    assert!(matches!(
        hooks.create(&first).await,
        Err(StoreError::Conflict(_))
    ));
    let mut insecure = webhook(NotifyScope::System, "plain", &[EventKind::Push]);
    insecure.url = "http://hooks.example.test".to_owned();
    assert!(matches!(
        hooks.create(&insecure).await,
        Err(StoreError::Conflict(_))
    ));
    let ghost_scope = webhook(
        NotifyScope::Project(ProjectId::generate()),
        "nowhere",
        &[EventKind::Push],
    );
    assert!(matches!(
        hooks.create(&ghost_scope).await,
        Err(StoreError::Conflict(_))
    ));
    for index in 1..20 {
        hooks
            .create(&webhook(
                NotifyScope::Project(created.id),
                &format!("h{index}"),
                &[EventKind::Pull],
            ))
            .await
            .unwrap();
    }
    let over = webhook(
        NotifyScope::Project(created.id),
        "twenty-first",
        &[EventKind::Push],
    );
    assert!(
        matches!(hooks.create(&over).await, Err(StoreError::Conflict(_))),
        "at most 20 per scope"
    );
    assert_eq!(
        hooks
            .list(Some(NotifyScope::Project(created.id)))
            .await
            .unwrap()
            .len(),
        20
    );
    assert!(
        hooks
            .list(Some(NotifyScope::System))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(hooks.list(None).await.unwrap().len(), 20);

    let mut edited = first.clone();
    edited.name = "renamed".to_owned();
    edited.secret = Secret::new("a-different-secret-value");
    edited.scope = NotifyScope::System;
    hooks.update(&edited, 1).await.unwrap();
    let stored = hooks.get(first.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.version, stored.name.as_str(), stored.scope),
        (2, "renamed", NotifyScope::Project(created.id))
    );
    assert_eq!(
        stored.secret.expose(),
        "signing-secret-0123456789",
        "only rotate_secret changes it"
    );
    assert!(matches!(
        hooks.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = edited.clone();
    ghost.id = WebhookId::generate();
    assert!(matches!(
        hooks.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));
    let mut no_events = edited.clone();
    no_events.events.clear();
    assert!(matches!(
        hooks.update(&no_events, 2).await,
        Err(StoreError::Conflict(_))
    ));

    let version = hooks
        .rotate_secret(first.id, &Secret::new("rotated-secret-0123456789"), 2)
        .await
        .unwrap();
    assert_eq!(version, 3);
    assert_eq!(
        hooks.get(first.id).await.unwrap().unwrap().secret.expose(),
        "rotated-secret-0123456789"
    );
    assert!(matches!(
        hooks.rotate_secret(first.id, &Secret::new("x"), 2).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        hooks.rotate_secret(first.id, &Secret::new(""), 3).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        hooks
            .rotate_secret(WebhookId::generate(), &Secret::new("x"), 1)
            .await,
        Err(StoreError::NotFound(_))
    ));
    hooks.delete(first.id).await.unwrap();
    assert!(matches!(
        hooks.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn matching_picks_enabled_unsuspended_webhooks_of_the_events_scopes() {
    let env = env();
    let created = project(&env.store, "matching").await;
    let repo = hosted(&env.store, "matching-repo").await;
    let hooks = env.store.webhooks();
    let system = webhook(NotifyScope::System, "system", &[EventKind::Push]);
    let in_project = webhook(
        NotifyScope::Project(created.id),
        "project",
        &[EventKind::Push, EventKind::Delete],
    );
    let in_repo = webhook(NotifyScope::Repository(repo.id), "repo", &[EventKind::Push]);
    let mut off = webhook(NotifyScope::System, "off", &[EventKind::Push]);
    off.enabled = false;
    let mut suspended = webhook(NotifyScope::System, "suspended", &[EventKind::Push]);
    suspended.suspended_at = Some(T0);
    for hook in [&system, &in_project, &in_repo, &off, &suspended] {
        hooks.create(hook).await.unwrap();
    }
    let names = |hooks: Vec<Webhook>| {
        let mut names: Vec<String> = hooks.into_iter().map(|h| h.name).collect();
        names.sort();
        names
    };
    let all = hooks
        .matching(EventKind::Push, Some(created.id), Some(repo.id))
        .await
        .unwrap();
    assert_eq!(names(all), ["project", "repo", "system"]);
    let system_only = hooks.matching(EventKind::Push, None, None).await.unwrap();
    assert_eq!(names(system_only), ["system"]);
    let deletes = hooks
        .matching(EventKind::Delete, Some(created.id), None)
        .await
        .unwrap();
    assert_eq!(names(deletes), ["project"]);
    assert!(hooks.any_subscribes_to(EventKind::Delete).await.unwrap());
    assert!(!hooks.any_subscribes_to(EventKind::Pull).await.unwrap());
    // A project removal takes its webhooks along.
    env.store.projects().delete(created.id).await.unwrap();
    assert!(hooks.get(in_project.id).await.unwrap().is_none());
}

#[tokio::test]
async fn webhooks_are_suspended_after_enough_dead_deliveries_in_a_row() {
    let env = env();
    let hooks = env.store.webhooks();
    let hook = webhook(NotifyScope::System, "flaky", &[EventKind::Push]);
    hooks.create(&hook).await.unwrap();
    hooks.record_outcome(hook.id, false, 3).await.unwrap();
    hooks.record_outcome(hook.id, false, 3).await.unwrap();
    hooks.record_outcome(hook.id, true, 3).await.unwrap();
    let after_success = hooks.get(hook.id).await.unwrap().unwrap();
    assert_eq!(
        (
            after_success.consecutive_failures,
            after_success.suspended_at
        ),
        (0, None)
    );
    for _ in 0..3 {
        hooks.record_outcome(hook.id, false, 3).await.unwrap();
    }
    let suspended = hooks.get(hook.id).await.unwrap().unwrap();
    assert_eq!(suspended.suspended_at, Some(env.clock_now()));
    assert!(!suspended.is_deliverable());
    env.clock.advance(Duration::from_secs(60));
    hooks.record_outcome(hook.id, false, 3).await.unwrap();
    assert_eq!(
        hooks.get(hook.id).await.unwrap().unwrap().suspended_at,
        Some(T0),
        "the first suspension stays"
    );
    hooks
        .record_outcome(WebhookId::generate(), false, 3)
        .await
        .unwrap();
    // Zero means never.
    let other = webhook(NotifyScope::System, "patient", &[EventKind::Push]);
    hooks.create(&other).await.unwrap();
    for _ in 0..5 {
        hooks.record_outcome(other.id, false, 0).await.unwrap();
    }
    assert!(
        hooks
            .get(other.id)
            .await
            .unwrap()
            .unwrap()
            .suspended_at
            .is_none()
    );
}

// deliveries

fn advance(consumer: &'static str, from: u64, to: u64) -> CursorAdvance {
    CursorAdvance {
        consumer,
        from: OutboxCursor {
            tx_id: from,
            seq: from.try_into().unwrap(),
        },
        to: OutboxCursor {
            tx_id: to,
            seq: to.try_into().unwrap(),
        },
    }
}

#[tokio::test]
async fn deliveries_are_created_once_per_webhook_and_event_with_the_cursor_hand_off() {
    let env = env();
    let hook = webhook(NotifyScope::System, "target", &[EventKind::Push]);
    env.store.webhooks().create(&hook).await.unwrap();
    let event = EventId::generate();
    let deliveries = env.store.deliveries();
    let rows = [
        (hook.id, event, EventKind::Push),
        (WebhookId::generate(), event, EventKind::Push),
    ];
    let created = deliveries
        .create_for_events(&rows, Some(&advance("webhooks", 0, 1)))
        .await
        .unwrap();
    assert_eq!(created, 1, "a webhook that is gone gets no delivery");
    let replay = deliveries
        .create_for_events(&rows[..1], Some(&advance("webhooks", 1, 2)))
        .await
        .unwrap();
    assert_eq!(replay, 0, "one delivery per webhook and event");
    let lost = deliveries
        .create_for_events(
            &[(hook.id, EventId::generate(), EventKind::Push)],
            Some(&advance("webhooks", 0, 1)),
        )
        .await
        .unwrap();
    assert_eq!(lost, 0, "the cursor moved: nothing is written");
    assert_eq!(deliveries.pending_count().await.unwrap(), 1);
    let cursor = env.store.outbox().read_batch("webhooks", 10).await.unwrap();
    assert_eq!(cursor.from, OutboxCursor { tx_id: 2, seq: 2 });
}

#[tokio::test]
async fn a_delivery_is_claimed_with_a_lease_finished_by_its_owner_and_retried_by_status() {
    let env = env();
    let hook = webhook(NotifyScope::System, "target", &[EventKind::Push]);
    env.store.webhooks().create(&hook).await.unwrap();
    let deliveries = env.store.deliveries();
    let events: Vec<EventId> = (0..2).map(|_| EventId::generate()).collect();
    let rows: Vec<_> = events
        .iter()
        .map(|e| (hook.id, *e, EventKind::Push))
        .collect();
    deliveries.create_for_events(&rows, None).await.unwrap();
    let lease = Duration::from_secs(60);
    let first = deliveries.claim("w1", lease).await.unwrap().unwrap();
    assert_eq!(
        (first.attempt, first.status),
        (1, DeliveryStatus::Delivering)
    );
    let second = deliveries.claim("w2", lease).await.unwrap().unwrap();
    assert_ne!(first.id, second.id);
    assert!(deliveries.claim("w3", lease).await.unwrap().is_none());

    let retry_at = env.clock_now() + time::Duration::minutes(5);
    let outcome = DeliveryOutcome {
        status: DeliveryStatus::Pending,
        status_code: Some(503),
        latency_ms: Some(40),
        error: Some("e".repeat(100)),
        excerpt: Some("bad\u{7}gateway".to_owned()),
        next_attempt_at: Some(retry_at),
    };
    assert!(
        !deliveries
            .finish(first.id, "intruder", &outcome)
            .await
            .unwrap()
    );
    assert!(deliveries.finish(first.id, "w1", &outcome).await.unwrap());
    assert!(
        !deliveries.finish(first.id, "w1", &outcome).await.unwrap(),
        "the lease is gone"
    );
    let listed = deliveries.list(hook.id, &page(10)).await.unwrap();
    let retried = listed.items.iter().find(|d| d.id == first.id).unwrap();
    assert_eq!(retried.status, DeliveryStatus::Pending);
    assert_eq!(retried.last_error.as_deref().map(str::len), Some(64));
    assert_eq!(
        retried.response_excerpt.as_deref(),
        Some("badgateway"),
        "control characters removed"
    );
    assert_eq!(retried.next_attempt_at, Some(retry_at));
    assert_eq!(deliveries.pending_count().await.unwrap(), 1);
    let done = DeliveryOutcome {
        status: DeliveryStatus::Succeeded,
        status_code: Some(200),
        latency_ms: Some(10),
        error: None,
        excerpt: None,
        next_attempt_at: None,
    };
    assert!(deliveries.finish(second.id, "w2", &done).await.unwrap());
    let delivered = deliveries.list(hook.id, &page(10)).await.unwrap();
    assert!(
        delivered
            .items
            .iter()
            .any(|d| d.status == DeliveryStatus::Succeeded && d.delivered_at.is_some())
    );
    // The retry is due after its time.
    assert!(deliveries.claim("w3", lease).await.unwrap().is_none());
    env.clock.advance(Duration::from_secs(301));
    let again = deliveries.claim("w3", lease).await.unwrap().unwrap();
    assert_eq!((again.id, again.attempt), (first.id, 2));
    // A claimed delivery whose lease ran out is claimed again by someone else.
    env.clock.advance(Duration::from_secs(61));
    let taken = deliveries.claim("w4", lease).await.unwrap().unwrap();
    assert_eq!((taken.id, taken.attempt), (first.id, 3));
    assert!(!deliveries.finish(first.id, "w3", &done).await.unwrap());
    env.clock.advance(Duration::from_secs(60));
    assert_eq!(deliveries.purge(env.clock_now()).await.unwrap(), 2);
    assert!(
        deliveries
            .list(hook.id, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_a_webhook_deletes_its_deliveries() {
    let env = env();
    let hook = webhook(NotifyScope::System, "short-lived", &[EventKind::Push]);
    env.store.webhooks().create(&hook).await.unwrap();
    env.store
        .deliveries()
        .create_for_events(&[(hook.id, EventId::generate(), EventKind::Push)], None)
        .await
        .unwrap();
    env.store.webhooks().delete(hook.id).await.unwrap();
    assert_eq!(env.store.deliveries().pending_count().await.unwrap(), 0);
}

// notifications

fn subscription(
    scope: NotifyScope,
    events: &[EventKind],
    recipients: Vec<Recipient>,
) -> NotificationSubscription {
    NotificationSubscription::new(
        SubscriptionId::generate(),
        scope,
        events.to_vec(),
        recipients,
    )
}

#[tokio::test]
async fn subscriptions_are_validated_versioned_and_matched_by_scope_and_event() {
    let env = env();
    let created = project(&env.store, "mailing").await;
    let repo = hosted(&env.store, "mailing-repo").await;
    let mail = env.store.notifications();
    let address = Recipient::address("ops@example.test").unwrap();
    let system = subscription(
        NotifyScope::System,
        &[EventKind::TaskFinished],
        vec![address.clone()],
    );
    mail.upsert(&system, None).await.unwrap();
    assert!(matches!(
        mail.upsert(&system, None).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = system.clone();
    ghost.id = SubscriptionId::generate();
    assert!(matches!(
        mail.upsert(&ghost, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    let mut edited = system.clone();
    edited.enabled = false;
    mail.upsert(&edited, Some(1)).await.unwrap();
    assert!(matches!(
        mail.upsert(&edited, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    let listed = mail.list(Some(NotifyScope::System)).await.unwrap();
    assert_eq!((listed[0].version, listed[0].enabled), (2, false));
    edited.enabled = true;
    mail.upsert(&edited, Some(2)).await.unwrap();

    let mut empty = system.clone();
    empty.id = SubscriptionId::generate();
    empty.events.clear();
    assert!(matches!(
        mail.upsert(&empty, None).await,
        Err(StoreError::Conflict(_))
    ));
    let nowhere = subscription(
        NotifyScope::Project(ProjectId::generate()),
        &[EventKind::Push],
        vec![address.clone()],
    );
    assert!(matches!(
        mail.upsert(&nowhere, None).await,
        Err(StoreError::Conflict(_))
    ));
    let by_role = subscription(
        NotifyScope::Project(created.id),
        &[EventKind::Push, EventKind::TokenExpiring],
        vec![Recipient::ProjectRole {
            role: ProjectRole::Maintainer,
        }],
    );
    mail.upsert(&by_role, None).await.unwrap();
    let of_repo = subscription(
        NotifyScope::Repository(repo.id),
        &[EventKind::Push],
        vec![Recipient::TokenOwner],
    );
    mail.upsert(&of_repo, None).await.unwrap();

    let matched = mail
        .matching(EventKind::Push, Some(created.id), Some(repo.id))
        .await
        .unwrap();
    assert_eq!(matched.len(), 2);
    assert_eq!(
        mail.matching(EventKind::TaskFinished, None, None)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        mail.matching(EventKind::Delete, Some(created.id), Some(repo.id))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(mail.list(None).await.unwrap().len(), 3);
    mail.delete(system.id).await.unwrap();
    assert!(matches!(
        mail.delete(system.id).await,
        Err(StoreError::NotFound(_))
    ));
    env.store.repos().delete(repo.id).await.unwrap();
    assert!(
        mail.list(Some(NotifyScope::Repository(repo.id)))
            .await
            .unwrap()
            .is_empty()
    );
}

fn email(to: &str, kind: EventKind, subject: &str) -> NewEmail {
    NewEmail {
        subscription_id: None,
        event_id: EventId::generate(),
        to: to.to_owned(),
        kind,
        subject_key: subject.to_owned(),
    }
}

#[tokio::test]
async fn identical_mails_within_ten_minutes_are_suppressed_and_sending_is_counted() {
    let env = env();
    let mail = env.store.notifications();
    let rows = [
        email("a@example.test", EventKind::TokenExpiring, "token-1"),
        email("a@example.test", EventKind::TokenExpiring, "token-1"),
        email("a@example.test", EventKind::TokenExpiring, "token-2"),
        email("b@example.test", EventKind::TokenExpiring, "token-1"),
    ];
    assert_eq!(
        mail.create_for_events(&rows, Some(&advance("email", 0, 1)))
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        mail.create_for_events(&rows, Some(&advance("email", 0, 1)))
            .await
            .unwrap(),
        0,
        "the cursor moved"
    );
    let lease = Duration::from_secs(60);
    let mut sent = 0;
    let mut owners = Vec::new();
    while let Some(delivery) = mail.claim("mailer", lease).await.unwrap() {
        owners.push(delivery.to.clone());
        assert!(
            mail.finish(delivery.id, "mailer", EmailStatus::Sent, None, None)
                .await
                .unwrap()
        );
        sent += 1;
    }
    assert_eq!(sent, 3, "the duplicate was suppressed");
    assert_eq!(mail.count_sent_since(T0).await.unwrap(), 3);
    assert_eq!(
        mail.count_sent_since(env.clock_now() + time::Duration::hours(1))
            .await
            .unwrap(),
        0
    );
    // After the window the same message is sent again.
    env.clock.advance(Duration::from_secs(601));
    let again = [email("a@example.test", EventKind::TokenExpiring, "token-1")];
    assert_eq!(mail.create_for_events(&again, None).await.unwrap(), 1);
    assert!(mail.claim("mailer", lease).await.unwrap().is_some());
}

#[tokio::test]
async fn a_mail_is_retried_failed_or_taken_over_like_any_leased_row() {
    let env = env();
    let mail = env.store.notifications();
    mail.create_for_events(
        &[email("c@example.test", EventKind::TaskFinished, "run-1")],
        None,
    )
    .await
    .unwrap();
    let lease = Duration::from_secs(60);
    let first = mail.claim("m1", lease).await.unwrap().unwrap();
    assert_eq!(first.attempts, 1);
    assert!(matches!(
        mail.finish(first.id, "m1", EmailStatus::Pending, None, None)
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(
        !mail
            .finish(first.id, "intruder", EmailStatus::Sent, None, None)
            .await
            .unwrap()
    );
    assert!(
        mail.finish(
            first.id,
            "m1",
            EmailStatus::Pending,
            Some("smtp_busy"),
            Some(Duration::from_secs(120))
        )
        .await
        .unwrap()
    );
    assert!(
        mail.claim("m1", lease).await.unwrap().is_none(),
        "not due yet"
    );
    env.clock.advance(Duration::from_secs(121));
    let second = mail.claim("m1", lease).await.unwrap().unwrap();
    assert_eq!((second.id, second.attempts), (first.id, 2));
    env.clock.advance(Duration::from_secs(61));
    let taken = mail.claim("m2", lease).await.unwrap().unwrap();
    assert_eq!(taken.attempts, 3);
    assert!(
        mail.finish(taken.id, "m2", EmailStatus::Dead, Some("rejected"), None)
            .await
            .unwrap()
    );
    assert_eq!(mail.count_sent_since(T0).await.unwrap(), 0);
    env.clock.advance(Duration::from_secs(60));
    assert_eq!(mail.purge(env.clock_now()).await.unwrap(), 1);
}

// promotion

struct Promotion {
    source: RepositoryId,
    target: RepositoryId,
    components: Vec<ComponentId>,
}

async fn promotion_fixture(store: &MemStore) -> Promotion {
    let source = hosted(store, "staging").await;
    let target = hosted(store, "release").await;
    let mut components = Vec::new();
    for version in ["1", "2"] {
        let mut new = NewComponent::new(source.id, RepoFormat::Raw, "pkg");
        new.version = Some(version.to_owned());
        components.push(store.components().find_or_create(&new).await.unwrap().id);
    }
    store
        .assets()
        .publish_with(&asset(source.id, "pkg/file", b"x"), &WriteOptions::new())
        .await
        .unwrap();
    Promotion {
        source: source.id,
        target: target.id,
        components,
    }
}

fn request(fixture: &Promotion, requester: UserId, required: u8) -> PromotionRequest {
    let mut request = PromotionRequest::new(
        PromotionId::generate(),
        PromotionMode::Copy,
        fixture.source,
        fixture.target,
        PromotionSelector::Components {
            ids: fixture.components.clone(),
        },
        requester,
        T0,
    );
    request.required_approvals = required;
    request
}

fn decision(
    request: &PromotionRequest,
    approver: UserId,
    approve: bool,
    counted: bool,
) -> PromotionDecision {
    PromotionDecision {
        promotion_id: request.id,
        approver,
        approve,
        comment: "looks fine".to_owned(),
        counted,
        at: T0,
    }
}

#[tokio::test]
async fn a_promotion_is_approved_by_counted_approvals_and_queues_its_executor() {
    let env = env();
    let fixture = promotion_fixture(&env.store).await;
    let requester = UserId::generate();
    let promotions = env.store.promotions();
    let created = request(&fixture, requester, 2);
    let event = EventDraft::new(
        EventKind::PromotionRequested,
        actor(),
        serde_json::json!({}),
    );
    promotions
        .create(&created, &fixture.components, &[event])
        .await
        .unwrap();
    assert!(matches!(
        promotions.create(&created, &[], &[]).await,
        Err(StoreError::Conflict(_))
    ));
    let items = promotions.items(created.id, &page(10)).await.unwrap();
    assert_eq!(items.items.len(), 2);
    assert!(
        items
            .items
            .iter()
            .all(|i| i.status == PromotionItemStatus::Pending)
    );

    let approver = UserId::generate();
    assert_eq!(
        promotions
            .decide(&decision(&created, requester, true, false), &[])
            .await
            .unwrap(),
        PromotionStatus::PendingApproval,
        "the requester's own approval does not count"
    );
    assert_eq!(
        promotions
            .decide(&decision(&created, approver, true, true), &[])
            .await
            .unwrap(),
        PromotionStatus::PendingApproval
    );
    assert!(
        matches!(
            promotions
                .decide(&decision(&created, approver, true, true), &[])
                .await,
            Err(StoreError::Conflict(_))
        ),
        "one decision per approver"
    );
    let last = UserId::generate();
    let event = EventDraft::new(EventKind::PromotionApproved, actor(), serde_json::json!({}));
    assert_eq!(
        promotions
            .decide(&decision(&created, last, true, true), &[event])
            .await
            .unwrap(),
        PromotionStatus::Approved
    );
    assert_eq!(promotions.decisions(created.id).await.unwrap().len(), 3);
    assert!(
        matches!(
            promotions
                .decide(&decision(&created, UserId::generate(), true, true), &[])
                .await,
            Err(StoreError::Conflict(_))
        ),
        "no longer waiting"
    );

    // The executor job is queued with the dedupe key promotion:<id>, once.
    let job = env
        .store
        .jobs()
        .claim("promotion", "exec", Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.payload["promotion_id"], serde_json::json!(created.id));
    assert!(
        env.store
            .jobs()
            .claim("promotion", "exec", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none()
    );
    let kinds: Vec<EventKind> = env
        .store
        .outbox()
        .read_batch("c", 10)
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        [EventKind::PromotionRequested, EventKind::PromotionApproved]
    );

    // Execution.
    assert!(
        promotions
            .mark_running(created.id, env.clock_now())
            .await
            .unwrap()
    );
    assert!(
        !promotions
            .mark_running(created.id, env.clock_now())
            .await
            .unwrap(),
        "already running"
    );
    assert!(matches!(
        promotions.mark_running(PromotionId::generate(), T0).await,
        Err(StoreError::NotFound(_))
    ));
    promotions
        .set_item(
            created.id,
            fixture.components[0],
            PromotionItemStatus::Done,
            None,
        )
        .await
        .unwrap();
    promotions
        .set_item(
            created.id,
            fixture.components[1],
            PromotionItemStatus::Skipped,
            Some(&"r".repeat(100)),
        )
        .await
        .unwrap();
    let after = promotions.items(created.id, &page(10)).await.unwrap().items;
    assert!(after.iter().any(|i| i.status == PromotionItemStatus::Done));
    let skipped = after
        .iter()
        .find(|i| i.status == PromotionItemStatus::Skipped)
        .unwrap();
    assert_eq!(skipped.reason.as_deref().map(str::len), Some(64));
    assert!(matches!(
        promotions
            .set_item(
                created.id,
                ComponentId::generate(),
                PromotionItemStatus::Done,
                None
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        promotions
            .finish(created.id, PromotionStatus::Running, T0, &[])
            .await,
        Err(StoreError::Invalid(_))
    ));
    let done = EventDraft::new(
        EventKind::PromotionCompleted,
        actor(),
        serde_json::json!({}),
    );
    promotions
        .finish(
            created.id,
            PromotionStatus::PartiallyFailed,
            env.clock_now(),
            &[done],
        )
        .await
        .unwrap();
    let stored = promotions.get(created.id).await.unwrap().unwrap();
    assert_eq!(stored.status, PromotionStatus::PartiallyFailed);
    assert!(matches!(
        promotions
            .finish(created.id, PromotionStatus::Succeeded, T0, &[])
            .await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn a_rejection_a_cancel_and_an_expiry_each_end_a_pending_request() {
    let env = env();
    let fixture = promotion_fixture(&env.store).await;
    let promotions = env.store.promotions();
    let requester = UserId::generate();
    let rejected = request(&fixture, requester, 1);
    promotions
        .create(&rejected, &fixture.components, &[])
        .await
        .unwrap();
    let outcome = promotions
        .decide(&decision(&rejected, UserId::generate(), false, true), &[])
        .await
        .unwrap();
    assert_eq!(outcome, PromotionStatus::Rejected);
    assert!(
        promotions
            .get(rejected.id)
            .await
            .unwrap()
            .unwrap()
            .finished_at
            .is_some()
    );

    let cancelled = request(&fixture, requester, 1);
    promotions
        .create(&cancelled, &fixture.components, &[])
        .await
        .unwrap();
    assert!(
        matches!(
            promotions.cancel(cancelled.id, UserId::generate()).await,
            Err(StoreError::Conflict(_))
        ),
        "only the requester"
    );
    promotions.cancel(cancelled.id, requester).await.unwrap();
    assert!(
        matches!(
            promotions.cancel(cancelled.id, requester).await,
            Err(StoreError::Conflict(_))
        ),
        "no longer pending"
    );
    assert!(matches!(
        promotions.cancel(PromotionId::generate(), requester).await,
        Err(StoreError::NotFound(_))
    ));

    let expiring = request(&fixture, requester, 1);
    promotions
        .create(&expiring, &fixture.components, &[])
        .await
        .unwrap();
    assert_eq!(promotions.expire(env.clock_now()).await.unwrap(), 0);
    env.clock.advance(Duration::from_hours(360));
    assert!(
        matches!(
            promotions
                .decide(&decision(&expiring, UserId::generate(), true, true), &[])
                .await,
            Err(StoreError::Conflict(_))
        ),
        "an expired request cannot be decided"
    );
    assert_eq!(promotions.expire(env.clock_now()).await.unwrap(), 1);
    assert_eq!(
        promotions.get(expiring.id).await.unwrap().unwrap().status,
        PromotionStatus::Expired
    );

    let by_status = promotions
        .list(None, Some(PromotionStatus::Cancelled), &page(10))
        .await
        .unwrap();
    assert_eq!(by_status.items.len(), 1);
    let by_repo = promotions
        .list(Some(fixture.target), None, &page(10))
        .await
        .unwrap();
    assert_eq!(by_repo.items.len(), 3);
    let by_source = promotions
        .list(Some(fixture.source), None, &page(2))
        .await
        .unwrap();
    assert_eq!(by_source.items.len(), 2);
    assert!(by_source.next.is_some());
    assert!(
        promotions
            .list(Some(RepositoryId::generate()), None, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        promotions
            .get(PromotionId::generate())
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        promotions.items(PromotionId::generate(), &page(1)).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        promotions.decisions(PromotionId::generate()).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn promotion_requests_and_targets_are_validated() {
    let env = env();
    let fixture = promotion_fixture(&env.store).await;
    let promotions = env.store.promotions();
    let requester = UserId::generate();
    let mut same = request(&fixture, requester, 1);
    same.target_repository_id = same.source_repository_id;
    assert!(matches!(
        promotions.create(&same, &fixture.components, &[]).await,
        Err(StoreError::Conflict(_))
    ));
    let mut orphan = request(&fixture, requester, 1);
    orphan.target_repository_id = RepositoryId::generate();
    assert!(matches!(
        promotions.create(&orphan, &fixture.components, &[]).await,
        Err(StoreError::Conflict(_))
    ));
    let many: Vec<ComponentId> = (0..10_001).map(|_| ComponentId::generate()).collect();
    assert!(matches!(
        promotions
            .create(&request(&fixture, requester, 1), &many, &[])
            .await,
        Err(StoreError::Invalid(_))
    ));
    let duplicated = [fixture.components[0], fixture.components[0]];
    let one = request(&fixture, requester, 1);
    promotions.create(&one, &duplicated, &[]).await.unwrap();
    assert_eq!(
        promotions
            .items(one.id, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let mut huge = EventDraft::new(
        EventKind::PromotionRequested,
        actor(),
        serde_json::json!({"x": "y".repeat(20_000)}),
    );
    huge.repository_id = Some(fixture.target);
    assert!(matches!(
        promotions
            .create(&request(&fixture, requester, 1), &[], &[huge])
            .await,
        Err(StoreError::Invalid(_))
    ));

    let mut target = PromotionTarget::new(fixture.target);
    target.require_approval = true;
    target.checks = vec![PromotionCheck::SignatureValid];
    promotions.set_target(&target, None).await.unwrap();
    assert_eq!(
        promotions
            .get_target(fixture.target)
            .await
            .unwrap()
            .map(|t| t.version),
        Some(1)
    );
    assert!(matches!(
        promotions.set_target(&target, None).await,
        Err(StoreError::Conflict(_))
    ));
    target.required_approvals = 2;
    promotions.set_target(&target, Some(1)).await.unwrap();
    assert_eq!(
        promotions
            .get_target(fixture.target)
            .await
            .unwrap()
            .map(|t| (t.version, t.required_approvals)),
        Some((2, 2))
    );
    assert!(matches!(
        promotions.set_target(&target, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    let free = PromotionTarget::new(fixture.source);
    assert!(matches!(
        promotions.set_target(&free, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    let mut invalid = PromotionTarget::new(fixture.source);
    invalid.require_approval = true;
    invalid.required_approvals = 0;
    assert!(matches!(
        promotions.set_target(&invalid, None).await,
        Err(StoreError::Conflict(_))
    ));
    let nowhere = PromotionTarget::new(RepositoryId::generate());
    assert!(matches!(
        promotions.set_target(&nowhere, None).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        promotions
            .get_target(RepositoryId::generate())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn must_not_change_the_signing_secret_through_an_update_of_the_webhook() {
    let env = env();
    let hooks = env.store.webhooks();
    let created = webhook(NotifyScope::System, "stable-secret", &[EventKind::Push]);
    hooks.create(&created).await.unwrap();
    let mut edited = created.clone();
    edited.secret = Secret::new("a-secret-from-a-stale-read");
    edited.auth_header = Some(Secret::new("Bearer new-header"));
    hooks.update(&edited, 1).await.unwrap();
    let stored = hooks.get(created.id).await.unwrap().unwrap();
    assert_eq!(stored.secret.expose(), "signing-secret-0123456789");
    assert_eq!(
        stored.auth_header.as_ref().map(Secret::expose),
        Some("Bearer new-header"),
        "the auth header is an ordinary field of the form"
    );
}
