use std::fmt::Write as _;

use proptest::prelude::*;

use super::*;

#[test]
fn parses_the_docker_hub_challenge() {
    let challenge = parse_bearer(
        r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull""#,
    )
    .unwrap();
    assert_eq!(challenge.realm.as_str(), "https://auth.docker.io/token");
    assert_eq!(challenge.service.as_deref(), Some("registry.docker.io"));
}

#[test]
fn the_service_is_optional_and_unquoted_values_work() {
    let challenge = parse_bearer("bearer realm=https://auth.example.com/t").unwrap();
    assert_eq!(challenge.service, None);
    let challenge = parse_bearer("BEARER service=s, realm=\"https://a.example/t?x=1\"").unwrap();
    assert_eq!(challenge.realm.query(), Some("x=1"));
    assert_eq!(challenge.service.as_deref(), Some("s"));
}

#[test]
fn escapes_and_commas_inside_quotes_are_read_as_one_value() {
    let challenge =
        parse_bearer(r#"Bearer realm="https://a.example/t",service="a, b \"c\" d",error="x""#)
            .unwrap();
    assert_eq!(challenge.service.as_deref(), Some(r#"a, b "c" d"#));
}

#[test]
fn only_a_bearer_challenge_with_a_usable_realm_counts() {
    for bad in [
        "",
        "Basic realm=\"x\"",
        "Bearer",
        "Bearer service=x",
        "Bearer realm=\"\"",
        "Bearer realm=\"not a url\"",
        "Bearer realm=\"ftp://a.example/t\"",
        "Bearer realm=\"file:///etc/passwd\"",
        "Bearer realm=\"https://user:pw@a.example/t\"",
        "Bearer realm=\"https://user@a.example/t\"",
        "Bearer realm=\"javascript:alert(1)\"",
    ] {
        assert_eq!(parse_bearer(bad), None, "{bad}");
    }
}

#[test]
fn the_first_bearer_among_several_headers_wins() {
    let found = find_bearer([
        "Basic realm=\"x\"",
        "Bearer realm=\"https://one.example/t\"",
        "Bearer realm=\"https://two.example/t\"",
    ])
    .unwrap();
    assert_eq!(found.realm.host_str(), Some("one.example"));
    assert_eq!(find_bearer(["Basic realm=\"x\""]), None);
}

#[test]
fn the_first_realm_and_service_win() {
    let challenge = parse_bearer(
        "Bearer realm=\"https://first.example/t\", realm=\"https://second.example/t\", service=a, service=b",
    )
    .unwrap();
    assert_eq!(challenge.realm.host_str(), Some("first.example"));
    assert_eq!(challenge.service.as_deref(), Some("a"));
}

#[test]
fn an_oversized_challenge_is_ignored() {
    let long = format!("Bearer realm=\"https://a.example/{}\"", "x".repeat(5000));
    assert_eq!(parse_bearer(&long), None);
}

#[test]
fn parameters_past_the_cap_are_not_read() {
    let mut value = String::from("Bearer ");
    for i in 0..40 {
        let _ = write!(value, "p{i}=v, ");
    }
    value.push_str("realm=\"https://late.example/t\"");
    assert_eq!(parse_bearer(&value), None);
}

proptest! {
    #[test]
    fn parsing_never_panics(text in ".{0,400}") {
        let _ = parse_bearer(&text);
        let _ = find_bearer([text.as_str()]);
    }

    #[test]
    fn quote_soup_never_panics(text in "[A-Za-z0-9=,\"\\\\ ]{0,200}") {
        let _ = parse_bearer(&format!("Bearer {text}"));
    }
}
