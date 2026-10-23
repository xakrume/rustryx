//! Chain tests for Bearer credentials: internal JWT, API tokens, OIDC access tokens,
//! the principal cache and its invalidation.

use std::time::Duration;

use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthMethod, Credentials, IdpProvider, RoleOrigin, UserSource};
use rustryx_core::ports::{InvalidationBus, InvalidationEvent, UserRepo};
use rustryx_testkit::{MemInvalidationBus, fixtures};
use tokio_util::sync::CancellationToken;

use super::testing::{ADMIN_PATH, Options, harness, harness_with, names, oidc_token};
use crate::groups::{ProviderPolicy, RoleMapperConfig};
use crate::internal_jwt::GroupSnapshot;
use crate::tokens::{TokenSpec, create_token};

#[tokio::test]
async fn internal_jwt_of_a_local_user_resolves_manual_roles_and_is_cached() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let role = h.add_role("team-dev").await;
    h.grant_manual(&alice, &role).await;
    let token = h.issue_jwt(&alice, Duration::from_mins(10));
    let principal = h.bearer(&token).await.unwrap();
    assert_eq!(principal.auth_method, AuthMethod::InternalJwt);
    assert_eq!(names(&principal), ["team-dev"]);
    assert_eq!(h.service.principals.len().await, 1);
}

#[tokio::test]
async fn cached_principal_hides_state_changes_until_invalidated() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let issued = h
        .jwt
        .issue(&crate::internal_jwt::IssueRequest {
            user_id: alice.id,
            ttl: Duration::from_hours(1),
            groups: None,
        })
        .unwrap();
    let token = issued.token.expose().to_owned();
    h.bearer(&token).await.unwrap();
    h.jwt.revoke(&issued.tid, issued.expires_at).await.unwrap();
    h.bearer(&token)
        .await
        .expect("the cached principal is served until invalidated");
    h.service.invalidate_user(alice.id);
    let error = h.bearer(&token).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenRevoked), "{error:?}");
}

#[tokio::test]
async fn must_not_keep_a_principal_longer_than_five_minutes() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let issued = h
        .jwt
        .issue(&crate::internal_jwt::IssueRequest {
            user_id: alice.id,
            ttl: Duration::from_hours(8),
            groups: None,
        })
        .unwrap();
    let token = issued.token.expose().to_owned();
    h.bearer(&token).await.unwrap();
    h.jwt.revoke(&issued.tid, issued.expires_at).await.unwrap();
    h.clock.advance(Duration::from_secs(299));
    h.bearer(&token)
        .await
        .expect("still inside the cache window");
    h.clock.advance(Duration::from_secs(1));
    let error = h.bearer(&token).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenRevoked), "{error:?}");
}

#[tokio::test]
async fn must_not_serve_a_cached_principal_past_the_token_expiry() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let token = h.issue_jwt(&alice, Duration::from_mins(2));
    h.bearer(&token).await.unwrap();
    h.clock.advance(Duration::from_secs(120));
    let error = h.bearer(&token).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenExpired), "{error:?}");
}

#[tokio::test]
async fn api_tokens_are_never_cached_so_revocation_is_immediate() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let issued = create_token(&h.store, &h.clock, alice.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    let creds = Credentials::Bearer(issued.secret.clone());
    let principal = h.authenticate(&creds).await.unwrap();
    assert_eq!(principal.auth_method, AuthMethod::ApiToken);
    assert_eq!(h.service.principals.len().await, 0);
    rustryx_core::ports::TokenRepo::revoke(&h.store, issued.record.id, h.clock_now())
        .await
        .unwrap();
    let error = h.authenticate(&creds).await.unwrap_err();
    assert!(matches!(error, AuthError::TokenRevoked), "{error:?}");
}

#[tokio::test]
async fn api_token_keeps_its_scopes_and_the_owners_manual_roles() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let role = h.add_role("team-dev").await;
    h.grant_manual(&alice, &role).await;
    let issued = create_token(
        &h.store,
        &h.clock,
        alice.id,
        TokenSpec {
            scopes: vec!["read".into()],
            ..TokenSpec::new("scoped")
        },
    )
    .await
    .unwrap();
    let principal = h
        .authenticate(&Credentials::Bearer(issued.secret))
        .await
        .unwrap();
    assert_eq!(principal.token_scopes, Some(vec!["read".to_owned()]));
    assert_eq!(names(&principal), ["team-dev"]);
}

#[tokio::test]
async fn nobody_claims_an_unknown_bearer_token() {
    let h = harness();
    for token in ["opaque", "a.b.c", "ryx_short"] {
        let error = h.bearer(token).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidToken(_)),
            "{token}: {error:?}"
        );
    }
    let no_oidc = harness_with(Options {
        with_oidc: false,
        ..Options::default()
    });
    let token = oidc_token("carol", "sub-c", &[ADMIN_PATH], no_oidc.exp_in(300));
    let error = no_oidc.bearer(&token).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidToken("unsupported bearer token")),
        "{error:?}"
    );
}

#[tokio::test]
async fn oidc_bearer_provisions_the_user_maps_groups_and_is_cached() {
    let h = harness();
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Oidc, "/rustryx/readers", &readers)
        .await;
    let token = oidc_token(
        "carol",
        "sub-c",
        &["/rustryx/readers", "/other"],
        h.exp_in(300),
    );
    let first = h.bearer(&token).await.unwrap();
    assert_eq!(first.auth_method, AuthMethod::OidcBearer);
    assert_eq!(first.source, UserSource::Oidc);
    assert_eq!(names(&first), ["readers"]);
    assert_eq!(first.external_groups, ["/rustryx/readers"]);
    let stored = UserRepo::find_external(
        &h.store,
        UserSource::Oidc,
        Some(fixtures::OIDC_ISSUER),
        Some("sub-c"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first.user_id, Some(stored.id));

    let second = h.bearer(&token).await.unwrap();
    assert_eq!(second.user_id, first.user_id);
    assert_eq!(
        h.oidc_auth.calls(),
        1,
        "the second request is served from the cache"
    );
    assert_eq!(h.oidc_resolver.calls(), 1);
}

#[tokio::test]
async fn oidc_cache_entry_ends_with_the_access_token() {
    let h = harness();
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Oidc, "/rustryx/readers", &readers)
        .await;
    let token = oidc_token("carol", "sub-c", &["/rustryx/readers"], h.exp_in(60));
    h.bearer(&token).await.unwrap();
    h.clock.advance(Duration::from_secs(59));
    h.bearer(&token).await.unwrap();
    assert_eq!(h.oidc_auth.calls(), 1);
    h.clock.advance(Duration::from_secs(1));
    h.bearer(&token).await.unwrap();
    assert_eq!(
        h.oidc_auth.calls(),
        2,
        "past `exp` the authenticator decides again"
    );
}

#[tokio::test]
async fn oidc_errors_are_final_and_never_cached() {
    let h = harness();
    let mut parts: Vec<String> = oidc_token("carol", "sub-c", &[], h.exp_in(300))
        .split('.')
        .map(str::to_owned)
        .collect();
    // A token for another issuer: the scripted authenticator refuses it.
    parts[1] = {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"iss":"https://evil.example.test","sub":"s","exp":9999999999}"#)
    };
    let token = parts.join(".");
    for _ in 0..2 {
        let error = h.bearer(&token).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidToken("wrong issuer")),
            "{error:?}"
        );
    }
    assert_eq!(h.oidc_auth.calls(), 2);
    assert_eq!(h.service.principals.len().await, 0);
}

#[tokio::test]
async fn must_not_link_an_oidc_login_to_a_local_account_with_the_same_name() {
    let h = harness();
    let root = h.add_local_user("root", "correct horse").await;
    let token = oidc_token("root", "sub-attacker", &[ADMIN_PATH], h.exp_in(300));
    let error = h.bearer(&token).await.unwrap_err();
    assert!(matches!(error, AuthError::UsernameConflict), "{error:?}");
    let untouched = UserRepo::get(&h.store, root.id).await.unwrap().unwrap();
    assert_eq!(untouched.source, UserSource::Local);
    assert_eq!(
        UserRepo::roles_of(&h.store, root.id).await.unwrap().len(),
        0
    );
}

#[tokio::test]
async fn invalidation_events_drop_cached_principals() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let bob = h.add_local_user("bob", "correct horse").await;
    let alice_token = h.issue_jwt(&alice, Duration::from_mins(10));
    let bob_token = h.issue_jwt(&bob, Duration::from_mins(10));
    h.bearer(&alice_token).await.unwrap();
    h.bearer(&bob_token).await.unwrap();
    assert_eq!(h.service.principals.len().await, 2);

    let bus = MemInvalidationBus::new();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(h.service.invalidation_task(&bus, shutdown.clone()));

    bus.publish(InvalidationEvent::User(alice.id.as_uuid()))
        .await
        .unwrap();
    wait_for_len(&h, 1).await;
    bus.publish(InvalidationEvent::Rbac).await.unwrap();
    wait_for_len(&h, 0).await;

    h.bearer(&bob_token).await.unwrap();
    bus.publish(InvalidationEvent::Reset).await.unwrap();
    wait_for_len(&h, 0).await;

    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn invalidation_task_ends_when_the_bus_goes_away() {
    let h = harness();
    let bus = MemInvalidationBus::with_capacity(4);
    let task = tokio::spawn(h.service.invalidation_task(&bus, CancellationToken::new()));
    drop(bus);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

async fn wait_for_len(h: &super::testing::Harness, expected: u64) {
    for _ in 0..200 {
        if h.service.principals.len().await == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    unreachable!("cache never reached {expected} entries");
}

#[tokio::test]
async fn internal_jwt_of_an_oidc_user_uses_the_snapshot_inside_the_token() {
    let h = harness();
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Oidc, "/rustryx/readers", &readers)
        .await;
    let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    let token = h
        .jwt
        .issue(&crate::internal_jwt::IssueRequest {
            user_id: carol.id,
            ttl: Duration::from_mins(30),
            groups: Some(GroupSnapshot {
                groups: vec!["/rustryx/readers".into(), ADMIN_PATH.into()],
                confirmed_at: h.clock_now(),
            }),
        })
        .unwrap();
    let principal = h.bearer(token.token.expose()).await.unwrap();
    assert_eq!(principal.auth_method, AuthMethod::InternalJwt);
    assert_eq!(names(&principal), ["nx-admin", "readers"]);
    assert_eq!(
        h.oidc_resolver.calls(),
        0,
        "the verified token already carries the groups"
    );
}

#[tokio::test]
async fn must_not_honour_a_group_snapshot_older_than_five_minutes() {
    let h = harness();
    let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    let token = h
        .jwt
        .issue(&crate::internal_jwt::IssueRequest {
            user_id: carol.id,
            ttl: Duration::from_hours(1),
            groups: Some(GroupSnapshot {
                groups: vec![ADMIN_PATH.into()],
                confirmed_at: h.clock_now(),
            }),
        })
        .unwrap();
    let token = token.token.expose().to_owned();
    h.clock.advance(Duration::from_secs(299));
    h.bearer(&token).await.unwrap();
    h.clock.advance(Duration::from_secs(2));
    let error = h.bearer(&token).await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_cache_a_principal_beyond_its_group_confirmation() {
    let h = harness();
    let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    let token = h
        .jwt
        .issue(&crate::internal_jwt::IssueRequest {
            user_id: carol.id,
            ttl: Duration::from_hours(1),
            groups: Some(GroupSnapshot {
                groups: vec![ADMIN_PATH.into()],
                confirmed_at: h.clock_now() - time::Duration::seconds(200),
            }),
        })
        .unwrap();
    let token = token.token.expose().to_owned();
    h.bearer(&token).await.unwrap();
    h.clock.advance(Duration::from_secs(101));
    let error = h.bearer(&token).await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "the entry must die with the confirmation (200 s old at login, 5 min cap): {error:?}"
    );
}

#[tokio::test]
async fn must_not_grant_roles_to_an_oidc_session_without_a_group_snapshot() {
    let h = harness();
    let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    // A row that would grant admin if rows were trusted.
    let admin = rustryx_testkit::builtin_role_id("nx-admin").unwrap();
    UserRepo::replace_roles_for_origin(&h.store, carol.id, RoleOrigin::Oidc, &[admin])
        .await
        .unwrap();
    let token = h.issue_jwt(&carol, Duration::from_mins(10));
    let error = h.bearer(&token).await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn api_token_of_an_oidc_owner_carries_no_external_roles() {
    let h = harness();
    let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    let admin = rustryx_testkit::builtin_role_id("nx-admin").unwrap();
    UserRepo::replace_roles_for_origin(&h.store, carol.id, RoleOrigin::Oidc, &[admin])
        .await
        .unwrap();
    let issued = create_token(&h.store, &h.clock, carol.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    let principal = h
        .authenticate(&Credentials::Bearer(issued.secret))
        .await
        .unwrap();
    assert_eq!(
        principal.roles.len(),
        0,
        "persisted oidc rows are not a grant"
    );
    assert_eq!(principal.source, UserSource::Oidc);
    assert!(principal.groups_confirmed_at.is_none());
}

#[tokio::test]
async fn api_token_of_an_oidc_owner_gets_manual_roles_only_with_supplement_and_never_admin() {
    let supplement = RoleMapperConfig {
        allow_local_role_supplement: true,
        oidc: ProviderPolicy {
            admin_groups: vec![ADMIN_PATH.into()],
            ..ProviderPolicy::default()
        },
        ..RoleMapperConfig::default()
    };
    let h = harness_with(Options {
        mapper: supplement,
        ..Options::default()
    });
    let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    let extra = h.add_role("extra").await;
    let admin = rustryx_testkit::builtin_role_id("nx-admin").unwrap();
    UserRepo::replace_roles_for_origin(&h.store, carol.id, RoleOrigin::Manual, &[extra.id, admin])
        .await
        .unwrap();
    let issued = create_token(&h.store, &h.clock, carol.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    let principal = h
        .authenticate(&Credentials::Bearer(issued.secret))
        .await
        .unwrap();
    assert_eq!(names(&principal), ["extra"]);
}

#[tokio::test]
async fn api_token_of_an_ldap_owner_asks_the_directory() {
    let h = harness();
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Ldap, "cn=readers,ou=g", &readers)
        .await;
    h.ldap_resolver.set(super::testing::Answer::Groups(vec![
        "cn=readers,ou=g".into(),
    ]));
    let bob = UserRepo::create(&h.store, &fixtures::user("bob", UserSource::Ldap))
        .await
        .unwrap();
    let issued = create_token(&h.store, &h.clock, bob.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    let principal = h
        .authenticate(&Credentials::Bearer(issued.secret.clone()))
        .await
        .unwrap();
    assert_eq!(names(&principal), ["readers"]);
    assert_eq!(h.ldap_resolver.calls(), 1);

    h.ldap_resolver.set(super::testing::Answer::Fail);
    h.clock.advance(Duration::from_secs(301));
    let error = h
        .authenticate(&Credentials::Bearer(issued.secret))
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn debug_output_of_the_service_reveals_no_secrets() {
    let h = harness();
    let rendered = format!("{:?}", h.service);
    assert!(rendered.contains("AuthService"));
    let secret = Secret::new("hunter2");
    assert!(!format!("{secret:?}").contains("hunter2"));
}
