//! What a refusal says about itself (which method refused), the series an authentication
//! emits, the break-glass counter and the request deadline.

use std::time::{Duration, Instant};

use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials};
use rustryx_core::ports::{Authenticator, TokenRepo};
use serde_json::json;

use super::AuthServiceConfig;
use super::testing::{
    ADMIN_PATH, Options, basic, harness, harness_with, oidc_token, oidc_token_from,
};
use crate::internal_jwt::IssueRequest;
use crate::passwords::{GateClass, GateConfig, LaneLimits};
use crate::telemetry::recording::Capture;
use crate::tokens::{TokenSpec, create_token};

const PASSWORD: &str = "correct horse";

fn one_lane() -> GateConfig {
    GateConfig {
        password: LaneLimits {
            permits: 1,
            max_waiting: 0,
        },
        general: LaneLimits {
            permits: 4,
            max_waiting: 4,
        },
        permit_wait: Duration::from_millis(1),
    }
}

#[tokio::test]
async fn every_refusal_names_the_method_that_refused() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    h.add_ldap_user("bob", "ldap secret");

    let failure = h.refusal(&basic("alice", "wrong horse")).await;
    assert_eq!(failure.method, Some(AuthMethod::BasicLocal));
    assert!(matches!(failure.error, AuthError::InvalidCredentials));

    let issued = create_token(&h.store, &h.clock, alice.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    TokenRepo::revoke(&h.store, issued.record.id, h.clock_now())
        .await
        .unwrap();
    let failure = h.refusal(&Credentials::Bearer(issued.secret.clone())).await;
    assert_eq!(failure.method, Some(AuthMethod::ApiToken));
    assert!(matches!(failure.error, AuthError::TokenRevoked));
    let through_basic = basic("__token__", issued.secret.expose());
    assert_eq!(
        h.refusal(&through_basic).await.method,
        Some(AuthMethod::ApiToken)
    );

    let jwt = h
        .jwt
        .issue(&IssueRequest {
            user_id: alice.id,
            ttl: Duration::from_mins(10),
            groups: None,
        })
        .unwrap();
    h.jwt.revoke(&jwt.tid, jwt.expires_at).await.unwrap();
    let failure = h.refusal(&Credentials::Bearer(jwt.token.clone())).await;
    assert_eq!(failure.method, Some(AuthMethod::InternalJwt));
    assert!(matches!(failure.error, AuthError::TokenRevoked));

    let foreign = oidc_token_from(&json!({
        "iss": "https://evil.example/realms/x",
        "sub": "s",
        "preferred_username": "mallory",
        "exp": h.exp_in(300),
    }));
    let failure = h.refusal(&Credentials::Bearer(Secret::new(foreign))).await;
    assert_eq!(failure.method, Some(AuthMethod::OidcBearer));
    assert!(matches!(failure.error, AuthError::InvalidToken(_)));

    let failure = h.refusal(&basic("bob", "not the secret")).await;
    assert_eq!(failure.method, Some(AuthMethod::BasicLdap));
    assert!(matches!(failure.error, AuthError::InvalidCredentials));
}

#[tokio::test]
async fn an_access_denied_after_authentication_carries_the_method_of_the_principal() {
    let h = harness();
    let token = oidc_token("carol", "sub-c", &["/unmapped"], h.exp_in(300));
    let failure = h.refusal(&Credentials::Bearer(Secret::new(token))).await;
    assert_eq!(failure.method, Some(AuthMethod::OidcBearer));
    assert!(matches!(failure.error, AuthError::AccessDenied(_)));

    h.add_ldap_user("bob", "ldap secret");
    let failure = h.refusal(&basic("bob", "ldap secret")).await;
    assert_eq!(failure.method, Some(AuthMethod::BasicLdap));
    assert!(matches!(failure.error, AuthError::AccessDenied(_)));
}

#[tokio::test]
async fn a_refusal_before_any_member_looked_at_the_credentials_has_no_method() {
    let h = harness();
    h.add_local_user("alice", PASSWORD).await;
    for blank in [basic("alice", ""), basic("  ", PASSWORD)] {
        let failure = h.refusal(&blank).await;
        assert_eq!(failure.method, None);
        assert!(matches!(failure.error, AuthError::InvalidCredentials));
    }
    let unclaimed = h
        .refusal(&Credentials::Bearer(Secret::new("opaque-token")))
        .await;
    assert_eq!(unclaimed.method, None);
    assert!(matches!(unclaimed.error, AuthError::InvalidToken(_)));
    // With no directory configured, nobody claims an unknown account.
    let local_only = harness_with(Options {
        with_ldap: false,
        with_oidc: false,
        ..Options::default()
    });
    let nobody = local_only.refusal(&basic("nobody", PASSWORD)).await;
    assert_eq!(nobody.method, None, "nobody claimed an unknown account");
    assert!(matches!(nobody.error, AuthError::InvalidCredentials));

    for _ in 0..5 {
        h.basic("alice", "wrong horse").await.unwrap_err();
    }
    let locked = h.refusal(&basic("alice", PASSWORD)).await;
    assert_eq!(locked.method, None, "the guard answers before any member");
    assert!(matches!(locked.error, AuthError::LockedOut { .. }));
}

#[tokio::test]
async fn an_unreachable_guard_store_has_no_method_either() {
    let h = harness_with(Options {
        separate_guard_store: true,
        ..Options::default()
    });
    h.add_local_user("alice", PASSWORD).await;
    h.guard_store.set_unavailable(true);
    let failure = h.refusal(&basic("alice", PASSWORD)).await;
    assert_eq!(failure.method, None);
    assert!(matches!(failure.error, AuthError::GuardUnavailable));
}

#[tokio::test]
async fn the_plain_and_the_detailed_entry_make_the_same_decision() {
    let h = harness();
    h.add_local_user("alice", PASSWORD).await;
    let context = AuthContext::new();
    for (user, password) in [("alice", PASSWORD), ("alice", "wrong horse")] {
        let credentials = basic(user, password);
        let plain = h.service.authenticate(&credentials, &context).await;
        let detailed = h
            .service
            .authenticate_detailed(&credentials, &context)
            .await;
        match (plain, detailed) {
            (Ok(Some(a)), Ok(Some(b))) => assert_eq!(a.user_id, b.user_id),
            (Err(a), Err(b)) => assert_eq!(a.to_string(), b.error.to_string()),
            (a, b) => unreachable!("different decisions: {a:?} and {b:?}"),
        }
    }
}

#[tokio::test]
async fn auth_attempts_are_counted_with_the_method_and_the_result() {
    let capture = Capture::default();
    let _guard = capture.install();
    let h = harness_with(Options {
        gate: Some(one_lane()),
        ..Options::default()
    });
    h.add_local_user("alice", PASSWORD).await;
    h.add_ldap_user("bob", "ldap secret");

    h.authenticate(&Credentials::Anonymous).await.unwrap();
    h.basic("alice", PASSWORD).await.unwrap();
    h.basic("alice", PASSWORD).await.unwrap();
    h.basic("alice", "wrong horse").await.unwrap_err();
    h.basic("bob", "not the secret").await.unwrap_err();
    h.bearer(&oidc_token("carol", "sub-c", &["/unmapped"], h.exp_in(300)))
        .await
        .unwrap_err();
    h.bearer("opaque-token").await.unwrap_err();
    let held = h.gate.acquire(GateClass::Password).await.unwrap();
    h.clock.advance(Duration::from_mins(2));
    h.basic("alice", PASSWORD).await.unwrap_err();
    drop(held);

    let series = |method: &str, result: &str| {
        capture.counter(&format!(
            r#"auth_attempts_total{{method="{method}",result="{result}"}}"#
        ))
    };
    assert_eq!(series("local", "ok"), 2, "a cache hit is an attempt too");
    assert_eq!(series("local", "denied"), 1);
    assert_eq!(series("ldap", "denied"), 1);
    assert_eq!(series("oidc", "denied"), 1, "access denied is the caller's");
    assert_eq!(series("unknown", "denied"), 1, "nobody claimed the token");
    assert_eq!(
        series("local", "error"),
        1,
        "no permit for the hash is ours, not the caller's"
    );
    assert!(
        capture
            .counter_names()
            .iter()
            .all(|name| !name.contains("anonymous")),
        "an anonymous request is not an attempt: {:?}",
        capture.counter_names()
    );
}

#[tokio::test]
async fn the_attempt_counter_can_be_left_to_the_http_layer() {
    let capture = Capture::default();
    let _guard = capture.install();
    let h = harness_with(Options {
        service: AuthServiceConfig {
            record_attempt_metrics: false,
            ..AuthServiceConfig::default()
        },
        ..Options::default()
    });
    h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    h.basic("alice", "wrong horse").await.unwrap_err();
    assert!(
        capture
            .counter_names()
            .iter()
            .all(|name| !name.starts_with("auth_attempts_total")),
        "{:?}",
        capture.counter_names()
    );
}

fn breakglass() -> Options {
    Options {
        gate: Some(one_lane()),
        service: AuthServiceConfig {
            breakglass_username: Some("Admin".to_owned()),
            ..AuthServiceConfig::default()
        },
        ..Options::default()
    }
}

#[tokio::test]
async fn a_password_login_of_the_breakglass_account_is_counted_once_per_verification() {
    let capture = Capture::default();
    let _guard = capture.install();
    let h = harness_with(breakglass());
    h.add_local_user("admin", PASSWORD).await;
    h.add_local_user("alice", PASSWORD).await;

    h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(capture.counter("auth_breakglass_logins_total"), 0);
    h.basic("admin", "wrong horse").await.unwrap_err();
    assert_eq!(
        capture.counter("auth_breakglass_logins_total"),
        0,
        "a failed attempt is not a login"
    );
    h.basic("admin", PASSWORD).await.unwrap();
    assert_eq!(capture.counter("auth_breakglass_logins_total"), 1);
    // Served from the verification cache: the same login, not a new one.
    h.basic("admin", PASSWORD).await.unwrap();
    assert_eq!(capture.counter("auth_breakglass_logins_total"), 1);
    // After the cache entry ended it is a new verification, so a new login.
    h.clock.advance(Duration::from_mins(2));
    h.basic("admin", PASSWORD).await.unwrap();
    assert_eq!(capture.counter("auth_breakglass_logins_total"), 2);
}

#[tokio::test]
async fn an_api_token_of_the_breakglass_account_is_not_a_password_login() {
    let capture = Capture::default();
    let _guard = capture.install();
    let h = harness_with(breakglass());
    let admin = h.add_local_user("admin", PASSWORD).await;
    let issued = create_token(&h.store, &h.clock, admin.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    h.authenticate(&Credentials::Bearer(issued.secret))
        .await
        .unwrap();
    assert_eq!(capture.counter("auth_breakglass_logins_total"), 0);
}

#[tokio::test]
async fn nothing_is_counted_as_breakglass_without_a_configured_account() {
    let capture = Capture::default();
    let _guard = capture.install();
    let h = harness();
    h.add_local_user("admin", PASSWORD).await;
    h.basic("admin", PASSWORD).await.unwrap();
    assert_eq!(capture.counter("auth_breakglass_logins_total"), 0);
    let blank = harness_with(Options {
        service: AuthServiceConfig {
            breakglass_username: Some("   ".to_owned()),
            ..AuthServiceConfig::default()
        },
        ..Options::default()
    });
    blank.add_local_user("admin", PASSWORD).await;
    blank.basic("admin", PASSWORD).await.unwrap();
    assert_eq!(
        capture.counter("auth_breakglass_logins_total"),
        0,
        "a blank name matches nobody"
    );
}

#[tokio::test]
async fn must_not_start_a_hash_after_the_request_deadline() {
    // No directory: an unknown account is claimed by nobody and pays the dummy verification.
    let h = harness_with(Options {
        with_ldap: false,
        with_oidc: false,
        ..Options::default()
    });
    h.add_local_user("alice", PASSWORD).await;
    // A deadline of "now" has passed by the time anything looks at it.
    let expired = AuthContext::new().with_deadline(Instant::now());

    // The account exists: the verification would run, but the request is out of time.
    let error = h
        .service
        .authenticate(&basic("alice", PASSWORD), &expired)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::Overloaded), "{error:?}");
    // No account: the dummy verification is refused the same way, so this stays unobservable.
    let error = h
        .service
        .authenticate(&basic("nobody", PASSWORD), &expired)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::Overloaded), "{error:?}");

    let alive = AuthContext::new().with_deadline(Instant::now() + Duration::from_secs(30));
    h.service
        .authenticate(&basic("alice", PASSWORD), &alive)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_oidc_token_does_not_need_a_permit_so_the_deadline_does_not_touch_it() {
    let h = harness();
    let expired = AuthContext::new().with_deadline(Instant::now());
    let token = oidc_token("root", "sub-root", &[ADMIN_PATH], h.exp_in(300));
    let credentials = Credentials::Bearer(Secret::new(token));
    h.service
        .authenticate(&credentials, &expired)
        .await
        .unwrap();
}
