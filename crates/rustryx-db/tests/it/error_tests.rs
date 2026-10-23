use std::time::Duration;

use rustryx_core::identity::{NewUser, UserSource};
use rustryx_core::ports::{BlobStoreRepo, UserRepo};
use rustryx_core::{Secret, StoreError};
use rustryx_db::{PgConfig, connect, connect_lazy, ping};

use crate::fixtures::{blob_store, unique};
use crate::support::TestDb;

#[derive(Clone, Copy)]
enum Expect {
    Backend,
    Unavailable,
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_put_the_conflicting_value_in_the_error() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let secret_name = unique("customer-secret-name");
    store
        .blob_stores
        .create(&blob_store(&secret_name))
        .await
        .unwrap();
    let err = store
        .blob_stores
        .create(&blob_store(&secret_name))
        .await
        .unwrap_err();
    let shown = format!("{err} | {err:?}");
    assert!(!shown.contains(&secret_name), "{shown}");
    assert!(!shown.contains("INSERT"), "{shown}");
    assert!(!shown.contains("Key ("), "{shown}");

    let username = unique("someone");
    let new = NewUser {
        username: username.clone(),
        source: UserSource::Ldap,
        issuer: None,
        subject: None,
        email: None,
        display_name: None,
        password_hash: None,
    };
    store.users.create(&new).await.unwrap();
    let err = store.users.create(&new).await.unwrap_err();
    assert!(!format!("{err} | {err:?}").contains(&username));
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_leak_sql_text_or_row_values_from_a_backend_error() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let sql =
        "SELECT canary_column_name FROM canary_table_name WHERE token = 'canary-secret-value'";
    let raw = sqlx::query(sql).execute(&pool).await.unwrap_err();
    let mapped = rustryx_db::error::map_err("probe", raw);
    let shown = format!("{mapped} | {mapped:?}");
    assert!(matches!(mapped, StoreError::Backend { .. }), "{shown}");
    assert!(!shown.contains("canary-secret-value"), "{shown}");
    assert!(!shown.contains("SELECT"), "{shown}");
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_use_the_sql_safety_escape_hatch_in_library_code() {
    // sqlx 0.9 refuses a non-static SQL string unless it is wrapped in `AssertSqlSafe`. The
    // library never wraps one: every statement is a literal or built by `QueryBuilder`.
    let mut pending = vec![std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src"
    ))];
    let mut checked = 0;
    while let Some(dir) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();
            if entry.file_type().await.unwrap().is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = tokio::fs::read_to_string(&path).await.unwrap();
                let code: String = text
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(!code.contains("AssertSqlSafe"), "{}", path.display());
                assert!(!code.contains("raw_sql"), "{}", path.display());
                checked += 1;
            }
        }
    }
    assert!(checked > 15, "walked the source tree ({checked} files)");
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_answers_and_a_closed_pool_is_unavailable() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    ping(&pool).await.unwrap();
    pool.close().await;
    let err = ping(&pool).await.unwrap_err();
    assert!(matches!(err, StoreError::Unavailable(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_is_unavailable_and_lazy_pools_start_anyway() {
    let mut config = PgConfig::new(Secret::new("postgres://u:p@127.0.0.1:1/none"));
    config.acquire_timeout = Duration::from_millis(500);
    let err = connect(&config).await.unwrap_err();
    assert!(matches!(err, StoreError::Unavailable(_)), "{err:?}");

    let lazy = connect_lazy(&config).expect("a lazy pool does not touch the network");
    let started = std::time::Instant::now();
    let err = ping(&lazy).await.unwrap_err();
    assert!(matches!(err, StoreError::Unavailable(_)), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "ping is bounded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_leak_the_password_when_connecting_fails() {
    let config = PgConfig::new(Secret::new("postgres://svc:pw-canary-123@127.0.0.1:1/none"));
    let err = connect(&config).await.unwrap_err();
    assert!(!format!("{err} | {err:?}").contains("pw-canary-123"));
    let bad = PgConfig::new(Secret::new("postgres://svc:pw-canary-123@host:notaport/db"));
    let err = connect(&bad).await.unwrap_err();
    assert!(!format!("{err} | {err:?}").contains("pw-canary-123"));
    assert!(!format!("{config:?}").contains("pw-canary-123"));
}

#[tokio::test(flavor = "multi_thread")]
async fn statement_timeout_and_pool_limits_are_applied_to_every_connection() {
    let db = TestDb::new().await;
    let mut config = PgConfig::new(db.url.clone());
    config.statement_timeout = Some(Duration::from_millis(1500));
    config.max_connections = 3;
    config.application_name = "rustryx-test".to_owned();
    let pool = connect(&config).await.unwrap();
    let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(timeout, "1500ms");
    let name: String = sqlx::query_scalar("SHOW application_name")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "rustryx-test");
    let err = sqlx::query("SELECT pg_sleep(5)")
        .execute(&pool)
        .await
        .unwrap_err();
    let mapped = rustryx_db::error::map_err("slow", err);
    assert!(matches!(mapped, StoreError::Unavailable(_)), "{mapped:?}");
    assert_eq!(pool.options().get_max_connections(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn database_error_classes_map_to_store_errors() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let cases: [(&str, Expect); 4] = [
        ("SELECT 1/0", Expect::Backend),
        ("SELECT * FROM table_that_does_not_exist", Expect::Backend),
        (
            "SELECT pg_terminate_backend(pg_backend_pid())",
            Expect::Unavailable,
        ),
        (
            "SELECT 1 WHERE 1 = (SELECT 1 UNION ALL SELECT 2)",
            Expect::Backend,
        ),
    ];
    for (sql, check) in cases {
        let err = sqlx::query(sql).execute(&pool).await.unwrap_err();
        let mapped = rustryx_db::error::map_err("case", err);
        let ok = match check {
            Expect::Backend => matches!(mapped, StoreError::Backend { .. }),
            Expect::Unavailable => matches!(mapped, StoreError::Unavailable(_)),
        };
        assert!(ok, "{sql}: {mapped:?}");
    }
}
