//! Bearer validation: allowlist, `kid`, issuer, audience, time claims, token kinds, headers.

use rustryx_core::AuthError;
use rustryx_core::identity::{AuthMethod, Principal, UserSource};
use serde_json::{Value, json};

use crate::oidc::test_support::{AUDIENCE, TestEnv, access_claims, keys, mint, now, with};

type AuthResult = Result<Option<Principal>, AuthError>;

fn assert_not_authenticated(result: &AuthResult) {
    assert!(
        !matches!(result, Ok(Some(_))),
        "token must not authenticate: {result:?}"
    );
}

fn assert_invalid(result: AuthResult, reason: &'static str) {
    match result {
        Err(AuthError::InvalidToken(actual)) => assert_eq!(actual, reason),
        other => panic!("expected InvalidToken({reason}), got {other:?}"),
    }
}

async fn warm() -> TestEnv {
    TestEnv::start_warm().await
}

fn rsa_token(env: &TestEnv, claims: &Value) -> String {
    let _ = env;
    keys().rsa.sign(claims)
}

#[tokio::test]
async fn accepts_valid_rs256_token_and_builds_principal() {
    let env = TestEnv::start().await;
    let token = env.access_token(&keys().rsa);
    let principal = env.authenticate(&token).await.unwrap().unwrap();
    assert_eq!(principal.username, "alice");
    assert_eq!(principal.source, UserSource::Oidc);
    assert_eq!(principal.auth_method, AuthMethod::OidcBearer);
    assert_eq!(principal.issuer.as_deref(), Some(env.issuer().as_str()));
    assert_eq!(principal.subject.as_deref(), Some("5a0b1c2d-user-1"));
    assert_eq!(
        principal.external_groups,
        ["/rustryx/admins", "/rustryx/team-payments/publishers"]
    );
    assert!(principal.user_id.is_none() && principal.roles.is_empty());
    assert!(principal.token_id.is_none() && principal.token_scopes.is_none());
    assert!(principal.groups_confirmed_at.is_some());
}

#[tokio::test]
async fn accepts_valid_es256_token() {
    let env = warm().await;
    let token = env.access_token(&keys().ec);
    assert!(env.authenticate(&token).await.unwrap().is_some());
}

#[tokio::test]
async fn validate_exposes_email_and_expiry_for_provisioning() {
    let env = warm().await;
    let verified = env
        .provider
        .bearer_authenticator()
        .validate(&env.access_token(&keys().rsa))
        .await
        .unwrap();
    assert_eq!(
        verified.identity.email.as_deref(),
        Some("alice@example.test")
    );
    assert!(verified.expires_at.unix_timestamp() > now());
    let err = env
        .provider
        .bearer_authenticator()
        .validate("ryx_not_a_jwt")
        .await;
    assert!(matches!(
        err,
        Err(AuthError::InvalidToken("not an oidc token"))
    ));
}

#[tokio::test]
async fn debug_output_names_no_token_or_key() {
    let env = TestEnv::start().await;
    let shown = format!("{:?}", env.provider.bearer_authenticator());
    assert_eq!(shown, "OidcBearerAuthenticator { .. }");
    assert_eq!(
        format!("{:?}", crate::oidc::OidcGroupResolver::new()),
        "OidcGroupResolver"
    );
}

#[tokio::test]
async fn leaves_foreign_credentials_to_the_chain() {
    use rustryx_core::Secret;
    use rustryx_core::identity::{AuthContext, Credentials};
    use rustryx_core::ports::Authenticator;

    let env = warm().await;
    let authenticator = env.provider.bearer_authenticator();
    let foreign_alg = mint(
        &json!({"alg": "EdDSA", "kid": "internal-1"}),
        &access_claims("x"),
        None,
    );
    let other_issuer = keys().rsa.sign(&with(
        access_claims("x"),
        "iss",
        json!("https://other.example/realms/x"),
    ));
    let oversized = format!("{}.{}.{}", "a".repeat(40_000), "b", "c");
    let credentials = [
        Credentials::Anonymous,
        Credentials::Basic {
            username: "u".into(),
            password: Secret::new("p"),
        },
        Credentials::Bearer(Secret::new("ryx_0123456789abcdef")),
        Credentials::Bearer(Secret::new("")),
        Credentials::Bearer(Secret::new("only.two")),
        Credentials::Bearer(Secret::new("a.b.c.d")),
        Credentials::Bearer(Secret::new("!!!.@@@.###")),
        Credentials::Bearer(Secret::new(foreign_alg)),
        Credentials::Bearer(Secret::new(other_issuer)),
        Credentials::Bearer(Secret::new(oversized)),
    ];
    for credential in &credentials {
        let result = authenticator
            .authenticate(credential, &AuthContext::default())
            .await;
        assert!(matches!(result, Ok(None)), "{credential:?} gave {result:?}");
    }
}

#[tokio::test]
async fn must_not_accept_wrong_issuer() {
    let env = warm().await;
    for issuer in [
        "https://evil.example/realms/test".to_owned(),
        format!("{}/", env.issuer()),
        env.issuer().to_uppercase(),
        String::new(),
    ] {
        let token = rsa_token(
            &env,
            &with(access_claims(&env.issuer()), "iss", json!(issuer)),
        );
        assert_not_authenticated(&env.authenticate(&token).await);
        assert!(
            env.provider
                .bearer_authenticator()
                .validate(&token)
                .await
                .is_err()
        );
    }
    let numeric = rsa_token(&env, &with(access_claims(&env.issuer()), "iss", json!(7)));
    assert_not_authenticated(&env.authenticate(&numeric).await);
    let missing = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "iss", Value::Null),
    );
    assert_not_authenticated(&env.authenticate(&missing).await);
}

#[tokio::test]
async fn must_not_accept_wrong_audience_or_account_alone() {
    let env = warm().await;
    let bad = [
        json!(["other-client"]),
        json!("other-client"),
        json!(["account"]),
        json!("account"),
        json!([]),
        json!(["Rustryx"]),
        json!(["rustryx-ui"]),
    ];
    for aud in bad {
        let token = rsa_token(
            &env,
            &with(access_claims(&env.issuer()), "aud", aud.clone()),
        );
        assert_invalid(env.authenticate(&token).await, "audience mismatch");
    }
    let absent = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "aud", Value::Null),
    );
    assert_invalid(env.authenticate(&absent).await, "required claim missing");
}

#[tokio::test]
async fn accepts_audience_list_that_contains_the_client_next_to_account() {
    let env = warm().await;
    for aud in [
        json!(["account", AUDIENCE]),
        json!(AUDIENCE),
        json!([AUDIENCE, "account"]),
    ] {
        let token = rsa_token(&env, &with(access_claims(&env.issuer()), "aud", aud));
        assert!(env.authenticate(&token).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn must_not_accept_expired_token_but_tolerates_the_skew() {
    let env = warm().await;
    let expired = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "exp", json!(now() - 3600)),
    );
    assert!(matches!(
        env.authenticate(&expired).await,
        Err(AuthError::TokenExpired)
    ));
    let just_beyond_skew = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "exp", json!(now() - 90)),
    );
    assert!(matches!(
        env.authenticate(&just_beyond_skew).await,
        Err(AuthError::TokenExpired)
    ));
    let within_skew = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "exp", json!(now() - 30)),
    );
    assert!(env.authenticate(&within_skew).await.unwrap().is_some());
}

#[tokio::test]
async fn must_not_accept_token_without_exp() {
    let env = warm().await;
    let token = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "exp", Value::Null),
    );
    assert_invalid(env.authenticate(&token).await, "required claim missing");
}

#[tokio::test]
async fn must_not_accept_future_nbf_beyond_skew() {
    let env = warm().await;
    let far = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "nbf", json!(now() + 3600)),
    );
    assert_invalid(env.authenticate(&far).await, "token not yet valid");
    let beyond = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "nbf", json!(now() + 90)),
    );
    assert_invalid(env.authenticate(&beyond).await, "token not yet valid");
    let within = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "nbf", json!(now() + 30)),
    );
    assert!(env.authenticate(&within).await.unwrap().is_some());
}

#[tokio::test]
async fn must_not_accept_future_iat_beyond_skew() {
    let env = warm().await;
    let far = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "iat", json!(now() + 3600)),
    );
    assert_invalid(env.authenticate(&far).await, "token not yet valid");
    let within = rsa_token(
        &env,
        &with(access_claims(&env.issuer()), "iat", json!(now() + 30)),
    );
    assert!(env.authenticate(&within).await.unwrap().is_some());
}

#[tokio::test]
async fn must_not_accept_alg_none() {
    let env = warm().await;
    for alg in ["none", "None", "NONE"] {
        let token = mint(
            &json!({"alg": alg, "typ": "JWT", "kid": "rsa-1"}),
            &access_claims(&env.issuer()),
            None,
        );
        assert_not_authenticated(&env.authenticate(&token).await);
        assert!(
            env.provider
                .bearer_authenticator()
                .validate(&token)
                .await
                .is_err()
        );
    }
    let no_alg = mint(
        &json!({"kid": "rsa-1"}),
        &access_claims(&env.issuer()),
        None,
    );
    assert_not_authenticated(&env.authenticate(&no_alg).await);
}

#[tokio::test]
async fn must_not_accept_hs256_signed_with_the_public_key_as_secret() {
    use jsonwebtoken::{Algorithm, EncodingKey};
    let env = warm().await;
    let public = &keys().rsa;
    let hmac_key = EncodingKey::from_secret(public.public_pem.as_bytes());
    let claims = access_claims(&env.issuer());
    // 1. Honest header: the token says HS256. It never reaches signature verification.
    let hs256 = mint(
        &json!({"alg": "HS256", "typ": "JWT", "kid": public.kid}),
        &claims,
        Some((&hmac_key, Algorithm::HS256)),
    );
    assert_not_authenticated(&env.authenticate(&hs256).await);
    assert!(
        env.provider
            .bearer_authenticator()
            .validate(&hs256)
            .await
            .is_err()
    );
    // 2. Lying header: RS256 on top of an HMAC signature fails verification.
    let lying = mint(
        &json!({"alg": "RS256", "typ": "JWT", "kid": public.kid}),
        &claims,
        Some((&hmac_key, Algorithm::HS256)),
    );
    assert_invalid(
        env.authenticate(&lying).await,
        "signature verification failed",
    );
    // 3. The other HMAC variants are not in the allowlist either.
    for alg in [Algorithm::HS384, Algorithm::HS512] {
        let token = mint(
            &json!({"alg": format!("{alg:?}"), "kid": public.kid}),
            &claims,
            Some((&hmac_key, alg)),
        );
        assert_not_authenticated(&env.authenticate(&token).await);
    }
}

#[tokio::test]
async fn must_not_accept_algorithms_outside_the_allowlist() {
    let env = warm().await;
    for alg in [
        "RS384", "RS512", "PS256", "ES384", "ES512", "EdDSA", "RSA-OAEP", "",
    ] {
        let token = keys()
            .rsa
            .sign_with_header(json!({"alg": alg}), &access_claims(&env.issuer()));
        assert!(matches!(env.authenticate(&token).await, Ok(None)), "{alg}");
    }
}

#[tokio::test]
async fn must_not_accept_key_published_for_another_algorithm() {
    // The token claims ES256 under the kid of the RSA key.
    let env = warm().await;
    let token = keys().ec.sign_with_header(
        json!({"kid": keys().rsa.kid}),
        &access_claims(&env.issuer()),
    );
    assert_invalid(
        env.authenticate(&token).await,
        "key does not match algorithm",
    );
}

#[tokio::test]
async fn must_not_accept_token_without_kid() {
    let env = warm().await;
    let token = keys()
        .rsa
        .sign_with_header(json!({"kid": null}), &access_claims(&env.issuer()));
    assert_invalid(env.authenticate(&token).await, "kid is required");
    let empty = keys()
        .rsa
        .sign_with_header(json!({"kid": ""}), &access_claims(&env.issuer()));
    assert_invalid(env.authenticate(&empty).await, "kid is required");
    let long = keys().rsa.sign_with_header(
        json!({"kid": "k".repeat(300)}),
        &access_claims(&env.issuer()),
    );
    assert_invalid(env.authenticate(&long).await, "kid is required");
}

#[tokio::test]
async fn must_not_accept_forged_tampered_or_truncated_tokens() {
    let env = warm().await;
    let genuine = env.access_token(&keys().rsa);
    // Payload of a genuine token swapped for an admin one, signature kept.
    let forged_claims = with(access_claims(&env.issuer()), "sub", json!("someone-else"));
    let forged_payload = mint(&json!({}), &forged_claims, None);
    let parts: Vec<&str> = genuine.split('.').collect();
    let swapped = format!(
        "{}.{}.{}",
        parts[0],
        forged_payload.split('.').nth(1).unwrap(),
        parts[2]
    );
    assert_invalid(
        env.authenticate(&swapped).await,
        "signature verification failed",
    );
    // A cut-off signature is no longer canonical base64 or has the wrong length: either way no principal.
    let truncated = &genuine[..genuine.len() - 8];
    assert!(matches!(
        env.authenticate(truncated).await,
        Err(AuthError::InvalidToken(
            "malformed token" | "signature verification failed"
        ))
    ));
    // Signed with a different key under the known kid.
    let attacker = keys().attacker_rsa.sign(&access_claims(&env.issuer()));
    assert_invalid(
        env.authenticate(&attacker).await,
        "signature verification failed",
    );
    // Signed by an unknown key under an unknown kid.
    let unknown = keys().attacker_ec.sign(&access_claims(&env.issuer()));
    assert_invalid(env.authenticate(&unknown).await, "signing key unknown");
}

#[tokio::test]
async fn must_not_follow_jku_x5u_or_jwk_headers() {
    let env = warm().await;
    let attacker = &keys().attacker_ec;
    let attacker_url = format!("https://localhost:{}/attacker/jwks", env.mock.addr.port());
    let tricks = json!({
        "jku": attacker_url,
        "x5u": attacker_url,
        "jwk": attacker.jwk.clone(),
    });
    let claims = access_claims(&env.issuer());
    // Signed by the attacker, pointing at the attacker's own key: refused, nothing fetched.
    let forged = attacker.sign_with_header(tricks.clone(), &claims);
    assert_invalid(env.authenticate(&forged).await, "signing key unknown");
    assert_eq!(env.mock.hits().attacker(), 0);
    // The same trick under the kid of a trusted key: signature check against the real key fails.
    let hijack = attacker.sign_with_header(
        {
            let mut headers = tricks.clone();
            headers["kid"] = json!(keys().ec.kid);
            headers
        },
        &claims,
    );
    assert_invalid(
        env.authenticate(&hijack).await,
        "signature verification failed",
    );
    // A genuine token is not bothered by the extra headers either: they are ignored.
    let genuine = keys().ec.sign_with_header(tricks, &claims);
    assert!(env.authenticate(&genuine).await.unwrap().is_some());
    assert_eq!(env.mock.hits().attacker(), 0);
}

#[tokio::test]
async fn must_not_accept_id_token_as_bearer() {
    let env = warm().await;
    for typ in ["ID", "id"] {
        let token = rsa_token(&env, &with(access_claims(&env.issuer()), "typ", json!(typ)));
        assert_invalid(
            env.authenticate(&token).await,
            "id token is not an access token",
        );
    }
    // A realistic ID token of the product client: right audience, nonce, no scope.
    let mut id = crate::oidc::test_support::id_claims(&env.issuer(), "n-0S6_WzA2Mj");
    id["aud"] = json!(AUDIENCE);
    id["azp"] = json!(AUDIENCE);
    assert_invalid(
        env.authenticate(&rsa_token(&env, &id)).await,
        "id token is not an access token",
    );
}

#[tokio::test]
async fn must_not_accept_offline_or_other_token_kinds() {
    let env = warm().await;
    let offline_scope = with(
        access_claims(&env.issuer()),
        "scope",
        json!("openid offline_access"),
    );
    assert_invalid(
        env.authenticate(&rsa_token(&env, &offline_scope)).await,
        "offline tokens are not accepted",
    );
    let offline_type = with(access_claims(&env.issuer()), "typ", json!("Offline"));
    assert_invalid(
        env.authenticate(&rsa_token(&env, &offline_type)).await,
        "offline tokens are not accepted",
    );
    for typ in [
        json!("Refresh"),
        json!("Logout"),
        json!("Serialized-ID"),
        Value::Null,
    ] {
        let claims = with(access_claims(&env.issuer()), "typ", typ);
        assert_invalid(
            env.authenticate(&rsa_token(&env, &claims)).await,
            "unexpected token type",
        );
    }
    // `offline_access` as a realm role is on every Keycloak user and is not a signal.
    assert!(
        env.authenticate(&env.access_token(&keys().rsa))
            .await
            .unwrap()
            .is_some()
    );
    let lookalike = with(
        access_claims(&env.issuer()),
        "scope",
        json!("openid offline_access_not"),
    );
    assert!(
        env.authenticate(&rsa_token(&env, &lookalike))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn must_not_accept_authorized_party_other_than_the_client() {
    let env = warm().await;
    let wrong = with(
        access_claims(&env.issuer()),
        "azp",
        json!("some-other-client"),
    );
    assert_invalid(
        env.authenticate(&rsa_token(&env, &wrong)).await,
        "authorized party mismatch",
    );
    let absent = with(access_claims(&env.issuer()), "azp", Value::Null);
    assert!(
        env.authenticate(&rsa_token(&env, &absent))
            .await
            .unwrap()
            .is_some()
    );

    let cli = TestEnv::start_with(|c| c.authorized_parties = vec!["rustryx-cli".into()]).await;
    let cli_token = keys().rsa.sign(&with(
        access_claims(&cli.issuer()),
        "azp",
        json!("rustryx-cli"),
    ));
    assert!(cli.authenticate(&cli_token).await.unwrap().is_some());
    let default_azp = keys().rsa.sign(&access_claims(&cli.issuer()));
    assert_invalid(
        cli.authenticate(&default_azp).await,
        "authorized party mismatch",
    );
}

#[tokio::test]
async fn must_not_authenticate_without_subject_or_with_hostile_names() {
    let env = warm().await;
    let no_sub = with(access_claims(&env.issuer()), "sub", Value::Null);
    assert_invalid(
        env.authenticate(&rsa_token(&env, &no_sub)).await,
        "required claim missing",
    );
    let hostile = with(
        access_claims(&env.issuer()),
        "preferred_username",
        json!("alice\nadmin"),
    );
    assert_invalid(
        env.authenticate(&rsa_token(&env, &hostile)).await,
        "invalid claims",
    );
    let wrong_type = with(access_claims(&env.issuer()), "sub", json!(42));
    assert!(
        env.authenticate(&rsa_token(&env, &wrong_type))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn username_falls_back_to_the_subject() {
    let env = warm().await;
    let claims = with(
        access_claims(&env.issuer()),
        "preferred_username",
        Value::Null,
    );
    let principal = env
        .authenticate(&rsa_token(&env, &claims))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(principal.username, "5a0b1c2d-user-1");
}

#[tokio::test]
async fn must_not_trust_a_token_before_its_key_is_known_when_keycloak_is_down() {
    let env = TestEnv::start().await;
    env.mock.kill();
    let token = env.access_token(&keys().rsa);
    assert!(matches!(
        env.authenticate(&token).await,
        Err(AuthError::ProviderUnavailable(_))
    ));
}
