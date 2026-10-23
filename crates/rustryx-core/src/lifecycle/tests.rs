//! Unit tests of the parent module.

use super::*;
use crate::ids::GcRunId;
use crate::tasks::TaskTrigger;
use crate::testing::{assert_wire, at, round_trip_json};

fn globs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn policy_scope_wire_form_is_adjacently_tagged() {
    assert_wire(&PolicyScope::System, r#"{"scope":"system"}"#);
    let repo = RepositoryId::generate();
    assert_wire(
        &PolicyScope::Repository(repo),
        &format!(r#"{{"scope":"repository","id":"{repo}"}}"#),
    );
    let project = ProjectId::generate();
    assert_wire(
        &PolicyScope::Project(project),
        &format!(r#"{{"scope":"project","id":"{project}"}}"#),
    );
}

#[test]
fn matching_selector_needs_a_glob_and_excluding_may_be_empty() {
    assert!(NameSelector::matching(Vec::new()).is_err());
    assert!(NameSelector::excluding(Vec::new()).is_ok());
    assert_eq!(
        NameSelector::everything(),
        NameSelector::excluding(Vec::new()).expect("ok")
    );
}

#[test]
fn selector_limits_are_enforced() {
    let sixteen: Vec<String> = (0..16).map(|i| format!("v{i}*")).collect();
    assert!(NameSelector::matching(sixteen).is_ok());
    let seventeen: Vec<String> = (0..17).map(|i| format!("v{i}*")).collect();
    assert!(NameSelector::matching(seventeen).is_err());
    assert!(
        NameSelector::matching(globs(&["{a"])).is_err(),
        "glob rules apply"
    );
    assert!(NameSelector::matching(globs(&[""])).is_err());
}

#[test]
fn selector_translates_every_glob_in_order() {
    let selector = NameSelector::matching(globs(&["v1.*", "{rc,beta}-?"])).expect("valid");
    assert_eq!(
        selector.to_regexes().expect("regexes"),
        vec![r"^v1\.[^/]*$".to_owned(), "^(?:rc|beta)-[^/]$".to_owned()]
    );
}

#[test]
fn selector_modes_decide_with_any_or_none() {
    let matching = NameSelector::matching(globs(&["a*", "b*"])).expect("valid");
    assert!(matching.selects(|re| re.starts_with("^b")).expect("ok"));
    assert!(!matching.selects(|_| false).expect("ok"));
    let excluding = NameSelector::excluding(globs(&["a*"])).expect("valid");
    assert!(
        excluding.selects(|_| false).expect("ok"),
        "matches none: selected"
    );
    assert!(
        !excluding.selects(|_| true).expect("ok"),
        "matches one: not selected"
    );
    assert!(
        NameSelector::everything().selects(|_| true).expect("ok"),
        "empty excluding selects all"
    );
}

#[test]
fn selector_from_untrusted_json_is_revalidated_by_the_service() {
    let selector: NameSelector =
        serde_json::from_str(r#"{"mode":"matching","globs":["{a,{b}}"]}"#).expect("shape ok");
    assert!(selector.validate().is_err());
    assert!(
        selector.to_regexes().is_err(),
        "never translates an invalid glob"
    );
}

#[test]
fn cleanup_criteria_need_at_least_one_criterion() {
    let mut criteria = CleanupCriteria::default();
    assert!(criteria.is_empty() && criteria.validate().is_err());
    criteria.extra = serde_json::json!({});
    assert!(criteria.is_empty(), "an empty object is not a criterion");
    criteria.extra = serde_json::json!({"snapshot_min_count": 2});
    assert!(!criteria.is_empty() && criteria.validate().is_ok());
    criteria.extra = serde_json::Value::Null;
    criteria.max_age_days = Some(30);
    assert!(criteria.validate().is_ok());
    criteria.name_regex = Some("a".repeat(256));
    assert!(criteria.validate().is_ok());
    criteria.name_regex = Some("a".repeat(257));
    assert!(criteria.validate().is_err());
}

#[test]
fn cleanup_criteria_deserialize_without_extra() {
    let criteria: CleanupCriteria =
        serde_json::from_str(r#"{"max_age_days":7,"not_downloaded_days":null,"keep_latest_versions":null,"release_type":"prerelease","name_regex":null}"#)
            .expect("extra defaults to null");
    assert_eq!(criteria.release_type, Some(ReleaseType::Prerelease));
    assert!(criteria.extra.is_null());
}

#[test]
fn cleanup_policy_constructor_and_validation() {
    let criteria = CleanupCriteria {
        max_age_days: Some(30),
        ..CleanupCriteria::default()
    };
    let mut policy =
        CleanupPolicy::new(CleanupPolicyId::generate(), "old snapshots", criteria, at());
    assert_eq!((policy.version, policy.format), (1, None));
    assert!(policy.validate().is_ok());
    policy.name.clear();
    assert!(policy.validate().is_err());
    policy.name = "n".repeat(129);
    assert!(policy.validate().is_err());
    policy.name = "ok".into();
    policy.description = "d".repeat(501);
    assert!(policy.validate().is_err());
}

fn rule() -> RetentionRule {
    RetentionRule {
        template: RetentionTemplate::KeepLastPushed { count: 5 },
        repository: NameSelector::everything(),
        item: NameSelector::matching(globs(&["**"])).expect("valid"),
        include_untagged: false,
    }
}

#[test]
fn retention_templates_are_internally_tagged() {
    assert_wire(
        &RetentionTemplate::KeepLastPulled { count: 3 },
        r#"{"template":"keep_last_pulled","count":3}"#,
    );
    assert_wire(
        &RetentionTemplate::KeepPushedWithinDays { days: 7 },
        r#"{"template":"keep_pushed_within_days","days":7}"#,
    );
    assert_wire(
        &RetentionTemplate::KeepLatestVersions { count: 2 },
        r#"{"template":"keep_latest_versions","count":2}"#,
    );
}

#[test]
fn retention_policy_rules_scope_and_defaults() {
    let project = PolicyScope::Project(ProjectId::generate());
    let mut policy = RetentionPolicy::new(
        RetentionPolicyId::generate(),
        project,
        "keep five",
        vec![rule()],
        at(),
    );
    assert!(
        !policy.allow_delete_last_version,
        "the last version is protected by default"
    );
    assert_eq!(policy.version, 1);
    assert!(policy.validate().is_ok());
    policy.rules = Vec::new();
    assert!(policy.validate().is_err(), "at least one rule");
    policy.rules = (0..15).map(|_| rule()).collect();
    assert!(policy.validate().is_ok());
    policy.rules.push(rule());
    assert!(policy.validate().is_err(), "at most 15 rules");
    policy.rules = vec![rule()];
    policy.scope = PolicyScope::System;
    assert!(policy.validate().is_err(), "never system scope");
    policy.scope = project;
    policy.rules[0].item = NameSelector {
        mode: SelectorMode::Matching,
        globs: globs(&["{"]),
    };
    assert!(policy.validate().is_err(), "rule selectors are validated");
}

#[test]
fn lifecycle_enums_use_snake_case() {
    assert_wire(&LifecycleKind::LabelCleanup, "\"label_cleanup\"");
    assert_wire(
        &ItemDecision::ProtectedLastVersion,
        "\"protected_last_version\"",
    );
    assert_wire(&ItemDecision::DeleteFailed, "\"delete_failed\"");
    assert_wire(&ItemKind::Tag, "\"tag\"");
    assert_wire(&ReleaseType::Prerelease, "\"prerelease\"");
    assert_wire(&SelectorMode::Excluding, "\"excluding\"");
    assert_wire(&SoftQuotaKind::MinFree, "\"min_free\"");
}

#[test]
fn lifecycle_run_constructor_and_wire_form() {
    let run = LifecycleRun::new(LifecycleRunId::generate(), LifecycleKind::Cleanup, true, 4);
    assert_eq!(run.status, TaskRunStatus::Queued);
    assert!(run.dry_run && !run.truncated && !run.confirmed_mass_delete);
    assert_eq!(run.counts.deleted, 0);
    let mut finished = run;
    finished.started_at = Some(at());
    finished.finished_at = Some(at() + time::Duration::minutes(1));
    let json = round_trip_json(&finished);
    assert_eq!(json["started_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["finished_at"], "2026-10-24T10:01:00Z");
    assert_eq!(json["status"], "queued");
    assert_eq!(json["kind"], "cleanup");
}

#[test]
fn holds_are_active_until_released_or_expired() {
    let now = at();
    let target = HoldTarget::Repository(RepositoryId::generate());
    let mut hold = LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Legal,
        target,
        "case 12",
        now,
    );
    assert!(hold.is_active_at(now) && hold.validate().is_ok());
    hold.expires_at = Some(now + time::Duration::hours(1));
    assert!(hold.is_active_at(now));
    assert!(
        !hold.is_active_at(now + time::Duration::hours(1)),
        "expiry at now is expired"
    );
    hold.expires_at = None;
    hold.released_at = Some(now);
    assert!(!hold.is_active_at(now), "a released hold protects nothing");
}

#[test]
fn hold_reason_is_required_and_bounded() {
    let target = HoldTarget::Component(ComponentId::generate());
    let make = |reason: String| {
        LifecycleHold::new(
            LifecycleHoldId::generate(),
            HoldKind::Release,
            target,
            reason,
            at(),
        )
    };
    assert!(make(String::new()).validate().is_err());
    assert!(make("r".repeat(500)).validate().is_ok());
    assert!(make("r".repeat(501)).validate().is_err());
}

#[test]
fn hold_target_wire_form() {
    let id = ComponentId::generate();
    assert_wire(
        &HoldTarget::Component(id),
        &format!(r#"{{"kind":"component","id":"{id}"}}"#),
    );
    assert_wire(&HoldKind::Promotion, "\"promotion\"");
}

#[test]
fn immutability_rule_defaults_and_validation() {
    let item = NameSelector::matching(globs(&["v*"])).expect("valid");
    let scope = PolicyScope::Repository(RepositoryId::generate());
    let mut rule = ImmutabilityRule::new(
        ImmutabilityRuleId::generate(),
        scope,
        "releases",
        ItemKind::Version,
        item,
        at(),
    );
    assert!(rule.enabled && rule.version == 1);
    assert_eq!(rule.repository, NameSelector::everything());
    assert!(rule.validate().is_ok());
    rule.scope = PolicyScope::System;
    assert!(rule.validate().is_err(), "never system scope");
    rule.scope = scope;
    rule.item = NameSelector {
        mode: SelectorMode::Matching,
        globs: Vec::new(),
    };
    assert!(
        rule.validate().is_err(),
        "an empty matching selector would protect nothing by accident"
    );
    rule.item = NameSelector::everything();
    rule.name = "x".repeat(129);
    assert!(rule.validate().is_err());
    let json = round_trip_json(&ImmutabilityRule::new(
        ImmutabilityRuleId::generate(),
        scope,
        "r",
        ItemKind::Tag,
        NameSelector::everything(),
        at(),
    ));
    assert_eq!(json["item_kind"], "tag");
    assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
}

#[test]
fn soft_quota_alerts_but_never_decides_a_write() {
    let max_used = SoftQuota {
        kind: SoftQuotaKind::MaxUsed,
        bytes: 100,
    };
    assert!(!max_used.is_exceeded(100, None) && max_used.is_exceeded(101, None));
    let min_free = SoftQuota {
        kind: SoftQuotaKind::MinFree,
        bytes: 100,
    };
    assert!(min_free.is_exceeded(0, Some(99)) && !min_free.is_exceeded(0, Some(100)));
    assert!(
        !min_free.is_exceeded(0, None),
        "unknown free space is not reported as over"
    );
}

#[test]
fn gc_options_defaults_and_limits() {
    let options = GcOptions::new(3600);
    assert!(!options.dry_run && !options.delete_untagged);
    assert_eq!((options.workers, options.min_age_secs), (2, 3600));
    assert!(options.validate(3600).is_ok());
    assert!(
        options.validate(7200).is_err(),
        "never below the configured minimum age"
    );
    for workers in [0u8, 9] {
        let mut bad = options.clone();
        bad.workers = workers;
        assert!(bad.validate(0).is_err(), "{workers}");
    }
    for workers in [1u8, 8] {
        let mut good = options.clone();
        good.workers = workers;
        assert!(good.validate(0).is_ok(), "{workers}");
    }
}

#[test]
fn gc_run_constructor_and_wire_form() {
    let run = GcRun::new(GcRunId::generate(), TaskTrigger::Manual, GcOptions::new(60));
    assert_eq!(run.status, TaskRunStatus::Queued);
    assert!(!run.stop_requested && run.per_store.is_object());
    let json = round_trip_json(&run);
    assert_eq!(json["trigger"], "manual");
    assert_eq!(json["options"]["workers"], 2);
    let sample = GcSample {
        run_id: run.id,
        blob_store: "default".into(),
        key: "k".into(),
        size_bytes: 1,
        orphaned_at: at(),
    };
    assert_eq!(
        round_trip_json(&sample)["orphaned_at"],
        "2026-10-24T10:00:00Z"
    );
}

#[test]
fn lifecycle_item_and_counts_round_trip() {
    let item = LifecycleItem {
        run_id: LifecycleRunId::generate(),
        seq: 7,
        repository: "app".into(),
        component_id: ComponentId::generate(),
        coordinate: "app:1.0".into(),
        tags: vec!["latest".into()],
        size_bytes: 10,
        decision: ItemDecision::Delete,
        reason: "rule:0".into(),
    };
    assert_eq!(round_trip_json(&item)["decision"], "delete");
    let counts = round_trip_json(&LifecycleCounts::default());
    assert_eq!(counts["protected_immutable"], 0);
    let defaults = round_trip_json(&GcCounts::default());
    assert_eq!(defaults["untagged_deleted"], 0);
}
