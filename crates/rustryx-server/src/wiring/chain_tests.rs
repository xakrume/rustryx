//! The identity wiring against in-memory ports: the real chain, token service and issuer, with
//! nothing mocked between them. What the PostgreSQL run in `tests/postgres_serve.rs` proves for
//! one login, this proves for the rules around it, quickly and without Docker.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rustryx_api::{SessionIssuer, SessionRequest};
use rustryx_config::AppConfig;
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthContext, Credentials, UserSource};
use rustryx_core::ports::{Authenticator, InvalidationBus, InvalidationEvent, UserRepo};
use rustryx_testkit::{MemInvalidationBus, MemStore, fixtures};

use super::identity::{self, Identity};
use super::sso::JwtSessionIssuer;
use super::store::Repos;

const PASSWORD: &str = "a-long-enough-password-1";

struct Fixture {
    identity: Identity,
    issuer: JwtSessionIssuer,
    bus: MemInvalidationBus,
    user: rustryx_core::identity::User,
}

async fn fixture() -> Fixture {
    let store = MemStore::with_builtin_roles();
    let repos = Repos::from_mem(&store);
    let config = AppConfig::default();
    let identity = identity::build(&config, &repos, &identity::cookie_key(&config))
        .await
        .expect("the chain assembles over in-memory ports");
    let user = UserRepo::create(&store, &fixtures::user("alice", UserSource::Local))
        .await
        .expect("user");
    let hash = identity
        .passwords
        .hash(&Secret::new(PASSWORD))
        .await
        .expect("hash");
    UserRepo::set_password_hash(&store, user.id, hash.expose())
        .await
        .expect("password");
    let bus = MemInvalidationBus::new();
    let issuer = JwtSessionIssuer::new(
        Arc::clone(&identity.jwt),
        Arc::clone(&identity.service),
        Arc::new(bus.clone()),
    );
    Fixture {
        identity,
        issuer,
        bus,
        user,
    }
}

fn basic(password: &str) -> Credentials {
    Credentials::Basic {
        username: "alice".into(),
        password: Secret::new(password),
    }
}

#[tokio::test]
async fn a_local_account_signs_in_through_the_assembled_chain() {
    let f = fixture().await;
    let principal = f
        .identity
        .service
        .authenticate(&basic(PASSWORD), &AuthContext::new())
        .await
        .expect("no error")
        .expect("a principal");
    assert_eq!(principal.username, "alice");
}

#[tokio::test]
async fn must_not_sign_in_with_a_wrong_password() {
    let f = fixture().await;
    let outcome = f
        .identity
        .service
        .authenticate(&basic("not-the-password-123"), &AuthContext::new())
        .await;
    assert!(
        matches!(outcome, Err(AuthError::InvalidCredentials)),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_session_token_authenticates_until_the_issuer_revokes_it() {
    let f = fixture().await;
    let issued = f
        .issuer
        .issue(&SessionRequest::new(f.user.id, Duration::from_secs(3600)))
        .expect("a token");
    let bearer = Credentials::Bearer(issued.token.clone());
    for _ in 0..2 {
        let principal = f
            .identity
            .service
            .authenticate(&bearer, &AuthContext::new())
            .await
            .expect("no error")
            .expect("a principal");
        assert_eq!(principal.username, "alice");
    }
    f.issuer
        .revoke(f.user.id, &issued.tid, issued.expires_at)
        .await
        .expect("revoked");
    let after = f
        .identity
        .service
        .authenticate(&bearer, &AuthContext::new())
        .await;
    assert!(
        matches!(after, Err(AuthError::TokenRevoked)),
        "must not authenticate a revoked session from the principal cache: {after:?}"
    );
}

#[tokio::test]
async fn a_logout_is_announced_to_the_other_nodes_for_that_user() {
    let f = fixture().await;
    let mut events = f.bus.subscribe();
    let issued = f
        .issuer
        .issue(&SessionRequest::new(f.user.id, Duration::from_secs(3600)))
        .expect("a token");
    f.issuer
        .revoke(f.user.id, &issued.tid, issued.expires_at)
        .await
        .expect("revoked");
    let event = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("an event")
        .expect("open stream");
    assert_eq!(event, InvalidationEvent::User(f.user.id.as_uuid()));
}

#[tokio::test]
async fn a_session_longer_than_eight_hours_is_not_signed() {
    let f = fixture().await;
    let refused = f
        .issuer
        .issue(&SessionRequest::new(f.user.id, Duration::from_hours(9)));
    assert!(
        refused.is_err(),
        "the token service enforces the absolute limit itself"
    );
}
