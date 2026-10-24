use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use http::Request;

use super::*;

fn nets(list: &[&str]) -> Vec<IpNet> {
    list.iter().map(|n| n.parse().expect("cidr")).collect()
}

fn parts(peer: Option<&str>, headers: &[(&str, &str)]) -> Parts {
    let mut builder = Request::builder().uri("/x");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(()).expect("request");
    if let Some(peer) = peer {
        let addr: std::net::IpAddr = peer.parse().expect("ip");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(addr, 40_000)));
    }
    request.into_parts().0
}

// ---- is_https ------------------------------------------------------------------------

#[test]
fn a_trusted_proxy_that_says_https_makes_the_request_secure() {
    let trusted = nets(&["10.0.0.0/8"]);
    assert!(is_https(
        &parts(Some("10.0.0.5"), &[("x-forwarded-proto", "https")]),
        &trusted
    ));
    assert!(is_https(
        &parts(Some("10.0.0.5"), &[("x-forwarded-proto", "HTTPS")]),
        &trusted
    ));
    assert!(is_https(
        &parts(
            Some("10.0.0.5"),
            &[("forwarded", "for=1.2.3.4;proto=https;by=10.0.0.5")]
        ),
        &trusted
    ));
    assert!(is_https(
        &parts(Some("10.0.0.5"), &[("forwarded", "proto=\"https\"")]),
        &trusted
    ));
}

#[test]
fn must_not_believe_the_protocol_header_of_an_untrusted_peer() {
    let trusted = nets(&["10.0.0.0/8"]);
    let lying = [("x-forwarded-proto", "https")];
    assert!(!is_https(&parts(Some("203.0.113.9"), &lying), &trusted));
    assert!(
        !is_https(&parts(Some("203.0.113.9"), &lying), &[]),
        "no proxy configured"
    );
    assert!(
        !is_https(&parts(None, &lying), &trusted),
        "no peer address at all"
    );
}

#[test]
fn must_not_count_a_plain_request_behind_a_trusted_proxy_as_secure() {
    let trusted = nets(&["10.0.0.0/8"]);
    assert!(
        !is_https(&parts(Some("10.0.0.5"), &[]), &trusted),
        "no header, no evidence"
    );
    assert!(!is_https(
        &parts(Some("10.0.0.5"), &[("x-forwarded-proto", "http")]),
        &trusted
    ));
    assert!(!is_https(
        &parts(Some("10.0.0.5"), &[("x-forwarded-proto", "")]),
        &trusted
    ));
    assert!(!is_https(
        &parts(Some("10.0.0.5"), &[("x-forwarded-proto", "wss")]),
        &trusted
    ));
}

#[test]
fn the_right_most_value_is_the_one_the_proxy_wrote() {
    let trusted = nets(&["10.0.0.0/8"]);
    // The client sent `https` itself; the proxy in front of the node appended what it saw.
    assert!(!is_https(
        &parts(Some("10.0.0.5"), &[("x-forwarded-proto", "https, http")]),
        &trusted
    ));
    assert!(is_https(
        &parts(Some("10.0.0.5"), &[("x-forwarded-proto", "http, https")]),
        &trusted
    ));
    let mut split = parts(Some("10.0.0.5"), &[("x-forwarded-proto", "https")]);
    split
        .headers
        .append("x-forwarded-proto", "http".parse().expect("value"));
    assert!(!is_https(&split, &trusted), "the last header field counts");
    assert!(!is_https(
        &parts(
            Some("10.0.0.5"),
            &[("forwarded", "proto=https, proto=http")]
        ),
        &trusted
    ));
}

#[test]
fn a_contradiction_between_the_two_headers_is_read_as_not_secure() {
    let trusted = nets(&["10.0.0.0/8"]);
    let mixed = [("x-forwarded-proto", "https"), ("forwarded", "proto=http")];
    assert!(!is_https(&parts(Some("10.0.0.5"), &mixed), &trusted));
    let agreeing = [("x-forwarded-proto", "https"), ("forwarded", "proto=https")];
    assert!(is_https(&parts(Some("10.0.0.5"), &agreeing), &trusted));
}

#[test]
fn an_ipv4_mapped_ipv6_peer_matches_the_ipv4_proxy_range() {
    let trusted = nets(&["10.0.0.0/8"]);
    let secure = [("x-forwarded-proto", "https")];
    assert!(is_https(&parts(Some("::ffff:10.0.0.5"), &secure), &trusted));
}

#[test]
fn unreadable_headers_are_no_evidence() {
    let trusted = nets(&["10.0.0.0/8"]);
    let mut request = Request::builder().uri("/x").body(()).expect("request");
    request.headers_mut().insert(
        X_FORWARDED_PROTO,
        http::HeaderValue::from_bytes(b"\xff\xfe").expect("opaque"),
    );
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 5], 1))));
    assert!(!is_https(&request.into_parts().0, &trusted));
}

// ---- normalize_origin ----------------------------------------------------------------

#[test]
fn origins_are_normalized_without_default_ports_and_case() {
    assert_eq!(
        normalize_origin("https://Repo.Example.TEST").as_deref(),
        Some("https://repo.example.test")
    );
    assert_eq!(
        normalize_origin("https://repo.example.test:443").as_deref(),
        Some("https://repo.example.test")
    );
    assert_eq!(
        normalize_origin("http://localhost:80/").as_deref(),
        Some("http://localhost")
    );
    assert_eq!(
        normalize_origin("http://localhost:8081/path?q#f").as_deref(),
        Some("http://localhost:8081")
    );
    for bad in [
        "",
        "null",
        "ftp://x",
        "https://",
        "https://user@host",
        "repo.example.test",
        "https:///x",
    ] {
        assert_eq!(normalize_origin(bad), None, "{bad:?}");
    }
}

// ---- OriginPolicy --------------------------------------------------------------------

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(
            http::HeaderName::from_bytes(name.as_bytes()).expect("name"),
            value.parse().expect("value"),
        );
    }
    map
}

#[test]
fn the_configured_origin_and_the_cors_origins_are_the_only_ones_permitted() {
    let policy = OriginPolicy::new(
        Some("https://repo.example.test"),
        &["https://ui.example.test".to_owned()],
    );
    assert!(policy.permits(&headers(&[("origin", "https://repo.example.test")])));
    assert!(policy.permits(&headers(&[("origin", "https://REPO.example.test:443")])));
    assert!(policy.permits(&headers(&[("origin", "https://ui.example.test")])));
    for foreign in [
        "https://evil.example.test",
        "http://repo.example.test",
        "https://repo.example.test:8443",
        "https://repo.example.test.evil.test",
        "null",
        "",
    ] {
        assert!(
            !policy.permits(&headers(&[("origin", foreign)])),
            "{foreign:?}"
        );
    }
}

#[test]
fn must_not_permit_a_missing_or_repeated_origin() {
    let policy = OriginPolicy::new(Some("https://repo.example.test"), &[]);
    assert!(!policy.permits(&HeaderMap::new()));
    assert!(!policy.permits(&headers(&[
        ("origin", "https://repo.example.test"),
        ("origin", "https://repo.example.test"),
    ])));
    let mut binary = HeaderMap::new();
    binary.insert(
        ORIGIN,
        http::HeaderValue::from_bytes(b"\xff").expect("opaque"),
    );
    assert!(!policy.permits(&binary));
}

#[test]
fn without_a_configured_origin_the_host_header_decides() {
    let policy = OriginPolicy::new(None, &[]);
    assert!(policy.permits(&headers(&[
        ("origin", "http://localhost:8081"),
        ("host", "localhost:8081")
    ])));
    assert!(policy.permits(&headers(&[
        ("origin", "https://repo.example.test"),
        ("host", "repo.example.test")
    ])));
    assert!(policy.permits(&headers(&[
        ("origin", "https://repo.example.test:443"),
        ("host", "repo.example.test:443")
    ])));
    // A cross site page carries its own origin next to the victim's host.
    assert!(!policy.permits(&headers(&[
        ("origin", "https://evil.example.test"),
        ("host", "repo.example.test")
    ])));
    assert!(
        !policy.permits(&headers(&[("origin", "http://localhost:8081")])),
        "no Host to compare"
    );
}

#[test]
fn an_entry_that_is_not_an_origin_is_dropped_and_leaves_the_host_rule_in_force() {
    let policy = OriginPolicy::new(Some("not an origin"), &["*".to_owned()]);
    assert_eq!(policy.allowed.len(), 0);
    assert!(policy.permits(&headers(&[("origin", "http://a.test"), ("host", "a.test")])));
    assert!(!policy.permits(&headers(&[("origin", "http://b.test"), ("host", "a.test")])));
}
