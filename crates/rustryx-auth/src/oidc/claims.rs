//! Claims of Keycloak tokens and their mapping to a product identity.
//!
//! Only `sub`, `preferred_username`, `email` and `groups` feed the identity.
//! `realm_access`, `resource_access` and client roles are never read: every Keycloak
//! user holds `offline_access` and `default-roles-*` there, and README 8.5 forbids
//! them as a source of product roles. Group strings are passed on byte for byte:
//! no trimming, no case folding, no slash handling, so an exact comparison later
//! cannot be fooled by normalisation here.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

/// A verified Keycloak identity, input of just-in-time provisioning and of the role mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcIdentity {
    /// Issuer exactly as configured and as it appears in the token (`iss`).
    pub issuer: String,
    /// Stable subject (`sub`). Together with the issuer the only key of an OIDC account.
    pub subject: String,
    /// Login name: `preferred_username`, or `sub` when the claim is absent.
    pub username: String,
    /// Contact address, informational only. Never used to link accounts.
    pub email: Option<String>,
    /// The `groups` claim: full group paths, unmodified. Empty when absent or empty.
    pub groups: Vec<String>,
}

/// Which claim carries the groups and the limits that keep a hostile or bloated claim from
/// growing memory and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupPolicy {
    /// Name of the top-level claim, `groups` unless configured otherwise.
    pub(crate) claim: String,
    pub(crate) max_groups: usize,
    pub(crate) max_group_bytes: usize,
}

/// Counters for the metrics layer.
#[derive(Debug, Default)]
pub(crate) struct ClaimsStats {
    /// Group entries dropped because of the count or length cap.
    truncated: AtomicU64,
    /// `groups` claims or entries of an unexpected shape.
    malformed: AtomicU64,
}

impl ClaimsStats {
    pub(crate) fn truncated(&self) -> u64 {
        self.truncated.load(Ordering::Relaxed)
    }

    pub(crate) fn malformed(&self) -> u64 {
        self.malformed.load(Ordering::Relaxed)
    }
}

/// The claims this module reads. Everything else, the role claims included, lands in `rest`
/// where only the configured groups claim is ever looked at.
#[derive(Debug, Deserialize)]
pub(crate) struct RawClaims {
    pub(crate) sub: Option<String>,
    pub(crate) preferred_username: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) azp: Option<String>,
    pub(crate) typ: Option<String>,
    pub(crate) scope: Option<String>,
    pub(crate) nonce: Option<String>,
    pub(crate) exp: Option<serde_json::Number>,
    pub(crate) iat: Option<serde_json::Number>,
    #[serde(flatten)]
    pub(crate) rest: serde_json::Map<String, serde_json::Value>,
}

/// A claim made the token unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimError {
    MissingSubject,
    InvalidValue,
}

const MAX_NAME_BYTES: usize = 255;
const MAX_EMAIL_BYTES: usize = 320;

/// Builds the identity from verified claims.
pub(crate) fn identity_from(
    claims: &RawClaims,
    issuer: &str,
    policy: &GroupPolicy,
    stats: &ClaimsStats,
) -> Result<OidcIdentity, ClaimError> {
    let subject = claims
        .sub
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(ClaimError::MissingSubject)?;
    check_text(subject, MAX_NAME_BYTES)?;
    let username = match claims
        .preferred_username
        .as_deref()
        .filter(|u| !u.is_empty())
    {
        Some(name) => {
            check_text(name, MAX_NAME_BYTES)?;
            name
        }
        None => subject,
    };
    let email = match claims.email.as_deref().filter(|e| !e.is_empty()) {
        Some(email) => {
            check_text(email, MAX_EMAIL_BYTES)?;
            Some(email.to_owned())
        }
        None => None,
    };
    Ok(OidcIdentity {
        issuer: issuer.to_owned(),
        subject: subject.to_owned(),
        username: username.to_owned(),
        email,
        groups: extract_groups(claims.rest.get(&policy.claim), policy, stats),
    })
}

/// Names end up in logs and audit records: no control characters, bounded length.
fn check_text(value: &str, max_bytes: usize) -> Result<(), ClaimError> {
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ClaimError::InvalidValue);
    }
    Ok(())
}

/// Reads the `groups` claim. Absent, `null` or empty gives no groups, which removes the
/// roles of origin `oidc` for the request (README 8.5). Whatever cannot be read safely is
/// dropped, never guessed: fewer groups can only mean less access.
pub(crate) fn extract_groups(
    claim: Option<&serde_json::Value>,
    limits: &GroupPolicy,
    stats: &ClaimsStats,
) -> Vec<String> {
    let entries = match claim {
        None | Some(serde_json::Value::Null) => return Vec::new(),
        Some(serde_json::Value::Array(entries)) => entries,
        Some(_) => {
            stats.malformed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("oidc groups claim is not an array, treated as empty");
            return Vec::new();
        }
    };
    let mut groups = Vec::with_capacity(entries.len().min(limits.max_groups));
    let (mut truncated, mut malformed) = (0u64, 0u64);
    for entry in entries {
        match entry.as_str() {
            None | Some("") => malformed += 1,
            Some(group) if group.len() > limits.max_group_bytes => truncated += 1,
            Some(_) if groups.len() >= limits.max_groups => truncated += 1,
            Some(group) => groups.push(group.to_owned()),
        }
    }
    if truncated > 0 {
        stats.truncated.fetch_add(truncated, Ordering::Relaxed);
        tracing::warn!(
            dropped = truncated,
            max_groups = limits.max_groups,
            max_group_bytes = limits.max_group_bytes,
            "oidc groups claim truncated"
        );
    }
    if malformed > 0 {
        stats.malformed.fetch_add(malformed, Ordering::Relaxed);
        tracing::warn!(
            dropped = malformed,
            "oidc groups claim had malformed entries"
        );
    }
    groups
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::oidc::test_support::assert_empty;

    fn limits() -> GroupPolicy {
        GroupPolicy {
            claim: "groups".to_owned(),
            max_groups: 256,
            max_group_bytes: 512,
        }
    }

    fn groups_of(value: &serde_json::Value) -> (Vec<String>, ClaimsStats) {
        let stats = ClaimsStats::default();
        let groups = extract_groups(Some(value), &limits(), &stats);
        (groups, stats)
    }

    #[test]
    fn groups_are_passed_through_byte_for_byte() {
        let input = [
            "/rustryx/admins",
            "rustryx/admins",
            "/Rustryx/Admins",
            " /rustryx/admins",
            "/rustryx/admins/",
            "/rustryx/team-payments/publishers",
            "/rustryx/\u{0442}\u{0435}\u{0441}\u{0442}",
        ];
        let (groups, stats) = groups_of(&json!(input));
        assert_eq!(groups, input);
        assert_eq!((stats.truncated(), stats.malformed()), (0, 0));
    }

    #[test]
    fn missing_null_and_empty_claim_mean_no_groups() {
        let stats = ClaimsStats::default();
        assert_empty(&extract_groups(None, &limits(), &stats));
        assert_empty(&extract_groups(Some(&json!(null)), &limits(), &stats));
        assert_empty(&extract_groups(Some(&json!([])), &limits(), &stats));
        assert_eq!((stats.truncated(), stats.malformed()), (0, 0));
    }

    #[test]
    fn must_not_read_groups_of_unexpected_shape() {
        for shape in [
            json!("/rustryx/admins"),
            json!({"a": 1}),
            json!(7),
            json!(true),
        ] {
            let (groups, stats) = groups_of(&shape);
            assert_empty(&groups);
            assert_eq!(stats.malformed(), 1);
        }
    }

    #[test]
    fn drops_non_string_and_empty_entries() {
        let (groups, stats) = groups_of(&json!(["/a", 5, null, "", ["/nested"], "/b"]));
        assert_eq!(groups, ["/a", "/b"]);
        assert_eq!(stats.malformed(), 4);
    }

    #[test]
    fn caps_group_count_and_counts_the_overflow() {
        let many: Vec<String> = (0..300).map(|i| format!("/g/{i}")).collect();
        let (groups, stats) = groups_of(&json!(many));
        assert_eq!(groups.len(), 256);
        assert_eq!(groups.first().map(String::as_str), Some("/g/0"));
        assert_eq!(groups.last().map(String::as_str), Some("/g/255"));
        assert_eq!(stats.truncated(), 44);
    }

    #[test]
    fn drops_oversized_group_paths_but_keeps_the_boundary() {
        let exact = format!("/{}", "a".repeat(511));
        let too_long = format!("/{}", "a".repeat(512));
        let (groups, stats) = groups_of(&json!([exact, too_long, "/ok"]));
        assert_eq!(groups, [exact, "/ok".to_owned()]);
        assert_eq!(stats.truncated(), 1);
    }

    fn claims(value: serde_json::Value) -> RawClaims {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn identity_uses_preferred_username_and_falls_back_to_sub() {
        let stats = ClaimsStats::default();
        let with_name = claims(json!({"sub": "u1", "preferred_username": "alice", "email": "a@x"}));
        let id = identity_from(&with_name, "https://kc/realms/r", &limits(), &stats).unwrap();
        assert_eq!(
            (
                id.username.as_str(),
                id.subject.as_str(),
                id.email.as_deref()
            ),
            ("alice", "u1", Some("a@x"))
        );
        assert_eq!(id.issuer, "https://kc/realms/r");
        let without = claims(json!({"sub": "u1"}));
        let id = identity_from(&without, "iss", &limits(), &stats).unwrap();
        assert_eq!((id.username.as_str(), id.email), ("u1", None));
    }

    #[test]
    fn must_not_build_identity_from_bad_subject_or_names() {
        let stats = ClaimsStats::default();
        let long = "x".repeat(256);
        for bad in [
            json!({}),
            json!({"sub": ""}),
            json!({"sub": long}),
            json!({"sub": "a\nb"}),
            json!({"sub": "u", "preferred_username": "bad\u{0}name"}),
            json!({"sub": "u", "email": "e".repeat(321)}),
        ] {
            assert!(identity_from(&claims(bad), "iss", &limits(), &stats).is_err());
        }
    }

    #[test]
    fn must_not_take_roles_from_realm_access_or_resource_access() {
        let stats = ClaimsStats::default();
        let raw = claims(json!({
            "sub": "u1",
            "realm_access": {"roles": ["nx-admin", "offline_access"]},
            "resource_access": {"rustryx": {"roles": ["nx-admin"]}},
            "roles": ["nx-admin"],
        }));
        let id = identity_from(&raw, "iss", &limits(), &stats).unwrap();
        assert_empty(&id.groups);
    }
}
