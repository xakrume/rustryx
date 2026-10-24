//! The first-start and maintenance tasks against a real PostgreSQL (Docker required).
//!
//! These are the parts of the composition root that only make sense with a schema: creating the
//! administrator and the default blob store exactly once, and the housekeeping that keeps audit
//! writes possible. Each rule has a `must_not_*` test where the requirement is negative.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    missing_docs
)]

use std::sync::Arc;
use std::time::Duration;

use rustryx_auth::passwords::{BlockingGate, GateConfig, PasswordConfig, Passwords};
use rustryx_blob::{BlobStoreRegistry, EnvSecretResolver};
use rustryx_config::{AppConfig, BlobConfig, ConfigSecret};
use rustryx_core::Secret;
use rustryx_core::ports::{
    BlobStoreRepo, LockBackend, LoginAttemptRepo, LoginKey, RevokedTokenRepo, SystemClock,
};
use rustryx_db::{AeadSealer, KeyEntry, PgLockBackend, PgStore};
use rustryx_observability::MetricsHandle;
use rustryx_server::wiring::bootstrap::{AdminSeed, Bootstrap};
use rustryx_server::wiring::maintenance::{Maintenance, Pass};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

mod common;
use common::Postgres;

struct Env {
    postgres: Postgres,
    pool: PgPool,
    store: PgStore,
    config: AppConfig,
    blobs: tempfile::TempDir,
}

async fn env(migrated: bool) -> Env {
    let postgres = Postgres::start().await;
    let mut config = AppConfig::default();
    config.database.url = Some(ConfigSecret::new(postgres.url()));
    if migrated {
        rustryx_server::wiring::migrator()
            .migrate(&config)
            .await
            .expect("migrations apply");
    }
    let pool = PgPool::connect(&postgres.url()).await.unwrap();
    let key = KeyEntry::from_bytes("test", [9; 32]).unwrap();
    let store = PgStore::new(
        pool.clone(),
        Arc::new(AeadSealer::new(key, Vec::new()).unwrap()),
    );
    Env {
        postgres,
        pool,
        store,
        config,
        blobs: tempfile::tempdir().unwrap(),
    }
}

fn passwords() -> Arc<Passwords> {
    Arc::new(
        Passwords::new(
            PasswordConfig::minimum(),
            Arc::new(BlockingGate::new(GateConfig::default())),
        )
        .unwrap(),
    )
}

fn bootstrap(env: &Env, admin: Option<(&str, &str)>) -> Bootstrap {
    let mut blob = BlobConfig::default();
    blob.local_path = env.blobs.path().join("store");
    let definitions: Arc<dyn BlobStoreRepo> = Arc::new(env.store.blob_stores.clone());
    Bootstrap {
        pool: env.pool.clone(),
        passwords: passwords(),
        admin: admin.map(|(username, password)| AdminSeed {
            username: username.to_owned(),
            password: Secret::new(password),
        }),
        blob,
        definitions: Arc::clone(&definitions),
        registry: Arc::new(BlobStoreRegistry::new(
            definitions,
            Arc::new(EnvSecretResolver),
        )),
        clock: Arc::new(SystemClock),
    }
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_start_creates_the_administrator_and_the_default_store_exactly_once() {
    let env = env(true).await;
    let first = bootstrap(&env, Some(("admin", "Correct-Horse-Battery-9")));
    first.once().await.expect("first pass");
    // A second node, or a restart, runs the same pass again.
    bootstrap(&env, Some(("admin", "Correct-Horse-Battery-9")))
        .once()
        .await
        .expect("second pass");

    let admins = count(
        &env.pool,
        "SELECT count(*) FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE r.name = 'nx-admin'",
    )
    .await;
    assert_eq!(admins, 1, "one administrator, however often the pass runs");
    let users = count(
        &env.pool,
        "SELECT count(*) FROM users WHERE username = 'admin'",
    )
    .await;
    assert_eq!(users, 1);
    let stores = count(
        &env.pool,
        "SELECT count(*) FROM blob_stores WHERE name = 'default'",
    )
    .await;
    assert_eq!(stores, 1, "the default store is seeded once");
    assert!(
        env.blobs.path().join("store").is_dir(),
        "a local store opens its root"
    );

    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE username = 'admin'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(
        hash.starts_with("$argon2id$"),
        "the password is stored as an argon2id hash"
    );
    assert!(!hash.contains("Correct-Horse"), "never the password itself");
    env.postgres.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn must_not_create_a_second_administrator_under_another_name() {
    let env = env(true).await;
    bootstrap(&env, Some(("admin", "Correct-Horse-Battery-9")))
        .once()
        .await
        .unwrap();
    bootstrap(&env, Some(("root", "Another-Long-Password-7")))
        .once()
        .await
        .unwrap();
    let roots = count(
        &env.pool,
        "SELECT count(*) FROM users WHERE username = 'root'",
    )
    .await;
    assert_eq!(
        roots, 0,
        "a deployment that already has an administrator gets no break-glass copy"
    );
    env.postgres.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_configured_password_means_no_administrator_but_the_store_is_still_seeded() {
    let env = env(true).await;
    bootstrap(&env, None).once().await.unwrap();
    assert_eq!(count(&env.pool, "SELECT count(*) FROM users").await, 0);
    assert_eq!(
        count(&env.pool, "SELECT count(*) FROM blob_stores").await,
        1
    );
    env.postgres.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_task_waits_for_the_schema_and_then_does_its_work() {
    let env = env(false).await;
    let task = bootstrap(&env, Some(("admin", "Correct-Horse-Battery-9")));
    // Run the real supervised task, not `once`: the wait for the schema is part of its contract.
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut supervisor = rustryx_server::supervisor::Supervisor::new(cancel.clone(), None);
    supervisor.spawn(task.task());
    tokio::time::sleep(Duration::from_secs(1)).await;
    // Nothing exists yet: there is no schema to write into, and the task must not have died.
    assert_eq!(supervisor.running(), 1);
    rustryx_server::wiring::migrator()
        .migrate(&env.config)
        .await
        .expect("migrations apply");
    let mut created = false;
    for _ in 0..60 {
        let users: Result<i64, _> =
            sqlx::query_scalar("SELECT count(*) FROM users WHERE username = 'admin'")
                .fetch_one(&env.pool)
                .await;
        if users.is_ok_and(|n| n == 1) {
            created = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(created, "the administrator appears after the schema does");
    let report = supervisor.shutdown(Duration::from_secs(5)).await;
    assert_eq!(
        report.aborted, 0,
        "a finished bootstrap parks until shutdown and stops cleanly"
    );
    env.postgres.stop().await;
}

fn maintenance(env: &Env) -> (Maintenance, rustryx_observability::JobMetrics) {
    let metrics = MetricsHandle::new().unwrap();
    let job = metrics.job("db_maintenance");
    let locks = PgLockBackend::new(&Secret::new(env.postgres.url())).unwrap();
    (
        Maintenance {
            pool: env.pool.clone(),
            revoked: Arc::new(env.store.revoked_tokens.clone()),
            locks: Arc::new(locks),
            lock_ttl: Duration::from_secs(30),
            metrics,
        },
        job,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maintenance_recreates_missing_audit_partitions_and_purges_dead_rows() {
    let env = env(true).await;
    let partitions = "SELECT count(*) FROM pg_inherits WHERE inhparent = 'audit_events'::regclass";
    let before = count(&env.pool, partitions).await;
    assert!(
        before >= 3,
        "the migration made the current month and two more"
    );
    // Remove the last month: the next audit write of that month would fail without maintenance.
    let last: String = sqlx::query_scalar(
        "SELECT inhrelid::regclass::text FROM pg_inherits WHERE inhparent = 'audit_events'::regclass \
         ORDER BY 1 DESC LIMIT 1",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    // The name comes from the catalog query above, not from input.
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {last}")))
        .execute(&env.pool)
        .await
        .unwrap();

    let now = OffsetDateTime::now_utc();
    env.store
        .revoked_tokens
        .revoke(Uuid::now_v7(), now - time::Duration::days(1))
        .await
        .unwrap();
    env.store
        .login_attempts
        .register_failure(
            &LoginKey {
                account: "ghost".into(),
                ip: None,
            },
            now - time::Duration::days(3),
        )
        .await
        .unwrap();

    let (maintenance, job) = maintenance(&env);
    assert_eq!(maintenance.pass(&job).await, Pass::Done);

    assert_eq!(
        count(&env.pool, partitions).await,
        before,
        "the lookahead is restored"
    );
    assert_eq!(
        count(&env.pool, "SELECT count(*) FROM revoked_tokens").await,
        0
    );
    assert_eq!(
        count(&env.pool, "SELECT count(*) FROM login_attempts").await,
        0
    );
    env.postgres.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn must_not_drop_old_audit_partitions() {
    let env = env(true).await;
    // An ancient partition: past any retention, and exactly what a drop would remove.
    sqlx::query("SELECT ensure_audit_partitions(now() - interval '400 days', 0)")
        .execute(&env.pool)
        .await
        .unwrap();
    let partitions = "SELECT count(*) FROM pg_inherits WHERE inhparent = 'audit_events'::regclass";
    let before = count(&env.pool, partitions).await;
    let (maintenance, job) = maintenance(&env);
    assert_eq!(maintenance.pass(&job).await, Pass::Done);
    assert_eq!(
        count(&env.pool, partitions).await,
        before,
        "retention drops delete evidence and need an archive first: this task never runs them"
    );
    env.postgres.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maintenance_skips_while_another_node_holds_the_lock_and_runs_after() {
    let env = env(true).await;
    let (maintenance, job) = maintenance(&env);
    let other_node = PgLockBackend::new(&Secret::new(env.postgres.url())).unwrap();
    let held = other_node
        .try_acquire("db_maintenance", Duration::from_secs(30))
        .await
        .unwrap()
        .expect("the lock is free");
    assert_eq!(maintenance.pass(&job).await, Pass::Skipped);
    held.release().await.unwrap();
    assert_eq!(maintenance.pass(&job).await, Pass::Done);
    env.postgres.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maintenance_does_nothing_before_the_schema_exists() {
    let env = env(false).await;
    let (maintenance, job) = maintenance(&env);
    assert_eq!(maintenance.pass(&job).await, Pass::SchemaNotReady);
    env.postgres.stop().await;
}
