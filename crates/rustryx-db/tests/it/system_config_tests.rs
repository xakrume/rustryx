use rustryx_core::StoreError;

use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn values_are_stored_replaced_and_removed() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    assert!(
        config
            .get("auth.anonymous_enabled")
            .await
            .unwrap()
            .is_none()
    );

    config
        .set("auth.anonymous_enabled", &serde_json::json!(true))
        .await
        .unwrap();
    assert_eq!(
        config.get("auth.anonymous_enabled").await.unwrap(),
        Some(serde_json::json!(true))
    );
    config
        .set(
            "auth.anonymous_enabled",
            &serde_json::json!({ "enabled": false }),
        )
        .await
        .unwrap();
    assert_eq!(
        config.get("auth.anonymous_enabled").await.unwrap(),
        Some(serde_json::json!({ "enabled": false }))
    );

    config.delete("auth.anonymous_enabled").await.unwrap();
    config.delete("auth.anonymous_enabled").await.unwrap();
    assert!(
        config
            .get("auth.anonymous_enabled")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_are_bounded() {
    let db = TestDb::new().await;
    let config = db.store().await.system_config;
    let err = config.set("", &serde_json::json!(1)).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    let err = config
        .set(&"k".repeat(201), &serde_json::json!(1))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}
