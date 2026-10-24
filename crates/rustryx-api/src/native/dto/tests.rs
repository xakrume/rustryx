use std::path::Path;

use rustryx_core::identity::{AuthMethod, UserSource};
use rustryx_core::ids::{RoleId, UserId};
use time::macros::datetime;

use super::*;

/// The lines of Rust source that name the core secret type, comments and strings aside.
///
/// `Secret` serializes its real value (persistence needs that), so a response type that holds
/// one would print it. `SecretStatus` is the allowed way to say "a secret is set". The scan is
/// textual on purpose: it reads the file that defines the response types, so a type added later
/// is covered without anyone remembering to list it.
fn secret_uses(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| {
            let code = line.trim_start();
            !code.starts_with("//") && !code.starts_with("#[doc")
        })
        .filter(|line| {
            let mut rest = *line;
            while let Some(position) = rest.find("Secret") {
                let before_ok = rest[..position]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                let after = &rest[position + "Secret".len()..];
                let after_ok = !after.starts_with("Status")
                    && after
                        .chars()
                        .next()
                        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                if before_ok && after_ok {
                    return true;
                }
                rest = after;
            }
            false
        })
        .map(str::to_owned)
        .collect()
}

/// Field names that mean a credential or a stored hash in a response.
const FORBIDDEN_FIELDS: [&str; 6] = [
    "token_hash",
    "password",
    "password_hash",
    "client_secret",
    "access_token",
    "refresh_token",
];

fn source_of(file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn must_not_embed_the_secret_type_in_a_response_dto() {
    let source = source_of("native/dto.rs");
    let found = secret_uses(&source);
    assert!(
        found.is_empty(),
        "the core `Secret` type in a response DTO: {found:?}"
    );
}

#[test]
fn must_not_name_a_credential_field_in_a_response_dto() {
    let source = source_of("native/dto.rs");
    for line in source.lines().filter(|l| !l.trim_start().starts_with("//")) {
        for forbidden in FORBIDDEN_FIELDS {
            assert!(
                !line.contains(&format!("pub {forbidden}:")),
                "a response DTO has a `{forbidden}` field: {line}"
            );
        }
    }
}

#[test]
fn the_scan_finds_what_it_is_meant_to_find() {
    let bad = "pub struct Leaky {\n    pub value: Secret,\n    pub boxed: Option<rustryx_core::Secret>,\n}\n";
    assert_eq!(secret_uses(bad).len(), 2);
    let fine = "// a `Secret` in a comment\n/// docs mention Secret\npub status: SecretStatus,\npub csrf_token: String,\nlet SecretKey = 1;\nlet MySecret = 2;\n";
    assert!(secret_uses(fine).is_empty(), "{:?}", secret_uses(fine));
    assert_eq!(secret_uses("use rustryx_core::Secret;").len(), 1);
}

#[test]
fn the_requests_that_carry_a_password_live_apart_from_the_responses() {
    // `requests.rs` holds the one type with a password (and no `Debug`); the scan above reads
    // `dto.rs` only, so this pins that the split stays.
    let requests = source_of("native/requests.rs");
    assert!(requests.contains("pub password: String"));
    assert!(!source_of("native/dto.rs").contains("password"));
}

#[test]
fn a_serialized_me_view_carries_rfc3339_timestamps_and_no_credential() {
    let view = MeView {
        authenticated: true,
        user: Some(MeUser {
            id: UserId::generate(),
            username: "alice".into(),
            display_name: None,
            email: None,
            source: UserSource::Local,
            issuer: None,
            subject: None,
            enabled: true,
            created_at: datetime!(2026-10-24 10:00:00 UTC),
        }),
        auth_method: AuthMethod::InternalJwt,
        roles: vec![EffectiveRole {
            role: RoleRefView {
                id: RoleId::generate(),
                name: "nx-developer".into(),
            },
            origin: rustryx_core::identity::RoleOrigin::Manual,
            groups: Vec::new(),
        }],
        groups: Vec::new(),
        groups_confirmed_at: Some(datetime!(2026-10-24 10:00:01 UTC)),
        token: None,
        permissions: PermissionSummary {
            is_admin: false,
            admin_areas: vec!["audit"],
            repository_grants: Vec::new(),
            can: Can {
                create_repository: false,
                upload: false,
                create_personal_token: true,
                view_audit: false,
                administer: false,
            },
        },
        token_policy: TokenPolicyView {
            default_ttl_days: 90,
            max_ttl_days: 365,
            require_expiry: true,
        },
        system: MeSystem {
            read_only: false,
            anonymous_enabled: false,
            base_url_configured: true,
        },
    };
    let json = serde_json::to_value(&view).expect("serialize");
    assert_eq!(json["user"]["created_at"], "2026-10-24T10:00:00Z");
    assert_eq!(json["groups_confirmed_at"], "2026-10-24T10:00:01Z");
    assert!(
        json["token"].is_null(),
        "a nullable field is present with null"
    );
    assert_eq!(json["auth_method"], "internal_jwt");
    let text = json.to_string();
    for forbidden in FORBIDDEN_FIELDS {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn the_session_view_keeps_every_field_even_when_it_is_empty() {
    let view = SessionView {
        authenticated: false,
        user: None,
        auth_method: None,
        csrf_token: None,
        issued_at: None,
        idle_expires_at: None,
        absolute_expires_at: None,
        idle_timeout_secs: 1800,
        absolute_timeout_secs: 28800,
    };
    let json = serde_json::to_value(&view).expect("serialize");
    let object = json.as_object().expect("object");
    for key in [
        "user",
        "auth_method",
        "csrf_token",
        "issued_at",
        "idle_expires_at",
        "absolute_expires_at",
    ] {
        assert!(
            object.get(key).is_some_and(serde_json::Value::is_null),
            "{key}"
        );
    }
    assert_eq!(object.len(), 9);
}
