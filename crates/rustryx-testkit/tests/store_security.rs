//! Scanners, scan reports and findings, CVE exceptions, scan and trust policies, trust
//! material, the Security Hub, exports and advisory data sets.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    clippy::assert_is_empty,
    reason = "each test is one scenario read top to bottom; filters and non exhaustive types are \
              filled field by field after `default()`, and `.is_empty()` reads better than a \
              comparison with an empty literal of an inferred type"
)]

mod common;

use std::time::Duration;

use common::{Ports, T0, env, hosted, page, project};
use futures::StreamExt;
use rustryx_core::error::StoreError;
use rustryx_core::events::{EventDraft, EventKind};
use rustryx_core::ids::{
    BlobId, ComponentId, CveAllowlistEntryId, ExportId, RepositoryId, ScanReportId,
    ScannerDataSetId, ScannerId, SecurityPolicyId, TrustMaterialId, TrustPolicyId, UserId,
};
use rustryx_core::lifecycle::PolicyScope;
use rustryx_core::model::{NewComponent, RepoFormat};
use rustryx_core::ports::{
    ExportJob, FindingFilter, HubFilter, InvalidationBus, InvalidationEvent, SecretView,
};
use rustryx_core::scan::{
    AllowlistMode, AllowlistScope, CveAllowlistEntry, Finding, GateMode, ProbeResult,
    PullGatePolicy, QuarantineState, RequiredSignatures, ScanKind, ScanPolicy, ScanReport,
    ScanStatus, ScanSummary, ScanTrigger, ScannerDataSet, ScannerKind, ScannerRecord, Severity,
    TrustMaterial, TrustMaterialKind, TrustPolicy,
};
use rustryx_core::tasks::TaskRunStatus;
use rustryx_testkit::{MemInvalidationBus, MemStore};

fn scanner(name: &str) -> ScannerRecord {
    ScannerRecord::new(ScannerId::generate(), name, ScannerKind::Trivy, T0)
}

fn http_scanner(name: &str) -> ScannerRecord {
    let mut record = ScannerRecord::new(ScannerId::generate(), name, ScannerKind::Http, T0);
    record.endpoint = Some("https://scanner.example.test".to_owned());
    record
}

fn finding(report: ScanReportId, vuln: &str, package: &str, severity: Severity) -> Finding {
    Finding {
        report_id: report,
        vuln_id: vuln.to_owned(),
        package: package.to_owned(),
        package_version: "1.0".to_owned(),
        fixed_version: None,
        severity,
        cvss: None,
        title: None,
    }
}

async fn component(store: &MemStore, repo: RepositoryId, name: &str) -> ComponentId {
    let mut new = NewComponent::new(repo, RepoFormat::Raw, name);
    new.version = Some("1".to_owned());
    store.components().find_or_create(&new).await.unwrap().id
}

fn report(component: ComponentId, kind: ScanKind) -> ScanReport {
    ScanReport::new(
        ScanReportId::generate(),
        component,
        ScannerKind::Trivy,
        kind,
        ScanTrigger::Manual,
        T0,
    )
}

async fn scan(
    store: &MemStore,
    component: ComponentId,
    findings: &[(&str, &str, Severity)],
    at: time::OffsetDateTime,
) -> ScanReportId {
    let started = report(component, ScanKind::Vulnerability);
    store.scans().start(&started).await.unwrap();
    store.scans().mark_running(started.id, at).await.unwrap();
    let rows: Vec<Finding> = findings
        .iter()
        .map(|(vuln, package, severity)| finding(started.id, vuln, package, *severity))
        .collect();
    let mut summary = ScanSummary::default();
    for row in &rows {
        summary.by_severity[row.severity.index()] += 1;
        summary.total += 1;
    }
    store
        .scans()
        .complete(started.id, &rows, &summary, "trivy 1", "db 1", at, &[])
        .await
        .unwrap();
    started.id
}

async fn security_revision(store: &MemStore) -> i64 {
    store
        .config()
        .get("security.revision", SecretView::Reveal)
        .await
        .unwrap()
        .map_or(0, |entry| entry.value.as_i64().unwrap())
}

// scanners

#[tokio::test]
async fn scanners_are_unique_validated_versioned_and_one_is_the_default() {
    let env = env();
    let scanners = env.store.scanners();
    let first = scanner("trivy-a");
    scanners.create(&first).await.unwrap();
    assert!(matches!(
        scanners.create(&first).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        scanners.create(&scanner("trivy-a")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut local_with_endpoint = scanner("bad");
    local_with_endpoint.endpoint = Some("https://x.example.test".to_owned());
    assert!(matches!(
        scanners.create(&local_with_endpoint).await,
        Err(StoreError::Conflict(_))
    ));
    scanners.create(&http_scanner("remote")).await.unwrap();
    let names: Vec<String> = scanners
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["remote", "trivy-a"]);

    let mut default_one = scanner("default-one");
    default_one.is_default = true;
    scanners.create(&default_one).await.unwrap();
    let mut default_two = scanner("default-two");
    default_two.is_default = true;
    assert!(
        matches!(
            scanners.create(&default_two).await,
            Err(StoreError::Conflict(_))
        ),
        "one default"
    );
    scanners.create(&scanner("default-two")).await.unwrap();
    let two = scanners
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.name == "default-two")
        .unwrap();
    scanners.set_default(two.id).await.unwrap();
    let defaults: Vec<String> = scanners
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter(|s| s.is_default)
        .map(|s| s.name)
        .collect();
    assert_eq!(
        defaults,
        ["default-two"],
        "the flag moved, it did not duplicate"
    );
    assert!(matches!(
        scanners.set_default(ScannerId::generate()).await,
        Err(StoreError::NotFound(_))
    ));

    let mut edited = first.clone();
    edited.enabled = false;
    edited.is_default = true;
    assert!(
        matches!(
            scanners.update(&edited, 1).await,
            Err(StoreError::Conflict(_))
        ),
        "a second default"
    );
    edited.is_default = false;
    scanners.update(&edited, 1).await.unwrap();
    let stored = scanners.get(first.id).await.unwrap().unwrap();
    assert_eq!((stored.version, stored.enabled), (2, false));
    assert!(matches!(
        scanners.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut clash = edited.clone();
    clash.name = "remote".to_owned();
    assert!(matches!(
        scanners.update(&clash, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut invalid = edited.clone();
    invalid.capabilities.clear();
    assert!(matches!(
        scanners.update(&invalid, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = edited.clone();
    ghost.id = ScannerId::generate();
    assert!(matches!(
        scanners.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_probe_is_recorded_and_survives_an_edit_from_a_stale_read() {
    let env = env();
    let record = scanner("probed");
    env.store.scanners().create(&record).await.unwrap();
    let probe = ProbeResult {
        at: T0,
        ok: true,
        version: Some("0.50".to_owned()),
        message: "ok".to_owned(),
    };
    env.store
        .scanners()
        .record_probe(record.id, &probe)
        .await
        .unwrap();
    assert!(matches!(
        env.store
            .scanners()
            .record_probe(ScannerId::generate(), &probe)
            .await,
        Err(StoreError::NotFound(_))
    ));
    let mut stale = record.clone();
    stale.enabled = false;
    env.store.scanners().update(&stale, 1).await.unwrap();
    let stored = env.store.scanners().get(record.id).await.unwrap().unwrap();
    assert_eq!(
        stored.last_probe.map(|p| p.version),
        Some(Some("0.50".to_owned()))
    );
}

#[tokio::test]
async fn must_not_delete_a_scanner_that_a_scan_policy_names_and_unlink_it_from_reports() {
    let env = env();
    let record = scanner("in-use");
    env.store.scanners().create(&record).await.unwrap();
    let repo = hosted(&env.store, "scanned").await;
    let mut policy = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Repository(repo.id),
    );
    policy.scanner_id = Some(record.id);
    env.store
        .security_policies()
        .upsert_scan_policy(&policy, None)
        .await
        .unwrap();
    assert!(matches!(
        env.store.scanners().delete(record.id).await,
        Err(StoreError::Conflict(_))
    ));
    env.store.repos().delete(repo.id).await.unwrap();

    let other = hosted(&env.store, "scanned-2").await;
    let second = component(&env.store, other.id, "pkg").await;
    let mut started = report(second, ScanKind::Vulnerability);
    started.scanner_id = Some(record.id);
    env.store.scans().start(&started).await.unwrap();
    env.store.scanners().delete(record.id).await.unwrap();
    assert!(env.store.scanners().get(record.id).await.unwrap().is_none());
    let kept = env
        .store
        .scans()
        .get_report(started.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.scanner_id, None, "the history stays, unlinked");
    assert!(matches!(
        env.store.scanners().delete(record.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// scans

#[tokio::test]
async fn a_completed_scan_replaces_the_previous_verdict_in_one_step() {
    let env = env();
    let repo = hosted(&env.store, "verdicts").await;
    let target = component(&env.store, repo.id, "pkg").await;
    assert!(env.store.scans().state(target).await.unwrap().is_none());
    let first = scan(
        &env.store,
        target,
        &[
            ("CVE-1", "libA", Severity::High),
            ("CVE-2", "libB", Severity::Low),
        ],
        env.clock_now(),
    )
    .await;
    let state = env.store.scans().state(target).await.unwrap().unwrap();
    assert_eq!(state.report_id, Some(first));
    assert!(state.is_scanned());
    assert_eq!(state.summary.count(Severity::High), 1);
    assert_eq!(state.scanned_at, Some(env.clock_now()));

    env.clock.advance(Duration::from_secs(60));
    let second = scan(
        &env.store,
        target,
        &[("CVE-3", "libC", Severity::Critical)],
        env.clock_now(),
    )
    .await;
    let state = env.store.scans().state(target).await.unwrap().unwrap();
    assert_eq!(state.report_id, Some(second));
    let findings = env
        .store
        .scans()
        .findings(target, &FindingFilter::default(), &page(10))
        .await
        .unwrap();
    assert_eq!(
        findings.items.len(),
        1,
        "findings of the old report are gone"
    );
    assert_eq!(findings.items[0].vuln_id, "CVE-3");
    let old = env.store.scans().get_report(first).await.unwrap().unwrap();
    assert!(!old.is_current);
    assert_eq!(old.status, ScanStatus::Complete);
    let current = env.store.scans().get_report(second).await.unwrap().unwrap();
    assert!(current.is_current);
    assert_eq!(
        (
            current.scanner_version.as_str(),
            current.dataset_version.as_str()
        ),
        ("trivy 1", "db 1")
    );
    let batch = env
        .store
        .scans()
        .states(&[target, ComponentId::generate()])
        .await
        .unwrap();
    assert_eq!(batch.len(), 1);
}

#[tokio::test]
async fn only_the_newest_three_reports_per_component_and_kind_are_kept() {
    let env = env();
    let repo = hosted(&env.store, "history").await;
    let target = component(&env.store, repo.id, "pkg").await;
    let mut ids = Vec::new();
    for round in 0..5 {
        env.clock.advance(Duration::from_secs(60));
        let mut started = report(target, ScanKind::Vulnerability);
        started.queued_at = env.clock_now();
        env.store.scans().start(&started).await.unwrap();
        env.store
            .scans()
            .complete(
                started.id,
                &[],
                &ScanSummary::default(),
                "v",
                "d",
                env.clock_now(),
                &[],
            )
            .await
            .unwrap();
        ids.push((round, started.id));
    }
    let mut kept = Vec::new();
    for (round, id) in &ids {
        if env.store.scans().get_report(*id).await.unwrap().is_some() {
            kept.push(*round);
        }
    }
    assert_eq!(kept, [2, 3, 4]);
    // An SBOM report is another kind with its own history and never touches the verdict.
    let sbom = report(target, ScanKind::Sbom);
    env.store.scans().start(&sbom).await.unwrap();
    env.store
        .scans()
        .complete(
            sbom.id,
            &[],
            &ScanSummary::default(),
            "v",
            "d",
            env.clock_now(),
            &[],
        )
        .await
        .unwrap();
    let state = env.store.scans().state(target).await.unwrap().unwrap();
    assert_eq!(state.report_id, Some(ids[4].1));
}

#[tokio::test]
async fn must_not_treat_a_failed_rescan_as_clean() {
    let env = env();
    let repo = hosted(&env.store, "failing").await;
    let target = component(&env.store, repo.id, "pkg").await;
    let first = scan(
        &env.store,
        target,
        &[("CVE-9", "lib", Severity::High)],
        env.clock_now(),
    )
    .await;
    let retry = report(target, ScanKind::Vulnerability);
    env.store.scans().start(&retry).await.unwrap();
    env.store
        .scans()
        .fail(
            retry.id,
            ScanStatus::Failed,
            &"x".repeat(900),
            env.clock_now(),
            &[],
        )
        .await
        .unwrap();
    let state = env.store.scans().state(target).await.unwrap().unwrap();
    assert_eq!(state.scan_status, Some(ScanStatus::Failed));
    assert!(!state.is_scanned(), "not scanned is a state, never clean");
    assert_eq!(
        state.report_id,
        Some(first),
        "the previous report is untouched"
    );
    assert_eq!(state.summary.count(Severity::High), 1);
    let stored = env
        .store
        .scans()
        .get_report(retry.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.error.map(|e| e.chars().count()), Some(500));
    assert!(!stored.is_current);
    // A component that was never scanned gets a state row that says failed, with no report.
    let other = component(&env.store, repo.id, "fresh").await;
    let never = report(other, ScanKind::Vulnerability);
    env.store.scans().start(&never).await.unwrap();
    env.store
        .scans()
        .fail(
            never.id,
            ScanStatus::Unsupported,
            "no_adapter",
            env.clock_now(),
            &[],
        )
        .await
        .unwrap();
    let state = env.store.scans().state(other).await.unwrap().unwrap();
    assert_eq!(
        (state.scan_status, state.report_id),
        (Some(ScanStatus::Unsupported), None)
    );
}

#[tokio::test]
async fn scan_transitions_are_one_way_and_validated() {
    let env = env();
    let repo = hosted(&env.store, "transitions").await;
    let target = component(&env.store, repo.id, "pkg").await;
    let started = report(target, ScanKind::Vulnerability);
    env.store.scans().start(&started).await.unwrap();
    assert!(matches!(
        env.store.scans().start(&started).await,
        Err(StoreError::Conflict(_))
    ));
    let orphan = report(ComponentId::generate(), ScanKind::Vulnerability);
    assert!(matches!(
        env.store.scans().start(&orphan).await,
        Err(StoreError::Conflict(_))
    ));
    let mut pre_current = report(target, ScanKind::Vulnerability);
    pre_current.is_current = true;
    pre_current.status = ScanStatus::Complete;
    env.store.scans().start(&pre_current).await.unwrap();
    let stored = env
        .store
        .scans()
        .get_report(pre_current.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.status, stored.is_current),
        (ScanStatus::Queued, false),
        "only `complete` makes a report current"
    );

    env.store
        .scans()
        .mark_running(started.id, T0)
        .await
        .unwrap();
    assert!(matches!(
        env.store.scans().mark_running(started.id, T0).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .scans()
            .mark_running(ScanReportId::generate(), T0)
            .await,
        Err(StoreError::NotFound(_))
    ));
    let none = ScanSummary::default();
    let bad_finding = finding(started.id, "cve-lower", "p", Severity::Low);
    assert!(matches!(
        env.store
            .scans()
            .complete(started.id, &[bad_finding], &none, "v", "d", T0, &[])
            .await,
        Err(StoreError::Conflict(_))
    ));
    let twin = finding(started.id, "CVE-1", "p", Severity::Low);
    assert!(matches!(
        env.store
            .scans()
            .complete(started.id, &[twin.clone(), twin], &none, "v", "d", T0, &[])
            .await,
        Err(StoreError::Conflict(_))
    ));
    let too_many: Vec<Finding> = (0..50_001)
        .map(|i| finding(started.id, &format!("CVE-{i}"), "p", Severity::Low))
        .collect();
    assert!(matches!(
        env.store
            .scans()
            .complete(started.id, &too_many, &none, "v", "d", T0, &[])
            .await,
        Err(StoreError::Invalid(_))
    ));
    env.store
        .scans()
        .complete(started.id, &[], &none, "v", "d", T0, &[])
        .await
        .unwrap();
    assert!(matches!(
        env.store
            .scans()
            .complete(started.id, &[], &none, "v", "d", T0, &[])
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .scans()
            .fail(started.id, ScanStatus::Failed, "late", T0, &[])
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .scans()
            .fail(pre_current.id, ScanStatus::Complete, "x", T0, &[])
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        env.store
            .scans()
            .fail(ScanReportId::generate(), ScanStatus::Failed, "x", T0, &[])
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        env.store
            .scans()
            .complete(ScanReportId::generate(), &[], &none, "v", "d", T0, &[])
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn scan_events_ride_with_the_verdict() {
    let env = env();
    let repo = hosted(&env.store, "scan-events").await;
    let target = component(&env.store, repo.id, "pkg").await;
    let started = report(target, ScanKind::Vulnerability);
    env.store.scans().start(&started).await.unwrap();
    let actor = rustryx_core::audit::ActorRef::system("scan");
    let done = EventDraft::new(
        EventKind::ScanCompleted,
        actor.clone(),
        serde_json::json!({}),
    );
    env.store
        .scans()
        .complete(
            started.id,
            &[],
            &ScanSummary::default(),
            "v",
            "d",
            T0,
            &[done],
        )
        .await
        .unwrap();
    let broken = report(target, ScanKind::Vulnerability);
    env.store.scans().start(&broken).await.unwrap();
    let failed = EventDraft::new(EventKind::ScanFailed, actor.clone(), serde_json::json!({}));
    env.store
        .scans()
        .fail(broken.id, ScanStatus::Stopped, "stop", T0, &[failed])
        .await
        .unwrap();
    let huge = EventDraft::new(
        EventKind::ScanFailed,
        actor,
        serde_json::json!({"x": "y".repeat(20_000)}),
    );
    let again = report(target, ScanKind::Vulnerability);
    env.store.scans().start(&again).await.unwrap();
    assert!(matches!(
        env.store
            .scans()
            .fail(again.id, ScanStatus::Failed, "x", T0, &[huge])
            .await,
        Err(StoreError::Invalid(_))
    ));
    let kinds: Vec<EventKind> = env
        .store
        .outbox()
        .read_batch("c", 10)
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, [EventKind::ScanCompleted, EventKind::ScanFailed]);
}

#[tokio::test]
async fn findings_are_filtered_and_the_effective_summary_drops_what_is_allowed() {
    let env = env();
    let repo = hosted(&env.store, "filters").await;
    let target = component(&env.store, repo.id, "pkg").await;
    let started = report(target, ScanKind::Vulnerability);
    env.store.scans().start(&started).await.unwrap();
    let mut fixable = finding(started.id, "CVE-1", "openssl", Severity::Critical);
    fixable.fixed_version = Some("2.0".to_owned());
    let rows = [
        fixable,
        finding(started.id, "CVE-2", "zlib", Severity::High),
        finding(started.id, "CVE-3", "zlib", Severity::Low),
        finding(started.id, "GHSA-4", "curl", Severity::Unknown),
    ];
    env.store
        .scans()
        .complete(
            started.id,
            &rows,
            &ScanSummary::default(),
            "v",
            "d",
            T0,
            &[],
        )
        .await
        .unwrap();
    let list = |filter: FindingFilter| {
        let store = env.store.clone();
        async move {
            store
                .scans()
                .findings(target, &filter, &page(10))
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|f| f.vuln_id)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        list(FindingFilter::default()).await,
        ["CVE-1", "CVE-2", "CVE-3", "GHSA-4"]
    );
    let mut at_least_high = FindingFilter::default();
    at_least_high.severity_at_least = Some(Severity::High);
    assert_eq!(list(at_least_high).await, ["CVE-1", "CVE-2"]);
    let mut only_fixable = FindingFilter::default();
    only_fixable.fixable = Some(true);
    assert_eq!(list(only_fixable.clone()).await, ["CVE-1"]);
    only_fixable.fixable = Some(false);
    assert_eq!(list(only_fixable).await, ["CVE-2", "CVE-3", "GHSA-4"]);
    let mut by_id = FindingFilter::default();
    by_id.vuln_id = Some("ghsa-4".to_owned());
    assert_eq!(list(by_id).await, ["GHSA-4"]);
    let mut by_package = FindingFilter::default();
    by_package.package = Some("zlib".to_owned());
    assert_eq!(list(by_package).await, ["CVE-2", "CVE-3"]);
    assert!(
        env.store
            .scans()
            .findings(
                ComponentId::generate(),
                &FindingFilter::default(),
                &page(10)
            )
            .await
            .unwrap()
            .items
            .is_empty()
    );

    let allowed = vec!["CVE-1".to_owned(), "CVE-3".to_owned()];
    let summary = env
        .store
        .scans()
        .effective_summary(target, &allowed)
        .await
        .unwrap();
    assert_eq!(
        (summary.total, summary.suppressed, summary.fixable),
        (2, 2, 0)
    );
    assert_eq!(
        (
            summary.count(Severity::High),
            summary.count(Severity::Unknown)
        ),
        (1, 1)
    );
    let all = env
        .store
        .scans()
        .effective_summary(target, &[])
        .await
        .unwrap();
    assert_eq!((all.total, all.fixable), (4, 1));
    let none = env
        .store
        .scans()
        .effective_summary(ComponentId::generate(), &[])
        .await
        .unwrap();
    assert_eq!(none.total, 0);
}

#[tokio::test]
async fn stale_components_are_the_unscanned_and_the_long_unscanned_newest_first() {
    let env = env();
    let project_one = project(&env.store, "stale").await;
    let repo = hosted(&env.store, "stale-repo").await;
    env.store
        .projects()
        .attach_repository(project_one.id, repo.id)
        .await
        .unwrap();
    let outside = hosted(&env.store, "stale-outside").await;
    let old = component(&env.store, repo.id, "old").await;
    env.clock.advance(Duration::from_secs(60));
    let fresh = component(&env.store, repo.id, "fresh").await;
    env.clock.advance(Duration::from_secs(60));
    let never = component(&env.store, repo.id, "never").await;
    let elsewhere = component(&env.store, outside.id, "elsewhere").await;
    scan(&env.store, old, &[], T0).await;
    scan(&env.store, fresh, &[], env.clock_now()).await;
    let cutoff = env.clock_now() - time::Duration::seconds(30);
    let stale = env
        .store
        .scans()
        .stale_components(cutoff, None, 10)
        .await
        .unwrap();
    assert_eq!(stale, [elsewhere, never, old], "newest component first");
    let in_project = env
        .store
        .scans()
        .stale_components(cutoff, Some(PolicyScope::Project(project_one.id)), 10)
        .await
        .unwrap();
    assert_eq!(in_project, [never, old]);
    let in_repo = env
        .store
        .scans()
        .stale_components(cutoff, Some(PolicyScope::Repository(outside.id)), 10)
        .await
        .unwrap();
    assert_eq!(in_repo, [elsewhere]);
    let system = env
        .store
        .scans()
        .stale_components(cutoff, Some(PolicyScope::System), 2)
        .await
        .unwrap();
    assert_eq!(system.len(), 2);
}

#[tokio::test]
async fn quarantine_changes_bump_the_security_revision_and_record_the_release() {
    let env = env();
    let bus = MemInvalidationBus::new();
    let store = env.store.clone().with_invalidation_bus(bus.clone());
    let mut events = bus.subscribe();
    let repo = hosted(&store, "quarantine").await;
    let target = component(&store, repo.id, "pkg").await;
    let admin = UserId::generate();
    assert_eq!(security_revision(&store).await, 0);
    store
        .scans()
        .set_quarantine(target, QuarantineState::Quarantined, None, T0, &[])
        .await
        .unwrap();
    let revision = match events.next().await {
        Some(InvalidationEvent::Config(revision)) => revision,
        other => panic!("{other:?}"),
    };
    assert!(revision >= 1);
    assert_eq!(security_revision(&store).await, 1);
    store
        .scans()
        .set_quarantine(
            target,
            QuarantineState::Released,
            Some(admin),
            env.clock_now(),
            &[],
        )
        .await
        .unwrap();
    let state = store.scans().state(target).await.unwrap().unwrap();
    assert_eq!(state.quarantine, QuarantineState::Released);
    assert_eq!(
        (state.released_by, state.released_at),
        (Some(admin), Some(env.clock_now()))
    );
    store
        .scans()
        .set_quarantine(target, QuarantineState::Quarantined, None, T0, &[])
        .await
        .unwrap();
    let state = store.scans().state(target).await.unwrap().unwrap();
    assert_eq!((state.released_by, state.released_at), (None, None));
    assert!(matches!(
        store
            .scans()
            .set_quarantine(
                ComponentId::generate(),
                QuarantineState::None,
                None,
                T0,
                &[]
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
}

// exceptions

fn exception(scope: AllowlistScope, vuln: &str) -> CveAllowlistEntry {
    CveAllowlistEntry::new(
        CveAllowlistEntryId::generate(),
        scope,
        vuln,
        "accepted risk",
        T0,
    )
}

#[tokio::test]
async fn cve_exceptions_are_unique_per_scope_listed_without_the_expired_and_versioned() {
    let env = env();
    let created = project(&env.store, "excepted").await;
    let list = env.store.allowlist();
    let system = exception(AllowlistScope::System, "CVE-1");
    list.create(&system).await.unwrap();
    assert!(matches!(
        list.create(&system).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        list.create(&exception(AllowlistScope::System, "CVE-1"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    list.create(&exception(AllowlistScope::Project(created.id), "CVE-1"))
        .await
        .unwrap();
    assert!(matches!(
        list.create(&exception(
            AllowlistScope::Project(rustryx_core::ids::ProjectId::generate()),
            "CVE-2"
        ))
        .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        list.create(&exception(
            AllowlistScope::Component(ComponentId::generate()),
            "CVE-3"
        ))
        .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        list.create(&exception(AllowlistScope::System, "lower"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let mut expiring = exception(AllowlistScope::System, "CVE-5");
    expiring.expires_at = Some(env.clock_now() + time::Duration::hours(1));
    list.create(&expiring).await.unwrap();
    assert_eq!(
        list.list(AllowlistScope::System, false, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    env.clock.advance(Duration::from_hours(2));
    assert_eq!(
        list.list(AllowlistScope::System, false, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        list.list(AllowlistScope::System, true, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    assert_eq!(
        list.list(AllowlistScope::Project(created.id), false, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        1
    );

    let mut edited = system.clone();
    edited.reason = "still accepted".to_owned();
    edited.scope = AllowlistScope::Project(created.id);
    list.update(&edited, 1).await.unwrap();
    let stored = list
        .list(AllowlistScope::System, false, &page(10))
        .await
        .unwrap()
        .items;
    let updated = stored.iter().find(|e| e.id == system.id).unwrap();
    assert_eq!(
        (updated.version, updated.reason.as_str()),
        (2, "still accepted")
    );
    assert!(matches!(
        list.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut invalid = edited.clone();
    invalid.reason = String::new();
    assert!(matches!(
        list.update(&invalid, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = edited.clone();
    ghost.id = CveAllowlistEntryId::generate();
    assert!(matches!(
        list.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));
    list.delete(system.id).await.unwrap();
    assert!(matches!(
        list.delete(system.id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn exceptions_follow_the_mode_and_expire_once() {
    let env = env();
    let created = project(&env.store, "modes").await;
    let repo = hosted(&env.store, "modes-repo").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    let target = component(&env.store, repo.id, "pkg").await;
    let list = env.store.allowlist();
    list.create(&exception(AllowlistScope::System, "CVE-S"))
        .await
        .unwrap();
    list.create(&exception(AllowlistScope::Project(created.id), "CVE-P"))
        .await
        .unwrap();
    list.create(&exception(AllowlistScope::Component(target), "CVE-C"))
        .await
        .unwrap();
    let mut short = exception(AllowlistScope::Component(target), "CVE-X");
    short.expires_at = Some(env.clock_now() + time::Duration::minutes(5));
    list.create(&short).await.unwrap();
    assert_eq!(
        list.active_ids(target, AllowlistMode::System)
            .await
            .unwrap(),
        ["CVE-C", "CVE-S", "CVE-X"]
    );
    assert_eq!(
        list.active_ids(target, AllowlistMode::Project)
            .await
            .unwrap(),
        ["CVE-C", "CVE-P", "CVE-X"]
    );
    assert!(
        list.active_ids(ComponentId::generate(), AllowlistMode::System)
            .await
            .unwrap()
            .is_empty()
    );
    env.clock.advance(Duration::from_secs(600));
    assert_eq!(
        list.active_ids(target, AllowlistMode::System)
            .await
            .unwrap(),
        ["CVE-C", "CVE-S"],
        "expired is never included"
    );

    let now = env.clock_now();
    let due = list.take_expired_unaudited(now, 10).await.unwrap();
    assert_eq!(
        due.iter().map(|e| e.vuln_id.as_str()).collect::<Vec<_>>(),
        ["CVE-X"]
    );
    assert!(
        list.take_expired_unaudited(now, 10)
            .await
            .unwrap()
            .is_empty(),
        "marked in the same call"
    );
    // A new expiry date re-arms the audit.
    let mut extended = short.clone();
    extended.expires_at = Some(now + time::Duration::minutes(1));
    list.update(&extended, 1).await.unwrap();
    env.clock.advance(Duration::from_secs(120));
    assert_eq!(
        list.take_expired_unaudited(env.clock_now(), 0)
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        list.take_expired_unaudited(env.clock_now(), 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn the_system_list_is_copied_to_a_project_as_a_snapshot() {
    let env = env();
    let created = project(&env.store, "snapshot").await;
    let list = env.store.allowlist();
    list.create(&exception(AllowlistScope::System, "CVE-1"))
        .await
        .unwrap();
    list.create(&exception(AllowlistScope::System, "CVE-2"))
        .await
        .unwrap();
    let mut dead = exception(AllowlistScope::System, "CVE-3");
    dead.expires_at = Some(env.clock_now() - time::Duration::days(1));
    list.create(&dead).await.unwrap();
    let mut own = exception(AllowlistScope::Project(created.id), "CVE-2");
    own.reason = "project reason".to_owned();
    list.create(&own).await.unwrap();
    let editor = Some(UserId::generate());
    let copied = list
        .copy_system_to_project(created.id, editor)
        .await
        .unwrap();
    assert_eq!(
        copied, 1,
        "an expired entry and a vulnerability the project already allows are skipped"
    );
    let project_list = list
        .list(AllowlistScope::Project(created.id), true, &page(10))
        .await
        .unwrap();
    assert_eq!(project_list.items.len(), 2);
    let copy = project_list
        .items
        .iter()
        .find(|e| e.vuln_id == "CVE-1")
        .unwrap();
    assert_eq!(copy.created_by, editor);
    assert_eq!(copy.version, 1);
    let own_after = project_list
        .items
        .iter()
        .find(|e| e.vuln_id == "CVE-2")
        .unwrap();
    assert_eq!(own_after.reason, "project reason");
    assert_eq!(
        list.copy_system_to_project(created.id, None).await.unwrap(),
        0
    );
    assert!(matches!(
        list.copy_system_to_project(rustryx_core::ids::ProjectId::generate(), None)
            .await,
        Err(StoreError::NotFound(_))
    ));
    // A later system removal does not reach the copy.
    let system_entries = list
        .list(AllowlistScope::System, true, &page(10))
        .await
        .unwrap()
        .items;
    for entry in system_entries {
        list.delete(entry.id).await.unwrap();
    }
    assert_eq!(
        list.list(AllowlistScope::Project(created.id), true, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        2
    );
}

#[tokio::test]
async fn every_security_write_bumps_the_revision_and_tells_the_bus() {
    let env = env();
    let bus = MemInvalidationBus::new();
    let store = env.store.clone().with_invalidation_bus(bus.clone());
    let mut events = bus.subscribe();
    let entry = exception(AllowlistScope::System, "CVE-1");
    store.allowlist().create(&entry).await.unwrap();
    let mut revisions = Vec::new();
    revisions.push(match events.next().await {
        Some(InvalidationEvent::Config(revision)) => revision,
        other => panic!("{other:?}"),
    });
    store.allowlist().delete(entry.id).await.unwrap();
    revisions.push(match events.next().await {
        Some(InvalidationEvent::Config(revision)) => revision,
        other => panic!("{other:?}"),
    });
    assert!(revisions[1] > revisions[0], "{revisions:?}");
    assert_eq!(security_revision(&store).await, 2);
    // `security.revision` is no `settings.` or `system.` row, so the node revision is untouched.
    assert_eq!(store.config().current_revision().await.unwrap(), 0);
}

// policies

#[tokio::test]
async fn scan_policies_merge_field_wise_from_system_over_project_over_repository() {
    let env = env();
    let created = project(&env.store, "merged").await;
    let repo = hosted(&env.store, "merged-repo").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    let policies = env.store.security_policies();
    // Without any stored row the built-in system default answers, and it is complete.
    let defaults = policies.effective_for(repo.id).await.unwrap();
    assert!(defaults.is_complete());
    assert_eq!(defaults.scope, PolicyScope::Repository(repo.id));
    assert!(
        policies
            .get_scan_policy(PolicyScope::System)
            .await
            .unwrap()
            .is_none()
    );

    let mut system = ScanPolicy::system_default(SecurityPolicyId::generate());
    system.quarantine = Some(false);
    policies.upsert_scan_policy(&system, None).await.unwrap();
    let mut for_project = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Project(created.id),
    );
    for_project.quarantine = Some(true);
    for_project.sbom_on_push = Some(true);
    policies
        .upsert_scan_policy(&for_project, None)
        .await
        .unwrap();
    let mut for_repo = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Repository(repo.id),
    );
    for_repo.sbom_on_push = Some(false);
    let mut gate = PullGatePolicy::default();
    gate.mode = GateMode::Enforce;
    for_repo.gate = Some(gate);
    policies.upsert_scan_policy(&for_repo, None).await.unwrap();

    let effective = policies.effective_for(repo.id).await.unwrap();
    assert_eq!(
        effective.quarantine,
        Some(true),
        "the project beats the system"
    );
    assert_eq!(
        effective.sbom_on_push,
        Some(false),
        "the repository beats the project"
    );
    assert_eq!(effective.gate.map(|g| g.mode), Some(GateMode::Enforce));
    assert_eq!(
        effective.auto_scan,
        Some(true),
        "unset fields come from below"
    );
    assert!(effective.is_complete());
    assert!(matches!(
        policies.effective_for(RepositoryId::generate()).await,
        Err(StoreError::NotFound(_))
    ));

    let mut edited = for_repo.clone();
    edited.auto_scan = Some(false);
    policies.upsert_scan_policy(&edited, Some(1)).await.unwrap();
    let stored = policies
        .get_scan_policy(PolicyScope::Repository(repo.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((stored.version, stored.id), (2, for_repo.id));
    assert!(matches!(
        policies.upsert_scan_policy(&edited, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        policies.upsert_scan_policy(&edited, None).await,
        Err(StoreError::Conflict(_))
    ));
    let other_repo = hosted(&env.store, "merged-other").await;
    let mut fresh = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Repository(other_repo.id),
    );
    assert!(matches!(
        policies.upsert_scan_policy(&fresh, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    fresh.id = for_repo.id;
    assert!(
        matches!(
            policies.upsert_scan_policy(&fresh, None).await,
            Err(StoreError::Conflict(_))
        ),
        "id taken"
    );
    let mut bad_gate = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Repository(other_repo.id),
    );
    let mut weak = PullGatePolicy::default();
    weak.threshold = Some(Severity::Negligible);
    bad_gate.gate = Some(weak);
    assert!(matches!(
        policies.upsert_scan_policy(&bad_gate, None).await,
        Err(StoreError::Conflict(_))
    ));
    let gone = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Project(rustryx_core::ids::ProjectId::generate()),
    );
    assert!(matches!(
        policies.upsert_scan_policy(&gone, None).await,
        Err(StoreError::Conflict(_))
    ));
}

fn material(name: &str, kind: TrustMaterialKind, fingerprint_seed: char) -> TrustMaterial {
    TrustMaterial::new(
        TrustMaterialId::generate(),
        kind,
        name,
        "-----BEGIN PUBLIC KEY-----\nMFkw\n-----END PUBLIC KEY-----",
        fingerprint_seed.to_string().repeat(64),
        T0,
    )
}

#[tokio::test]
async fn trust_material_and_trust_policies_reference_each_other_safely() {
    let env = env();
    let created = project(&env.store, "trusting").await;
    let repo = hosted(&env.store, "trusting-repo").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    let trust = env.store.trust();
    let key = material("release-key", TrustMaterialKind::CosignPublicKey, 'a');
    trust.create(&key).await.unwrap();
    assert!(matches!(
        trust.create(&key).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        trust
            .create(&material("release-key", TrustMaterialKind::CosignRoot, 'b'))
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        matches!(
            trust
                .create(&material("other", TrustMaterialKind::CosignPublicKey, 'a'))
                .await,
            Err(StoreError::Conflict(_))
        ),
        "same kind and fingerprint"
    );
    trust
        .create(&material(
            "same-fp-other-kind",
            TrustMaterialKind::NotationCa,
            'a',
        ))
        .await
        .unwrap();
    let mut private = material("private", TrustMaterialKind::CosignPublicKey, 'c');
    private.pem = "-----BEGIN PRIVATE KEY-----".to_owned();
    assert!(matches!(
        trust.create(&private).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(trust.list(None).await.unwrap().len(), 2);
    assert_eq!(
        trust
            .list(Some(TrustMaterialKind::NotationCa))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        trust.get(key.id).await.unwrap().map(|m| m.name),
        Some("release-key".to_owned())
    );

    let policies = env.store.security_policies();
    let mut policy = TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::Project(created.id));
    policy.required = RequiredSignatures::Cosign;
    assert!(
        matches!(
            policies.upsert_trust_policy(&policy, None).await,
            Err(StoreError::Conflict(_))
        ),
        "needs a key"
    );
    policy.cosign_keys = vec![TrustMaterialId::generate()];
    assert!(
        matches!(
            policies.upsert_trust_policy(&policy, None).await,
            Err(StoreError::Conflict(_))
        ),
        "unknown material"
    );
    policy.cosign_keys = vec![key.id];
    policies.upsert_trust_policy(&policy, None).await.unwrap();
    assert!(
        matches!(trust.delete(key.id).await, Err(StoreError::Conflict(_))),
        "a policy lists it"
    );

    // The most specific policy wins as a whole.
    assert_eq!(
        policies
            .effective_trust_for(repo.id)
            .await
            .unwrap()
            .map(|p| p.id),
        Some(policy.id)
    );
    let mut own = TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::Repository(repo.id));
    own.mode = GateMode::Enforce;
    policies.upsert_trust_policy(&own, None).await.unwrap();
    let effective = policies
        .effective_trust_for(repo.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (effective.id, effective.required),
        (own.id, RequiredSignatures::None)
    );
    let system = TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::System);
    policies.upsert_trust_policy(&system, None).await.unwrap();
    let loose = hosted(&env.store, "loose").await;
    assert_eq!(
        policies
            .effective_trust_for(loose.id)
            .await
            .unwrap()
            .map(|p| p.id),
        Some(system.id)
    );
    assert!(
        policies
            .get_trust_policy(PolicyScope::System)
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        policies.effective_trust_for(RepositoryId::generate()).await,
        Err(StoreError::NotFound(_))
    ));

    let mut edited = policy.clone();
    edited.verify_children = false;
    policies
        .upsert_trust_policy(&edited, Some(1))
        .await
        .unwrap();
    assert!(matches!(
        policies.upsert_trust_policy(&edited, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        policies.upsert_trust_policy(&edited, None).await,
        Err(StoreError::Conflict(_))
    ));
    let mut taken = TrustPolicy::new(
        policy.id,
        PolicyScope::Project(rustryx_core::ids::ProjectId::generate()),
    );
    assert!(matches!(
        policies.upsert_trust_policy(&taken, None).await,
        Err(StoreError::Conflict(_))
    ));
    taken.scope = PolicyScope::Repository(loose.id);
    assert!(
        matches!(
            policies.upsert_trust_policy(&taken, None).await,
            Err(StoreError::Conflict(_))
        ),
        "id taken"
    );
    let fresh = TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::Repository(loose.id));
    assert!(matches!(
        policies.upsert_trust_policy(&fresh, Some(3)).await,
        Err(StoreError::Conflict(_))
    ));
    // Deleting the repository takes its policy along, and the project's removal frees the key.
    env.store
        .projects()
        .detach_repository(repo.id)
        .await
        .unwrap();
    env.store.projects().delete(created.id).await.unwrap();
    trust.delete(key.id).await.unwrap();
    assert!(matches!(
        trust.delete(key.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// the Security Hub

#[tokio::test]
async fn the_hub_searches_by_vulnerability_and_refreshes_its_numbers() {
    let env = env();
    let created = project(&env.store, "hub").await;
    let repo = hosted(&env.store, "hub-repo").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    let other_repo = hosted(&env.store, "hub-other").await;
    let one = component(&env.store, repo.id, "one").await;
    let two = component(&env.store, other_repo.id, "two").await;
    let started = report(one, ScanKind::Vulnerability);
    env.store.scans().start(&started).await.unwrap();
    let mut scored = finding(started.id, "CVE-1", "openssl", Severity::Critical);
    scored.cvss = Some(9.8);
    scored.fixed_version = Some("3.0".to_owned());
    env.store
        .scans()
        .complete(
            started.id,
            &[scored, finding(started.id, "CVE-2", "zlib", Severity::Low)],
            &ScanSummary::default(),
            "v",
            "d",
            T0,
            &[],
        )
        .await
        .unwrap();
    scan(
        &env.store,
        two,
        &[("CVE-1", "openssl", Severity::Critical)],
        T0,
    )
    .await;
    let clean = component(&env.store, repo.id, "clean").await;
    scan(&env.store, clean, &[], T0).await;

    let hub = env.store.hub();
    assert!(hub.stats().await.unwrap().is_none());
    assert!(matches!(
        hub.search("  ", &HubFilter::default(), &page(10)).await,
        Err(StoreError::Invalid(_))
    ));
    let all = hub
        .search("cve-1", &HubFilter::default(), &page(10))
        .await
        .unwrap();
    assert_eq!(all.items.len(), 2);
    let row = all.items.iter().find(|r| r.component_id == one).unwrap();
    assert_eq!(
        (row.repository.as_str(), row.coordinate.as_str()),
        ("hub-repo", "one:1")
    );
    assert_eq!(row.fixed_version.as_deref(), Some("3.0"));
    let mut in_project = HubFilter::default();
    in_project.project = Some(created.id);
    assert_eq!(
        hub.search("CVE-1", &in_project, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let mut in_repo = HubFilter::default();
    in_repo.repository = Some(other_repo.id);
    assert_eq!(
        hub.search("CVE-1", &in_repo, &page(10))
            .await
            .unwrap()
            .items[0]
            .component_id,
        two
    );
    let mut scored_high = HubFilter::default();
    scored_high.min_cvss = Some(9.0);
    scored_high.max_cvss = Some(10.0);
    assert_eq!(
        hub.search("CVE-1", &scored_high, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        1,
        "an unscored finding is outside any range"
    );
    let mut by_severity = HubFilter::default();
    by_severity.severity = Some(Severity::Low);
    assert!(
        hub.search("CVE-1", &by_severity, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut by_package = HubFilter::default();
    by_package.package = Some("zlib".to_owned());
    assert!(
        hub.search("CVE-1", &by_package, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut by_digest = HubFilter::default();
    by_digest.digest = Some("1".to_owned());
    assert_eq!(
        hub.search("CVE-1", &by_digest, &page(10))
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    by_digest.digest = Some("sha256:nothing".to_owned());
    assert!(
        hub.search("CVE-1", &by_digest, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut by_tag = HubFilter::default();
    by_tag.tag = Some("latest".to_owned());
    assert!(
        hub.search("CVE-1", &by_tag, &page(10))
            .await
            .unwrap()
            .items
            .is_empty(),
        "tags are not modelled"
    );
    let first_page = hub
        .search("CVE-1", &HubFilter::default(), &page(1))
        .await
        .unwrap();
    assert!(first_page.next.is_some());

    hub.refresh_stats(env.clock_now()).await.unwrap();
    let stats = hub.stats().await.unwrap().unwrap();
    assert_eq!(stats.computed_at, env.clock_now());
    assert_eq!(stats.components_by_severity[Severity::Critical.index()], 2);
    assert_eq!(
        stats.components_by_severity[0], 1,
        "the clean component counts at index 0"
    );
    assert_eq!(stats.findings_by_severity[Severity::Critical.index()], 2);
    assert_eq!(stats.findings_by_severity[Severity::Low.index()], 1);
    assert_eq!(stats.top_vulns[0], ("CVE-1".to_owned(), 2));
    assert_eq!(stats.top_components[0], (one, 2));
}

// exports

#[tokio::test]
async fn an_export_allows_one_download_by_whoever_asked_before_it_expires() {
    let env = env();
    let exports = env.store.exports();
    let owner = UserId::generate();
    let mut job = ExportJob::new(
        ExportId::generate(),
        "vulnerabilities",
        serde_json::json!({}),
        env.clock_now() + time::Duration::hours(24),
        env.clock_now(),
    );
    job.requested_by = Some(owner);
    exports.create(&job).await.unwrap();
    assert!(matches!(
        exports.create(&job).await,
        Err(StoreError::Conflict(_))
    ));
    let blob = BlobId::generate();
    exports
        .finish(job.id, TaskRunStatus::Succeeded, Some(blob))
        .await
        .unwrap();
    let stored = exports.get(job.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.status, stored.blob_id),
        (TaskRunStatus::Succeeded, Some(blob))
    );

    assert!(
        !exports
            .claim_download(job.id, Some(UserId::generate()), env.clock_now())
            .await
            .unwrap(),
        "someone else"
    );
    assert!(
        !exports
            .claim_download(job.id, None, env.clock_now())
            .await
            .unwrap()
    );
    assert!(
        exports
            .claim_download(job.id, Some(owner), env.clock_now())
            .await
            .unwrap()
    );
    assert!(
        !exports
            .claim_download(job.id, Some(owner), env.clock_now())
            .await
            .unwrap(),
        "one download"
    );
    assert_eq!(
        exports.get(job.id).await.unwrap().unwrap().downloaded_at,
        Some(env.clock_now())
    );

    let mut anonymous = job.clone();
    anonymous.id = ExportId::generate();
    anonymous.requested_by = None;
    anonymous.downloaded_at = None;
    exports.create(&anonymous).await.unwrap();
    env.clock.advance(Duration::from_hours(25));
    assert!(
        !exports
            .claim_download(anonymous.id, None, env.clock_now())
            .await
            .unwrap(),
        "expired"
    );

    let due = exports.due_for_purge(env.clock_now(), 10).await.unwrap();
    assert_eq!(due.len(), 2, "downloaded and expired jobs");
    assert_eq!(
        exports
            .due_for_purge(env.clock_now(), 1)
            .await
            .unwrap()
            .len(),
        1
    );
    exports.delete(job.id).await.unwrap();
    assert!(matches!(
        exports.delete(job.id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        exports.finish(job.id, TaskRunStatus::Failed, None).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        exports.claim_download(job.id, None, env.clock_now()).await,
        Err(StoreError::NotFound(_))
    ));
}

// advisory data sets and scan tokens

fn dataset(version: &str, blob: BlobId, fetched: time::OffsetDateTime) -> ScannerDataSet {
    ScannerDataSet::new(
        ScannerDataSetId::generate(),
        ScannerKind::Trivy,
        "vuln-db",
        version,
        blob,
        "a".repeat(64),
        fetched,
    )
}

#[tokio::test]
async fn publishing_a_data_set_moves_the_current_flag_and_keeps_the_newest_two() {
    let env = env();
    let sets = env.store.datasets();
    assert!(
        sets.current(ScannerKind::Trivy, "vuln-db")
            .await
            .unwrap()
            .is_none()
    );
    let mut published = Vec::new();
    for round in 0..4 {
        let blob = rustryx_testkit::fixtures::blob_record(&format!("ds-{round}"), b"data");
        env.store.blobs().insert(&blob).await.unwrap();
        let set = dataset(
            &format!("v{round}"),
            blob.id,
            T0 + time::Duration::hours(round),
        );
        sets.publish(&set).await.unwrap();
        published.push(set);
    }
    let current = sets
        .current(ScannerKind::Trivy, "vuln-db")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((current.version.as_str(), current.is_current), ("v3", true));
    assert!(
        sets.current(ScannerKind::Osv, "vuln-db")
            .await
            .unwrap()
            .is_none()
    );
    let prunable = sets.prunable(ScannerKind::Trivy, "vuln-db").await.unwrap();
    let mut versions: Vec<&str> = prunable.iter().map(|d| d.version.as_str()).collect();
    versions.sort_unstable();
    assert_eq!(versions, ["v0", "v1"]);
    sets.delete(prunable[0].id).await.unwrap();
    assert!(matches!(
        sets.delete(prunable[0].id).await,
        Err(StoreError::NotFound(_))
    ));

    let unknown_blob = dataset("v9", BlobId::generate(), T0);
    assert!(matches!(
        sets.publish(&unknown_blob).await,
        Err(StoreError::Conflict(_))
    ));
    let mut bad_digest = published[3].clone();
    bad_digest.id = ScannerDataSetId::generate();
    bad_digest.sha256 = "short".to_owned();
    assert!(matches!(
        sets.publish(&bad_digest).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        matches!(
            sets.publish(&published[3]).await,
            Err(StoreError::Conflict(_))
        ),
        "id taken"
    );
}

#[tokio::test]
async fn a_scan_token_is_stored_as_a_digest_and_stops_working_at_its_expiry() {
    let env = env();
    let sets = env.store.datasets();
    let hash = "b".repeat(64);
    let scan_id = ScanReportId::generate();
    let repo = RepositoryId::generate();
    let expires = env.clock_now() + time::Duration::minutes(15);
    sets.create_scan_token(&hash, scan_id, repo, expires)
        .await
        .unwrap();
    assert!(matches!(
        sets.create_scan_token(&hash, scan_id, repo, expires).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        matches!(
            sets.create_scan_token("the-token-itself", scan_id, repo, expires)
                .await,
            Err(StoreError::Conflict(_))
        ),
        "only a digest is ever stored"
    );
    assert_eq!(
        sets.find_scan_token(&hash, env.clock_now()).await.unwrap(),
        Some((scan_id, repo))
    );
    assert!(
        sets.find_scan_token(&"c".repeat(64), env.clock_now())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        sets.find_scan_token(&hash, expires)
            .await
            .unwrap()
            .is_none(),
        "an expired token is not returned"
    );
}
