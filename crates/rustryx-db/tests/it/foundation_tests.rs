//! Migration 0007 (wave 2 foundation) and 0008 (routing rules): what the schema itself
//! guarantees, and that a binary of the previous release keeps working against it.

use rustryx_core::StoreError;
use rustryx_db::error::map_err;
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::{TestDb, sqlstate};

async fn exec(pool: &PgPool, sql: &'static str) -> Result<(), sqlx::Error> {
    sqlx::query(sql).execute(pool).await.map(|_| ())
}

async fn partitions_of(pool: &PgPool, parent: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
         WHERE i.inhparent = $1::regclass ORDER BY 1",
    )
    .bind(parent)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn insert_event_at(pool: &PgPool, at: &str) {
    sqlx::query(
        "INSERT INTO event_outbox (id, occurred_at, kind, actor_name, actor_kind, data) \
         VALUES ($1, $2::timestamptz, 'push', 'test', 'system', '{}')",
    )
    .bind(Uuid::now_v7())
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_foundation_creates_its_tables_and_nothing_of_the_streams() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = 'public' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for expected in [
        "projects",
        "revoked_tokens",
        "node_heartbeats",
        "event_outbox",
        "outbox_cursors",
        "task_definitions",
        "task_runs",
        "task_run_log",
        "routing_rules",
        "immutability_rules",
        "lifecycle_holds",
        "retention_policies",
        "lifecycle_runs",
        "lifecycle_run_items",
        "quotas",
        "quota_usage",
        "robot_tokens",
        "project_members",
        "labels",
        "component_labels",
    ] {
        assert!(tables.iter().any(|t| t == expected), "missing {expected}");
    }
    // Owned by the streams, in their own migration ranges (design section 1.5).
    for foreign in [
        "cleanup_policies",
        "scanners",
        "webhooks",
        "registries",
        "blob_store_members",
        "asset_downloads_daily",
    ] {
        assert!(
            !tables.iter().any(|t| t == foreign),
            "{foreign} belongs to a stream"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_outbox_starts_with_the_current_month_and_two_ahead_and_ensure_is_idempotent() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let names = partitions_of(&pool, "event_outbox").await;
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.iter().all(|n| n.starts_with("event_outbox_y")));

    let created: i32 = sqlx::query_scalar("SELECT ensure_outbox_partitions(now(), 2)")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(created, 0);
    let created: i32 = sqlx::query_scalar("SELECT ensure_outbox_partitions(now(), 4)")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(created, 2);
    for bad in [-1, 25] {
        let err = sqlx::query("SELECT ensure_outbox_partitions(now(), $1)")
            .bind(bad)
            .execute(&pool)
            .await
            .unwrap_err();
        assert_eq!(sqlstate(&err), "22023", "lookahead {bad}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn old_outbox_partitions_are_dropped_whole_and_only_by_age() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    sqlx::query("SELECT ensure_outbox_partitions('2020-01-15'::timestamptz, 1)")
        .execute(&pool)
        .await
        .unwrap();
    insert_event_at(&pool, "2020-01-10T00:00:00Z").await;
    insert_event_at(&pool, "2020-02-10T00:00:00Z").await;

    // 2020-02-20: January ended 20 days ago, February is still open. Retention 30: nothing.
    let dropped: i32 =
        sqlx::query_scalar("SELECT drop_old_outbox_partitions('2020-02-20'::timestamptz, 30)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dropped, 0);
    // 2020-03-05: January ended 34 days ago, February (ends 03-01) only 4. One is dropped.
    let dropped: i32 =
        sqlx::query_scalar("SELECT drop_old_outbox_partitions('2020-03-05'::timestamptz, 30)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dropped, 1);
    let names = partitions_of(&pool, "event_outbox").await;
    assert!(
        !names.contains(&"event_outbox_y2020m01".to_owned()),
        "{names:?}"
    );
    assert!(
        names.contains(&"event_outbox_y2020m02".to_owned()),
        "{names:?}"
    );
    // The current partitions are never candidates.
    assert!(names.iter().any(|n| !n.starts_with("event_outbox_y2020")));

    let err = sqlx::query("SELECT drop_old_outbox_partitions(now(), 0)")
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&err), "22023");
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_drop_an_outbox_partition_that_holds_events_a_consumer_has_not_handled() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    sqlx::query("SELECT ensure_outbox_partitions('2020-01-15'::timestamptz, 0)")
        .execute(&pool)
        .await
        .unwrap();
    insert_event_at(&pool, "2020-01-10T00:00:00Z").await;
    // One consumer exists and has handled nothing (cursor at the start).
    sqlx::query("INSERT INTO outbox_cursors (consumer) VALUES ('webhooks')")
        .execute(&pool)
        .await
        .unwrap();

    let drop_it = || async {
        sqlx::query_scalar::<_, i32>(
            "SELECT drop_old_outbox_partitions('2021-01-01'::timestamptz, 30)",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(drop_it().await, 0, "an unhandled event pins its partition");
    assert!(
        partitions_of(&pool, "event_outbox")
            .await
            .contains(&"event_outbox_y2020m01".to_owned())
    );

    // A second, faster consumer does not release it: the SLOWEST cursor decides.
    sqlx::query(
        "INSERT INTO outbox_cursors (consumer, last_tx_id, last_seq) \
         VALUES ('email', (SELECT max(tx_id) FROM event_outbox), (SELECT max(seq) FROM event_outbox))",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(drop_it().await, 0);

    // Once the slow one has caught up, the partition goes.
    sqlx::query(
        "UPDATE outbox_cursors SET last_tx_id = (SELECT max(tx_id) FROM event_outbox), \
         last_seq = (SELECT max(seq) FROM event_outbox) WHERE consumer = 'webhooks'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(drop_it().await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partition_attached_by_hand_is_never_dropped() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    exec(
        &pool,
        "CREATE TABLE event_outbox_legal_hold PARTITION OF event_outbox \
         FOR VALUES FROM ('2019-01-01') TO ('2019-02-01')",
    )
    .await
    .unwrap();
    // Far in the future every generated partition is old: the three of the migration go, the
    // one with a hand chosen name stays.
    let dropped: i32 =
        sqlx::query_scalar("SELECT drop_old_outbox_partitions('2030-01-01'::timestamptz, 30)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dropped, 3);
    assert!(
        partitions_of(&pool, "event_outbox")
            .await
            .contains(&"event_outbox_legal_hold".to_owned())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_id_is_unique_within_its_timestamp() {
    // The unique index of a partitioned table has to contain the partition key, so the id is
    // unique together with `occurred_at`: a safety net against a duplicated write of one
    // event (same id, same instant), not a global guarantee. Ids are `UUIDv7` made by this
    // crate, which is what makes a collision across instants impossible in practice.
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO event_outbox (id, occurred_at, kind, actor_name, actor_kind, data) \
         VALUES ($1, date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC', \
         'push', 'a', 'system', '{}')",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let err = sqlx::query(
        "INSERT INTO event_outbox (id, occurred_at, kind, actor_name, actor_kind, data) \
         VALUES ($1, date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC', \
         'push', 'a', 'system', '{}')",
    )
    .bind(id)
    .execute(&pool)
    .await
    .map_err(|e| map_err("insert event", e))
    .unwrap_err();
    // The violated index is the one of the partition, whose generated name no static text knows.
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_settings_revision_comes_from_one_sequence_and_every_update_bumps_it() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    // Phase 1 statements: no revision, no author.
    sqlx::query(
        "INSERT INTO system_config (key, value) VALUES ('settings.a', '1'), ('settings.b', '2')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let first: Vec<(String, i64)> =
        sqlx::query_as("SELECT key, revision FROM system_config ORDER BY key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        first[0].1 < first[1].1,
        "distinct and increasing from the sequence"
    );

    // The N-1 writer's upsert does not know the column; the trigger bumps it anyway.
    sqlx::query(
        "INSERT INTO system_config (key, value) VALUES ('settings.a', '9') \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .execute(&pool)
    .await
    .unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT revision FROM system_config WHERE key = 'settings.a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        after > first[1].1,
        "the bump is a new value of the shared sequence"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binary_of_the_previous_release_keeps_working_against_the_new_schema() {
    // The statements of phase 1, as that binary issues them: no new column is named anywhere.
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let store_id = Uuid::now_v7();
    let repo_id = Uuid::now_v7();
    let blob_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let component_id = Uuid::now_v7();
    sqlx::query("INSERT INTO blob_stores (id, name, kind, config, quota_bytes, created_at) VALUES ($1, 'old', 'local', '{}', NULL, now())")
        .bind(store_id).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO repositories (id, name, format, repo_type, blob_store_id, write_policy, \
         allow_anonymous, online, quota_bytes, cleanup_policy_ids, routing_rule_id, \
         proxy_config, group_config, format_config, created_at, updated_at) \
         VALUES ($1, 'old-repo', 'maven2', 'hosted', $2, 'allow', true, true, NULL, '{}', NULL, \
         NULL, NULL, '{}', now(), now())",
    )
    .bind(repo_id)
    .bind(store_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO blobs (id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at) VALUES ($1, $2, 'k', 1, $3, $4, $5, now())")
        .bind(blob_id).bind(store_id)
        .bind("0".repeat(40)).bind("0".repeat(64)).bind("0".repeat(32))
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO components (id, repository_id, format, group_id, name, version) VALUES ($1, $2, 'maven2', NULL, 'c', '1')")
        .bind(component_id).bind(repo_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO assets (id, repository_id, component_id, path, blob_id, content_type) VALUES ($1, $2, $3, 'p', $4, 'x/y')")
        .bind(asset_id).bind(repo_id).bind(component_id).bind(blob_id)
        .execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO audit_events (id, event_time, kind, actor_id, actor_name, client_ip, \
         request_id, action, target, outcome, detail) \
         VALUES ($1, now(), 'config_change', NULL, 'old', NULL, NULL, 'a', NULL, 'success', '{}')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    // Its own repository update (the mutable column list of phase 1).
    sqlx::query(
        "UPDATE repositories SET write_policy = 'allow', allow_anonymous = false, online = true, \
         quota_bytes = NULL, cleanup_policy_ids = '{}', routing_rule_id = NULL, proxy_config = NULL, \
         group_config = NULL, format_config = '{}', updated_at = now() WHERE id = $1",
    )
    .bind(repo_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE assets a SET last_downloaded_at = now() WHERE a.id = $1")
        .bind(asset_id)
        .execute(&pool)
        .await
        .unwrap();

    // What it reads back is what it wrote; the new columns carry their defaults.
    let (kind, project): (String, Option<Uuid>) = sqlx::query_as(
        "SELECT (SELECT actor_kind FROM audit_events LIMIT 1), \
                (SELECT project_id FROM repositories WHERE id = $1)",
    )
    .bind(repo_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kind, "user");
    assert!(project.is_none());

    // The binary floor is untouched: an expand migration never raises it, so a binary built
    // against schema 6 still reports ready (`OlderCompatible`).
    let floor: i64 = sqlx::query_scalar("SELECT min_binary_schema FROM schema_contract")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(floor, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_task_tables_enforce_their_shape() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let task = Uuid::now_v7();
    sqlx::query("INSERT INTO task_definitions (id, kind, name, schedule) VALUES ($1, 'blob_gc', 'gc', '{\"type\":\"manual\"}')")
        .bind(task).execute(&pool).await.unwrap();
    let err = sqlx::query("INSERT INTO task_definitions (id, kind, name, schedule) VALUES ($1, 'Bad Kind', 'x', '{}')")
        .bind(Uuid::now_v7()).execute(&pool).await;
    assert!(err.is_err(), "kind shape");
    let dup = sqlx::query("INSERT INTO task_definitions (id, kind, name, schedule) VALUES ($1, 'blob_gc', 'gc', '{}')")
        .bind(Uuid::now_v7()).execute(&pool).await
        .map_err(|e| map_err("insert task", e)).unwrap_err();
    assert!(
        matches!(dup, StoreError::Conflict(ref m) if m == "task name already exists"),
        "{dup:?}"
    );

    let first = Uuid::now_v7();
    let insert_run = |id: Uuid, status: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO task_runs (id, task_id, kind, scope_key, status, trigger) \
                 VALUES ($1, $2, 'blob_gc', 'repo-a', $3, 'manual')",
            )
            .bind(id)
            .bind(task)
            .bind(status)
            .execute(&pool)
            .await
            .map_err(|e| map_err("insert run", e))
        }
    };
    insert_run(first, "queued").await.unwrap();
    // One queued or running run per (kind, scope): the database side of a singleton task.
    let twin = insert_run(Uuid::now_v7(), "running").await.unwrap_err();
    assert!(
        matches!(twin, StoreError::Conflict(ref m) if m == "a run of this task is already queued or running"),
        "{twin:?}"
    );
    // A finished run does not count, so history can pile up.
    sqlx::query("UPDATE task_runs SET status = 'succeeded' WHERE id = $1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    insert_run(Uuid::now_v7(), "queued").await.unwrap();
    assert!(
        insert_run(Uuid::now_v7(), "exploded").await.is_err(),
        "status list"
    );

    // The error text is capped, the log cascades with its run.
    let long = sqlx::query("UPDATE task_runs SET error = $2 WHERE id = $1")
        .bind(first)
        .bind("e".repeat(501))
        .execute(&pool)
        .await;
    assert!(long.is_err());
    sqlx::query(
        "INSERT INTO task_run_log (run_id, seq, level, message) VALUES ($1, 1, 20, 'hello')",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM task_runs WHERE id = $1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM task_run_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(lines, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn node_heartbeats_and_revoked_tokens_enforce_their_shape() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    for (node, level) in [("", 1), ("n", 0)] {
        let err = sqlx::query(
            "INSERT INTO node_heartbeats (node_id, version, feature_level, started_at, seen_at) \
             VALUES ($1, '1.0', $2, now(), now())",
        )
        .bind(node)
        .bind(level)
        .execute(&pool)
        .await;
        assert!(err.is_err(), "node {node:?} level {level}");
    }
    let err = sqlx::query(
        "INSERT INTO node_heartbeats (node_id, version, feature_level, started_at, seen_at) \
         VALUES ($1, '1.0', 1, now(), now())",
    )
    .bind("n".repeat(201))
    .execute(&pool)
    .await;
    assert!(err.is_err());
    sqlx::query(
        "INSERT INTO node_heartbeats (node_id, version, feature_level, started_at, seen_at) \
         VALUES ('node-1', '1.0', 1, now(), now())",
    )
    .execute(&pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_rows_carry_actor_kind_and_project_and_the_project_index_is_partial() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let index: String = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes WHERE indexname = 'ix_audit_events_project'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(index.contains("project_id IS NOT NULL"), "{index}");
    let column: (String, String) = sqlx::query_as(
        "SELECT data_type, column_default FROM information_schema.columns \
         WHERE table_name = 'audit_events' AND column_name = 'actor_kind'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(column.0, "text");
    assert!(column.1.contains("user"));
}
