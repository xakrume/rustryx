//! Unit tests of the parent module.

use super::*;
use crate::ids::{
    BlobId, ComponentId, CveAllowlistEntryId, ProjectId, RepositoryId, ScanReportId,
    ScannerDataSetId, ScannerId, SecurityPolicyId, TrustMaterialId, TrustPolicyId,
};
use crate::lifecycle::PolicyScope;
use crate::secret::Secret;
use crate::testing::{assert_wire, at, round_trip_json};

fn summary(counts: [u32; 6]) -> ScanSummary {
    ScanSummary {
        by_severity: counts,
        total: counts.iter().sum(),
        ..ScanSummary::default()
    }
}

#[test]
fn cvss_buckets_follow_the_compat_ranges() {
    assert_eq!(
        cvss_bucket(Some(7.0), Severity::Low),
        Some(VulnBucket::Critical)
    );
    assert_eq!(
        cvss_bucket(Some(6.9), Severity::Critical),
        Some(VulnBucket::Severe)
    );
    assert_eq!(
        cvss_bucket(Some(1.0), Severity::Critical),
        Some(VulnBucket::Moderate)
    );
    assert_eq!(cvss_bucket(Some(0.9), Severity::Critical), None);
    assert_eq!(
        cvss_bucket(None, Severity::High),
        Some(VulnBucket::Critical)
    );
    assert_eq!(cvss_bucket(None, Severity::Unknown), None);
    let group = RepositoryHealth::sum(
        RepositoryId::generate(),
        &[RepositoryHealth::unscanned(RepositoryId::generate())],
    );
    assert!(!group.scanned);
}

#[test]
fn severity_order_and_indexes_follow_the_declaration() {
    for (index, severity) in Severity::ALL.iter().enumerate() {
        assert_eq!(severity.index(), index);
    }
    assert!(Severity::Unknown < Severity::Negligible && Severity::Negligible < Severity::Low);
    assert!(Severity::Low < Severity::Medium && Severity::High < Severity::Critical);
    for severity in Severity::ALL {
        assert_wire(&severity, &format!("\"{}\"", severity.as_str()));
    }
}

#[test]
fn scanner_labels_parse_in_any_case_and_unknown_words_stay_unknown() {
    assert_eq!(Severity::from_scanner_label("CRITICAL"), Severity::Critical);
    assert_eq!(Severity::from_scanner_label(" High "), Severity::High);
    assert_eq!(
        Severity::from_scanner_label("negligible"),
        Severity::Negligible
    );
    for odd in ["", "UNSPECIFIED", "important", "sev-9"] {
        assert_eq!(
            Severity::from_scanner_label(odd),
            Severity::Unknown,
            "{odd}"
        );
    }
}

#[test]
fn must_not_block_on_negligible_nor_let_an_unknown_severity_bypass_the_gate() {
    for threshold in [
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ] {
        assert!(
            !Severity::Negligible.blocks_at(threshold),
            "negligible never blocks"
        );
    }
    assert!(
        Severity::Unknown.blocks_at(Severity::Medium),
        "unknown counts as medium"
    );
    assert!(Severity::Unknown.blocks_at(Severity::Low));
    assert!(!Severity::Unknown.blocks_at(Severity::High));
    assert_eq!(Severity::Unknown.gate_equivalent(), Severity::Medium);
    assert_eq!(Severity::High.gate_equivalent(), Severity::High);
    assert!(
        Severity::High.blocks_at(Severity::High) && !Severity::Medium.blocks_at(Severity::High)
    );
}

#[test]
fn summary_reports_the_highest_blocking_severity_and_the_count() {
    // [unknown, negligible, low, medium, high, critical]
    let s = summary([2, 5, 3, 1, 4, 0]);
    assert_eq!(s.count(Severity::High), 4);
    assert_eq!(s.blocking(Severity::High), Some((Severity::High, 4)));
    assert_eq!(
        s.blocking(Severity::Medium),
        Some((Severity::High, 7)),
        "medium + unknown + high"
    );
    assert_eq!(
        s.blocking(Severity::Low),
        Some((Severity::High, 10)),
        "negligible never counts"
    );
    assert_eq!(s.blocking(Severity::Critical), None);
}

#[test]
fn unknown_findings_rank_with_medium_and_lose_the_tie() {
    let only_unknown = summary([3, 0, 0, 0, 0, 0]);
    assert_eq!(
        only_unknown.blocking(Severity::Medium),
        Some((Severity::Unknown, 3))
    );
    assert_eq!(only_unknown.blocking(Severity::High), None);
    let with_medium = summary([3, 0, 0, 1, 0, 0]);
    assert_eq!(
        with_medium.blocking(Severity::Medium),
        Some((Severity::Medium, 4))
    );
    let with_low = summary([1, 0, 9, 0, 0, 0]);
    assert_eq!(
        with_low.blocking(Severity::Low),
        Some((Severity::Unknown, 10))
    );
    assert_eq!(summary([0; 6]).blocking(Severity::Low), None);
}

#[test]
fn summary_counts_saturate_instead_of_wrapping() {
    let huge = ScanSummary {
        by_severity: [u32::MAX, 0, 0, u32::MAX, 0, 0],
        ..ScanSummary::default()
    };
    assert_eq!(
        huge.blocking(Severity::Medium).map(|(_, count)| count),
        Some(u32::MAX)
    );
}

#[test]
fn scan_status_wire_names_and_terminal_states() {
    let table = [
        (ScanStatus::Queued, "queued", false),
        (ScanStatus::Running, "running", false),
        (ScanStatus::Complete, "complete", true),
        (ScanStatus::Failed, "failed", true),
        (ScanStatus::Stopped, "stopped", true),
        (ScanStatus::Unsupported, "unsupported", true),
    ];
    for (status, name, terminal) in table {
        assert_wire(&status, &format!("\"{name}\""));
        assert_eq!(status.is_terminal(), terminal);
        // No wildcard arm: a new status must be placed in this table first.
        match status {
            ScanStatus::Queued
            | ScanStatus::Running
            | ScanStatus::Complete
            | ScanStatus::Failed
            | ScanStatus::Stopped
            | ScanStatus::Unsupported => {}
        }
    }
}

#[test]
fn small_enums_use_snake_case() {
    assert_wire(&ScanKind::Sbom, "\"sbom\"");
    assert_wire(&ScannerKind::Osv, "\"osv\"");
    assert_wire(&ScanTrigger::Rescan, "\"rescan\"");
    assert_wire(&QuarantineState::Quarantined, "\"quarantined\"");
    assert_wire(&GateMode::AuditOnly, "\"audit_only\"");
    assert_wire(&AllowlistMode::Project, "\"project\"");
    assert_wire(&RequiredSignatures::Both, "\"both\"");
    assert_wire(&TrustMaterialKind::CosignPublicKey, "\"cosign_public_key\"");
    assert_wire(&TrustMaterialKind::NotationCa, "\"notation_ca\"");
}

#[test]
fn must_not_offer_a_presence_only_notation_level() {
    // No wildcard arm and no `Skip`: the only levels are the three that verify.
    for level in [
        NotationLevel::Strict,
        NotationLevel::Permissive,
        NotationLevel::Audit,
    ] {
        let name = match level {
            NotationLevel::Strict => "strict",
            NotationLevel::Permissive => "permissive",
            NotationLevel::Audit => "audit",
        };
        assert_wire(&level, &format!("\"{name}\""));
    }
    assert!(serde_json::from_str::<NotationLevel>("\"skip\"").is_err());
}

#[test]
fn must_not_leak_a_scanner_credential_through_debug() {
    let auths = [
        ScannerAuth::Basic {
            username: "u".into(),
            password: Secret::new("canary-pass"),
        },
        ScannerAuth::Bearer {
            token: Secret::new("canary-token"),
        },
        ScannerAuth::ApiKey {
            header: "X-Key".into(),
            value: Secret::new("canary-key"),
        },
    ];
    for auth in auths {
        let mut record = ScannerRecord::new(ScannerId::generate(), "s", ScannerKind::Http, at());
        record.auth = auth.clone();
        for text in [format!("{auth:?}"), format!("{record:?}")] {
            assert!(!text.contains("canary"), "{text}");
            assert!(text.contains("REDACTED"), "{text}");
        }
    }
}

#[test]
fn scanner_auth_wire_form_is_internally_tagged() {
    assert_wire_json_type(&ScannerAuth::None, "none");
    assert_wire_json_type(
        &ScannerAuth::Bearer {
            token: Secret::new("t"),
        },
        "bearer",
    );
    assert_wire_json_type(
        &ScannerAuth::ApiKey {
            header: "h".into(),
            value: Secret::new("v"),
        },
        "api_key",
    );
}

fn assert_wire_json_type(auth: &ScannerAuth, expected: &str) {
    let json = round_trip_json(auth);
    assert_eq!(json["type"], expected);
}

#[test]
fn scanner_record_defaults_and_validation() {
    let mut local = ScannerRecord::new(ScannerId::generate(), "trivy", ScannerKind::Trivy, at());
    assert!(local.enabled && !local.is_default && local.version == 1);
    assert_eq!(local.capabilities, vec![ScanKind::Vulnerability]);
    assert!(local.validate().is_ok());
    local.endpoint = Some("https://x".into());
    assert!(local.validate().is_err(), "a local scanner has no endpoint");

    let mut http = ScannerRecord::new(ScannerId::generate(), "ext", ScannerKind::Http, at());
    assert!(
        http.validate().is_err(),
        "an http scanner needs an endpoint"
    );
    http.endpoint = Some("http://insecure".into());
    assert!(http.validate().is_err(), "https only");
    http.endpoint = Some("https://scanner.example.org".into());
    assert!(http.validate().is_ok());
    http.capabilities.clear();
    assert!(http.validate().is_err());
    http.capabilities.push(ScanKind::Sbom);
    http.name = String::new();
    assert!(http.validate().is_err());
}

#[test]
fn scanner_record_wire_form() {
    let mut record = ScannerRecord::new(ScannerId::generate(), "s", ScannerKind::Osv, at());
    record.last_probe = Some(ProbeResult {
        at: at(),
        ok: true,
        version: Some("1.9".into()),
        message: "ok".into(),
    });
    let json = round_trip_json(&record);
    assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["last_probe"]["at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["auth"]["type"], "none");
}

#[test]
fn report_constructor_defaults_validation_and_wire_form() {
    let mut report = ScanReport::new(
        ScanReportId::generate(),
        ComponentId::generate(),
        ScannerKind::Trivy,
        ScanKind::Vulnerability,
        ScanTrigger::Push,
        at(),
    );
    assert_eq!(report.status, ScanStatus::Queued);
    assert!(!report.is_current && report.error.is_none());
    assert_eq!(report.summary.total, 0);
    assert!(report.validate().is_ok());
    report.error = Some("e".repeat(500));
    assert!(report.validate().is_ok());
    report.error = Some("é".repeat(501));
    assert!(report.validate().is_err());
    report.error = None;
    report.finished_at = Some(at());
    let json = round_trip_json(&report);
    assert_eq!(json["queued_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["finished_at"], "2026-10-24T10:00:00Z");
    assert_eq!(
        json["summary"]["by_severity"],
        serde_json::json!([0, 0, 0, 0, 0, 0])
    );
}

#[test]
fn vulnerability_ids_are_normalized_to_upper_case() {
    assert_eq!(
        normalize_vuln_id("cve-2024-1234").expect("id"),
        "CVE-2024-1234"
    );
    assert_eq!(
        normalize_vuln_id("  GHSA-jfh8-c2jp-5v3q ").expect("id"),
        "GHSA-JFH8-C2JP-5V3Q"
    );
    assert!(normalize_vuln_id("").is_err());
    assert!(normalize_vuln_id("   ").is_err());
    assert!(normalize_vuln_id(&"X".repeat(65)).is_err());
    assert!(normalize_vuln_id(&"X".repeat(64)).is_ok());
    assert!(normalize_vuln_id("CVE 2024").is_err());
    assert!(normalize_vuln_id("CVE\n1").is_err());
    assert!(normalize_vuln_id("CVE-é").is_err());
}

#[test]
fn finding_validation() {
    let mut finding = Finding {
        report_id: ScanReportId::generate(),
        vuln_id: "CVE-2024-1".into(),
        package: "lib".into(),
        package_version: "1".into(),
        fixed_version: None,
        severity: Severity::High,
        cvss: Some(7.5),
        title: Some("t".repeat(200)),
    };
    assert!(finding.validate().is_ok());
    finding.title = Some("t".repeat(201));
    assert!(finding.validate().is_err());
    finding.title = None;
    finding.vuln_id = "cve-2024-1".into();
    assert!(finding.validate().is_err(), "ids are stored normalized");
    finding.vuln_id = "CVE-2024-1".into();
    for bad in [-0.1_f32, 10.1, f32::NAN] {
        finding.cvss = Some(bad);
        assert!(finding.validate().is_err(), "{bad}");
    }
    assert_eq!(round_trip_json(&finding)["severity"], "high");
}

#[test]
fn state_defaults_say_not_scanned_and_never_clean() {
    let mut state = ComponentSecurityState::new(ComponentId::generate());
    assert!(!state.is_scanned(), "no result is not clean");
    assert_eq!(state.quarantine, QuarantineState::None);
    for status in [
        ScanStatus::Queued,
        ScanStatus::Running,
        ScanStatus::Failed,
        ScanStatus::Stopped,
        ScanStatus::Unsupported,
    ] {
        state.scan_status = Some(status);
        assert!(!state.is_scanned(), "{status:?} is not clean");
    }
    state.scan_status = Some(ScanStatus::Complete);
    assert!(state.is_scanned());
    state.scanned_at = Some(at());
    assert_eq!(
        round_trip_json(&state)["scanned_at"],
        "2026-10-24T10:00:00Z"
    );
}

#[test]
fn gate_policy_defaults_to_audit_only_high_and_fail_closed() {
    let gate = PullGatePolicy::default();
    assert_eq!(gate.threshold, Some(Severity::High));
    assert_eq!(gate.mode, GateMode::AuditOnly);
    assert!(!gate.allow_unscanned);
    assert!(gate.max_report_age_days.is_none());
    assert!(gate.validate().is_ok());
}

#[test]
fn must_not_accept_a_gate_threshold_that_blocks_everything() {
    for bad in [Severity::Negligible, Severity::Unknown] {
        let gate = PullGatePolicy {
            threshold: Some(bad),
            ..PullGatePolicy::default()
        };
        assert!(gate.validate().is_err(), "{bad:?}");
    }
    for good in [None, Some(Severity::Low), Some(Severity::Critical)] {
        let gate = PullGatePolicy {
            threshold: good,
            ..PullGatePolicy::default()
        };
        assert!(gate.validate().is_ok(), "{good:?}");
    }
}

#[test]
fn the_seeded_system_policy_is_complete_and_a_new_policy_inherits() {
    let seeded = ScanPolicy::system_default(SecurityPolicyId::generate());
    assert_eq!(seeded.scope, PolicyScope::System);
    assert!(seeded.is_complete());
    assert_eq!(seeded.auto_scan, Some(true));
    assert_eq!(seeded.quarantine, Some(false));
    assert_eq!(seeded.allowlist_mode, Some(AllowlistMode::System));
    assert_eq!(
        (seeded.health_check, seeded.sbom_on_push),
        (Some(false), Some(false))
    );
    assert!(seeded.scanner_id.is_none());
    assert_eq!(seeded.gate, Some(PullGatePolicy::default()));
    let inheriting = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Project(ProjectId::generate()),
    );
    assert!(!inheriting.is_complete() && inheriting.gate.is_none() && inheriting.version == 1);
    assert!(inheriting.validate().is_ok());
    let mut bad = inheriting;
    bad.gate = Some(PullGatePolicy {
        threshold: Some(Severity::Negligible),
        ..PullGatePolicy::default()
    });
    assert!(bad.validate().is_err());
}

#[test]
fn allowlist_entries_expire_at_once_and_need_a_reason() {
    let mut entry = CveAllowlistEntry::new(
        CveAllowlistEntryId::generate(),
        AllowlistScope::Project(ProjectId::generate()),
        "CVE-2024-1",
        "accepted until the patch release",
        at(),
    );
    assert!(entry.validate().is_ok() && entry.is_active_at(at()));
    entry.expires_at = Some(at() + time::Duration::days(1));
    assert!(entry.is_active_at(at()));
    assert!(
        !entry.is_active_at(at() + time::Duration::days(1)),
        "expiry at now is expired"
    );
    entry.reason.clear();
    assert!(entry.validate().is_err());
    entry.reason = "r".repeat(500);
    assert!(entry.validate().is_ok());
    entry.reason.push('r');
    assert!(entry.validate().is_err());
    entry.reason = "ok".into();
    entry.vuln_id = "cve-2024-1".into();
    assert!(entry.validate().is_err());
}

#[test]
fn allowlist_scope_and_gate_decision_wire_forms() {
    let component = ComponentId::generate();
    assert_wire(&AllowlistScope::System, r#"{"scope":"system"}"#);
    assert_wire(
        &AllowlistScope::Component(component),
        &format!(r#"{{"scope":"component","id":"{component}"}}"#),
    );
    assert_wire(&GateDecision::Allow, r#"{"decision":"allow"}"#);
    let deny = GateDecision::Deny {
        reason: GateReason::AboveThreshold {
            highest: Severity::Critical,
            count: 2,
            threshold: Severity::High,
        },
    };
    assert_wire(
        &deny,
        r#"{"decision":"deny","reason":{"reason":"above_threshold","highest":"critical","count":2,"threshold":"high"}}"#,
    );
    assert_wire(
        &GateDecision::AllowAudited {
            reason: GateReason::NotScanned,
        },
        r#"{"decision":"allow_audited","reason":{"reason":"not_scanned"}}"#,
    );
    for (reason, name) in [
        (GateReason::ScanFailed, "scan_failed"),
        (GateReason::ScanStale, "scan_stale"),
        (GateReason::Quarantined, "quarantined"),
        (GateReason::SignatureMissing, "signature_missing"),
        (GateReason::SignatureInvalid, "signature_invalid"),
        (GateReason::Error, "error"),
    ] {
        assert_wire(&reason, &format!(r#"{{"reason":"{name}"}}"#));
    }
}

#[test]
fn trust_policy_defaults_and_the_material_rule() {
    let mut policy = TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::System);
    assert_eq!(policy.required, RequiredSignatures::None);
    assert_eq!(policy.mode, GateMode::AuditOnly);
    assert!(
        policy.verify_children,
        "an index is judged by its children by default"
    );
    assert_eq!(policy.notation_level, NotationLevel::Strict);
    assert!(policy.validate().is_ok());

    policy.required = RequiredSignatures::Cosign;
    assert!(
        policy.validate().is_err(),
        "cosign without a key or root verifies nothing"
    );
    policy.cosign_roots.push(TrustMaterialId::generate());
    assert!(policy.validate().is_ok());

    policy.required = RequiredSignatures::Both;
    assert!(
        policy.validate().is_err(),
        "both needs the notation store too"
    );
    policy.notation_stores.push(TrustMaterialId::generate());
    assert!(policy.validate().is_ok());

    policy.required = RequiredSignatures::Notation;
    policy.notation_stores.clear();
    assert!(policy.validate().is_err());
}

#[test]
fn trust_material_rejects_private_keys_and_malformed_fingerprints() {
    let ok = TrustMaterial::new(
        TrustMaterialId::generate(),
        TrustMaterialKind::CosignPublicKey,
        "release key",
        "-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----\n",
        "ab".repeat(32),
        at(),
    );
    assert!(ok.validate().is_ok());
    for private in [
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
        "x PRIVATE KEY x",
    ] {
        let mut bad = ok.clone();
        bad.pem = format!("{}\n{private}\n", ok.pem);
        assert!(bad.validate().is_err(), "{private}");
    }
    let mut long = ok.clone();
    long.pem = "a".repeat(MAX_TRUST_PEM_BYTES + 1);
    assert!(long.validate().is_err());
    for fingerprint in [
        "",
        "AB".repeat(32).as_str(),
        &"a".repeat(63),
        &"g".repeat(64),
    ] {
        let mut bad = ok.clone();
        bad.fingerprint_sha256 = fingerprint.to_owned();
        assert!(bad.validate().is_err(), "{fingerprint}");
    }
    let mut unnamed = ok;
    unnamed.name.clear();
    assert!(unnamed.validate().is_err());
}

#[test]
fn data_sets_need_a_hex_digest_and_serialize_the_fetch_time() {
    let mut set = ScannerDataSet::new(
        ScannerDataSetId::generate(),
        ScannerKind::Trivy,
        "trivy-db",
        "2",
        BlobId::generate(),
        "0".repeat(64),
        at(),
    );
    assert!(!set.is_current && set.source.is_empty());
    assert!(set.validate().is_ok());
    assert_eq!(round_trip_json(&set)["fetched_at"], "2026-10-24T10:00:00Z");
    set.sha256 = "xyz".into();
    assert!(set.validate().is_err());
}
