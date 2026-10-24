//! The revocation of replaced session tokens: the grace, the drain, the family link.
//!
//! The grace is measured with tokio's clock, so these run with paused time and move it by hand.
//! The session clock (`FixedClock`) is a separate thing and moves only where a test needs a
//! reissue to be due.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use http::{Method, StatusCode};
use rustryx_core::AuthError;
use rustryx_core::identity::{AuthContext, Credentials};
use rustryx_core::ports::Authenticator;
use tokio::time::Instant;

use super::tests::{Rig, key, read, rig, rig_with};
use super::*;
use crate::session_support::{Browser, EnvBuilder, FakeAuth, advance};
use crate::test_support::{call, request};
use crate::{CSRF_COOKIE, SESSION_COOKIE};

const MINUTE: Duration = Duration::from_mins(1);

/// Lets spawned tasks run to their next await point.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Moves tokio's clock and lets the revocation tasks react.
async fn pass(by: Duration) {
    tokio::time::advance(by).await;
    settle().await;
}

async fn accepted(auth: &FakeAuth, start: &SessionStart) -> bool {
    auth.authenticate(
        &Credentials::Bearer(start.token.clone()),
        &AuthContext::new(),
    )
    .await
    .is_ok_and(|principal| principal.is_some())
}

/// A session that is 20 minutes old, so that a reissue is due, with its first token.
fn aged(rig: &Rig) -> (SessionStart, CookieSession) {
    let first = rig.manager.start(rig.user, None, None).expect("start");
    let session = read(rig, &first);
    rig.clock.advance(MINUTE * 20);
    (first, session)
}

// ---- the grace --------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_old_token_works_inside_the_grace_and_is_refused_after_it() {
    let rig = rig();
    let (first, session) = aged(&rig);
    let second = rig.manager.renew(&session, false).await.expect("renew");

    assert!(
        accepted(&rig.auth, &first).await,
        "in flight requests still carry it"
    );
    assert_eq!(rig.manager.pending_revocations(), 1);
    pass(Duration::from_secs(29)).await;
    assert!(
        !rig.auth.is_revoked(&first.claims.tid),
        "29 seconds is inside the grace"
    );
    assert!(accepted(&rig.auth, &first).await);

    pass(Duration::from_secs(2)).await;
    assert!(rig.auth.is_revoked(&first.claims.tid), "31 seconds is not");
    assert!(
        !accepted(&rig.auth, &first).await,
        "the replayed old cookie is refused"
    );
    assert!(
        accepted(&rig.auth, &second).await,
        "the new token is untouched"
    );
    assert_eq!(rig.manager.pending_revocations(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_replaced_token_is_not_revoked_before_the_new_one_exists() {
    let rig = rig();
    let (first, session) = aged(&rig);
    *rig.auth.fail_issue.lock().expect("lock") = true;
    rig.manager
        .renew(&session, false)
        .await
        .expect_err("the token service is down");
    pass(MINUTE).await;
    assert!(
        !rig.auth.is_revoked(&first.claims.tid),
        "must not revoke the only token the user has when the reissue failed"
    );
    assert_eq!(rig.manager.pending_revocations(), 0);
    assert!(accepted(&rig.auth, &first).await);
}

#[tokio::test(start_paused = true)]
async fn parallel_reissues_from_one_old_token_wait_for_one_revocation() {
    let rig = rig();
    let (first, session) = aged(&rig);
    let one = rig.manager.renew(&session, false).await.expect("first");
    let two = rig.manager.renew(&session, false).await.expect("second");
    assert_ne!(one.claims.tid, two.claims.tid);
    assert_eq!(
        rig.manager.pending_revocations(),
        1,
        "one task per replaced token, however many requests replaced it"
    );
    pass(Duration::from_secs(31)).await;
    assert!(rig.auth.is_revoked(&first.claims.tid));
    assert_eq!(rig.manager.pending_revocations(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_grace_is_a_setting_and_zero_means_at_once() {
    let rig = rig_with(
        SessionSettings::default().with_revoke_grace(Duration::from_secs(5)),
        None,
    );
    let (first, session) = aged(&rig);
    rig.manager.renew(&session, false).await.expect("renew");
    pass(Duration::from_secs(4)).await;
    assert!(!rig.auth.is_revoked(&first.claims.tid));
    pass(Duration::from_secs(2)).await;
    assert!(
        rig.auth.is_revoked(&first.claims.tid),
        "a shorter grace is honoured"
    );

    let at_once = rig_with(
        SessionSettings::default().with_revoke_grace(Duration::ZERO),
        None,
    );
    let (old, session) = aged(&at_once);
    at_once.manager.renew(&session, false).await.expect("renew");
    assert!(
        at_once.auth.is_revoked(&old.claims.tid),
        "no waiting at all"
    );
    assert_eq!(at_once.manager.pending_revocations(), 0);
}

#[test]
fn must_not_let_the_grace_exceed_its_ceiling() {
    let settings = SessionSettings::default().with_revoke_grace(Duration::from_hours(2));
    assert_eq!(settings.revoke_grace, MAX_REVOKE_GRACE);
    assert_eq!(
        SessionSettings::default().revoke_grace,
        DEFAULT_REVOKE_GRACE,
        "30 seconds unless told otherwise"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failing_revocation_is_logged_and_never_fails_the_reissue() {
    let rig = rig();
    let (first, session) = aged(&rig);
    let second = rig.manager.renew(&session, false).await.expect("renew");
    *rig.auth.fail_revoke.lock().expect("lock") = true;
    pass(Duration::from_secs(31)).await;
    assert!(
        !rig.auth.is_revoked(&first.claims.tid),
        "the store refused, so the old token lives to its own end"
    );
    assert_eq!(
        rig.manager.pending_revocations(),
        0,
        "and the task ended instead of retrying forever"
    );
    assert!(accepted(&rig.auth, &second).await);
}

// ---- the drain --------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_shutdown_drain_revokes_what_is_waiting_at_once() {
    let rig = rig();
    let (first, session) = aged(&rig);
    rig.manager.renew(&session, false).await.expect("renew");
    assert!(!rig.auth.is_revoked(&first.claims.tid));

    let before = Instant::now();
    let unfinished = rig.manager.drain(Duration::from_secs(10)).await;
    assert_eq!(unfinished, 0);
    assert!(
        rig.auth.is_revoked(&first.claims.tid),
        "must not lose a pending revocation at shutdown"
    );
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "the drain does not wait out the grace"
    );
}

/// An issuer whose revocation never finishes, to show the drain deadline.
struct Hanging(Arc<FakeAuth>);

#[async_trait]
impl SessionIssuer for Hanging {
    fn issue(&self, request: &SessionRequest) -> Result<IssuedSession, AuthError> {
        self.0.issue(request)
    }

    async fn revoke(&self, _: UserId, _: &str, _: OffsetDateTime) -> Result<(), AuthError> {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn the_drain_gives_up_at_its_deadline_and_says_how_many_are_left() {
    let clock = rustryx_testkit::FixedClock::default();
    let auth = FakeAuth::new(clock.clone());
    let manager = SessionManager::new(
        SessionSettings::default(),
        Arc::new(Hanging(auth)),
        Arc::new(clock.clone()),
        key(),
        None,
    );
    let first = manager
        .start(UserId::generate(), None, None)
        .expect("start");
    let session = {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::COOKIE,
            http::HeaderValue::from_str(&format!("{SESSION_COOKIE}={}", first.token.expose()))
                .expect("header"),
        );
        SessionManager::read(&headers)
            .expect("a cookie")
            .expect("a session")
    };
    clock.advance(MINUTE * 20);
    manager.renew(&session, false).await.expect("renew");
    let unfinished = manager.drain(Duration::from_secs(2)).await;
    assert_eq!(unfinished, 1, "a stuck store does not hold the shutdown");
}

#[tokio::test(start_paused = true)]
async fn a_reissue_after_the_drain_revokes_at_once_instead_of_waiting_for_nobody() {
    let rig = rig();
    let (first, session) = aged(&rig);
    assert_eq!(rig.manager.drain(Duration::from_secs(1)).await, 0);
    rig.manager.renew(&session, false).await.expect("renew");
    settle().await;
    assert!(rig.auth.is_revoked(&first.claims.tid));
}

// ---- the family link --------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_logout_ends_the_token_it_replaced_without_waiting_for_the_grace() {
    let rig = rig();
    let (first, session) = aged(&rig);
    let second = rig.manager.renew(&session, false).await.expect("renew");
    rig.manager.revoke(&second.claims).await.expect("logout");
    assert!(
        rig.auth.is_revoked(&second.claims.tid),
        "the current token at once"
    );
    assert!(
        rig.auth.is_revoked(&first.claims.tid),
        "must not leave the predecessor valid for the rest of its grace"
    );
}

#[tokio::test(start_paused = true)]
async fn a_logout_ends_a_whole_chain_of_replaced_tokens() {
    let rig = rig();
    let a = rig.manager.start(rig.user, None, None).expect("start");
    rig.clock.advance(MINUTE * 20);
    let b = rig
        .manager
        .renew(&read(&rig, &a), false)
        .await
        .expect("a to b");
    rig.clock.advance(MINUTE * 20);
    let c = rig
        .manager
        .renew(&read(&rig, &b), false)
        .await
        .expect("b to c");
    rig.manager.revoke(&c.claims).await.expect("logout");
    for start in [&a, &b, &c] {
        assert!(
            rig.auth.is_revoked(&start.claims.tid),
            "{}",
            start.claims.tid
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_logout_does_not_touch_the_tokens_of_another_chain() {
    let rig = rig();
    let (first, session) = aged(&rig);
    let second = rig.manager.renew(&session, false).await.expect("renew");
    let other = rig
        .manager
        .start(rig.user, None, None)
        .expect("another login");
    rig.manager.revoke(&other.claims).await.expect("logout");
    assert!(!rig.auth.is_revoked(&second.claims.tid));
    assert!(
        !rig.auth.is_revoked(&first.claims.tid),
        "still inside its grace"
    );
}

// ---- over HTTP --------------------------------------------------------------------------

fn replay(cookie: &str) -> http::Request<Body> {
    request(Method::GET, "/api/v1/auth/csrf")
        .header("cookie", format!("{SESSION_COOKIE}={cookie}"))
        .body(Body::empty())
        .expect("request")
}

#[tokio::test(start_paused = true)]
async fn a_transparent_reissue_leaves_the_old_cookie_working_for_the_grace_only() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let old = browser.cookie(SESSION_COOKIE).expect("session");
    advance(&env, MINUTE * 20);
    assert_eq!(browser.get("/api/whoami").await.text(), "alice");
    let new = browser.cookie(SESSION_COOKIE).expect("session");
    assert_ne!(new, old, "the response moved the browser on");

    // A request that was in flight with the old cookie.
    pass(Duration::from_secs(10)).await;
    let in_flight = call(&env.router, replay(&old)).await;
    assert_eq!(in_flight.status, StatusCode::OK, "{}", in_flight.text());

    pass(Duration::from_secs(25)).await;
    let late = call(&env.router, replay(&old)).await;
    assert_eq!(
        late.status,
        StatusCode::UNAUTHORIZED,
        "a replayed old cookie after the grace is refused: {}",
        late.text()
    );
    assert_eq!(late.json()["error"]["code"], "token_expired");
    assert_eq!(
        call(&env.router, replay(&new)).await.status,
        StatusCode::OK,
        "the cookie the browser holds keeps working"
    );
}

#[tokio::test(start_paused = true)]
async fn the_explicit_refresh_retires_the_old_cookie_the_same_way() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let old = browser.cookie(SESSION_COOKIE).expect("session");
    advance(&env, MINUTE * 20);
    let refreshed = browser.post("/api/v1/auth/session/refresh", None).await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.text());
    assert_ne!(browser.cookie(SESSION_COOKIE).expect("session"), old);

    pass(Duration::from_secs(5)).await;
    assert_eq!(call(&env.router, replay(&old)).await.status, StatusCode::OK);
    pass(Duration::from_secs(30)).await;
    assert_eq!(
        call(&env.router, replay(&old)).await.status,
        StatusCode::UNAUTHORIZED,
        "refused after the grace"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_refresh_keeps_the_old_cookie_alive() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let old = browser.cookie(SESSION_COOKIE).expect("session");
    advance(&env, MINUTE * 20);
    *env.auth.fail_issue.lock().expect("lock") = true;
    let failed = browser.post("/api/v1/auth/session/refresh", None).await;
    assert_ne!(failed.status, StatusCode::OK);
    pass(MINUTE).await;
    assert_eq!(
        call(&env.router, replay(&old)).await.status,
        StatusCode::OK,
        "must not revoke the old token when no new one was issued"
    );
}

#[tokio::test(start_paused = true)]
async fn a_logout_right_after_a_reissue_kills_both_cookies_over_http() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let old = browser.cookie(SESSION_COOKIE).expect("session");
    advance(&env, MINUTE * 20);
    assert_eq!(browser.get("/api/whoami").await.text(), "alice");
    assert_ne!(browser.cookie(SESSION_COOKIE).expect("session"), old);
    assert!(browser.cookie(CSRF_COOKIE).is_some());

    let out = browser.post("/api/v1/auth/logout", None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.text());
    let ids = env.auth.issued();
    assert_eq!(ids.len(), 2, "the login and the reissue");
    for tid in &ids {
        assert!(env.auth.is_revoked(tid), "{tid} must end with the logout");
    }
    assert_eq!(
        call(&env.router, replay(&old)).await.status,
        StatusCode::UNAUTHORIZED,
        "the copied old cookie is useless the moment the user logs out"
    );
}

#[tokio::test(start_paused = true)]
async fn a_revocation_failure_does_not_fail_the_request_that_reissued() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    advance(&env, MINUTE * 20);
    *env.auth.fail_revoke.lock().expect("lock") = true;
    assert_eq!(browser.get("/api/whoami").await.text(), "alice");
    pass(Duration::from_secs(31)).await;
    assert_eq!(
        browser.get("/api/whoami").await.text(),
        "alice",
        "the user keeps working on the new token"
    );
}
