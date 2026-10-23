//! Tests of API token creation and authentication.

use proptest::prelude::*;
use rustryx_core::identity::UserSource;
use rustryx_testkit::{FixedClock, MemStore, fixtures};

use super::*;

struct Env {
    store: MemStore,
    clock: FixedClock,
    auth: TokenAuthenticator,
    owner: rustryx_core::identity::User,
}

async fn env() -> Env {
    let store = MemStore::with_builtin_roles();
    let clock = FixedClock::default();
    let owner = UserRepo::create(&store, &fixtures::user("alice", UserSource::Local))
        .await
        .unwrap();
    let auth = TokenAuthenticator::new(
        Arc::new(store.clone()),
        Arc::new(store.clone()),
        Arc::new(clock.clone()),
    );
    Env {
        store,
        clock,
        auth,
        owner,
    }
}

async fn issue(env: &Env, spec: TokenSpec) -> IssuedToken {
    create_token(&env.store, &env.clock, env.owner.id, spec)
        .await
        .unwrap()
}

fn bearer(token: &IssuedToken) -> Credentials {
    Credentials::Bearer(token.secret.clone())
}

fn ctx() -> AuthContext {
    AuthContext::default()
}

#[tokio::test]
async fn created_token_has_the_documented_shape_and_stores_only_its_hash() {
    let env = env().await;
    let issued = issue(&env, TokenSpec::new("ci")).await;
    let text = issued.secret.expose();
    assert!(text.starts_with("ryx_"));
    assert_eq!(text.len(), 4 + 43);
    assert!(is_well_formed_token(text));
    assert_eq!(issued.record.token_hash, hash_token(text));
    assert_eq!(issued.record.token_hash.len(), 64);
    assert_eq!(issued.record.display_prefix, text[..8]);
    let stored = TokenRepo::list_for_user(&env.store, env.owner.id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    let dump = format!("{stored:?}");
    assert!(
        !dump.contains(text),
        "the stored record must not contain the secret"
    );
    assert!(!dump.contains(&text[8..]));
}

#[tokio::test]
async fn tokens_are_unique_and_expire_in_ninety_days_by_default() {
    let env = env().await;
    let a = issue(&env, TokenSpec::new("a")).await;
    let b = issue(&env, TokenSpec::new("b")).await;
    assert_ne!(a.secret.expose(), b.secret.expose());
    let expires = a.record.expires_at.unwrap();
    assert_eq!(expires - a.record.created_at, time::Duration::days(90));
}

#[tokio::test]
async fn create_rejects_bad_specs() {
    let env = env().await;
    for spec in [
        TokenSpec::new("  "),
        TokenSpec::new("x".repeat(129)),
        TokenSpec {
            ttl: Duration::ZERO,
            ..TokenSpec::new("zero")
        },
        TokenSpec {
            ttl: MAX_TOKEN_TTL + Duration::from_secs(1),
            ..TokenSpec::new("forever")
        },
    ] {
        let result = create_token(&env.store, &env.clock, env.owner.id, spec).await;
        assert!(matches!(result, Err(TokenCreateError::InvalidSpec(_))));
    }
}

#[tokio::test]
async fn create_reports_a_store_outage() {
    let env = env().await;
    env.store.set_unavailable(true);
    let result = create_token(&env.store, &env.clock, env.owner.id, TokenSpec::new("x")).await;
    assert!(matches!(
        result,
        Err(TokenCreateError::Auth(AuthError::ProviderUnavailable(_)))
    ));
}

#[tokio::test]
async fn authenticates_bearer_and_basic_with_scopes() {
    let env = env().await;
    let issued = issue(
        &env,
        TokenSpec {
            scopes: vec!["repo:read".into()],
            ..TokenSpec::new("scoped")
        },
    )
    .await;
    let principal = env
        .auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(principal.username, "alice");
    assert_eq!(principal.auth_method, AuthMethod::ApiToken);
    assert_eq!(principal.token_scopes, Some(vec!["repo:read".to_owned()]));
    assert_eq!(principal.token_id, Some(issued.record.id));
    assert_eq!(principal.roles.len(), 0);

    // The user name of a Basic credential is ignored (`twine` sends `__token__`).
    let basic = Credentials::Basic {
        username: "__token__".into(),
        password: issued.secret.clone(),
    };
    let via_basic = env
        .auth
        .authenticate(&basic, &ctx())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(via_basic.user_id, Some(env.owner.id));
}

#[tokio::test]
async fn not_applicable_credentials_are_left_to_other_authenticators() {
    let env = env().await;
    let others = [
        Credentials::Anonymous,
        Credentials::Bearer(Secret::new("eyJhbGciOiJFZERTQSJ9.e30.sig")),
        Credentials::Basic {
            username: "alice".into(),
            password: Secret::new("a normal password"),
        },
    ];
    for credentials in others {
        assert!(
            env.auth
                .authenticate(&credentials, &ctx())
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn must_not_accept_malformed_or_unknown_tokens() {
    let env = env().await;
    let well_formed_unknown = generate_token_secret();
    for text in [
        "ryx_".to_owned(),
        "ryx_short".to_owned(),
        format!("ryx_{}", "A".repeat(44)),
        format!("ryx_{}!", "A".repeat(42)),
        well_formed_unknown.expose().to_owned(),
    ] {
        let bearer = Credentials::Bearer(Secret::new(text.clone()));
        let error = env.auth.authenticate(&bearer, &ctx()).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidToken(_)),
            "{text}: {error:?}"
        );
        let basic = Credentials::Basic {
            username: "alice".into(),
            password: Secret::new(text.clone()),
        };
        let error = env.auth.authenticate(&basic, &ctx()).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidCredentials),
            "{text}: {error:?}"
        );
    }
}

#[tokio::test]
async fn must_not_accept_a_revoked_token() {
    let env = env().await;
    let issued = issue(&env, TokenSpec::new("doomed")).await;
    env.auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap()
        .unwrap();
    TokenRepo::revoke(&env.store, issued.record.id, env.clock.now())
        .await
        .unwrap();
    let error = env
        .auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::TokenRevoked));
}

#[tokio::test]
async fn must_not_accept_an_expired_token() {
    let env = env().await;
    let issued = issue(
        &env,
        TokenSpec {
            ttl: Duration::from_secs(60),
            ..TokenSpec::new("short")
        },
    )
    .await;
    env.clock.advance(Duration::from_secs(59));
    env.auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap()
        .unwrap();
    env.clock.advance(Duration::from_secs(1));
    let error = env
        .auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::TokenExpired),
        "expiry is inclusive"
    );
}

#[tokio::test]
async fn must_not_accept_a_token_of_a_disabled_or_deleted_owner() {
    let env = env().await;
    let issued = issue(&env, TokenSpec::new("owned")).await;
    UserRepo::set_enabled(&env.store, env.owner.id, false)
        .await
        .unwrap();
    let error = env
        .auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled));

    UserRepo::set_enabled(&env.store, env.owner.id, true)
        .await
        .unwrap();
    env.auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap()
        .unwrap();
    UserRepo::delete(&env.store, env.owner.id).await.unwrap();
    let error = env
        .auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::InvalidToken(_)));
}

#[tokio::test]
async fn store_outage_is_a_503_class_error_not_a_denial() {
    let env = env().await;
    let issued = issue(&env, TokenSpec::new("x")).await;
    env.store.set_unavailable(true);
    let error = env
        .auth
        .authenticate(&bearer(&issued), &ctx())
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::ProviderUnavailable(_)));
}

#[tokio::test]
async fn last_use_is_written_once_per_interval_not_per_request() {
    let env = env().await;
    let issued = issue(&env, TokenSpec::new("busy")).await;
    let creds = bearer(&issued);
    for _ in 0..25 {
        env.auth.authenticate(&creds, &ctx()).await.unwrap();
    }
    let first = TokenRepo::list_for_user(&env.store, env.owner.id)
        .await
        .unwrap()[0]
        .last_used_at
        .unwrap();
    assert_eq!(first, env.clock.now());

    env.clock.advance(Duration::from_secs(30));
    env.auth.authenticate(&creds, &ctx()).await.unwrap();
    let unchanged = TokenRepo::list_for_user(&env.store, env.owner.id)
        .await
        .unwrap()[0]
        .last_used_at
        .unwrap();
    assert_eq!(unchanged, first, "no write inside the interval");

    env.clock.advance(Duration::from_secs(31));
    env.auth.authenticate(&creds, &ctx()).await.unwrap();
    let later = TokenRepo::list_for_user(&env.store, env.owner.id)
        .await
        .unwrap()[0]
        .last_used_at
        .unwrap();
    assert_eq!(later, env.clock.now());
}

#[tokio::test]
async fn a_failed_last_use_write_does_not_fail_the_request() {
    struct FailingTouch(MemStore);
    #[async_trait]
    impl TokenRepo for FailingTouch {
        async fn create(&self, token: &ApiToken) -> Result<(), rustryx_core::StoreError> {
            TokenRepo::create(&self.0, token).await
        }
        async fn find_by_hash(
            &self,
            hash: &str,
        ) -> Result<Option<ApiToken>, rustryx_core::StoreError> {
            self.0.find_by_hash(hash).await
        }
        async fn list_for_user(
            &self,
            id: UserId,
        ) -> Result<Vec<ApiToken>, rustryx_core::StoreError> {
            self.0.list_for_user(id).await
        }
        async fn revoke(
            &self,
            id: ApiTokenId,
            at: OffsetDateTime,
        ) -> Result<(), rustryx_core::StoreError> {
            TokenRepo::revoke(&self.0, id, at).await
        }
        async fn touch_last_used(
            &self,
            _id: ApiTokenId,
            _at: OffsetDateTime,
        ) -> Result<(), rustryx_core::StoreError> {
            Err(rustryx_core::StoreError::Unavailable("touch".into()))
        }
    }
    let env = env().await;
    let issued = issue(&env, TokenSpec::new("x")).await;
    let auth = TokenAuthenticator::new(
        Arc::new(FailingTouch(env.store.clone())),
        Arc::new(env.store.clone()),
        Arc::new(env.clock.clone()),
    );
    assert!(
        auth.authenticate(&bearer(&issued), &ctx())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn must_not_trust_a_row_whose_hash_differs_from_the_presented_one() {
    // A store bug that returns some other token's row for any key.
    struct WrongRow(MemStore, UserId);
    #[async_trait]
    impl TokenRepo for WrongRow {
        async fn create(&self, token: &ApiToken) -> Result<(), rustryx_core::StoreError> {
            TokenRepo::create(&self.0, token).await
        }
        async fn find_by_hash(
            &self,
            _hash: &str,
        ) -> Result<Option<ApiToken>, rustryx_core::StoreError> {
            Ok(self.0.list_for_user(self.1).await?.into_iter().next())
        }
        async fn list_for_user(
            &self,
            id: UserId,
        ) -> Result<Vec<ApiToken>, rustryx_core::StoreError> {
            self.0.list_for_user(id).await
        }
        async fn revoke(
            &self,
            id: ApiTokenId,
            at: OffsetDateTime,
        ) -> Result<(), rustryx_core::StoreError> {
            TokenRepo::revoke(&self.0, id, at).await
        }
        async fn touch_last_used(
            &self,
            id: ApiTokenId,
            at: OffsetDateTime,
        ) -> Result<(), rustryx_core::StoreError> {
            self.0.touch_last_used(id, at).await
        }
    }
    let env = env().await;
    let _real = issue(&env, TokenSpec::new("real")).await;
    let auth = TokenAuthenticator::new(
        Arc::new(WrongRow(env.store.clone(), env.owner.id)),
        Arc::new(env.store.clone()),
        Arc::new(env.clock.clone()),
    );
    let forged = Credentials::Bearer(generate_token_secret());
    let error = auth.authenticate(&forged, &ctx()).await.unwrap_err();
    assert!(matches!(error, AuthError::InvalidToken(_)));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn generated_tokens_always_parse_and_hash_stably(_seed in 0_u8..8) {
        let secret = generate_token_secret();
        prop_assert!(is_well_formed_token(secret.expose()));
        prop_assert_eq!(hash_token(secret.expose()), hash_token(secret.expose()));
    }

    #[test]
    fn parser_never_panics_and_accepts_only_the_exact_shape(text in "\\PC{0,80}") {
        let ok = is_well_formed_token(&text);
        if ok {
            prop_assert!(text.starts_with("ryx_"));
            prop_assert_eq!(text.len(), 47);
        }
    }

    #[test]
    fn any_single_character_edit_of_a_valid_token_is_a_different_hash(
        index in 4_usize..47,
        replacement in prop::sample::select(
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".to_vec()
        ),
    ) {
        let secret = generate_token_secret();
        let original = secret.expose().to_owned();
        let mut bytes = original.clone().into_bytes();
        if bytes[index] != replacement {
            bytes[index] = replacement;
            let edited = String::from_utf8(bytes).unwrap();
            prop_assert_ne!(hash_token(&edited), hash_token(&original));
        }
    }
}
