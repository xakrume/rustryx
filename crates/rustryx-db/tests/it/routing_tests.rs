use rustryx_core::StoreError;
use rustryx_core::ids::RoutingRuleId;
use rustryx_core::ports::{RepositoryRepo, RoutingRuleRepo};
use rustryx_core::routing::{RoutingMode, RoutingRule};

use crate::fixtures::{now, seed_repository};
use crate::support::TestDb;

fn rule(name: &str, patterns: &[&str]) -> RoutingRule {
    RoutingRule::new(
        RoutingRuleId::generate(),
        name,
        RoutingMode::Block,
        patterns.iter().map(|p| (*p).to_owned()).collect(),
        now(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn create_get_and_list_by_name() {
    let db = TestDb::new().await;
    let rules = db.store().await.routing_rules;
    let mut zeta = rule("zeta", &["^com/zeta/", "-SNAPSHOT/"]);
    zeta.description = "no snapshots".to_owned();
    let alpha = rule("alpha", &["^com/alpha/"]);
    rules.create(&zeta).await.unwrap();
    rules.create(&alpha).await.unwrap();

    let back = rules.get(zeta.id).await.unwrap().unwrap();
    assert_eq!(back.name, "zeta");
    assert_eq!(back.mode, RoutingMode::Block);
    assert_eq!(
        back.patterns,
        ["^com/zeta/", "-SNAPSHOT/"],
        "pattern order is kept"
    );
    assert_eq!(back.description, "no snapshots");
    assert_eq!(back.version, 1);
    assert_eq!(back.created_at, zeta.created_at);
    assert!(
        rules
            .get(RoutingRuleId::generate())
            .await
            .unwrap()
            .is_none()
    );

    let names: Vec<String> = rules
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, ["alpha", "zeta"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_name_is_unique_and_the_shape_limits_are_enforced() {
    let db = TestDb::new().await;
    let rules = db.store().await.routing_rules;
    rules.create(&rule("one", &["a"])).await.unwrap();
    let err = rules.create(&rule("one", &["b"])).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "routing rule name already exists"),
        "{err:?}"
    );

    let too_many: Vec<String> = (0..33).map(|i| format!("^p{i}$")).collect();
    let refs: Vec<&str> = too_many.iter().map(String::as_str).collect();
    let long = "a".repeat(257);
    let accepted_many: Vec<String> = (0..32).map(|i| format!("^p{i}$")).collect();
    for (name, patterns) in [
        ("empty", vec![]),
        ("blank", vec![""]),
        ("many", refs),
        ("long", vec![long.as_str()]),
    ] {
        let err = rules.create(&rule(name, &patterns)).await.unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref m) if m == "a routing rule needs 1 to 32 patterns of 1 to 256 bytes"),
            "{name}: {err:?}"
        );
    }
    let edge = RoutingRule::new(
        RoutingRuleId::generate(),
        "edge",
        RoutingMode::Allow,
        accepted_many,
        now(),
    );
    rules.create(&edge).await.unwrap();
    rules
        .create(&rule("max-length", &[&"a".repeat(256)]))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn update_is_a_compare_and_set_on_the_version() {
    let db = TestDb::new().await;
    let rules = db.store().await.routing_rules;
    let original = rule("editable", &["^a/"]);
    rules.create(&original).await.unwrap();

    let mut edited = original.clone();
    edited.mode = RoutingMode::Allow;
    edited.patterns = vec!["^b/".to_owned()];
    edited.description = "now allow".to_owned();
    rules.update(&edited, 1).await.unwrap();
    let back = rules.get(original.id).await.unwrap().unwrap();
    assert_eq!((back.mode, back.version), (RoutingMode::Allow, 2));
    assert_eq!(back.patterns, ["^b/"]);
    assert_eq!(back.description, "now allow");
    assert_eq!(back.created_at, original.created_at);
    assert!(back.updated_at >= original.updated_at);

    // A stale editor is told, and nothing changes.
    let mut stale = original.clone();
    stale.patterns = vec!["^stale/".to_owned()];
    let err = rules.update(&stale, 1).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    assert_eq!(
        rules.get(original.id).await.unwrap().unwrap().patterns,
        ["^b/"]
    );

    let ghost = rule("ghost", &["x"]);
    let err = rules.update(&ghost, 1).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_applied_and_cannot_take_another_rules_name() {
    let db = TestDb::new().await;
    let rules = db.store().await.routing_rules;
    let first = rule("first", &["a"]);
    let second = rule("second", &["b"]);
    rules.create(&first).await.unwrap();
    rules.create(&second).await.unwrap();

    let mut renamed = first.clone();
    renamed.name = "renamed".to_owned();
    rules.update(&renamed, 1).await.unwrap();
    assert_eq!(rules.get(first.id).await.unwrap().unwrap().name, "renamed");

    let mut clash = renamed.clone();
    clash.name = "second".to_owned();
    let err = rules.update(&clash, 2).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    let back = rules.get(first.id).await.unwrap().unwrap();
    assert_eq!(
        back.name, "renamed",
        "the failed rename rolled the whole update back"
    );
    assert_eq!(back.version, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_in_use_cannot_be_deleted_and_assigned_to_lists_the_repositories() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let used = rule("used", &["a"]);
    store.routing_rules.create(&used).await.unwrap();
    let (_, mut first) = seed_repository(&store).await;
    let (_, mut second) = seed_repository(&store).await;
    first.routing_rule_id = Some(used.id);
    second.routing_rule_id = Some(used.id);
    store.repositories.update(&first).await.unwrap();
    store.repositories.update(&second).await.unwrap();

    let mut expected = vec![first.name.to_string(), second.name.to_string()];
    expected.sort();
    let assigned = store.routing_rules.assigned_to(used.id).await.unwrap();
    let mut names = Vec::new();
    for id in &assigned {
        names.push(
            store
                .repositories
                .get(*id)
                .await
                .unwrap()
                .unwrap()
                .name
                .to_string(),
        );
    }
    assert_eq!(names, expected, "ordered by repository name");

    let err = store.routing_rules.delete(used.id).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    assert!(store.routing_rules.get(used.id).await.unwrap().is_some());

    first.routing_rule_id = None;
    second.routing_rule_id = None;
    store.repositories.update(&first).await.unwrap();
    store.repositories.update(&second).await.unwrap();
    assert!(
        store
            .routing_rules
            .assigned_to(used.id)
            .await
            .unwrap()
            .is_empty()
    );
    store.routing_rules.delete(used.id).await.unwrap();
    assert!(store.routing_rules.get(used.id).await.unwrap().is_none());
    let err = store.routing_rules.delete(used.id).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repository_cannot_point_at_a_rule_that_does_not_exist() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, mut repo) = seed_repository(&store).await;
    repo.routing_rule_id = Some(RoutingRuleId::generate());
    let err = store.repositories.update(&repo).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}
