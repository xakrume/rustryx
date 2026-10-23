//! Unit tests of the parent module.

use super::*;
use crate::identity::IdpProvider;
use crate::ids::TrustedCertificateId;
use crate::testing::{assert_wire, at, round_trip_json};

#[test]
fn section_keys_pointers_and_areas_are_pinned() {
    let expected = [
        (
            SettingsSection::Smtp,
            "settings.smtp",
            true,
            AppArea::Settings,
        ),
        (
            SettingsSection::OutboundProxy,
            "settings.outbound_proxy",
            true,
            AppArea::Settings,
        ),
        (
            SettingsSection::Trust,
            "settings.trust",
            false,
            AppArea::Settings,
        ),
        (
            SettingsSection::Logging,
            "settings.logging",
            false,
            AppArea::Settings,
        ),
        (
            SettingsSection::Projects,
            "settings.projects",
            false,
            AppArea::Settings,
        ),
        (
            SettingsSection::Quota,
            "settings.quota",
            false,
            AppArea::Quotas,
        ),
        (
            SettingsSection::Storage,
            "settings.storage",
            false,
            AppArea::Blobstores,
        ),
        (
            SettingsSection::Lifecycle,
            "settings.lifecycle",
            false,
            AppArea::Tasks,
        ),
        (
            SettingsSection::Webhooks,
            "settings.webhooks",
            false,
            AppArea::Webhooks,
        ),
        (
            SettingsSection::Notifications,
            "settings.notifications",
            false,
            AppArea::Settings,
        ),
    ];
    assert_eq!(SettingsSection::ALL.len(), expected.len());
    for (section, key, sealed, area) in expected {
        assert_eq!(section.key(), key);
        assert_eq!(section.secret_pointers().is_empty(), !sealed, "{section:?}");
        if sealed {
            assert_eq!(section.secret_pointers(), ["/password"]);
        }
        assert_eq!(section.area(), area, "{section:?}");
        assert_wire(
            &section,
            &format!("\"{}\"", key.trim_start_matches("settings.")),
        );
    }
    let keys: std::collections::HashSet<_> = SettingsSection::ALL.iter().map(|s| s.key()).collect();
    assert_eq!(keys.len(), SettingsSection::ALL.len(), "keys are unique");
}

#[test]
fn must_not_represent_plaintext_smtp() {
    // The only variants are the two TLS modes; no wire value and no field turns TLS off.
    for (tls, name) in [
        (SmtpTls::Implicit, "implicit"),
        (SmtpTls::Starttls, "starttls"),
    ] {
        assert_wire(&tls, &format!("\"{name}\""));
        match tls {
            SmtpTls::Implicit | SmtpTls::Starttls => {}
        }
    }
    for plain in ["plain", "none", "off", "false", ""] {
        assert!(
            serde_json::from_str::<SmtpTls>(&format!("\"{plain}\"")).is_err(),
            "{plain:?}"
        );
    }
    let json = serde_json::to_value(SmtpSettings::default()).expect("json");
    for field in json.as_object().expect("object").keys() {
        for forbidden in ["verify", "insecure", "skip", "plaintext", "no_tls"] {
            assert!(!field.contains(forbidden), "no switch to forget: {field}");
        }
    }
}

fn rejects_unknown_field<T: serde::de::DeserializeOwned>() -> bool {
    serde_json::from_str::<T>(r#"{"bogus":1}"#).is_err()
}

#[test]
fn must_not_accept_unknown_settings_fields() {
    assert!(rejects_unknown_field::<SmtpSettings>());
    assert!(rejects_unknown_field::<OutboundProxySettings>());
    assert!(rejects_unknown_field::<TrustSettings>());
    assert!(rejects_unknown_field::<LoggingSettings>());
    assert!(rejects_unknown_field::<ProjectsSettings>());
    assert!(rejects_unknown_field::<QuotaSettings>());
    assert!(rejects_unknown_field::<StorageSettings>());
    assert!(rejects_unknown_field::<LifecycleSettings>());
    assert!(rejects_unknown_field::<WebhooksSettings>());
    assert!(rejects_unknown_field::<NotificationSettings>());
    assert!(rejects_unknown_field::<GroupMappingPolicy>());
    assert!(rejects_unknown_field::<RobotSettings>());
    assert!(rejects_unknown_field::<ProxyApplication>());
    assert!(rejects_unknown_field::<ReadOnlyState>());
}

#[test]
fn an_empty_document_reads_as_the_default_of_every_section() {
    // A missing row and a document that lacks fields behave the same on every node.
    let smtp: SmtpSettings = serde_json::from_str("{}").expect("defaults");
    assert_eq!(
        (smtp.enabled, smtp.port, smtp.tls, smtp.timeout_secs),
        (false, 587, SmtpTls::Starttls, 10)
    );
    let trust: TrustSettings = serde_json::from_str("{}").expect("defaults");
    assert!(!trust.allow_fetch && trust.use_system_roots);
    let projects: ProjectsSettings = serde_json::from_str("{}").expect("defaults");
    assert_eq!(projects.creation, ProjectCreation::AdminsOnly);
    let lifecycle: LifecycleSettings = serde_json::from_str("{}").expect("defaults");
    assert_eq!(lifecycle.batch_size, 500);
    let partial: SmtpSettings =
        serde_json::from_str(r#"{"enabled":true,"host":"mail.example.org"}"#).expect("partial");
    assert!(
        partial.enabled && partial.port == 587,
        "absent fields take the default"
    );
    let read_only: ReadOnlyState = serde_json::from_str("{}").expect("defaults");
    assert!(!read_only.enabled && read_only.set_at.is_none());
}

#[test]
fn defaults_match_the_design_table() {
    let quota = QuotaSettings::default();
    assert_eq!(
        (quota.default_project_bytes, quota.default_warn_percent),
        (None, 80)
    );
    assert_eq!(
        StorageSettings::default().last_downloaded_interval_secs,
        3600
    );
    let lifecycle = LifecycleSettings::default();
    assert_eq!(lifecycle.dry_run_validity_days, 14);
    assert!((lifecycle.max_delete_ratio - 0.9).abs() < f32::EPSILON);
    assert_eq!(
        (lifecycle.mass_delete_min_candidates, lifecycle.batch_size),
        (20, 500)
    );
    assert!(WebhooksSettings::default().enabled);
    assert_eq!(NotificationSettings::default().task_recipients.len(), 0);
    let robots = RobotSettings::default();
    assert_eq!(robots.name_prefix, "robot");
    assert_eq!(
        (
            robots.default_ttl_days,
            robots.max_ttl_days,
            robots.expired_retention_days
        ),
        (30, 365, 30)
    );
    assert!(!robots.allow_never_expire);
    let mapping = GroupMappingPolicy::default();
    assert!(
        !mapping.project_admins_may_map
            && mapping.oidc_path_prefixes.is_empty()
            && mapping.ldap_dn_suffixes.is_empty()
    );
    let proxy = OutboundProxySettings::default();
    assert!(!proxy.enabled && proxy.http_proxy.is_none() && proxy.no_proxy.is_empty());
    assert!(LoggingSettings::default().overrides.is_empty());
    for section_default_validates in [
        SmtpSettings::default().validate(),
        OutboundProxySettings::default().validate(),
        ProjectsSettings::default().validate(),
        QuotaSettings::default().validate(),
        LifecycleSettings::default().validate(),
        NotificationSettings::default().validate(),
        LoggingSettings::default().validate(at()),
    ] {
        assert!(section_default_validates.is_ok());
    }
}

#[test]
fn must_not_leak_the_smtp_or_proxy_password_through_debug() {
    let smtp = SmtpSettings {
        username: Some("mailer".into()),
        password: Some(Secret::new("canary-smtp-password")),
        ..SmtpSettings::default()
    };
    let proxy = OutboundProxySettings {
        username: Some("proxy-user".into()),
        password: Some(Secret::new("canary-proxy-password")),
        ..OutboundProxySettings::default()
    };
    for text in [format!("{smtp:?}"), format!("{proxy:?}")] {
        assert!(!text.contains("canary"), "{text}");
        assert!(text.contains("REDACTED"), "{text}");
    }
}

#[test]
fn smtp_validation() {
    let enabled = SmtpSettings {
        enabled: true,
        host: "mail.example.org".into(),
        from_address: "rustryx@example.org".into(),
        ..SmtpSettings::default()
    };
    assert!(enabled.validate().is_ok());

    let mut bad = enabled.clone();
    bad.host = String::new();
    assert!(bad.validate().is_err(), "an enabled section needs a host");
    bad = enabled.clone();
    bad.port = 0;
    assert!(bad.validate().is_err());
    bad = enabled.clone();
    bad.from_address = "not-an-address".into();
    assert!(bad.validate().is_err());
    for timeout in [0u8, 61] {
        bad = enabled.clone();
        bad.timeout_secs = timeout;
        assert!(bad.validate().is_err(), "{timeout}");
    }
    for timeout in [1u8, 60] {
        bad = enabled.clone();
        bad.timeout_secs = timeout;
        assert!(bad.validate().is_ok(), "{timeout}");
    }
    bad = enabled.clone();
    bad.from_name = "n".repeat(101);
    assert!(bad.validate().is_err());
    bad.from_name = "n".repeat(100);
    assert!(bad.validate().is_ok());
    // A disabled section may be incomplete.
    assert!(SmtpSettings::default().validate().is_ok());
}

#[test]
fn must_not_allow_header_injection_in_settings() {
    for field in 0..4 {
        for injected in ["a\r\nBcc: x@y", "a\nb", "a\0b"] {
            let mut smtp = SmtpSettings {
                enabled: true,
                host: "mail.example.org".into(),
                from_address: "rustryx@example.org".into(),
                from_name: "Rustryx".into(),
                username: Some("u".into()),
                ..SmtpSettings::default()
            };
            match field {
                0 => smtp.host = injected.into(),
                1 => smtp.from_address = injected.into(),
                2 => smtp.from_name = injected.into(),
                _ => smtp.username = Some(injected.into()),
            }
            assert!(smtp.validate().is_err(), "field {field} {injected:?}");
        }
    }
    // The check also applies to a disabled section, so a bad value cannot wait for the switch.
    let disabled = SmtpSettings {
        from_name: "a\nb".into(),
        ..SmtpSettings::default()
    };
    assert!(disabled.validate().is_err());
}

#[test]
fn proxy_validation() {
    let mut proxy = OutboundProxySettings {
        enabled: true,
        http_proxy: Some("http://proxy.internal:3128".into()),
        https_proxy: Some("https://proxy.internal:3129".into()),
        no_proxy: vec!["localhost".into(), "*.internal".into(), "10.0.0.0/8".into()],
        ..OutboundProxySettings::default()
    };
    assert!(proxy.validate().is_ok());
    proxy.http_proxy = Some("http://proxy.internal".into());
    assert!(proxy.validate().is_ok(), "the port is optional");

    for bad in [
        "proxy.internal:3128",
        "ftp://proxy.internal",
        "socks5://proxy.internal:1080",
        "http://",
        "http://user:pw@proxy.internal:3128",
        "http://user@proxy.internal",
        "http://proxy.internal:3128/path",
        "http://proxy.internal?x=1",
        "http://proxy .internal",
    ] {
        let mut candidate = proxy.clone();
        candidate.http_proxy = Some(bad.into());
        assert!(candidate.validate().is_err(), "{bad:?}");
    }

    let mut none = OutboundProxySettings {
        enabled: true,
        ..OutboundProxySettings::default()
    };
    assert!(none.validate().is_err(), "enabled needs a URL");
    none.enabled = false;
    assert!(none.validate().is_ok());

    let mut many = proxy.clone();
    many.no_proxy = (0..100).map(|i| format!("h{i}.example.org")).collect();
    assert!(many.validate().is_ok());
    many.no_proxy.push("one-more".into());
    assert!(many.validate().is_err(), "at most 100 entries");
    many.no_proxy = vec!["a".repeat(254)];
    assert!(many.validate().is_err());
    many.no_proxy = vec!["bad entry".into()];
    assert!(many.validate().is_err());
    many.no_proxy = vec![String::new()];
    assert!(many.validate().is_err());
}

#[test]
fn must_not_accept_userinfo_in_a_proxy_url() {
    let proxy = OutboundProxySettings {
        https_proxy: Some("https://admin:secret@proxy.internal:3128".into()),
        ..OutboundProxySettings::default()
    };
    let error = proxy.validate().expect_err("user info is refused");
    assert!(!error.to_string().contains("secret"), "{error}");
}

#[test]
fn logging_overrides_are_capped_and_time_boxed() {
    let now = at();
    let over = |target: &str, hours: i64| LogOverride {
        target: target.into(),
        level: LogLevel::Debug,
        expires_at: now + time::Duration::hours(hours),
    };
    let ok = LoggingSettings {
        overrides: vec![over("rustryx_db", 1), over("hyper", 24)],
    };
    assert!(ok.validate(now).is_ok());
    let too_long = LoggingSettings {
        overrides: vec![over("rustryx_db", 25)],
    };
    assert!(too_long.validate(now).is_err(), "at most 24 hours ahead");
    let empty_target = LoggingSettings {
        overrides: vec![over("", 1)],
    };
    assert!(empty_target.validate(now).is_err());
    let many = LoggingSettings {
        overrides: (0..20).map(|_| over("t", 1)).collect(),
    };
    assert!(many.validate(now).is_ok());
    let mut too_many = many.clone();
    too_many.overrides.push(over("t", 1));
    assert!(too_many.validate(now).is_err(), "at most 20 entries");
    let past = LoggingSettings {
        overrides: vec![over("t", -1)],
    };
    assert!(
        past.validate(now).is_ok(),
        "an expired entry is merely over"
    );
}

#[test]
fn must_revert_a_level_at_expiry() {
    let now = at();
    let settings = LoggingSettings {
        overrides: vec![
            LogOverride {
                target: "a".into(),
                level: LogLevel::Trace,
                expires_at: now + time::Duration::minutes(5),
            },
            LogOverride {
                target: "b".into(),
                level: LogLevel::Debug,
                expires_at: now + time::Duration::hours(1),
            },
        ],
    };
    assert_eq!(settings.active_at(now).count(), 2);
    let later = now + time::Duration::minutes(5);
    let active: Vec<_> = settings
        .active_at(later)
        .map(|o| o.target.as_str())
        .collect();
    assert_eq!(active, ["b"], "an override whose expiry equals now is over");
    assert_eq!(
        settings.active_at(now + time::Duration::hours(2)).count(),
        0
    );
}

#[test]
fn log_levels_are_ordered_and_snake_case() {
    assert!(LogLevel::Error < LogLevel::Warn && LogLevel::Debug < LogLevel::Trace);
    for (level, name) in [
        (LogLevel::Error, "error"),
        (LogLevel::Warn, "warn"),
        (LogLevel::Info, "info"),
        (LogLevel::Debug, "debug"),
        (LogLevel::Trace, "trace"),
    ] {
        assert_wire(&level, &format!("\"{name}\""));
    }
    let json = round_trip_json(&LogOverride {
        target: "t".into(),
        level: LogLevel::Warn,
        expires_at: at(),
    });
    assert_eq!(json["expires_at"], "2026-10-24T10:00:00Z");
}

#[test]
fn must_not_let_a_project_admin_map_a_group_outside_the_policy() {
    let closed = GroupMappingPolicy::default();
    assert!(!closed.project_admin_may_map(IdpProvider::Oidc, "/teams/payments"));
    assert!(!closed.project_admin_may_map(IdpProvider::Ldap, "cn=x,dc=example,dc=org"));

    let open = GroupMappingPolicy {
        project_admins_may_map: true,
        oidc_path_prefixes: vec!["/teams/".into()],
        ldap_dn_suffixes: vec!["ou=teams,dc=example,dc=org".into()],
    };
    assert!(open.project_admin_may_map(IdpProvider::Oidc, "/teams/payments"));
    assert!(open.project_admin_may_map(IdpProvider::Ldap, "cn=pay,ou=teams,dc=example,dc=org"));
    // Exact: no case folding, no trimming, no substring, no pattern.
    assert!(!open.project_admin_may_map(IdpProvider::Oidc, "/Teams/payments"));
    assert!(!open.project_admin_may_map(IdpProvider::Oidc, " /teams/payments"));
    assert!(!open.project_admin_may_map(IdpProvider::Oidc, "/other/teams/payments"));
    assert!(!open.project_admin_may_map(IdpProvider::Oidc, "/admins"));
    assert!(!open.project_admin_may_map(IdpProvider::Ldap, "cn=pay,OU=teams,dc=example,dc=org"));
    assert!(!open.project_admin_may_map(IdpProvider::Ldap, "cn=pay,ou=teams,dc=example,dc=org "));
    assert!(!open.project_admin_may_map(IdpProvider::Ldap, "cn=x,ou=other,dc=example,dc=org"));
    // A provider's list does not apply to the other provider.
    assert!(!open.project_admin_may_map(IdpProvider::Ldap, "/teams/payments"));
    assert!(!open.project_admin_may_map(IdpProvider::Oidc, "cn=pay,ou=teams,dc=example,dc=org"));
    // An empty list allows nothing even when the switch is on.
    let empty = GroupMappingPolicy {
        project_admins_may_map: true,
        ..GroupMappingPolicy::default()
    };
    assert!(!empty.project_admin_may_map(IdpProvider::Oidc, "/anything"));
}

#[test]
fn group_mapping_policy_validation() {
    let mut policy = GroupMappingPolicy {
        project_admins_may_map: true,
        oidc_path_prefixes: vec!["/teams/".into()],
        ldap_dn_suffixes: vec!["dc=example,dc=org".into()],
    };
    assert!(policy.validate().is_ok());
    policy.oidc_path_prefixes.push("teams/".into());
    assert!(
        policy.validate().is_err(),
        "a prefix without a leading slash"
    );
    policy.oidc_path_prefixes = vec![String::new()];
    assert!(
        policy.validate().is_err(),
        "an empty prefix would match every group"
    );
    policy.oidc_path_prefixes = Vec::new();
    policy.ldap_dn_suffixes = vec![String::new()];
    assert!(
        policy.validate().is_err(),
        "an empty suffix would match every group"
    );
}

#[test]
fn robot_settings_validation() {
    let mut robots = RobotSettings::default();
    assert!(robots.validate().is_ok());
    for prefix in ["", "-x", "has space", "plus+sign", &"p".repeat(33)] {
        robots.name_prefix = prefix.into();
        assert!(robots.validate().is_err(), "{prefix:?}");
    }
    robots.name_prefix = "ci.robot-1".into();
    assert!(robots.validate().is_ok());
    robots.default_ttl_days = 0;
    assert!(robots.validate().is_err());
    robots.default_ttl_days = 366;
    assert!(robots.validate().is_err(), "default above the maximum");
    robots.default_ttl_days = 365;
    assert!(robots.validate().is_ok());
    robots.expired_retention_days = 0;
    assert!(robots.validate().is_err());
}

#[test]
fn project_creation_defaults_to_admins_only() {
    assert_eq!(ProjectCreation::default(), ProjectCreation::AdminsOnly);
    assert_wire(&ProjectCreation::AdminsOnly, "\"admins_only\"");
    assert_wire(&ProjectCreation::Everyone, "\"everyone\"");
}

#[test]
fn projects_settings_validate_their_parts() {
    let mut settings = ProjectsSettings::default();
    assert!(settings.validate().is_ok());
    settings.group_mapping.oidc_path_prefixes = vec!["no-slash".into()];
    assert!(settings.validate().is_err());
    settings.group_mapping = GroupMappingPolicy::default();
    settings.robots.default_ttl_days = 0;
    assert!(settings.validate().is_err());
}

#[test]
fn quota_and_lifecycle_validation() {
    let mut quota = QuotaSettings::default();
    for bad in [0u8, 101] {
        quota.default_warn_percent = bad;
        assert!(quota.validate().is_err(), "{bad}");
    }
    quota.default_warn_percent = 100;
    assert!(quota.validate().is_ok());

    let mut lifecycle = LifecycleSettings::default();
    for bad in [0.0_f32, -0.5, 1.01, f32::NAN, f32::INFINITY] {
        lifecycle.max_delete_ratio = bad;
        assert!(lifecycle.validate().is_err(), "{bad}");
    }
    lifecycle.max_delete_ratio = 1.0;
    assert!(lifecycle.validate().is_ok());
    for bad in [49u32, 2001] {
        lifecycle.batch_size = bad;
        assert!(lifecycle.validate().is_err(), "{bad}");
    }
    for good in [50u32, 2000] {
        lifecycle.batch_size = good;
        assert!(lifecycle.validate().is_ok(), "{good}");
    }
    lifecycle.dry_run_validity_days = 0;
    assert!(lifecycle.validate().is_err());
}

#[test]
fn notification_settings_validation() {
    let mut settings = NotificationSettings {
        task_recipients: vec!["ops@example.org".into()],
    };
    assert!(settings.validate().is_ok());
    settings.task_recipients = (0..20).map(|i| format!("u{i}@example.org")).collect();
    assert!(settings.validate().is_ok());
    settings.task_recipients.push("one-more@example.org".into());
    assert!(settings.validate().is_err(), "at most 20 addresses");
    settings.task_recipients = vec!["bad address".into()];
    assert!(settings.validate().is_err());
}

#[test]
fn read_only_state_serializes_rfc3339() {
    let state = ReadOnlyState {
        enabled: true,
        reason: "backup".into(),
        set_by: Some(UserId::generate()),
        set_at: Some(at()),
    };
    let json = round_trip_json(&state);
    assert_eq!(json["set_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["enabled"], true);
    let off = round_trip_json(&ReadOnlyState::default());
    assert!(off["set_at"].is_null());
}

fn certificate() -> TrustedCertificate {
    TrustedCertificate::new(
        TrustedCertificateId::generate(),
        "internal ca",
        "-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----\n",
        "ab".repeat(32),
        at() - time::Duration::days(1),
        at() + time::Duration::days(365),
        at(),
    )
}

#[test]
fn certificate_constructor_and_validation() {
    let mut cert = certificate();
    assert!(cert.is_ca && !cert.allow_leaf);
    assert!(cert.subject.is_empty() && cert.issuer.is_empty());
    assert!(cert.validate().is_ok());
    cert.name = String::new();
    assert!(cert.validate().is_err());
    cert.name = "ok".into();
    cert.pem = "a".repeat(MAX_CERTIFICATE_PEM_BYTES + 1);
    assert!(cert.validate().is_err());
    cert.pem = "-----BEGIN RSA PRIVATE KEY-----".into();
    assert!(cert.validate().is_err(), "a private key is refused");
    cert.pem = "pem".into();
    cert.subject = "s".repeat(513);
    assert!(cert.validate().is_err());
    cert.subject = "s".repeat(512);
    assert!(cert.validate().is_ok());
    for fp in [
        "",
        "AB".repeat(32).as_str(),
        &"a".repeat(63),
        &"z".repeat(64),
    ] {
        cert.fingerprint_sha256 = fp.to_owned();
        assert!(cert.validate().is_err(), "{fp}");
    }
    cert.fingerprint_sha256 = "cd".repeat(32);
    cert.not_after = cert.not_before;
    assert!(cert.validate().is_err(), "an empty validity window");
}

#[test]
fn must_not_accept_a_leaf_without_the_flag() {
    let mut cert = certificate();
    cert.is_ca = false;
    assert!(cert.validate().is_err());
    cert.allow_leaf = true;
    assert!(cert.validate().is_ok());
}

#[test]
fn must_not_trust_an_expired_certificate() {
    let cert = certificate();
    assert!(cert.is_valid_at(at()));
    assert!(
        !cert.is_valid_at(cert.not_before - time::Duration::seconds(1)),
        "not yet valid"
    );
    assert!(cert.is_valid_at(cert.not_before));
    assert!(cert.is_valid_at(cert.not_after - time::Duration::seconds(1)));
    assert!(
        !cert.is_valid_at(cert.not_after),
        "expiry at now is expired"
    );
    assert!(!cert.is_valid_at(cert.not_after + time::Duration::days(1)));
}

#[test]
fn certificate_serializes_rfc3339() {
    let json = round_trip_json(&certificate());
    assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["not_before"], "2026-10-23T10:00:00Z");
    assert_eq!(json["not_after"], "2027-10-24T10:00:00Z");
}

#[test]
fn smtp_settings_round_trip_keeps_the_sealed_password_for_persistence() {
    // Serialize writes the real value because persistence needs it; responses never embed
    // this type (they carry `SecretStatus`).
    let smtp = SmtpSettings {
        password: Some(Secret::new("persist-me")),
        ..SmtpSettings::default()
    };
    let json = serde_json::to_value(&smtp).expect("json");
    assert_eq!(json["password"], "persist-me");
    let back: SmtpSettings = serde_json::from_value(json).expect("back");
    assert_eq!(
        back.password.as_ref().map(Secret::expose),
        Some("persist-me")
    );
}
