//! The SQL policy guards (`RX004` to `RX008`) as the typed and the phase 1 methods report them.
//!
//! The guards themselves belong to later migrations (immutability, holds, read only mode), so
//! these tests install stand-in triggers that raise the documented SQLSTATEs with the
//! documented DETAIL and check what `rustryx-db` makes of them. What is proved here is the
//! contract between the guard text and the Rust side: the typed variant, the flattened phase 1
//! conflict, and the rule that no guard text reaches an error message.

use rustryx_core::StoreError;
use rustryx_core::error::{MutationError, PolicyViolation};
use rustryx_core::ids::{ImmutabilityRuleId, LifecycleHoldId, ProjectId};
use rustryx_core::model::WriteOptions;
use rustryx_core::ports::{AssetRepo, BlobRepo, ComponentRepo};
use rustryx_core::quota::QuotaScope;
use uuid::Uuid;

use crate::fixtures::{blob, new_asset, new_component, publish, seed_repository};
use crate::support::TestDb;

const GUARD_MESSAGE: &str = "guard-message-canary";

/// Installs a trigger on `table` that raises `sqlstate` with `detail` unless the transaction
/// carries the override setting (like the real immutability guard does).
async fn install_guard(
    pool: &sqlx::PgPool,
    table: &str,
    events: &str,
    sqlstate: &str,
    detail: Option<&str>,
) {
    let detail_clause = detail.map_or_else(String::new, |d| format!(", DETAIL = '{d}'"));
    let statements = [
        format!(
            "CREATE FUNCTION test_guard_{table}() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
               IF current_setting('rustryx.override_immutable', true) = 'on' THEN \
                 IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; \
                 RETURN NEW; \
               END IF; \
               RAISE EXCEPTION '{GUARD_MESSAGE}' USING ERRCODE = '{sqlstate}'{detail_clause}; \
             END $$"
        ),
        format!(
            "CREATE TRIGGER test_guard BEFORE {events} ON {table} \
             FOR EACH ROW EXECUTE FUNCTION test_guard_{table}()"
        ),
    ];
    for statement in statements {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(pool)
            .await
            .unwrap();
    }
}

fn typed(err: MutationError) -> PolicyViolation {
    match err {
        MutationError::Violation(violation) => violation,
        other => panic!("expected a violation, got {other:?}"),
    }
}

struct Case {
    sqlstate: &'static str,
    detail: Option<String>,
    expected: PolicyViolation,
    text: &'static str,
}

fn cases() -> Vec<Case> {
    let rule = Uuid::now_v7();
    let hold = Uuid::now_v7();
    let scope = Uuid::now_v7();
    vec![
        Case {
            sqlstate: "RX004",
            detail: Some(rule.to_string()),
            expected: PolicyViolation::Immutable {
                rule: ImmutabilityRuleId::from_uuid(rule),
            },
            text: "item is immutable",
        },
        Case {
            sqlstate: "RX005",
            detail: Some(hold.to_string()),
            expected: PolicyViolation::Held {
                hold: LifecycleHoldId::from_uuid(hold),
            },
            text: "item is under a hold",
        },
        Case {
            sqlstate: "RX006",
            detail: Some(format!("project:{scope}:1000:900:200")),
            expected: PolicyViolation::QuotaExceeded(rustryx_core::quota::QuotaExceeded {
                scope: QuotaScope::Project(ProjectId::from_uuid(scope)),
                limit_bytes: 1000,
                used_bytes: 900,
                requested_bytes: 200,
            }),
            text: "quota exceeded",
        },
        Case {
            sqlstate: "RX007",
            detail: None,
            expected: PolicyViolation::ReadOnly,
            text: "read only mode",
        },
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn every_guard_is_a_typed_violation_on_the_typed_methods_and_a_conflict_on_the_old_ones() {
    for case in cases() {
        let db = TestDb::new().await;
        let store = db.store().await;
        let (bs, repo) = seed_repository(&store).await;
        publish(&store, repo.id, bs.id, "kept").await;
        let component = store
            .components
            .find_or_create(&new_component(repo.id, None, "lib", Some("1")))
            .await
            .unwrap();
        install_guard(
            store.pool(),
            "assets",
            "INSERT OR UPDATE OF blob_id OR DELETE",
            case.sqlstate,
            case.detail.as_deref(),
        )
        .await;
        install_guard(
            store.pool(),
            "components",
            "DELETE",
            case.sqlstate,
            case.detail.as_deref(),
        )
        .await;
        let label = case.sqlstate;
        let options = WriteOptions::new();

        // Insert of a new path and switch of an existing one.
        for path in ["brand-new", "kept"] {
            let record = blob(bs.id, 4);
            let err = store
                .assets
                .publish_with(&new_asset(repo.id, path, record.clone()), &options)
                .await
                .unwrap_err();
            assert_eq!(typed(err), case.expected, "{label} publish {path}");
            assert!(
                store.blobs.get(record.id).await.unwrap().is_none(),
                "{label}: the refused publish left a blob row"
            );
            let err = store
                .assets
                .publish(&new_asset(repo.id, path, blob(bs.id, 4)))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, StoreError::Conflict(m) if m == case.text),
                "{label}: {err:?}"
            );
        }
        // Delete of an asset, typed and flattened.
        let err = store
            .assets
            .delete_by_path_checked(repo.id, "kept", &options)
            .await
            .unwrap_err();
        assert_eq!(typed(err), case.expected, "{label} delete asset");
        let err = store
            .assets
            .delete_by_path(repo.id, "kept")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, StoreError::Conflict(m) if m == case.text),
            "{label}: {err:?}"
        );
        assert!(
            store
                .assets
                .get_by_path(repo.id, "kept")
                .await
                .unwrap()
                .is_some()
        );
        // Delete of a component.
        let err = store
            .components
            .delete_checked(component.id, &options)
            .await
            .unwrap_err();
        assert_eq!(typed(err), case.expected, "{label} delete component");
        let err = store.components.delete(component.id).await.unwrap_err();
        assert!(
            matches!(&err, StoreError::Conflict(m) if m == case.text),
            "{label}: {err:?}"
        );
        assert!(store.components.get(component.id).await.unwrap().is_some());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scope_violation_is_a_conflict_for_both_flavours() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    install_guard(store.pool(), "assets", "INSERT", "RX008", None).await;
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "x", blob(bs.id, 1)),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, MutationError::Store(StoreError::Conflict(m)) if m == "scope violation"),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guard_without_the_documented_detail_degrades_to_a_fixed_conflict() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    // RX004 with a DETAIL that is not a rule id, as an older or hand written guard might send.
    install_guard(
        store.pool(),
        "assets",
        "INSERT",
        "RX004",
        Some("canary-detail-value"),
    )
    .await;
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "x", blob(bs.id, 1)),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, MutationError::Store(StoreError::Conflict(m)) if m == "item is immutable"),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_copy_guard_text_or_detail_into_an_error() {
    for (code, detail) in [
        ("RX004", "canary-detail-value"),
        ("RX005", "canary-detail-value"),
        ("RX006", "canary-detail-value"),
        ("RX007", "canary-detail-value"),
        ("RX008", "canary-detail-value"),
    ] {
        let db = TestDb::new().await;
        let store = db.store().await;
        let (bs, repo) = seed_repository(&store).await;
        install_guard(store.pool(), "assets", "INSERT", code, Some(detail)).await;
        let typed_err = store
            .assets
            .publish_with(
                &new_asset(repo.id, "x", blob(bs.id, 1)),
                &WriteOptions::new(),
            )
            .await
            .unwrap_err();
        let plain_err = store
            .assets
            .publish(&new_asset(repo.id, "y", blob(bs.id, 1)))
            .await
            .unwrap_err();
        let shown = format!("{typed_err} | {typed_err:?} | {plain_err} | {plain_err:?}");
        for forbidden in ["canary-detail-value", GUARD_MESSAGE, "INSERT", "assets"] {
            assert!(!shown.contains(forbidden), "{code}: {shown}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_immutability_override_reaches_the_guard_only_when_asked_for() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    publish(&store, repo.id, bs.id, "protected").await;
    let rule = Uuid::now_v7();
    install_guard(
        store.pool(),
        "assets",
        "UPDATE OF blob_id OR DELETE",
        "RX004",
        Some(&rule.to_string()),
    )
    .await;

    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "protected", blob(bs.id, 9)),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(typed(err), PolicyViolation::Immutable { .. }));

    let allowed = WriteOptions::new().with_override_immutable(true);
    let outcome = store
        .assets
        .publish_with(&new_asset(repo.id, "protected", blob(bs.id, 9)), &allowed)
        .await
        .unwrap();
    assert!(!outcome.created);
    let freed = store
        .assets
        .delete_by_path_checked(repo.id, "protected", &allowed)
        .await
        .unwrap();
    assert!(freed.is_some(), "an administrator driven run may delete");
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_carry_the_override_into_the_next_transaction_of_a_pooled_connection() {
    let db = TestDb::new().await;
    // One connection only: the transaction after the override runs on the SAME session.
    let mut config = rustryx_db::PgConfig::new(db.url.clone());
    config.max_connections = 1;
    let pool = rustryx_db::connect(&config).await.unwrap();
    let store = rustryx_db::PgStore::new(pool, crate::support::test_sealer());
    let (bs, repo) = seed_repository(&store).await;
    publish(&store, repo.id, bs.id, "p").await;

    let allowed = WriteOptions::new().with_override_immutable(true);
    store
        .assets
        .publish_with(&new_asset(repo.id, "p", blob(bs.id, 2)), &allowed)
        .await
        .unwrap();

    let leaked: Option<String> =
        sqlx::query_scalar("SELECT current_setting('rustryx.override_immutable', true)")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_ne!(
        leaked.as_deref(),
        Some("on"),
        "the setting must die with the transaction"
    );

    // And a guard installed now still refuses an ordinary publish on that same connection.
    install_guard(store.pool(), "assets", "UPDATE OF blob_id", "RX007", None).await;
    let err = store
        .assets
        .publish_with(
            &new_asset(repo.id, "p", blob(bs.id, 3)),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(typed(err), PolicyViolation::ReadOnly);
}
