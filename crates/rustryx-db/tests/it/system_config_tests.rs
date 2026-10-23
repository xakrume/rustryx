use rustryx_core::StoreError;
use rustryx_core::ports::{SecretView, SystemConfigRepo};

use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn values_are_stored_replaced_and_removed() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    let get = |key: &'static str| {
        let config = config.clone();
        async move { config.get(key, SecretView::Mask).await.unwrap() }
    };
    assert!(get("auth.anonymous_enabled").await.is_none());

    config
        .put(
            "auth.anonymous_enabled",
            &serde_json::json!(true),
            &[],
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        get("auth.anonymous_enabled").await.unwrap().value,
        serde_json::json!(true)
    );
    config
        .put(
            "auth.anonymous_enabled",
            &serde_json::json!({ "enabled": false }),
            &[],
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        get("auth.anonymous_enabled").await.unwrap().value,
        serde_json::json!({ "enabled": false })
    );

    config.delete("auth.anonymous_enabled", None).await.unwrap();
    config.delete("auth.anonymous_enabled", None).await.unwrap();
    assert!(get("auth.anonymous_enabled").await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_are_bounded() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    let err = config
        .put("", &serde_json::json!(1), &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    let err = config
        .put(&"k".repeat(201), &serde_json::json!(1), &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
}

use rustryx_core::ports::ConfigEntry;
use serde_json::json;

use crate::fixtures::user;
use crate::support::{listen_rbac, next_payload};

const WAIT: std::time::Duration = std::time::Duration::from_secs(5);
const CANARY: &str = "smtp-password-canary-7731";

async fn put_smtp(
    config: &rustryx_db::PgSystemConfig,
    value: serde_json::Value,
    expected: Option<i64>,
) -> Result<ConfigEntry, StoreError> {
    config
        .put("settings.smtp", &value, &["/password"], expected, None)
        .await
}

async fn raw_value(db: &TestDb, key: &str) -> serde_json::Value {
    sqlx::query_scalar("SELECT value FROM system_config WHERE key = $1")
        .bind(key)
        .fetch_one(&db.pool().await)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sealed_field_is_masked_for_the_api_and_revealed_for_the_runtime() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    let written = put_smtp(
        &config,
        json!({"host": "smtp.example.org", "password": CANARY}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        written.value,
        json!({"host": "smtp.example.org", "_sealed": ["/password"]}),
        "a write does not echo the secret"
    );

    let masked = config
        .get("settings.smtp", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        masked.value,
        json!({"host": "smtp.example.org", "_sealed": ["/password"]})
    );
    let revealed = config
        .get("settings.smtp", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        revealed.value,
        json!({"host": "smtp.example.org", "password": CANARY})
    );
    assert_eq!(revealed.revision, masked.revision);
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_store_a_sealed_setting_in_clear_text() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    put_smtp(&config, json!({"host": "h", "password": CANARY}), None)
        .await
        .unwrap();
    let raw = raw_value(&db, "settings.smtp").await;
    let text = raw.to_string();
    assert!(!text.contains(CANARY), "{text}");
    assert!(
        raw["password"].as_str().unwrap().starts_with("rx1."),
        "{text}"
    );
    assert_eq!(raw["_sealed"], json!(["/password"]));
    // Neither a dump of the whole table nor the masked read has it.
    let dump: String = sqlx::query_scalar("SELECT string_agg(t::text, ' ') FROM system_config t")
        .fetch_one(&db.pool().await)
        .await
        .unwrap();
    assert!(!dump.contains(CANARY));
    let masked = config
        .get("settings.smtp", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert!(!format!("{masked:?} {}", masked.value).contains(CANARY));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blank_field_keeps_the_stored_secret_and_null_clears_it() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    put_smtp(&config, json!({"host": "one", "password": CANARY}), None)
        .await
        .unwrap();
    let before = raw_value(&db, "settings.smtp").await["password"].clone();

    // The form edits the host and leaves the password out: the secret stays.
    put_smtp(&config, json!({"host": "two"}), None)
        .await
        .unwrap();
    let revealed = config
        .get("settings.smtp", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.value, json!({"host": "two", "password": CANARY}));
    assert_eq!(
        raw_value(&db, "settings.smtp").await["password"],
        before,
        "not even re-sealed"
    );

    // A new value replaces it (a fresh envelope: the nonce is random).
    put_smtp(&config, json!({"host": "two", "password": "other"}), None)
        .await
        .unwrap();
    assert_ne!(raw_value(&db, "settings.smtp").await["password"], before);
    let revealed = config
        .get("settings.smtp", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.value["password"], "other");

    // null clears it: not sealed any more, and not listed as set.
    put_smtp(&config, json!({"host": "two", "password": null}), None)
        .await
        .unwrap();
    let masked = config
        .get("settings.smtp", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(masked.value, json!({"host": "two", "password": null}));
    assert!(
        raw_value(&db, "settings.smtp")
            .await
            .get("_sealed")
            .is_none()
    );
    // And a later blank save has nothing to carry over.
    put_smtp(&config, json!({"host": "three"}), None)
        .await
        .unwrap();
    let revealed = config
        .get("settings.smtp", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.value, json!({"host": "three"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_pointers_and_several_sealed_fields_work_together() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    let value = json!({"proxy": {"url": "http://p", "auth": {"password": "p1", "user": "u"}}, "token": "t1"});
    config
        .put(
            "settings.proxy",
            &value,
            &["/proxy/auth/password", "/token"],
            None,
            None,
        )
        .await
        .unwrap();
    let masked = config
        .get("settings.proxy", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        masked.value,
        json!({
            "proxy": {"url": "http://p", "auth": {"user": "u"}},
            "_sealed": ["/proxy/auth/password", "/token"]
        })
    );
    let revealed = config
        .get("settings.proxy", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.value, value);

    // Leaving both out keeps both, even when the whole branch is absent from the new value.
    config
        .put(
            "settings.proxy",
            &json!({"proxy": {"url": "http://q"}}),
            &["/proxy/auth/password", "/token"],
            None,
            None,
        )
        .await
        .unwrap();
    let revealed = config
        .get("settings.proxy", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revealed.value["proxy"]["auth"]["password"], "p1");
    assert_eq!(revealed.value["token"], "t1");
    assert_eq!(revealed.value["proxy"]["url"], "http://q");
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_open_a_sealed_setting_copied_to_another_key_or_field() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    put_smtp(&config, json!({"host": "h", "password": CANARY}), None)
        .await
        .unwrap();
    config
        .put(
            "settings.other",
            &json!({"x": "y", "secret": "z"}),
            &["/secret"],
            None,
            None,
        )
        .await
        .unwrap();
    let pool = db.pool().await;

    // The whole value of one key copied over another key.
    sqlx::query("UPDATE system_config SET value = (SELECT value FROM system_config WHERE key = 'settings.smtp') WHERE key = 'settings.other'")
        .execute(&pool)
        .await
        .unwrap();
    let err = config
        .get("settings.other", SecretView::Reveal)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    assert!(!format!("{err} {err:?}").contains(CANARY));
    // Masked reads never decrypt, so they still work and still show nothing.
    assert!(config.get("settings.other", SecretView::Mask).await.is_ok());

    // The envelope of one field moved to another field of the same key.
    sqlx::query(
        "UPDATE system_config SET value = jsonb_set(jsonb_set(value, '{host}', value -> 'password'), '{_sealed}', '[\"/host\"]') \
         WHERE key = 'settings.smtp'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let err = config
        .get("settings.smtp", SecretView::Reveal)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn revisions_are_compare_and_set_and_come_from_one_sequence() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let config = &store.system_config;
    let author = user(&store).await;
    assert_eq!(config.current_revision().await.unwrap(), 0);

    let first = config
        .put("settings.a", &json!({"v": 1}), &[], None, Some(author))
        .await
        .unwrap();
    assert_eq!(first.updated_by, Some(author));
    let second = config
        .put("settings.b", &json!({"v": 1}), &[], None, None)
        .await
        .unwrap();
    assert!(second.revision > first.revision);
    assert_eq!(config.current_revision().await.unwrap(), second.revision);

    // Edit with the revision that was read: fine, and the revision moves on.
    let edited = config
        .put(
            "settings.a",
            &json!({"v": 2}),
            &[],
            Some(first.revision),
            None,
        )
        .await
        .unwrap();
    assert!(edited.revision > second.revision);
    assert_eq!(edited.updated_by, None, "the author is the latest writer");

    // A second editor still holding the old revision is refused, and nothing is written.
    let err = config
        .put(
            "settings.a",
            &json!({"v": 3}),
            &[],
            Some(first.revision),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    let now_stored = config
        .get("settings.a", SecretView::Mask)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(now_stored.value, json!({"v": 2}));
    assert_eq!(now_stored.revision, edited.revision);

    // An expected revision for a key that does not exist is stale by definition.
    let err = config
        .put("settings.none", &json!({}), &[], Some(1), None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    assert!(
        config
            .get("settings.none", SecretView::Mask)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_node_revision_follows_settings_and_system_keys_only() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    config
        .put("quota.counters_ready", &json!(true), &[], None, None)
        .await
        .unwrap();
    assert_eq!(
        config.current_revision().await.unwrap(),
        0,
        "another namespace"
    );
    let settings = config
        .put("settings.logging", &json!({}), &[], None, None)
        .await
        .unwrap();
    let system = config
        .put(
            "system.read_only",
            &json!({"enabled": false}),
            &[],
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        config.current_revision().await.unwrap(),
        system.revision.max(settings.revision)
    );
    config
        .put("quota.counters_ready", &json!(false), &[], None, None)
        .await
        .unwrap();
    assert_eq!(
        config.current_revision().await.unwrap(),
        system.revision,
        "a write outside the namespaces does not move it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_requests_are_refused_before_anything_is_written() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    let cases: Vec<(&str, serde_json::Value, Vec<&str>)> = vec![
        ("reserved", json!({"_sealed": ["/x"]}), vec![]),
        ("bad-pointer", json!({"a": "b"}), vec!["a"]),
        ("empty-token", json!({"a": "b"}), vec!["/a//b"]),
        ("into-sealed", json!({"a": "b"}), vec!["/_sealed"]),
        ("duplicate", json!({"a": "b"}), vec!["/a", "/a"]),
        ("not-a-string", json!({"a": 5}), vec!["/a"]),
        ("scalar-root", json!("text"), vec!["/a"]),
    ];
    for (key, value, seal) in cases {
        let err = config
            .put(&format!("settings.{key}"), &value, &seal, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{key}: {err:?}");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM system_config")
        .fetch_one(&db.pool().await)
        .await
        .unwrap();
    assert_eq!(count, 0);

    // A stored secret that has to be carried over into a place a scalar now occupies cannot
    // be placed: refused, and the stored value stays as it was.
    config
        .put(
            "settings.blocked",
            &json!({"c": {"d": "secret"}}),
            &["/c/d"],
            None,
            None,
        )
        .await
        .unwrap();
    let err = config
        .put(
            "settings.blocked",
            &json!({"c": "scalar"}),
            &["/c/d"],
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    let kept = config
        .get("settings.blocked", SecretView::Reveal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.value, json!({"c": {"d": "secret"}}));
    assert!(matches!(
        config.get("", SecretView::Mask).await.unwrap_err(),
        StoreError::Invalid(_)
    ));
    assert!(matches!(
        config.delete(&"k".repeat(201), None).await.unwrap_err(),
        StoreError::Invalid(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_prefix_is_literal_ordered_and_honours_the_view() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    put_smtp(&config, json!({"host": "h", "password": CANARY}), None)
        .await
        .unwrap();
    for key in [
        "settings.logging",
        "settings_x",
        "settings%y",
        "system.read_only",
    ] {
        config
            .put(key, &json!({"k": key}), &[], None, None)
            .await
            .unwrap();
    }
    let keys = |entries: Vec<ConfigEntry>| entries.into_iter().map(|e| e.key).collect::<Vec<_>>();
    assert_eq!(
        keys(
            config
                .list_prefix("settings.", SecretView::Mask)
                .await
                .unwrap()
        ),
        ["settings.logging", "settings.smtp"]
    );
    assert_eq!(
        keys(
            config
                .list_prefix("settings_", SecretView::Mask)
                .await
                .unwrap()
        ),
        ["settings_x"]
    );
    assert_eq!(
        keys(
            config
                .list_prefix("settings%", SecretView::Mask)
                .await
                .unwrap()
        ),
        ["settings%y"]
    );
    assert_eq!(
        config
            .list_prefix("", SecretView::Mask)
            .await
            .unwrap()
            .len(),
        5
    );

    let masked = config
        .list_prefix("settings.smtp", SecretView::Mask)
        .await
        .unwrap();
    assert!(!masked[0].value.to_string().contains(CANARY));
    let revealed = config
        .list_prefix("settings.smtp", SecretView::Reveal)
        .await
        .unwrap();
    assert_eq!(revealed[0].value["password"], CANARY);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_announce_their_revision_after_commit_and_a_refused_write_announces_nothing() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    let mut listener = listen_rbac(&db).await;

    let written = config
        .put(
            "settings.logging",
            &json!({"level": "info"}),
            &[],
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        next_payload(&mut listener, WAIT).await,
        Some(format!("config:{}", written.revision))
    );

    let err = config
        .put(
            "settings.logging",
            &json!({"level": "debug"}),
            &[],
            Some(written.revision + 100),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
    assert_eq!(
        next_payload(&mut listener, std::time::Duration::from_millis(400)).await,
        None
    );

    config.delete("settings.logging", None).await.unwrap();
    let announced = next_payload(&mut listener, WAIT)
        .await
        .expect("a delete is announced");
    let revision: i64 = announced.strip_prefix("config:").unwrap().parse().unwrap();
    assert!(
        revision > written.revision,
        "a removal takes a fresh revision"
    );

    config.delete("settings.logging", None).await.unwrap();
    assert_eq!(
        next_payload(&mut listener, std::time::Duration::from_millis(400)).await,
        None,
        "deleting what is not there announces nothing"
    );
}
