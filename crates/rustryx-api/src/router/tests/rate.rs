//! Rate limiting through the whole stack.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::{Method, StatusCode};
use rustryx_core::identity::{AuthMethod, Credentials};

use crate::RateLimitSettings;
use crate::test_support::{
    StubAuth, call, get_uri, principal, request, state_with_settings, test_router,
};

fn limited(rps: u32, burst: u32) -> axum::Router {
    test_router(state_with_settings(|s| {
        s.rate_limit = Some(RateLimitSettings {
            requests_per_second: rps,
            burst,
        });
    }))
}

fn from_peer(uri: &str, peer: [u8; 4], forwarded: Option<&str>) -> http::Request<Body> {
    let mut builder = request(Method::GET, uri);
    if let Some(value) = forwarded {
        builder = builder.header("x-forwarded-for", value);
    }
    let mut req = builder.body(Body::empty()).expect("req");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((peer, 4000))));
    req
}

#[tokio::test(start_paused = true)]
async fn requests_over_the_burst_are_refused_with_429_and_retry_after() {
    let router = limited(1, 3);
    for _ in 0..3 {
        assert_eq!(
            get_uri(&router, "/api/only-get").await.status,
            StatusCode::OK
        );
    }
    let refused = get_uri(&router, "/api/only-get").await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.error_code(), "rate_limited");
    assert_eq!(refused.header("retry-after"), Some("1"));
    assert!(refused.json()["error"]["request_id"].is_string());

    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::OK
    );
    let metrics = get_uri(&router, "/metrics").await.text();
    assert!(
        metrics.contains(r#"http_shed_total{reason="rate_limit"} 1"#),
        "{metrics}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_limit_is_shared_by_metadata_and_artifact_routes() {
    let router = limited(1, 2);
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::OK
    );
    assert_eq!(
        get_uri(&router, "/repository/a/b").await.status,
        StatusCode::OK
    );
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        get_uri(&router, "/repository/a/b").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test(start_paused = true)]
async fn must_not_rate_limit_probes_or_metrics() {
    let router = limited(1, 1);
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::OK
    );
    assert_eq!(
        get_uri(&router, "/api/only-get").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    for _ in 0..20 {
        assert_eq!(get_uri(&router, "/healthz").await.status, StatusCode::OK);
        assert_eq!(get_uri(&router, "/readyz").await.status, StatusCode::OK);
        assert_eq!(get_uri(&router, "/metrics").await.status, StatusCode::OK);
    }
}

#[tokio::test(start_paused = true)]
async fn anonymous_callers_are_limited_per_address() {
    let router = limited(1, 1);
    assert_eq!(
        call(&router, from_peer("/api/only-get", [192, 0, 2, 1], None))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, from_peer("/api/only-get", [192, 0, 2, 1], None))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        call(&router, from_peer("/api/only-get", [192, 0, 2, 2], None))
            .await
            .status,
        StatusCode::OK,
        "another address has its own bucket"
    );
}

#[tokio::test(start_paused = true)]
async fn must_not_let_a_client_reset_its_bucket_by_spoofing_forwarded_for() {
    let router = limited(1, 1);
    let first = call(
        &router,
        from_peer("/api/only-get", [203, 0, 113, 9], Some("1.1.1.1")),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);
    for forged in ["2.2.2.2", "3.3.3.3", "4.4.4.4, 5.5.5.5"] {
        let reply = call(
            &router,
            from_peer("/api/only-get", [203, 0, 113, 9], Some(forged)),
        )
        .await;
        assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS, "{forged}");
    }
}

#[tokio::test(start_paused = true)]
async fn authenticated_users_are_limited_by_name_whatever_their_address() {
    let auth = StubAuth::new(|credentials, _| match credentials {
        Credentials::Basic { username, .. } => {
            Ok(Some(principal(username, AuthMethod::BasicLocal, "r")))
        }
        _ => Ok(None),
    });
    let router = test_router(
        state_with_settings(|s| {
            s.rate_limit = Some(RateLimitSettings {
                requests_per_second: 1,
                burst: 2,
            });
        })
        .with_authenticator(auth),
    );
    let as_user = |name: &str, peer: [u8; 4]| {
        let mut req = from_peer("/api/whoami", peer, None);
        req.headers_mut().insert(
            "authorization",
            format!("Basic {}", STANDARD.encode(format!("{name}:pw")))
                .parse()
                .expect("header"),
        );
        req
    };
    assert_eq!(
        call(&router, as_user("alice", [10, 0, 0, 1])).await.status,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, as_user("alice", [10, 0, 0, 2])).await.status,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, as_user("alice", [10, 0, 0, 3])).await.status,
        StatusCode::TOO_MANY_REQUESTS,
        "moving to another address does not help"
    );
    assert_eq!(
        call(&router, as_user("bob", [10, 0, 0, 1])).await.status,
        StatusCode::OK,
        "bob has his own budget"
    );
}

#[tokio::test]
async fn without_settings_nothing_is_limited() {
    let router = test_router(state_with_settings(|_| {}));
    for _ in 0..200 {
        assert_eq!(
            get_uri(&router, "/api/only-get").await.status,
            StatusCode::OK
        );
    }
}

fn guess(peer: [u8; 4], token: &str) -> http::Request<Body> {
    let mut req = from_peer("/api/only-get", peer, None);
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("header"),
    );
    req
}

#[tokio::test(start_paused = true)]
async fn must_not_allow_unlimited_credential_guessing_from_one_address() {
    // The per-user limiter inside authentication never sees a caller that failed to
    // authenticate, so guessing needs its own counter keyed on the address.
    let auth = StubAuth::new(|_, _| Err(rustryx_core::AuthError::InvalidCredentials));
    let router = test_router(
        state_with_settings(|s| {
            s.rate_limit = Some(RateLimitSettings {
                requests_per_second: 1000,
                burst: 1000,
            });
        })
        .with_authenticator(auth),
    );
    for attempt in 0..10 {
        let reply = call(
            &router,
            guess([198, 51, 100, 7], &format!("ryx_guess{attempt}")),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "attempt {attempt}");
    }
    let refused = call(&router, guess([198, 51, 100, 7], "ryx_guess10")).await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.error_code(), "rate_limited");
    assert!(refused.header("retry-after").is_some());

    // Anonymous requests carry no credentials to guess: the challenge flow of docker,
    // maven and npm (anonymous first, then retry with credentials) must keep working.
    let anonymous = call(&router, from_peer("/api/only-get", [198, 51, 100, 7], None)).await;
    assert_ne!(anonymous.status, StatusCode::TOO_MANY_REQUESTS);

    // Another address has its own counter.
    let other = call(&router, guess([198, 51, 100, 8], "ryx_guess")).await;
    assert_eq!(other.status, StatusCode::UNAUTHORIZED);

    // The counter refills: ten failures per minute afterwards.
    tokio::time::advance(Duration::from_secs(6)).await;
    let later = call(&router, guess([198, 51, 100, 7], "ryx_guess11")).await;
    assert_eq!(
        later.status,
        StatusCode::UNAUTHORIZED,
        "one attempt regained"
    );
}

#[tokio::test(start_paused = true)]
async fn successful_credentials_do_not_use_up_the_failure_counter() {
    let auth = StubAuth::new(|credentials, _| match credentials {
        Credentials::Basic { username, .. } => {
            Ok(Some(principal(username, AuthMethod::BasicLocal, "r")))
        }
        _ => Ok(None),
    });
    let router = test_router(
        state_with_settings(|s| {
            s.rate_limit = Some(RateLimitSettings {
                requests_per_second: 1000,
                burst: 1000,
            });
        })
        .with_authenticator(auth),
    );
    for _ in 0..50 {
        let mut req = from_peer("/api/whoami", [10, 1, 1, 1], None);
        req.headers_mut().insert(
            "authorization",
            format!("Basic {}", STANDARD.encode("alice:pw"))
                .parse()
                .expect("header"),
        );
        assert_eq!(call(&router, req).await.status, StatusCode::OK);
    }
}

fn peer_v6(peer: std::net::Ipv6Addr, token: &str) -> http::Request<Body> {
    let mut req = request(Method::GET, "/api/only-get")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("req");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((peer, 4000))));
    req
}

#[tokio::test(start_paused = true)]
async fn must_not_let_a_client_dodge_the_guard_by_rotating_ipv6_addresses_in_its_prefix() {
    let auth = StubAuth::new(|_, _| Err(rustryx_core::AuthError::InvalidCredentials));
    let router = test_router(
        state_with_settings(|s| {
            s.rate_limit = Some(RateLimitSettings {
                requests_per_second: 1000,
                burst: 1000,
            });
        })
        .with_authenticator(auth),
    );
    for host in 0..10u16 {
        let addr = std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0, 0, 0, host);
        let reply = call(&router, peer_v6(addr, "ryx_guess")).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "host {host}");
    }
    let same_prefix = std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0xaaaa, 0xbbbb, 0xcccc, 0xdddd);
    assert_eq!(
        call(&router, peer_v6(same_prefix, "ryx_guess"))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS,
        "one /64 is one client"
    );
    let other_prefix = std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 2, 0, 0, 0, 1);
    assert_eq!(
        call(&router, peer_v6(other_prefix, "ryx_guess"))
            .await
            .status,
        StatusCode::UNAUTHORIZED,
        "another /64 has its own budget"
    );
}

#[tokio::test(start_paused = true)]
async fn must_not_lock_out_credentials_that_verified_from_a_shared_address() {
    // Behind a NAT every colleague shares one address. An attacker burning the budget
    // must not lock out the people who are logged in from it.
    let auth = StubAuth::new(|credentials, _| match credentials {
        Credentials::Basic { username, password }
            if username == "alice" && password.expose() == "pw" =>
        {
            Ok(Some(principal(username, AuthMethod::BasicLocal, "r")))
        }
        _ => Err(rustryx_core::AuthError::InvalidCredentials),
    });
    let router = test_router(
        state_with_settings(|s| {
            s.rate_limit = Some(RateLimitSettings {
                requests_per_second: 1000,
                burst: 1000,
            });
        })
        .with_authenticator(auth),
    );
    let basic = |user: &str, pw: &str| {
        let mut req = from_peer("/api/whoami", [203, 0, 113, 50], None);
        req.headers_mut().insert(
            "authorization",
            format!("Basic {}", STANDARD.encode(format!("{user}:{pw}")))
                .parse()
                .expect("header"),
        );
        req
    };
    assert_eq!(
        call(&router, basic("alice", "pw")).await.status,
        StatusCode::OK
    );
    for attempt in 0..10 {
        let reply = call(&router, basic("alice", &format!("wrong{attempt}"))).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        call(&router, basic("alice", "wrong-again")).await.status,
        StatusCode::TOO_MANY_REQUESTS,
        "a new guess is refused"
    );
    assert_eq!(
        call(&router, basic("alice", "pw")).await.status,
        StatusCode::OK,
        "the verified credential still works"
    );
}
