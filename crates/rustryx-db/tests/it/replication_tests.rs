//! Replication rules: unique names, versioned edits, and a registry that stays while a rule uses it.

use rustryx_core::StoreError;
use rustryx_core::ids::{ProjectId, RegistryId, ReplicationRuleId};
use rustryx_core::ports::{RegistryRepo, ReplicationRepo};
use rustryx_core::registry::{Registry, RegistryProvider};
use rustryx_core::replication::{
    ReplicationDirection, ReplicationRule, ReplicationScope, ReplicationTrigger,
};
use time::OffsetDateTime;

use crate::fixtures::unique;
use crate::support::{TestDb, test_sealer};

#[tokio::test(flavor = "multi_thread")]
async fn a_replication_rule_keeps_its_registry_until_the_rule_is_gone() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let registries = rustryx_db::repos::PgRegistryRepo::new(pool.clone(), test_sealer());
    let rules = rustryx_db::repos::PgReplicationRepo::new(pool.clone());

    let registry = Registry::new(
        RegistryId::generate(),
        unique("reg"),
        RegistryProvider::Oci,
        "https://harbor.example",
        OffsetDateTime::now_utc(),
    );
    registries.create(&registry).await.unwrap();
    let project = ProjectId::generate();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(project.as_uuid())
        .bind(unique("repl"))
        .execute(&pool)
        .await
        .unwrap();

    let mut rule = ReplicationRule::new(
        ReplicationRuleId::generate(),
        "copy-app",
        ReplicationDirection::Push,
        registry.id,
        ReplicationScope::Project(project),
        OffsetDateTime::now_utc(),
    );
    rule.filter.name_globs = vec!["app/*".to_owned()];
    rule.trigger = ReplicationTrigger::Event {
        on_push: true,
        on_delete: false,
    };
    rules.create_rule(&rule).await.unwrap();
    assert!(matches!(
        rules.create_rule(&rule).await,
        Err(StoreError::Conflict(_))
    ));
    let mut silent = rule.clone();
    silent.id = ReplicationRuleId::generate();
    silent.name = "silent".to_owned();
    silent.trigger = ReplicationTrigger::Event {
        on_push: false,
        on_delete: false,
    };
    assert!(matches!(
        rules.create_rule(&silent).await,
        Err(StoreError::Conflict(_))
    ));

    let stored = rules.get_rule(rule.id).await.unwrap().unwrap();
    assert_eq!(stored.filter.name_globs, ["app/*"]);
    assert_eq!(stored.trigger, rule.trigger);
    assert!(matches!(
        rules.update_rule(&stored, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut edited = stored.clone();
    edited.enabled = false;
    edited.updated_at = OffsetDateTime::now_utc();
    rules.update_rule(&edited, 1).await.unwrap();
    assert!(!rules.get_rule(rule.id).await.unwrap().unwrap().enabled);

    let dependents = registries.dependents(registry.id).await.unwrap();
    assert_eq!(dependents.replication_rules, vec![rule.id]);
    assert!(matches!(
        registries.delete(registry.id).await,
        Err(StoreError::Conflict(_))
    ));
    rules.delete_rule(rule.id).await.unwrap();
    registries.delete(registry.id).await.unwrap();
    assert!(matches!(
        rules.delete_rule(rule.id).await,
        Err(StoreError::NotFound(_))
    ));
}
