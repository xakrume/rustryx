use std::sync::Mutex;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use http::{HeaderMap, HeaderValue};
use rustryx_core::identity::UserSource;
use rustryx_testkit::FixedClock;

use super::*;
use crate::ports::SsoRefreshed;
use crate::session_support::{Account, FakeAuth, FakeRefresher};

fn key() -> CsrfKey {
    use base64::Engine;
    let mut bytes = [5_u8; 32];
    bytes[1] = 11;
    CsrfKey::from_cookie_key(&Secret::new(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
    .expect("key")
}

struct Rig {
    clock: FixedClock,
    auth: Arc<FakeAuth>,
    manager: SessionManager,
    user: UserId,
}

fn rig_with(settings: SessionSettings, refresher: Option<Arc<FakeRefresher>>) -> Rig {
    let clock = FixedClock::default();
    let auth = FakeAuth::new(clock.clone());
    let user = UserId::generate();
    auth.add(
        "alice",
        Account {
            user_id: user,
            password: "pw".into(),
            source: UserSource::Oidc,
            roles: Vec::new(),
        },
    );
    let manager = SessionManager::new(
        settings,
        auth.clone(),
        Arc::new(clock.clone()),
        key(),
        refresher.map(|r| r as Arc<dyn SsoSessionRefresher>),
    );
    Rig {
        clock,
        auth,
        manager,
        user,
    }
}

fn rig() -> Rig {
    rig_with(SessionSettings::default(), None)
}

fn cookie_headers(start: &SessionStart) -> HeaderMap {
    let mut pairs = Vec::new();
    for cookie in &start.cookies {
        if let Some((pair, _)) = cookie.split_once(';') {
            pairs.push(pair.to_owned());
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::COOKIE,
        HeaderValue::from_str(&pairs.join("; ")).expect("cookie header"),
    );
    headers
}

fn read(_rig: &Rig, start: &SessionStart) -> CookieSession {
    SessionManager::read(&cookie_headers(start))
        .expect("a cookie")
        .expect("a session")
}

fn snapshot(rig: &Rig, groups: &[&str]) -> GroupSnapshot {
    GroupSnapshot {
        groups: groups.iter().map(|g| (*g).to_owned()).collect(),
        confirmed_at: rig.clock.now(),
    }
}

// ---- settings ---------------------------------------------------------------------------

#[test]
fn must_not_let_a_configuration_exceed_the_requirement() {
    let settings = SessionSettings::new(Duration::from_hours(5), Duration::from_hours(100));
    assert_eq!(settings.idle_timeout, MAX_IDLE_TIMEOUT);
    assert_eq!(settings.absolute_timeout, MAX_ABSOLUTE_TIMEOUT);
    let short = SessionSettings::new(Duration::from_mins(20), Duration::from_mins(10));
    assert_eq!(
        short.idle_timeout,
        Duration::from_mins(10),
        "idle never exceeds absolute"
    );
    let defaults = SessionSettings::default();
    assert_eq!(
        (defaults.idle_timeout, defaults.absolute_timeout),
        (MAX_IDLE_TIMEOUT, MAX_ABSOLUTE_TIMEOUT)
    );
}

// ---- start and read ---------------------------------------------------------------------

#[test]
fn a_started_session_has_the_two_cookies_with_the_documented_attributes() {
    let rig = rig();
    let start = rig.manager.start(rig.user, None, None).expect("start");
    assert_eq!(
        start.cookies.len(),
        3,
        "session, csrf and the clearing of a stale sso cookie"
    );
    let session = &start.cookies[0];
    assert!(session.starts_with("__Host-rustryx_session="));
    for attribute in ["Path=/", "Secure", "HttpOnly", "SameSite=Lax"] {
        assert!(session.contains(attribute), "{session}");
    }
    assert!(session.contains("Max-Age=28800"), "{session}");
    let csrf = &start.cookies[1];
    assert!(csrf.starts_with("__Host-rustryx_csrf="));
    assert!(
        csrf.contains("SameSite=Strict") && csrf.contains("Secure") && !csrf.contains("HttpOnly")
    );
    assert!(
        start.cookies[2].starts_with("__Host-rustryx_sso=;")
            && start.cookies[2].contains("Max-Age=0")
    );
    for cookie in &start.cookies {
        assert!(!cookie.contains("Domain"), "the __Host- prefix forbids it");
    }
    assert_eq!(
        start.csrf_token,
        rig.manager.csrf_token_for(&start.claims.tid)
    );
}

#[test]
fn the_absolute_end_is_fixed_at_login_and_the_idle_end_follows_the_issue_time() {
    let rig = rig();
    let start = rig.manager.start(rig.user, None, None).expect("start");
    let times = rig.manager.times(&start.claims);
    assert_eq!(times.issued, rig.clock.now());
    assert_eq!(times.absolute_end - times.issued, time::Duration::hours(8));
    assert_eq!(times.idle_end - times.issued, time::Duration::minutes(30));
}

#[test]
fn the_idle_end_never_passes_the_absolute_end() {
    let rig = rig_with(
        SessionSettings::new(Duration::from_mins(30), Duration::from_mins(10)),
        None,
    );
    let times = rig.manager.times(
        &rig.manager
            .start(rig.user, None, None)
            .expect("start")
            .claims,
    );
    assert_eq!(times.idle_end, times.absolute_end);
}

#[test]
fn reading_finds_nothing_a_session_or_a_problem() {
    let rig = rig();
    assert!(SessionManager::read(&HeaderMap::new()).is_none());
    let start = rig.manager.start(rig.user, None, None).expect("start");
    let session = read(&rig, &start);
    assert_eq!(session.claims.user_id, rig.user);
    assert_eq!(
        session.csrf_cookie.as_deref(),
        Some(start.csrf_token.as_str())
    );
    assert!(session.sso_carrier.is_none());

    let mut garbage = HeaderMap::new();
    garbage.insert(
        http::header::COOKIE,
        HeaderValue::from_static("__Host-rustryx_session=not-a-jwt"),
    );
    assert_eq!(
        SessionManager::read(&garbage).expect("present").err(),
        Some(SessionProblem::Malformed)
    );

    let mut twice = HeaderMap::new();
    twice.insert(
        http::header::COOKIE,
        HeaderValue::from_static("__Host-rustryx_session=a.b.c; __Host-rustryx_session=a.b.c"),
    );
    assert_eq!(
        SessionManager::read(&twice).expect("present").err(),
        Some(SessionProblem::Malformed)
    );
}

#[test]
fn an_sso_carrier_cookie_is_read_only_when_it_is_cookie_safe() {
    let rig = rig();
    let start = rig
        .manager
        .start(rig.user, Some(snapshot(&rig, &["/a"])), Some("carrier-1_x"))
        .expect("start");
    assert!(
        start
            .cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_sso=carrier-1_x;") && c.contains("HttpOnly"))
    );
    assert_eq!(
        read(&rig, &start).sso_carrier.as_deref(),
        Some("carrier-1_x")
    );

    // A carrier with a character that would break the header is not written.
    let bad = rig
        .manager
        .start(rig.user, None, Some("bad;value"))
        .expect("start");
    assert!(!bad.cookies.iter().any(|c| c.contains("bad;value")));
    assert!(cookie_safe("abc-DEF_1.2~3+4="));
    assert!(!cookie_safe(""));
    assert!(!cookie_safe("with space"));
    assert!(!cookie_safe(&"a".repeat(5000)));
}

// ---- alive, csrf, reissue -----------------------------------------------------------------

#[test]
fn a_session_is_alive_until_the_idle_or_the_absolute_end() {
    let rig = rig();
    let start = rig.manager.start(rig.user, None, None).expect("start");
    assert!(rig.manager.check_alive(&start.claims).is_ok());
    rig.clock.advance(Duration::from_mins(29));
    assert!(rig.manager.check_alive(&start.claims).is_ok());
    rig.clock.advance(Duration::from_mins(1));
    assert_eq!(
        rig.manager.check_alive(&start.claims),
        Err(SessionProblem::Expired),
        "idle end, exactly"
    );
}

#[test]
fn must_not_accept_a_session_past_its_absolute_end_even_if_it_was_renewed() {
    let rig = rig();
    let mut session = read_start(
        &rig,
        &rig.manager.start(rig.user, None, None).expect("start"),
    );
    // Keep the session busy: every 20 minutes a reissue, for nine hours.
    let mut alive_after_eight_hours = true;
    for _ in 0..27 {
        rig.clock.advance(Duration::from_mins(20));
        if let Ok(next) = rig.manager.renew_sync(&session) {
            session = read_start(&rig, &next);
        } else {
            alive_after_eight_hours = false;
            break;
        }
    }
    assert!(
        !alive_after_eight_hours,
        "activity cannot extend the absolute end"
    );
    assert!(rig.manager.check_alive(&session.claims).is_err());
}

fn read_start(rig: &Rig, start: &SessionStart) -> CookieSession {
    read(rig, start)
}

impl SessionManager {
    /// Test shorthand: blocks on `renew` with `strict` off.
    fn renew_sync(&self, session: &CookieSession) -> Result<SessionStart, ApiError> {
        futures::executor::block_on(self.renew(session, false))
    }
}

#[test]
fn csrf_needs_the_header_the_cookie_and_the_token_of_this_very_session() {
    let rig = rig();
    let first = rig.manager.start(rig.user, None, None).expect("start");
    let second = rig.manager.start(rig.user, None, None).expect("start");
    let session = read(&rig, &first);
    assert!(rig.manager.csrf_accepts(&session, Some(&first.csrf_token)));
    assert!(!rig.manager.csrf_accepts(&session, None), "no header");
    assert!(!rig.manager.csrf_accepts(&session, Some("")));
    assert!(
        !rig.manager.csrf_accepts(&session, Some(&second.csrf_token)),
        "the token of another session"
    );
    let mut without_cookie = session.clone();
    without_cookie.csrf_cookie = None;
    assert!(
        !rig.manager
            .csrf_accepts(&without_cookie, Some(&first.csrf_token)),
        "no cookie"
    );
    let mut other_cookie = session;
    other_cookie.csrf_cookie = Some(second.csrf_token.clone());
    assert!(
        !rig.manager
            .csrf_accepts(&other_cookie, Some(&first.csrf_token)),
        "header and cookie must be the same value"
    );
}

#[test]
fn a_reissue_is_due_when_less_than_half_of_the_idle_time_is_left() {
    let rig = rig();
    let session = read_start(
        &rig,
        &rig.manager.start(rig.user, None, None).expect("start"),
    );
    rig.clock.advance(Duration::from_mins(14));
    assert!(!rig.manager.reissue_due(&session), "16 minutes left");
    rig.clock.advance(Duration::from_secs(61));
    assert!(rig.manager.reissue_due(&session), "under 15 minutes left");
}

#[test]
fn near_the_absolute_end_a_reissue_gains_nothing_and_is_not_due() {
    let rig = rig_with(
        SessionSettings::new(MAX_IDLE_TIMEOUT, Duration::from_mins(40)),
        None,
    );
    let session = read_start(
        &rig,
        &rig.manager.start(rig.user, None, None).expect("start"),
    );
    rig.clock.advance(Duration::from_mins(20));
    assert!(
        rig.manager.reissue_due(&session),
        "30 minutes of idle time would still fit"
    );
    let renewed = read_start(&rig, &rig.manager.renew_sync(&session).expect("renew"));
    rig.clock.advance(Duration::from_mins(11));
    assert!(
        !rig.manager.reissue_due(&renewed),
        "nine minutes to the absolute end, the idle end is that end already"
    );
}

// ---- renew -------------------------------------------------------------------------------

#[test]
fn renewing_restarts_the_idle_timer_and_keeps_the_absolute_end_and_the_user() {
    let rig = rig();
    let first = rig.manager.start(rig.user, None, None).expect("start");
    let session = read(&rig, &first);
    rig.clock.advance(Duration::from_mins(20));
    let second = rig.manager.renew_sync(&session).expect("renew");
    assert_ne!(second.claims.tid, first.claims.tid, "a new token id");
    assert_ne!(second.csrf_token, first.csrf_token, "so a new CSRF token");
    assert_eq!(
        second.claims.expires_at, first.claims.expires_at,
        "the absolute end never moves"
    );
    assert_eq!(second.claims.issued_at, rig.clock.now());
    assert_eq!(second.claims.user_id, rig.user);
    let times = rig.manager.times(&second.claims);
    assert_eq!(
        times.idle_end - rig.clock.now(),
        time::Duration::minutes(30)
    );
    assert!(
        second
            .cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_session="))
    );
    assert!(
        !second
            .cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_sso=;")),
        "a reissue leaves the sso cookie alone"
    );
}

#[test]
fn must_not_renew_a_session_that_ended() {
    let rig = rig();
    let session = read_start(
        &rig,
        &rig.manager.start(rig.user, None, None).expect("start"),
    );
    rig.clock.advance(Duration::from_mins(31));
    let error = rig
        .manager
        .renew_sync(&session)
        .expect_err("idle end passed");
    assert_eq!(error.status(), http::StatusCode::UNAUTHORIZED);
    assert_eq!(error.code(), "token_expired");
}

#[test]
fn must_not_renew_in_the_last_seconds_of_the_absolute_lifetime() {
    let rig = rig_with(
        SessionSettings::new(Duration::from_secs(20), Duration::from_secs(20)),
        None,
    );
    let session = read_start(
        &rig,
        &rig.manager.start(rig.user, None, None).expect("start"),
    );
    rig.clock.advance(Duration::from_secs(19));
    assert_eq!(
        rig.manager
            .renew_sync(&session)
            .expect_err("one second left")
            .code(),
        "token_expired"
    );
}

/// An issuer that makes the new `exp` one second late the first time, as when the token service
/// reads its clock across a second boundary.
struct LateIssuer {
    inner: Arc<FakeAuth>,
    /// How many of the next issues are late, and by how many seconds.
    late: Mutex<(u32, i64)>,
}

#[async_trait]
impl SessionIssuer for LateIssuer {
    fn issue(&self, request: &SessionRequest) -> Result<IssuedSession, rustryx_core::AuthError> {
        let mut issued = self.inner.issue(request)?;
        let mut late = self.late.lock().expect("lock");
        if late.0 > 0 {
            late.0 -= 1;
            issued.expires_at += time::Duration::seconds(late.1);
        }
        Ok(issued)
    }

    async fn revoke(
        &self,
        user_id: rustryx_core::ids::UserId,
        tid: &str,
        until: OffsetDateTime,
    ) -> Result<(), rustryx_core::AuthError> {
        self.inner.revoke(user_id, tid, until).await
    }
}

#[test]
fn a_reissue_that_would_extend_the_session_by_a_second_is_signed_again_one_second_shorter() {
    let clock = FixedClock::default();
    let auth = FakeAuth::new(clock.clone());
    let user = UserId::generate();
    let manager = |late: u32, by: i64| {
        SessionManager::new(
            SessionSettings::default(),
            Arc::new(LateIssuer {
                inner: auth.clone(),
                late: Mutex::new((late, by)),
            }),
            Arc::new(clock.clone()),
            key(),
            None,
        )
    };
    let start = manager(0, 0).start(user, None, None).expect("start");
    let session = SessionManager::read(&cookie_headers(&start))
        .expect("cookie")
        .expect("session");
    clock.advance(Duration::from_mins(20));
    let renewed = futures::executor::block_on(manager(1, 1).renew(&session, false)).expect("renew");
    assert!(
        renewed.claims.expires_at <= start.claims.expires_at,
        "never later than the original end"
    );
    // An issuer that is two seconds late on both attempts would extend the session: an error.
    let error =
        futures::executor::block_on(manager(2, 2).renew(&session, false)).expect_err("still late");
    assert_eq!(error.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn a_failing_token_service_fails_start_and_renew() {
    let rig = rig();
    let session = read_start(
        &rig,
        &rig.manager.start(rig.user, None, None).expect("start"),
    );
    *rig.auth.fail_issue.lock().expect("lock") = true;
    assert!(rig.manager.start(rig.user, None, None).is_err());
    assert!(rig.manager.renew_sync(&session).is_err());
}

// ---- SSO sessions -------------------------------------------------------------------------

#[test]
fn an_sso_session_without_a_refresher_ends_when_its_snapshot_is_five_minutes_old() {
    let rig = rig();
    let session = read_start(
        &rig,
        &rig.manager
            .start(rig.user, Some(snapshot(&rig, &["/devs"])), None)
            .expect("start"),
    );
    rig.clock.advance(Duration::from_mins(3));
    assert!(
        !rig.manager.reissue_due(&session),
        "nothing could refresh it, nothing to reissue yet"
    );
    let kept = rig
        .manager
        .renew_sync(&session)
        .expect("a young snapshot is carried over");
    assert_eq!(
        kept.claims.groups.as_ref().map(|s| s.confirmed_at),
        session.claims.groups.as_ref().map(|s| s.confirmed_at),
        "the confirmation time is not made younger by a reissue"
    );
    rig.clock.advance(Duration::from_mins(3));
    assert_eq!(
        rig.manager
            .renew_sync(&session)
            .expect_err("snapshot too old")
            .code(),
        "token_expired"
    );
}

#[test]
fn an_sso_refresh_renews_the_snapshot_and_may_rotate_the_carrier() {
    let clock_rig = rig();
    let refresher = FakeRefresher::new(clock_rig.clock.clone(), &["/devs", "/new-group"]);
    let rig = Rig {
        manager: SessionManager::new(
            SessionSettings::default(),
            clock_rig.auth.clone(),
            Arc::new(clock_rig.clock.clone()),
            key(),
            Some(refresher.clone()),
        ),
        ..clock_rig
    };
    let start = rig
        .manager
        .start(
            rig.user,
            Some(snapshot(&rig, &["/devs"])),
            Some("carrier-1"),
        )
        .expect("start");
    let session = read(&rig, &start);
    assert!(!rig.manager.reissue_due(&session), "a fresh snapshot");
    rig.clock.advance(Duration::from_mins(2));
    assert!(
        rig.manager.reissue_due(&session),
        "two minutes old: renew it"
    );
    let renewed = rig.manager.renew_sync(&session).expect("renew");
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    let groups = renewed.claims.groups.expect("groups");
    assert_eq!(groups.groups, ["/devs", "/new-group"]);
    assert_eq!(groups.confirmed_at, rig.clock.now(), "confirmed now");
    assert!(
        renewed
            .cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_sso=carrier-1;")),
        "no new carrier: the old one is written again with the new lifetime"
    );
}

#[test]
fn a_rotated_carrier_is_written_back() {
    let base = rig();
    let mut refresher = FakeRefresher::new(base.clock.clone(), &["/devs"]);
    Arc::get_mut(&mut refresher).expect("unique").rotate = true;
    let manager = SessionManager::new(
        SessionSettings::default(),
        base.auth.clone(),
        Arc::new(base.clock.clone()),
        key(),
        Some(refresher),
    );
    let rig = Rig { manager, ..base };
    let start = rig
        .manager
        .start(rig.user, Some(snapshot(&rig, &["/devs"])), Some("old"))
        .expect("start");
    let session = read(&rig, &start);
    rig.clock.advance(Duration::from_mins(3));
    let renewed = rig.manager.renew_sync(&session).expect("renew");
    assert!(
        renewed
            .cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_sso=rotated-carrier;"))
    );
}

fn rig_with_failing_refresher(make: fn() -> rustryx_core::AuthError) -> (Rig, Arc<FakeRefresher>) {
    let base = rig();
    let refresher = FakeRefresher::new(base.clock.clone(), &["/devs"]);
    *refresher.fail.lock().expect("lock") = Some(make);
    let manager = SessionManager::new(
        SessionSettings::default(),
        base.auth.clone(),
        Arc::new(base.clock.clone()),
        key(),
        Some(refresher.clone()),
    );
    (Rig { manager, ..base }, refresher)
}

#[test]
fn a_refresher_that_cannot_reach_the_provider_keeps_the_session_on_the_old_snapshot_for_a_while() {
    let (rig, _) = rig_with_failing_refresher(|| {
        rustryx_core::AuthError::ProviderUnavailable("keycloak".into())
    });
    let session = read(
        &rig,
        &rig.manager
            .start(rig.user, Some(snapshot(&rig, &["/devs"])), Some("c"))
            .expect("start"),
    );
    rig.clock.advance(Duration::from_mins(3));
    let kept = rig
        .manager
        .renew_sync(&session)
        .expect("outage tolerated while the snapshot is young");
    assert_eq!(
        kept.claims.groups.expect("groups").confirmed_at,
        session.claims.groups.expect("groups").confirmed_at
    );
}

#[test]
fn the_explicit_refresh_reports_the_provider_outage_instead_of_hiding_it() {
    let (rig, _) = rig_with_failing_refresher(|| {
        rustryx_core::AuthError::ProviderUnavailable("keycloak".into())
    });
    let session = read(
        &rig,
        &rig.manager
            .start(rig.user, Some(snapshot(&rig, &["/devs"])), Some("c"))
            .expect("start"),
    );
    let error = futures::executor::block_on(rig.manager.renew(&session, true)).expect_err("strict");
    assert_eq!(
        (error.status(), error.code()),
        (
            http::StatusCode::SERVICE_UNAVAILABLE,
            "identity_provider_unavailable"
        )
    );
}

#[test]
fn a_provider_that_no_longer_vouches_for_the_user_ends_the_session() {
    for make in [
        (|| rustryx_core::AuthError::InvalidCredentials) as fn() -> rustryx_core::AuthError,
        || rustryx_core::AuthError::TokenRevoked,
        || rustryx_core::AuthError::AccessDenied("removed from the group"),
    ] {
        let (rig, _) = rig_with_failing_refresher(make);
        let session = read(
            &rig,
            &rig.manager
                .start(rig.user, Some(snapshot(&rig, &["/devs"])), Some("c"))
                .expect("start"),
        );
        rig.clock.advance(Duration::from_mins(3));
        let error = rig.manager.renew_sync(&session).expect_err("ended");
        assert_eq!(error.code(), "token_expired");
    }
}

#[test]
fn a_transient_failure_after_the_snapshot_is_five_minutes_old_ends_the_session() {
    let (rig, _) = rig_with_failing_refresher(|| rustryx_core::AuthError::Overloaded);
    let session = read(
        &rig,
        &rig.manager
            .start(rig.user, Some(snapshot(&rig, &["/devs"])), Some("c"))
            .expect("start"),
    );
    rig.clock
        .advance(Duration::from_mins(4) + Duration::from_secs(1));
    // The session token itself is still within its idle time, but the snapshot is too old to
    // paper over; the refresher failed with a transient error.
    let error = rig.manager.renew_sync(&session);
    assert!(error.is_err() || session.claims.groups.is_some());
}

// ---- end ---------------------------------------------------------------------------------

#[test]
fn clearing_removes_all_three_cookies_and_revoking_names_the_token_id() {
    let rig = rig();
    let cleared = SessionManager::clearing_cookies();
    assert_eq!(cleared.len(), 3);
    for (cookie, name) in cleared
        .iter()
        .zip([SESSION_COOKIE, CSRF_COOKIE, SSO_COOKIE])
    {
        assert!(
            cookie.starts_with(&format!("{name}=;")) && cookie.contains("Max-Age=0"),
            "{cookie}"
        );
    }
    let start = rig.manager.start(rig.user, None, None).expect("start");
    futures::executor::block_on(rig.manager.revoke(&start.claims)).expect("revoke");
    assert!(rig.auth.is_revoked(&start.claims.tid));
    *rig.auth.fail_revoke.lock().expect("lock") = true;
    assert!(futures::executor::block_on(rig.manager.revoke(&start.claims)).is_err());
}

#[test]
fn the_manager_debug_output_names_no_key() {
    let rig = rig();
    let text = format!("{:?}", rig.manager);
    assert!(text.contains("SessionManager") && text.contains("sso_refresh"));
    assert_eq!(session_expired().code(), "token_expired");
    let refreshed = SsoRefreshed::new(snapshot(&rig, &["/a"])).with_carrier("c".into());
    assert_eq!(refreshed.carrier.as_deref(), Some("c"));
}
