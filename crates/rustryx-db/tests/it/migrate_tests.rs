use std::collections::HashSet;

use rustryx_core::{Secret, StoreError};
use rustryx_db::migrate::{embedded_migration_count, expected_schema_version};
use rustryx_db::{SchemaStatus, migrate, schema_status};
use tokio::task::JoinSet;

use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn migrate_creates_the_schema_and_is_idempotent() {
    let db = TestDb::empty().await;
    let first = migrate(&db.url).await.expect("first migrate");
    let migration_count = embedded_migration_count();
    assert_eq!(first.applied.len(), migration_count);
    assert_eq!(first.applied[0].version, 1);
    assert_eq!(first.applied[0].description, "core");
    assert_eq!(first.schema_version, expected_schema_version());
    let second = migrate(&db.url).await.expect("second migrate");
    assert!(second.applied.is_empty());
    assert_eq!(second.schema_version, first.schema_version);

    let pool = db.pool().await;
    assert_eq!(schema_status(&pool).await.unwrap(), SchemaStatus::Current);
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for expected in [
        "assets",
        "audit_events",
        "blob_stores",
        "blobs",
        "components",
        "content_selectors",
        "idp_group_mappings",
        "job_leases",
        "login_attempts",
        "privileges",
        "repositories",
        "role_privileges",
        "role_roles",
        "roles",
        "schema_contract",
        "system_config",
        "user_roles",
        "user_tokens",
        "users",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_migrators_apply_every_migration_exactly_once() {
    let db = TestDb::empty().await;
    let mut tasks = JoinSet::new();
    for _ in 0..4 {
        let url = db.url.clone();
        tasks.spawn(async move { migrate(&url).await });
    }
    let mut applied_versions = Vec::new();
    let mut busy_runs = 0;
    while let Some(joined) = tasks.join_next().await {
        let report = joined.unwrap().expect("no caller fails");
        if !report.applied.is_empty() {
            busy_runs += 1;
        }
        applied_versions.extend(report.applied.iter().map(|m| m.version));
    }
    assert_eq!(
        busy_runs, 1,
        "one caller did the work, the others found nothing left"
    );
    let migration_count = embedded_migration_count();
    let unique: HashSet<_> = applied_versions.iter().collect();
    assert_eq!(applied_versions.len(), migration_count);
    assert_eq!(unique.len(), migration_count, "no migration ran twice");
    let pool = db.pool().await;
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        recorded,
        i64::try_from(migration_count).expect("count fits i64")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_database_is_behind_and_not_serving() {
    let db = TestDb::empty().await;
    let pool = db.pool().await;
    let status = schema_status(&pool).await.unwrap();
    assert_eq!(
        status,
        SchemaStatus::Behind {
            applied: 0,
            expected: expected_schema_version()
        }
    );
    assert!(!status.is_serving());
    assert_eq!(status.as_str(), "behind");
    assert_eq!(status.to_string(), "behind");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_schema_older_than_the_binary_is_behind() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    sqlx::query(
        "DELETE FROM _sqlx_migrations WHERE version = (SELECT max(version) FROM _sqlx_migrations)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let status = schema_status(&pool).await.unwrap();
    let expected = expected_schema_version();
    assert_eq!(
        status,
        SchemaStatus::Behind {
            applied: expected - 1,
            expected
        }
    );
    assert!(!status.is_serving(), "readiness stays at 503");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_schema_keeps_an_older_binary_ready_until_a_contract_migration() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let expected = expected_schema_version();
    // The migrate job of the next release ran: one expand migration on top of ours.
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES ($1, 'next release', true, '\\x00', 0)",
    )
    .bind(expected + 1)
    .execute(&pool)
    .await
    .unwrap();
    let status = schema_status(&pool).await.unwrap();
    assert_eq!(
        status,
        SchemaStatus::OlderCompatible {
            applied: expected + 1,
            expected
        }
    );
    assert!(status.is_serving(), "this binary is N-1 and still works");

    // A contract migration of the release after that drops what this binary reads.
    sqlx::query("UPDATE schema_contract SET min_binary_schema = $1")
        .bind(expected + 1)
        .execute(&pool)
        .await
        .unwrap();
    let status = schema_status(&pool).await.unwrap();
    assert_eq!(
        status,
        SchemaStatus::Ahead {
            applied: expected + 1,
            expected
        }
    );
    assert!(!status.is_serving());
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_run_migrations_when_the_history_was_tampered_with() {
    let db = TestDb::new().await;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1")
        .execute(&db.pool().await)
        .await
        .unwrap();
    let err = migrate(&db.url).await.unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    assert!(err.to_string().contains("apply migrations"));
}

#[tokio::test(flavor = "multi_thread")]
async fn migrate_reports_unreachable_servers_and_bad_urls_without_the_password() {
    let err = migrate(&Secret::new("postgres://svc:pw-canary-9@127.0.0.1:1/none"))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Unavailable(_)), "{err:?}");
    assert!(!format!("{err} | {err:?}").contains("pw-canary-9"));
    let err = migrate(&Secret::new("postgres://svc:pw-canary-9@h:badport/x"))
        .await
        .unwrap_err();
    assert!(!format!("{err} | {err:?}").contains("pw-canary-9"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_database_refuses_a_second_default_style_schema_rewrite() {
    // The schema contract row exists exactly once and starts at the first version.
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let min: i64 = sqlx::query_scalar("SELECT min_binary_schema FROM schema_contract")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(min, 1);
    let err = sqlx::query("INSERT INTO schema_contract (id, min_binary_schema) VALUES (false, 1)")
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(err.as_database_error().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binary_older_than_the_schema_refuses_to_migrate_instead_of_guessing() {
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (999, 'from a newer release', true, '\\x00', 0)",
    )
    .execute(&db.pool().await)
    .await
    .unwrap();
    let err = migrate(&db.url).await.unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "store backend failure during apply migrations"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_applied_migration_blocks_further_runs() {
    let db = TestDb::new().await;
    sqlx::query("UPDATE _sqlx_migrations SET success = false WHERE version = 3")
        .execute(&db.pool().await)
        .await
        .unwrap();
    let err = migrate(&db.url).await.unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    let pool = db.pool().await;
    let status = schema_status(&pool).await.unwrap();
    assert_eq!(
        status,
        SchemaStatus::Behind {
            applied: 2,
            expected: expected_schema_version()
        }
    );
    assert!(!status.is_serving());
}
