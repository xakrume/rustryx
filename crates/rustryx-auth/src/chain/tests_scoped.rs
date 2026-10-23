//! Robot and scan principals, the profile fields of a principal, credential expiry as the
//! cache bound, and the invalidation events that change nothing.

use std::time::Duration;

use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthMethod, Credentials, Principal, UserSource};
use rustryx_core::ports::{InvalidationBus, InvalidationEvent, UserRepo};
use rustryx_testkit::MemInvalidationBus;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::testing::{
    ADMIN_PATH, Handler, Options, harness, harness_with, oidc_token, oidc_token_from,
};
use crate::telemetry::recording::Capture;
use crate::tokens::{TokenSpec, create_token};

const PASSWORD: &str = "correct horse";

/// A member that answers one fixed token with a principal no real authenticator produces yet.
fn issuing(principal: fn() -> Principal) -> Handler {
    Box::new(move |credentials| match credentials {
        Credentials::Bearer(token) if token.expose() == "scripted" => Ok(Some(principal())),
        _ => Ok(None),
    })
}

fn robot() -> Principal {
    let mut principal = Principal::new("robot+ci", UserSource::Local, AuthMethod::RobotToken);
    principal.token_scopes = Some(vec!["repo:libs:read".to_owned()]);
    principal
}

fn scan() -> Principal {
    let mut principal = Principal::new("scanner", UserSource::Local, AuthMethod::ScanToken);
    principal.token_scopes = Some(vec!["repo:libs:read".to_owned()]);
    principal
}

fn robot_without_scopes() -> Principal {
    Principal::new("robot+ci", UserSource::Local, AuthMethod::RobotToken)
}

fn anonymous_member() -> Principal {
    Principal::new("anonymous", UserSource::Local, AuthMethod::Anonymous)
}

fn with_member(principal: fn() -> Principal) -> super::testing::Harness {
    harness_with(Options {
        oidc_handler: Some(issuing(principal)),
        ..Options::default()
    })
}

async fn user_count(h: &super::testing::Harness) -> usize {
    UserRepo::list(&h.store, &rustryx_core::ports::PageRequest::default())
        .await
        .unwrap()
        .items
        .len()
}

#[tokio::test]
async fn robot_and_scan_principals_keep_their_scopes_and_get_neither_roles_nor_an_account() {
    for (principal, method) in [
        (robot as fn() -> Principal, AuthMethod::RobotToken),
        (scan, AuthMethod::ScanToken),
    ] {
        let h = with_member(principal);
        let resolved = h.bearer("scripted").await.unwrap();
        assert_eq!(resolved.auth_method, method);
        assert_eq!(
            resolved.token_scopes,
            Some(vec!["repo:libs:read".to_owned()])
        );
        assert!(resolved.roles.is_empty() && resolved.external_groups.is_empty());
        assert!(resolved.user_id.is_none(), "no account is created for it");
        assert_eq!(user_count(&h).await, 0);
    }
}

#[tokio::test]
async fn must_not_pass_a_robot_or_scan_principal_without_scopes() {
    // `token_scopes: None` reads as an unrestricted personal token to the authorizer.
    let h = with_member(robot_without_scopes);
    let error = h.bearer("scripted").await.unwrap_err();
    assert!(
        matches!(
            error,
            AuthError::Backend {
                context: "role resolution",
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_resolve_roles_for_a_method_that_is_not_an_authentication() {
    // A member that hands the chain an "anonymous" principal is a bug, and the answer to a
    // bug is a refusal, never an account without roles that reads as a valid identity.
    let h = with_member(anonymous_member);
    let error = h.bearer("scripted").await.unwrap_err();
    assert!(
        matches!(
            error,
            AuthError::Backend {
                context: "role resolution",
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn robot_and_scan_attempts_have_their_own_method_label() {
    let capture = Capture::default();
    let _guard = capture.install();
    for principal in [robot as fn() -> Principal, scan] {
        let h = with_member(principal);
        h.bearer("scripted").await.unwrap();
    }
    assert_eq!(
        capture.counter(r#"auth_attempts_total{method="robot",result="ok"}"#),
        1
    );
    assert_eq!(
        capture.counter(r#"auth_attempts_total{method="scan",result="ok"}"#),
        1
    );
}

#[tokio::test]
async fn an_unknown_ryx_token_is_refused_as_unknown_whatever_kind_it_would_have_been() {
    // Robot and scan tokens share the `ryx_` shape; until their authenticators exist a token
    // no table knows is refused the same way as any unknown personal token.
    let h = harness();
    let unknown = crate::tokens::generate_token_secret();
    for credentials in [
        Credentials::Bearer(unknown.clone()),
        super::testing::basic("robot+ci", unknown.expose()),
    ] {
        let error = h.authenticate(&credentials).await.unwrap_err();
        assert!(
            matches!(
                error,
                AuthError::InvalidToken("unknown API token") | AuthError::InvalidCredentials
            ),
            "{error:?}"
        );
    }
    assert_eq!(user_count(&h).await, 0);
}

#[tokio::test]
async fn principals_carry_the_profile_and_the_expiry_of_their_credential() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;

    let by_password = h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(by_password.email.as_deref(), Some("alice@example.test"));
    assert_eq!(by_password.display_name.as_deref(), Some("alice"));
    assert!(by_password.expires_at.is_none() && by_password.external_id.is_none());

    let jwt = h
        .jwt
        .issue(&crate::internal_jwt::IssueRequest {
            user_id: alice.id,
            ttl: Duration::from_mins(10),
            groups: None,
        })
        .unwrap();
    let by_session = h.bearer(jwt.token.expose()).await.unwrap();
    assert_eq!(by_session.email.as_deref(), Some("alice@example.test"));
    assert_eq!(by_session.expires_at, Some(jwt.expires_at));

    let issued = create_token(&h.store, &h.clock, alice.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    let by_token = h
        .authenticate(&Credentials::Bearer(issued.secret))
        .await
        .unwrap();
    assert_eq!(by_token.expires_at, issued.record.expires_at);
    assert_eq!(by_token.display_name.as_deref(), Some("alice"));
}

#[tokio::test]
async fn a_new_oidc_account_stores_the_email_and_name_of_the_token() {
    let h = harness();
    let token = oidc_token_from(&json!({
        "iss": super::testing::OIDC_ISSUER,
        "sub": "sub-root",
        "preferred_username": "root-sso",
        "email": "root@example.test",
        "name": "Root Administrator",
        "groups": [ADMIN_PATH],
        "exp": h.exp_in(300),
    }));
    let principal = h.bearer(&token).await.unwrap();
    assert_eq!(principal.email.as_deref(), Some("root@example.test"));
    assert_eq!(
        principal.display_name.as_deref(),
        Some("Root Administrator")
    );
    assert_eq!(
        principal
            .expires_at
            .map(time::OffsetDateTime::unix_timestamp),
        Some(h.exp_in(300))
    );

    let stored = UserRepo::get(&h.store, principal.user_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.email.as_deref(), Some("root@example.test"));
    assert_eq!(stored.display_name.as_deref(), Some("Root Administrator"));
}

#[tokio::test]
async fn must_not_use_the_email_of_a_token_to_find_or_link_an_account() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    let token = oidc_token_from(&json!({
        "iss": super::testing::OIDC_ISSUER,
        "sub": "sub-mallory",
        "preferred_username": "mallory",
        "email": "alice@example.test",
        "groups": [ADMIN_PATH],
        "exp": h.exp_in(300),
    }));
    let principal = h.bearer(&token).await.unwrap();
    assert_ne!(
        principal.user_id,
        Some(alice.id),
        "the e-mail links nothing"
    );
    assert_eq!(principal.username, "mallory");
}

#[tokio::test]
async fn must_not_cache_a_principal_whose_credential_carries_no_expiry() {
    let h = harness();
    let token = oidc_token_from(&json!({
        "iss": super::testing::OIDC_ISSUER,
        "sub": "sub-root",
        "preferred_username": "root-sso",
        "groups": [ADMIN_PATH],
    }));
    let first = h.bearer(&token).await.unwrap();
    assert!(first.expires_at.is_none());
    h.bearer(&token).await.unwrap();
    assert_eq!(h.service.principals.len().await, 0);
    assert_eq!(h.oidc_auth.calls(), 2, "every request verified the token");
}

#[tokio::test]
async fn the_cache_bound_is_the_expiry_the_principal_carries() {
    let h = harness();
    let token = oidc_token("root-sso", "sub-root", &[ADMIN_PATH], h.exp_in(20));
    h.bearer(&token).await.unwrap();
    h.bearer(&token).await.unwrap();
    assert_eq!(h.oidc_auth.calls(), 1);
    h.clock.advance(Duration::from_secs(20));
    // The scripted member does not check expiry (the real authenticator does); what matters
    // here is that the chain asked it again instead of serving the entry.
    h.bearer(&token).await.unwrap();
    assert_eq!(
        h.oidc_auth.calls(),
        2,
        "past `exp` the token is verified again"
    );
}

#[tokio::test]
async fn events_that_change_no_principal_leave_the_caches_alone() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    let bob = h.add_local_user("bob", PASSWORD).await;
    let alice_token = h.issue_jwt(&alice, Duration::from_mins(10));
    let bob_token = h.issue_jwt(&bob, Duration::from_mins(10));
    h.bearer(&alice_token).await.unwrap();
    h.bearer(&bob_token).await.unwrap();
    h.basic("bob", PASSWORD).await.unwrap();
    assert_eq!(h.service.principals.len().await, 2);
    assert_eq!(h.service.basic.len().await, 1);

    let bus = MemInvalidationBus::new();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(h.service.invalidation_task(&bus, shutdown.clone()));
    bus.publish(InvalidationEvent::Projects).await.unwrap();
    bus.publish(InvalidationEvent::Config(7)).await.unwrap();
    // Events are applied in order: once this one has taken effect, the two above have been seen.
    bus.publish(InvalidationEvent::User(alice.id.as_uuid()))
        .await
        .unwrap();
    for _ in 0..200 {
        if h.service.principals.len().await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        h.service.principals.len().await,
        1,
        "only alice was dropped"
    );
    assert_eq!(
        h.service.basic.len().await,
        1,
        "bob's password stays remembered"
    );

    bus.publish(InvalidationEvent::Rbac).await.unwrap();
    for _ in 0..200 {
        if h.service.basic.len().await == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        h.service.basic.len().await,
        0,
        "an RBAC change drops it too"
    );
    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn a_cached_password_of_a_user_is_dropped_when_that_user_changes() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(h.service.basic.len().await, 1);
    let bus = MemInvalidationBus::new();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(h.service.invalidation_task(&bus, shutdown.clone()));
    bus.publish(InvalidationEvent::User(alice.id.as_uuid()))
        .await
        .unwrap();
    for _ in 0..200 {
        if h.service.basic.len().await == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(h.service.basic.len().await, 0);
    shutdown.cancel();
    task.await.unwrap();
}
