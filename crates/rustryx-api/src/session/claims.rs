//! The claims of the session token, read WITHOUT verifying the signature.
//!
//! The session layer needs `tid`, `iat`, `exp` and the group snapshot before and after the
//! authentication chain runs: to refuse an idle session without a database read, to bind the
//! CSRF token, to reissue the token. None of that is trusted on its own. The chain verifies
//! the whole token (signature, issuer, lifetime, account state, revocation) on the same
//! request, and a request whose token fails that is refused or downgraded to anonymous, so a
//! claim that was forged here is never acted upon: either the signature covers the very
//! values read here, or the request does not get past the chain.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustryx_core::ids::UserId;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::ports::GroupSnapshot;

/// Longest token read at all. The internal JWT service refuses the same size.
const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// What the session layer reads from a session token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionClaims {
    /// `sub`: the account.
    pub user_id: UserId,
    /// `tid`: this token's id.
    pub tid: String,
    /// `iat`: when this token was issued (login or the last reissue).
    pub issued_at: OffsetDateTime,
    /// `exp`: the absolute end of the session.
    pub expires_at: OffsetDateTime,
    /// The SSO group snapshot (`grp` and `gat`), when the token carries one.
    pub groups: Option<GroupSnapshot>,
}

#[derive(Deserialize)]
struct RawClaims {
    sub: String,
    tid: String,
    iat: i64,
    exp: i64,
    #[serde(default)]
    grp: Option<Vec<String>>,
    #[serde(default)]
    gat: Option<i64>,
}

fn at(seconds: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(seconds).ok()
}

/// Reads the payload of a compact JWT. `None` for anything that is not a three part token with
/// a decodable payload of the expected shape.
pub(crate) fn peek(token: &str) -> Option<SessionClaims> {
    if token.len() > MAX_TOKEN_BYTES {
        return None;
    }
    let mut parts = token.split('.');
    let (_header, payload, _signature, None) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next())
    else {
        return None;
    };
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let raw: RawClaims = serde_json::from_slice(&bytes).ok()?;
    let groups = match (raw.grp, raw.gat) {
        (None, None) => None,
        (Some(groups), Some(confirmed)) => Some(GroupSnapshot {
            groups,
            confirmed_at: at(confirmed)?,
        }),
        // An unaged group set cannot be trusted; the chain refuses it too.
        _ => return None,
    };
    if raw.tid.is_empty() || raw.tid.len() > 64 {
        return None;
    }
    Some(SessionClaims {
        user_id: raw.sub.parse().ok()?,
        tid: raw.tid,
        issued_at: at(raw.iat)?,
        expires_at: at(raw.exp)?,
        groups,
    })
}

#[cfg(test)]
pub(crate) fn forge(claims: &serde_json::Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","kid":"k1"}"#);
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{payload}.c2lnbmF0dXJl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_json(extra: &serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "sub": UserId::generate().to_string(),
            "tid": "tid-1",
            "iat": 1_000,
            "exp": 5_000,
        });
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            base.extend(extra.clone());
        }
        base
    }

    #[test]
    fn a_plain_token_yields_its_times_and_no_groups() {
        let parsed = peek(&forge(&claims_json(&serde_json::json!({})))).expect("parses");
        assert_eq!(parsed.tid, "tid-1");
        assert_eq!(parsed.issued_at.unix_timestamp(), 1_000);
        assert_eq!(parsed.expires_at.unix_timestamp(), 5_000);
        assert!(parsed.groups.is_none());
    }

    #[test]
    fn a_group_snapshot_needs_both_halves() {
        let both = claims_json(&serde_json::json!({"grp": ["/a", "/b"], "gat": 900}));
        let parsed = peek(&forge(&both)).expect("parses");
        let snapshot = parsed.groups.expect("snapshot");
        assert_eq!(snapshot.groups, ["/a", "/b"]);
        assert_eq!(snapshot.confirmed_at.unix_timestamp(), 900);
        assert!(peek(&forge(&claims_json(&serde_json::json!({"grp": ["/a"]})))).is_none());
        assert!(peek(&forge(&claims_json(&serde_json::json!({"gat": 900})))).is_none());
    }

    #[test]
    fn anything_else_is_not_a_session_token() {
        assert!(peek("").is_none());
        assert!(peek("a.b").is_none());
        assert!(peek("a.b.c.d").is_none());
        assert!(peek("a.!!!.c").is_none());
        assert!(peek(&format!("a.{}.c", URL_SAFE_NO_PAD.encode("not json"))).is_none());
        assert!(peek(&"x".repeat(MAX_TOKEN_BYTES + 1)).is_none());
        let no_user = serde_json::json!({"sub": "nope", "tid": "t", "iat": 1, "exp": 2});
        assert!(peek(&forge(&no_user)).is_none());
        for bad_tid in ["", &"t".repeat(65)] {
            let c = claims_json(&serde_json::json!({ "tid": bad_tid }));
            assert!(peek(&forge(&c)).is_none());
        }
        let far = claims_json(&serde_json::json!({"exp": i64::MAX}));
        assert!(
            peek(&forge(&far)).is_none(),
            "out of range timestamps are refused"
        );
    }
}
