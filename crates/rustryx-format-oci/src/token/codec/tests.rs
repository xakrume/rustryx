use proptest::prelude::*;
use time::Duration;

use super::*;

const KEY: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap()
}

fn codec() -> HmacTokenCodec {
    HmacTokenCodec::new(KEY, "rustryx", "registry").unwrap()
}

fn claims() -> RegistryTokenClaims {
    RegistryTokenClaims {
        issuer: "rustryx".into(),
        subject: "alice".into(),
        subject_id: Some(UserId::generate()),
        audience: "registry".into(),
        issued_at: at(0),
        not_before: at(0),
        expires_at: at(300),
        token_id: "jti-1".into(),
        access: vec![
            AccessGrant {
                resource: GrantResource::Repository("docker-hosted/team/app".into()),
                actions: vec![Action::Pull, Action::Push],
            },
            AccessGrant {
                resource: GrantResource::Catalog,
                actions: vec![Action::Pull],
            },
        ],
    }
}

fn resign(payload: &serde_json::Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(payload.to_string());
    let mut mac = HmacSha256::new_from_slice(KEY).unwrap();
    mac.update(format!("{header}.{payload}").as_bytes());
    format!(
        "{header}.{payload}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

fn payload() -> serde_json::Value {
    json!({
        "iss": "rustryx", "sub": "alice", "aud": "registry",
        "iat": 1_800_000_000_i64, "nbf": 1_800_000_000_i64, "exp": 1_800_000_300_i64,
        "jti": "x", "access": [],
    })
}

#[test]
fn a_token_round_trips() {
    let original = claims();
    let token = codec().issue(&original).unwrap();
    assert_eq!(token.split('.').count(), 3);
    assert_eq!(codec().verify(&token, at(10)).unwrap(), original);
}

#[test]
fn permits_follows_the_grants() {
    let claims = claims();
    assert!(claims.permits("docker-hosted/team/app", Action::Pull));
    assert!(claims.permits("docker-hosted/team/app", Action::Push));
    assert!(!claims.permits("docker-hosted/team/app", Action::Delete));
    assert!(!claims.permits("docker-hosted/team/other", Action::Pull));
    assert!(!claims.permits("docker-hosted/team/ap", Action::Pull));
    assert!(claims.permits_catalog());
    let mut without = claims;
    without.access.pop();
    assert!(!without.permits_catalog());
}

#[test]
fn must_not_accept_an_expired_token() {
    let token = codec().issue(&claims()).unwrap();
    assert_eq!(codec().verify(&token, at(300)), Err(TokenError::Expired));
    assert_eq!(codec().verify(&token, at(10_000)), Err(TokenError::Expired));
    assert!(codec().verify(&token, at(299)).is_ok());
}

#[test]
fn must_not_accept_a_token_that_is_not_valid_yet() {
    let mut later = claims();
    later.not_before = at(100);
    later.issued_at = at(100);
    let token = codec().issue(&later).unwrap();
    assert_eq!(codec().verify(&token, at(0)), Err(TokenError::NotYetValid));
    // Within the clock skew leeway it is accepted.
    assert!(codec().verify(&token, at(96)).is_ok());
    assert!(codec().verify(&token, at(100)).is_ok());
}

#[test]
fn must_not_accept_a_tampered_payload() {
    let token = codec().issue(&claims()).unwrap();
    let parts: Vec<&str> = token.split('.').collect();
    let mut forged = payload();
    forged["sub"] = json!("admin");
    forged["access"] = json!([{"type": "repository", "name": "docker-hosted/x", "actions": ["pull", "push", "delete"]}]);
    let forged_payload = URL_SAFE_NO_PAD.encode(forged.to_string());
    let tampered = format!("{}.{forged_payload}.{}", parts[0], parts[2]);
    assert_eq!(
        codec().verify(&tampered, at(1)),
        Err(TokenError::BadSignature)
    );
}

#[test]
fn must_not_accept_a_tampered_or_missing_signature() {
    let token = codec().issue(&claims()).unwrap();
    let (body, signature) = token.rsplit_once('.').unwrap();
    let mut flipped = signature.to_owned().into_bytes();
    flipped[0] = if flipped[0] == b'A' { b'B' } else { b'A' };
    let flipped = String::from_utf8(flipped).unwrap();
    assert_eq!(
        codec().verify(&format!("{body}.{flipped}"), at(1)),
        Err(TokenError::BadSignature)
    );
    assert_eq!(
        codec().verify(&format!("{body}."), at(1)),
        Err(TokenError::BadSignature)
    );
    assert_eq!(codec().verify(body, at(1)), Err(TokenError::Malformed));
    assert_eq!(
        codec().verify(&format!("{token}.extra"), at(1)),
        Err(TokenError::Malformed)
    );
}

#[test]
fn must_not_accept_a_token_signed_with_another_key() {
    let other =
        HmacTokenCodec::new(b"ffffffffffffffffffffffffffffffff", "rustryx", "registry").unwrap();
    let token = other.issue(&claims()).unwrap();
    assert_eq!(codec().verify(&token, at(1)), Err(TokenError::BadSignature));
}

#[test]
fn must_not_accept_algorithm_none_or_another_algorithm() {
    for header in [
        r#"{"alg":"none","typ":"JWT"}"#,
        r#"{"alg":"HS512","typ":"JWT"}"#,
        r#"{"alg":"RS256","typ":"JWT"}"#,
        r#"{"alg":"hs256","typ":"JWT"}"#,
        r#"{"typ":"JWT"}"#,
    ] {
        let header = URL_SAFE_NO_PAD.encode(header);
        let body = URL_SAFE_NO_PAD.encode(payload().to_string());
        // Even with a signature that is correct for the text, the header decides first.
        let mut mac = HmacSha256::new_from_slice(KEY).unwrap();
        mac.update(format!("{header}.{body}").as_bytes());
        let signed = format!(
            "{header}.{body}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        let unsigned = format!("{header}.{body}.");
        for token in [signed, unsigned] {
            let error = codec().verify(&token, at(1)).unwrap_err();
            assert!(
                matches!(
                    error,
                    TokenError::UnsupportedAlgorithm | TokenError::Malformed
                ),
                "{header}: {error:?}"
            );
        }
    }
}

#[test]
fn must_not_accept_another_audience_or_issuer() {
    let mut wrong_audience = payload();
    wrong_audience["aud"] = json!("other-registry");
    assert_eq!(
        codec().verify(&resign(&wrong_audience), at(1)),
        Err(TokenError::WrongAudience)
    );
    let mut wrong_issuer = payload();
    wrong_issuer["iss"] = json!("someone");
    assert_eq!(
        codec().verify(&resign(&wrong_issuer), at(1)),
        Err(TokenError::WrongIssuer)
    );
    assert!(codec().verify(&resign(&payload()), at(1)).is_ok());
}

#[test]
fn a_signed_payload_of_the_wrong_shape_is_malformed() {
    for broken in [
        json!({"iss": "rustryx"}),
        json!([]),
        json!("text"),
        {
            let mut p = payload();
            p["access"] = json!([{"type": "repository", "actions": []}]);
            p
        },
        {
            let mut p = payload();
            p["access"] = json!([{"type": "plugin", "name": "x", "actions": []}]);
            p
        },
        {
            let mut p = payload();
            p["uid"] = json!("not-a-uuid");
            p
        },
        {
            let mut p = payload();
            p["exp"] = json!(i64::MAX);
            p
        },
    ] {
        assert_eq!(
            codec().verify(&resign(&broken), at(1)),
            Err(TokenError::Malformed),
            "{broken}"
        );
    }
}

#[test]
fn unknown_actions_in_a_signed_token_are_ignored() {
    let mut p = payload();
    p["access"] = json!([{"type": "repository", "name": "r/a", "actions": ["pull", "admin"]}]);
    let claims = codec().verify(&resign(&p), at(1)).unwrap();
    assert_eq!(claims.access[0].actions, [Action::Pull]);
}

#[test]
fn an_oversized_token_is_refused_before_decoding() {
    let huge = "a".repeat(MAX_TOKEN_LEN + 1);
    assert_eq!(codec().verify(&huge, at(1)), Err(TokenError::Malformed));
}

#[test]
fn a_short_key_is_refused_and_the_key_is_not_printed() {
    assert_eq!(
        HmacTokenCodec::new(b"short", "i", "s").unwrap_err(),
        TokenError::KeyUnusable
    );
    let shown = format!("{:?}", codec());
    assert!(!shown.contains("0123456789abcdef"), "{shown}");
    assert!(shown.contains("REDACTED"));
}

#[test]
fn the_subject_id_is_optional() {
    let mut anonymous = claims();
    anonymous.subject = "anonymous".into();
    anonymous.subject_id = None;
    let token = codec().issue(&anonymous).unwrap();
    assert_eq!(codec().verify(&token, at(1)).unwrap().subject_id, None);
}

proptest! {
    #[test]
    fn verify_never_panics(token in ".{0,300}") {
        let _ = codec().verify(&token, at(1));
    }

    #[test]
    fn dotted_garbage_never_panics(a in "[A-Za-z0-9_-]{0,60}", b in "[A-Za-z0-9_-]{0,60}", c in "[A-Za-z0-9_=-]{0,60}") {
        let _ = codec().verify(&format!("{a}.{b}.{c}"), at(1));
    }

    #[test]
    fn any_single_character_change_is_refused(position in 0usize..400, replacement in "[A-Za-z0-9_-]") {
        let token = codec().issue(&claims()).unwrap();
        let index = position % token.len();
        let original = token.as_bytes()[index];
        let replacement = replacement.as_bytes()[0];
        prop_assume!(replacement != original && original != b'.');
        let mut bytes = token.into_bytes();
        bytes[index] = replacement;
        let changed = String::from_utf8(bytes).unwrap();
        // The final signature character carries padding bits: a change there may decode to the
        // same bytes. Everything else must be refused.
        if index + 1 < changed.len() {
            prop_assert!(codec().verify(&changed, at(1)).is_err());
        }
    }

    #[test]
    fn expiry_is_exact(offset in -600i64..600) {
        let token = codec().issue(&claims()).unwrap();
        let result = codec().verify(&token, at(offset));
        if offset >= 300 {
            prop_assert_eq!(result, Err(TokenError::Expired));
        } else if offset < -5 {
            prop_assert_eq!(result, Err(TokenError::NotYetValid));
        } else {
            prop_assert!(result.is_ok());
        }
        let _ = Duration::seconds(offset);
    }
}
