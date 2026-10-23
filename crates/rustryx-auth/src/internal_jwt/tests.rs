//! Tests of the internal JWT: round trip, rotation, and every refusal path.

use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use jsonwebtoken::EncodingKey;
use rustryx_core::identity::UserSource;
use rustryx_testkit::{FixedClock, MemStore, fixtures};
use serde_json::{Value, json};

use super::*;

const KID: &str = "k1";
const TTL: Duration = Duration::from_secs(600);

fn seed(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn ring(kid: &str, seed_byte: u8, previous: &[(&str, u8)]) -> KeyRing {
    let previous: Vec<(String, ed25519_dalek::VerifyingKey)> = previous
        .iter()
        .map(|(kid, byte)| {
            (
                (*kid).to_owned(),
                SigningKey::from_bytes(&seed(*byte)).verifying_key(),
            )
        })
        .collect();
    KeyRing::from_seed(kid, &seed(seed_byte), &previous).unwrap()
}

fn encoding_key(seed_byte: u8) -> EncodingKey {
    let der = SigningKey::from_bytes(&seed(seed_byte))
        .to_pkcs8_der()
        .unwrap();
    EncodingKey::from_ed_der(der.as_bytes())
}

/// Signs arbitrary claims with the `EdDSA` key made from `seed_byte`.
fn sign(seed_byte: u8, kid: Option<&str>, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = kid.map(str::to_owned);
    encode(&header, claims, &encoding_key(seed_byte)).unwrap()
}

fn segment(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}

struct Env {
    store: MemStore,
    clock: FixedClock,
    revoked: Arc<MemRevokedTokens>,
    service: Arc<InternalJwtService>,
    auth: InternalJwtAuthenticator,
    user: User,
}

async fn env_with(key_ring: KeyRing) -> Env {
    let store = MemStore::with_builtin_roles();
    let clock = FixedClock::default();
    let revoked = Arc::new(MemRevokedTokens::new());
    let user = UserRepo::create(&store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    let service = Arc::new(InternalJwtService::new(
        key_ring,
        InternalJwtConfig::default(),
        Arc::new(clock.clone()),
        Arc::new(store.clone()),
        revoked.clone(),
    ));
    let auth = InternalJwtAuthenticator::new(Arc::clone(&service));
    Env {
        store,
        clock,
        revoked,
        service,
        auth,
        user,
    }
}

async fn env() -> Env {
    env_with(ring(KID, 1, &[])).await
}

fn request(env: &Env) -> IssueRequest {
    IssueRequest {
        user_id: env.user.id,
        ttl: TTL,
        groups: None,
    }
}

fn issue(env: &Env) -> IssuedJwt {
    env.service.issue(&request(env)).unwrap()
}

fn now_seconds(env: &Env) -> i64 {
    env.clock.now().unix_timestamp()
}

/// A well formed `tid` that this service never issued.
const FORGED_TID: &str = "0197f2c2-0000-7000-8000-000000000001";

fn claims_for(env: &Env) -> Value {
    let now = now_seconds(env);
    json!({
        "iss": DEFAULT_ISSUER,
        "sub": env.user.id.to_string(),
        "tid": FORGED_TID,
        "iat": now,
        "exp": now + 600,
    })
}

fn bearer(token: &str) -> Credentials {
    Credentials::Bearer(Secret::new(token))
}

async fn authenticate(env: &Env, token: &str) -> Result<Option<Principal>, AuthError> {
    env.auth
        .authenticate(&bearer(token), &AuthContext::default())
        .await
}

#[tokio::test]
async fn issues_a_token_that_verifies_and_yields_a_principal() {
    let env = env().await;
    let issued = issue(&env);
    assert_eq!(
        issued.expires_at - issued.issued_at,
        time::Duration::seconds(600)
    );
    let header = decode_header(issued.token.expose()).unwrap();
    assert_eq!(header.alg, Algorithm::EdDSA);
    assert_eq!(header.kid.as_deref(), Some(KID));

    let principal = authenticate(&env, issued.token.expose())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(principal.user_id, Some(env.user.id));
    assert_eq!(principal.username, "carol");
    assert_eq!(principal.source, UserSource::Oidc);
    assert_eq!(principal.issuer, env.user.issuer);
    assert_eq!(principal.subject, env.user.subject);
    assert_eq!(principal.auth_method, AuthMethod::InternalJwt);
    assert_eq!(principal.roles.len(), 0);
    assert_eq!(principal.external_groups.len(), 0);
    assert!(principal.groups_confirmed_at.is_none());

    let verified = env.service.verify(issued.token.expose()).await.unwrap();
    assert_eq!(verified.tid, issued.tid);
    assert_eq!(verified.user.id, env.user.id);
}

#[tokio::test]
async fn every_token_gets_a_fresh_tid() {
    let env = env().await;
    assert_ne!(issue(&env).tid, issue(&env).tid);
}

#[tokio::test]
async fn group_snapshot_travels_with_the_token() {
    let env = env().await;
    let confirmed_at = env.clock.now();
    let issued = env
        .service
        .issue(&IssueRequest {
            groups: Some(GroupSnapshot {
                groups: vec!["/rustryx/admins".into(), "/rustryx/readers".into()],
                confirmed_at,
            }),
            ..request(&env)
        })
        .unwrap();
    let principal = authenticate(&env, issued.token.expose())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        principal.external_groups,
        ["/rustryx/admins", "/rustryx/readers"]
    );
    assert_eq!(principal.groups_confirmed_at, Some(confirmed_at));
}

#[tokio::test]
async fn issue_enforces_lifetime_and_group_limits() {
    let env = env().await;
    let too_long = IssueRequest {
        ttl: MAX_SESSION_TTL + Duration::from_nanos(1),
        ..request(&env)
    };
    assert!(matches!(
        env.service.issue(&too_long),
        Err(IssueError::TtlOutOfRange)
    ));
    let zero = IssueRequest {
        ttl: Duration::ZERO,
        ..request(&env)
    };
    assert!(matches!(
        env.service.issue(&zero),
        Err(IssueError::TtlOutOfRange)
    ));
    let eight_hours = IssueRequest {
        ttl: MAX_SESSION_TTL,
        ..request(&env)
    };
    assert!(env.service.issue(&eight_hours).is_ok());
    let crowded = IssueRequest {
        groups: Some(GroupSnapshot {
            groups: (0..=MAX_GROUPS_IN_TOKEN)
                .map(|i| format!("/g{i}"))
                .collect(),
            confirmed_at: env.clock.now(),
        }),
        ..request(&env)
    };
    assert!(matches!(
        env.service.issue(&crowded),
        Err(IssueError::TooManyGroups)
    ));
}

#[tokio::test]
async fn sub_second_ttl_rounds_up_to_a_whole_second() {
    let env = env().await;
    let issued = env
        .service
        .issue(&IssueRequest {
            ttl: Duration::from_millis(1),
            ..request(&env)
        })
        .unwrap();
    assert_eq!(
        issued.expires_at - issued.issued_at,
        time::Duration::seconds(1)
    );
}

#[tokio::test]
async fn must_not_accept_a_token_at_or_after_its_exp() {
    let env = env().await;
    let issued = issue(&env);
    env.clock
        .advance(TTL.checked_sub(Duration::from_secs(1)).unwrap());
    authenticate(&env, issued.token.expose())
        .await
        .unwrap()
        .unwrap();
    env.clock.advance(Duration::from_secs(1));
    let error = authenticate(&env, issued.token.expose()).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenExpired));
}

#[tokio::test]
async fn must_not_accept_a_lifetime_beyond_the_session_maximum() {
    let env = env().await;
    let mut claims = claims_for(&env);
    let iat = now_seconds(&env);
    claims["exp"] = json!(iat + 8 * 3600);
    let at_limit = sign(1, Some(KID), &claims);
    authenticate(&env, &at_limit).await.unwrap().unwrap();
    claims["exp"] = json!(iat + 8 * 3600 + 1);
    let beyond = sign(1, Some(KID), &claims);
    let error = authenticate(&env, &beyond).await.unwrap_err();
    assert!(
        matches!(
            error,
            AuthError::InvalidToken("lifetime exceeds the session maximum")
        ),
        "a correctly signed token must still be refused: {error:?}"
    );
}

#[tokio::test]
async fn must_not_accept_alg_none_in_any_spelling() {
    let env = env().await;
    let payload = segment(&claims_for(&env));
    for alg in ["none", "None", "NONE", "nOnE"] {
        let header = segment(&json!({"alg": alg, "typ": "JWT", "kid": KID}));
        for token in [
            format!("{header}.{payload}."),
            format!("{header}.{payload}.AAAA"),
        ] {
            let error = authenticate(&env, &token).await.unwrap_err();
            assert!(
                matches!(error, AuthError::InvalidToken(_)),
                "{alg}: {error:?}"
            );
            assert!(env.service.verify(&token).await.is_err(), "{alg}");
        }
    }
}

#[tokio::test]
async fn must_not_accept_hs256_keyed_with_the_public_key() {
    let env = env().await;
    let public = SigningKey::from_bytes(&seed(1)).verifying_key();
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    for secret in [
        public.to_bytes().to_vec(),
        hex::encode(public.to_bytes()).into_bytes(),
    ] {
        let token = encode(
            &header,
            &claims_for(&env),
            &EncodingKey::from_secret(&secret),
        )
        .unwrap();
        let error = authenticate(&env, &token).await.unwrap_err();
        assert!(matches!(error, AuthError::InvalidToken(_)), "{error:?}");
        assert!(env.service.verify(&token).await.is_err());
    }
}

#[tokio::test]
async fn must_not_accept_asymmetric_algorithms_other_than_eddsa() {
    let env = env().await;
    let payload = segment(&claims_for(&env));
    for alg in ["RS256", "ES256", "PS256", "HS384", "HS512"] {
        let header = segment(&json!({"alg": alg, "typ": "JWT", "kid": KID}));
        let token = format!("{header}.{payload}.c2ln");
        let error = authenticate(&env, &token).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidToken(_)),
            "{alg}: {error:?}"
        );
        assert!(env.service.verify(&token).await.is_err(), "{alg}");
    }
}

#[tokio::test]
async fn must_not_accept_a_token_signed_by_another_key() {
    let env = env().await;
    let forged = sign(9, Some(KID), &claims_for(&env));
    let error = authenticate(&env, &forged).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("bad signature")),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_accept_tampered_claims() {
    let env = env().await;
    let issued = issue(&env);
    let mut parts: Vec<String> = issued
        .token
        .expose()
        .split('.')
        .map(str::to_owned)
        .collect();
    let mut claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&parts[1]).unwrap()).unwrap();
    claims["exp"] = json!(claims["exp"].as_i64().unwrap() + 1_000_000);
    parts[1] = segment(&claims);
    let error = authenticate(&env, &parts.join(".")).await.unwrap_err();
    assert!(matches!(error, AuthError::InvalidToken(_)), "{error:?}");
}

#[tokio::test]
async fn must_not_accept_a_missing_or_unknown_kid() {
    let env = env().await;
    let claims = claims_for(&env);
    let no_kid = sign(1, None, &claims);
    let error = authenticate(&env, &no_kid).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("kid missing")),
        "{error:?}"
    );
    let unknown = sign(1, Some("nope"), &claims);
    let error = authenticate(&env, &unknown).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("unknown kid")),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_use_a_key_the_token_did_not_name() {
    // Two valid keys in the ring: a token signed by k2 but labelled k1 must fail
    // even though k2 would have verified it.
    let env = env_with(ring("k2", 2, &[("k1", 1)])).await;
    let mislabelled = sign(2, Some("k1"), &claims_for(&env));
    let error = authenticate(&env, &mislabelled).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("bad signature")),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_follow_jwk_or_jku_headers() {
    let env = env().await;
    // An attacker signs with their own key and embeds it in the header. The
    // verification key comes from the ring by `kid`, so the embedded one is moot.
    let attacker = SigningKey::from_bytes(&seed(9)).verifying_key();
    let header = segment(&json!({
        "alg": "EdDSA", "typ": "JWT", "kid": KID,
        "jku": "https://attacker.example/jwks.json",
        "jwk": {"kty": "OKP", "crv": "Ed25519", "x": URL_SAFE_NO_PAD.encode(attacker.to_bytes())},
    }));
    let payload = segment(&claims_for(&env));
    let message = format!("{header}.{payload}");
    let signature = {
        use ed25519_dalek::Signer;
        URL_SAFE_NO_PAD.encode(
            SigningKey::from_bytes(&seed(9))
                .sign(message.as_bytes())
                .to_bytes(),
        )
    };
    let error = authenticate(&env, &format!("{message}.{signature}"))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::InvalidToken(_)), "{error:?}");
}

#[tokio::test]
async fn previous_key_verifies_but_never_signs_and_rotation_is_live() {
    let old = env_with(ring("k1", 1, &[])).await;
    let legacy = issue(&old);

    let rotated = InternalJwtService::new(
        ring("k2", 2, &[("k1", 1)]),
        InternalJwtConfig::default(),
        Arc::new(old.clock.clone()),
        Arc::new(old.store.clone()),
        old.revoked.clone(),
    );
    let auth = InternalJwtAuthenticator::new(Arc::new(rotated));
    let creds = bearer(legacy.token.expose());
    assert!(
        auth.authenticate(&creds, &AuthContext::default())
            .await
            .unwrap()
            .is_some()
    );

    // New tokens carry the new kid.
    let fresh = old.service.issue(&request(&old)).unwrap();
    assert_eq!(
        decode_header(fresh.token.expose()).unwrap().kid.as_deref(),
        Some("k1")
    );
    old.service.replace_ring(ring("k2", 2, &[("k1", 1)]));
    let signed_after = issue(&old);
    assert_eq!(
        decode_header(signed_after.token.expose())
            .unwrap()
            .kid
            .as_deref(),
        Some("k2")
    );
    assert!(
        authenticate(&old, signed_after.token.expose())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        authenticate(&old, legacy.token.expose())
            .await
            .unwrap()
            .is_some()
    );

    // Dropping the old key retires its tokens at once.
    old.service.replace_ring(ring("k2", 2, &[]));
    let error = authenticate(&old, legacy.token.expose()).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("unknown kid")),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_accept_a_token_issued_before_tokens_valid_after() {
    let env = env().await;
    let before = issue(&env);
    env.clock.advance(Duration::from_secs(10));
    UserRepo::set_tokens_valid_after(&env.store, env.user.id, env.clock.now())
        .await
        .unwrap();
    let error = authenticate(&env, before.token.expose()).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenRevoked), "{error:?}");
    // `iat >= tokens_valid_after` holds for a token issued at the same instant.
    let after = issue(&env);
    authenticate(&env, after.token.expose())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn must_not_accept_a_revoked_tid() {
    let env = env().await;
    let issued = issue(&env);
    let other = issue(&env);
    authenticate(&env, issued.token.expose())
        .await
        .unwrap()
        .unwrap();
    env.service
        .revoke(&issued.tid, issued.expires_at)
        .await
        .unwrap();
    let error = authenticate(&env, issued.token.expose()).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenRevoked), "{error:?}");
    authenticate(&env, other.token.expose())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn must_not_leave_a_session_alive_when_its_tid_is_respelled_for_revocation() {
    // The list is keyed by `Uuid`, so the hyphenated, upper case and urn spellings of one id
    // are the same revocation and none of them can slip past it.
    let env = env().await;
    let issued = issue(&env);
    let respelled = format!("urn:uuid:{}", issued.tid.to_uppercase());
    env.service
        .revoke(&respelled, issued.expires_at)
        .await
        .unwrap();
    let error = authenticate(&env, issued.token.expose()).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenRevoked), "{error:?}");
}

#[tokio::test]
async fn must_not_revoke_under_a_tid_that_is_not_a_uuid() {
    let env = env().await;
    let until = now_seconds(&env) + 60;
    let until = OffsetDateTime::from_unix_timestamp(until).unwrap();
    for bad in ["", "not-a-uuid", "x".repeat(65).as_str()] {
        let error = env.service.revoke(bad, until).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidToken("malformed tid")),
            "{bad:?}: {error:?}"
        );
    }
    assert!(env.revoked.is_empty(), "nothing was stored for a bad id");
}

#[tokio::test]
async fn must_fail_closed_when_the_revocation_list_or_store_is_down() {
    let env = env().await;
    let issued = issue(&env);
    env.revoked.set_unavailable(true);
    let error = authenticate(&env, issued.token.expose()).await.unwrap_err();
    assert!(
        matches!(error, AuthError::ProviderUnavailable(_)),
        "{error:?}"
    );
    assert!(
        env.service
            .revoke(&issued.tid, issued.expires_at)
            .await
            .is_err()
    );
    env.revoked.set_unavailable(false);
    authenticate(&env, issued.token.expose())
        .await
        .unwrap()
        .unwrap();
    env.store.set_unavailable(true);
    let error = authenticate(&env, issued.token.expose()).await.unwrap_err();
    assert!(
        matches!(error, AuthError::ProviderUnavailable(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_accept_tokens_of_disabled_or_deleted_accounts() {
    let env = env().await;
    let issued = issue(&env);
    UserRepo::set_enabled(&env.store, env.user.id, false)
        .await
        .unwrap();
    let error = authenticate(&env, issued.token.expose()).await.unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");
    UserRepo::set_enabled(&env.store, env.user.id, true)
        .await
        .unwrap();
    UserRepo::delete(&env.store, env.user.id).await.unwrap();
    let error = authenticate(&env, issued.token.expose()).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("unknown subject")),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_query_the_database_for_a_badly_signed_token() {
    let env = env().await;
    // With the store down, a forged token must still fail on its signature, not on
    // the store: proof that nothing was looked up before the signature check.
    env.store.set_unavailable(true);
    let forged = sign(9, Some(KID), &claims_for(&env));
    let error = authenticate(&env, &forged).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("bad signature")),
        "{error:?}"
    );
}

#[tokio::test]
async fn issuer_other_than_ours_is_not_claimed_and_not_verified() {
    let env = env().await;
    let mut claims = claims_for(&env);
    claims["iss"] = json!("https://idp.example.test/realms/other");
    let foreign = sign(1, Some(KID), &claims);
    assert!(authenticate(&env, &foreign).await.unwrap().is_none());
    let error = env.service.verify(&foreign).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("wrong issuer")),
        "{error:?}"
    );
}

#[tokio::test]
async fn not_applicable_credentials_return_none() {
    let env = env().await;
    let keycloak_like = format!(
        "{}.{}.c2ln",
        segment(&json!({"alg": "RS256", "kid": "abc"})),
        segment(&json!({"iss": "https://idp.example.test/realms/rustryx"}))
    );
    for token in [
        "opaque-token".to_owned(),
        "a.b".to_owned(),
        "a.b.c.d".to_owned(),
        "a.b.c.d.e".to_owned(),
        "!!!.???.***".to_owned(),
        format!("ryx_{}", "A".repeat(43)),
        keycloak_like,
        "x".repeat(MAX_TOKEN_BYTES + 1),
    ] {
        let outcome = authenticate(&env, &token).await.unwrap();
        assert!(outcome.is_none(), "{}", &token[..token.len().min(20)]);
    }
    for credentials in [
        Credentials::Anonymous,
        Credentials::Basic {
            username: "u".into(),
            password: Secret::new("p"),
        },
    ] {
        let outcome = env
            .auth
            .authenticate(&credentials, &AuthContext::default())
            .await
            .unwrap();
        assert!(outcome.is_none());
    }
}

#[tokio::test]
async fn must_not_accept_an_eddsa_token_of_a_foreign_issuer_as_ours() {
    let env = env().await;
    let mut claims = claims_for(&env);
    claims["iss"] = json!("someone-else");
    let token = sign(1, Some(KID), &claims);
    assert!(authenticate(&env, &token).await.unwrap().is_none());
}

#[tokio::test]
async fn must_not_accept_malformed_claims() {
    let env = env().await;
    let base = claims_for(&env);
    let now = now_seconds(&env);
    let cases: Vec<(&str, Value, &str)> = vec![
        ("sub", json!("not-a-uuid"), "malformed subject"),
        ("tid", json!(""), "malformed tid"),
        ("tid", json!("t".repeat(65)), "malformed tid"),
        ("tid", json!("forged-tid"), "malformed tid"),
        ("exp", json!(now), "exp is not after iat"),
        ("iat", json!(now + 6), "iat is in the future"),
        ("iat", json!(i64::MAX), "timestamp out of range"),
    ];
    for (field, value, expected) in cases {
        let mut claims = base.clone();
        claims[field] = value;
        let token = sign(1, Some(KID), &claims);
        let error = authenticate(&env, &token).await.unwrap_err();
        assert!(
            matches!(&error, AuthError::InvalidToken(reason) if *reason == expected),
            "{field}: expected {expected}, got {error:?}"
        );
    }
    let mut claims = base.clone();
    claims["iat"] = json!(now + 5);
    claims["exp"] = json!(now + 605);
    authenticate(&env, &sign(1, Some(KID), &claims))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn must_not_accept_missing_required_claims() {
    let env = env().await;
    for field in ["iss", "sub", "tid", "iat", "exp"] {
        let mut claims = claims_for(&env);
        claims.as_object_mut().unwrap().remove(field);
        let token = sign(1, Some(KID), &claims);
        // Without `iss` the token does not even name us, so the authenticator leaves it
        // to the others; the service itself refuses it either way.
        if field == "iss" {
            assert!(authenticate(&env, &token).await.unwrap().is_none());
        } else {
            let error = authenticate(&env, &token).await.unwrap_err();
            assert!(
                matches!(error, AuthError::InvalidToken(_)),
                "{field}: {error:?}"
            );
        }
        assert!(env.service.verify(&token).await.is_err(), "{field}");
    }
}

#[tokio::test]
async fn must_not_accept_an_incomplete_or_oversized_group_snapshot() {
    let env = env().await;
    let now = now_seconds(&env);
    let mut only_groups = claims_for(&env);
    only_groups["grp"] = json!(["/rustryx/admins"]);
    let mut only_time = claims_for(&env);
    only_time["gat"] = json!(now);
    let mut crowded = claims_for(&env);
    crowded["grp"] = json!(
        (0..=MAX_GROUPS_IN_TOKEN)
            .map(|i| format!("/g{i}"))
            .collect::<Vec<_>>()
    );
    crowded["gat"] = json!(now);
    let mut future = claims_for(&env);
    future["grp"] = json!(["/g"]);
    future["gat"] = json!(now + 60);
    for (label, claims) in [
        ("groups without time", only_groups),
        ("time without groups", only_time),
        ("too many groups", crowded),
        ("snapshot from the future", future),
    ] {
        let token = sign(1, Some(KID), &claims);
        let error = authenticate(&env, &token).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidToken(_)),
            "{label}: {error:?}"
        );
    }
}

#[tokio::test]
async fn must_not_leak_token_or_key_material_in_debug_output() {
    let env = env().await;
    let issued = issue(&env);
    let rendered = format!("{issued:?} {:?} {:?}", env.service, env.auth);
    assert!(!rendered.contains(issued.token.expose()));
    assert!(!rendered.contains(issued.token.expose().split('.').nth(2).unwrap()));
    assert!(rendered.contains("REDACTED"));
    assert!(rendered.contains(KID));
}

#[test]
fn peek_ignores_unusable_tokens() {
    assert!(peek("a.b.c").is_none());
    let no_alg = format!(
        "{}.{}.c",
        segment(&json!({"typ": "JWT"})),
        segment(&json!({}))
    );
    assert!(peek(&no_alg).is_none());
    let numeric_alg = format!("{}.{}.c", segment(&json!({"alg": 5})), segment(&json!({})));
    assert!(peek(&numeric_alg).is_none());
    let ok = format!(
        "{}.{}.c",
        segment(&json!({"alg": "EdDSA"})),
        segment(&json!({"iss": "x"}))
    );
    let peeked = peek(&ok).unwrap();
    assert_eq!(
        (peeked.alg.as_str(), peeked.iss.as_deref()),
        ("EdDSA", Some("x"))
    );
}
