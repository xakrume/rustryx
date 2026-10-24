//! Rule tests. Each rule has a positive and a negative case; every "must not"
//! requirement is a `must_not_*` test.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ipnet::IpNet;

use crate::error::ConfigIssues;
use crate::schema::{BlobKind, Environment, GroupStrategy, LockBackendKind};
use crate::secret::ConfigSecret;
use crate::{AppConfig, validate};

// Wrapping in `Some` keeps the call sites (`c.database.url = secret(..)`) short: every
// secret field of the schema is an `Option`.
#[allow(clippy::unnecessary_wraps)]
fn secret(value: &str) -> Option<ConfigSecret> {
    Some(ConfigSecret::new(value))
}

/// A production configuration that satisfies every rule.
fn valid() -> AppConfig {
    let mut c = AppConfig::default();
    c.database.url = secret("postgres://rustryx:pw@db.example:5432/rustryx?sslmode=verify-full");
    c.auth.bootstrap_admin.password = secret("Tr0ub4dor&3-horse-battery");
    c.auth.internal_jwt_key =
        secret("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----");
    c.auth.cookie_key = secret(&STANDARD.encode([7_u8; 32]));
    c.observability.metrics_token = secret("0123456789abcdef0123");
    c.database.sealing_key = secret(SEALING_KEY);
    // TLS ends at an ingress: without a trusted proxy no request counts as HTTPS.
    c.server.trusted_proxies = vec!["10.0.0.0/8".parse().expect("cidr")];
    c
}

const SEALING_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn dev() -> AppConfig {
    let mut c = valid();
    c.environment = Environment::Development;
    c
}

fn issues(config: &AppConfig) -> ConfigIssues {
    config.validate().err().unwrap_or(ConfigIssues(Vec::new()))
}

fn warnings(config: &AppConfig) -> Vec<String> {
    config
        .validate()
        .expect("valid config")
        .join("\n")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[track_caller]
fn assert_fails(config: &AppConfig, key: &str) {
    let found = issues(config);
    assert!(
        found.mentions(key),
        "expected an issue for {key}, got: {found}"
    );
}

#[track_caller]
fn assert_ok(config: &AppConfig) {
    let found = issues(config);
    assert!(found.is_empty(), "unexpected issues: {found}");
}

fn ldap_enabled() -> AppConfig {
    let mut c = valid();
    let l = &mut c.auth.ldap;
    l.enabled = true;
    l.url = Some("ldaps://ldap.example:636".into());
    l.bind_dn = Some("cn=svc,dc=example,dc=org".into());
    l.bind_password = secret("svc-password");
    l.user_base_dn = Some("ou=people,dc=example,dc=org".into());
    c
}

fn oidc_enabled() -> AppConfig {
    let mut c = valid();
    c.server.base_url = Some("https://rustryx.example".into());
    let o = &mut c.auth.oidc;
    o.enabled = true;
    o.issuer = Some("https://sso.example/realms/main".into());
    o.client_id = Some("rustryx-ui".into());
    o.client_secret = secret("client-secret");
    o.admin_groups = vec!["/rustryx/admins".into()];
    c
}

#[test]
fn valid_production_configuration_passes_without_warnings() {
    assert_ok(&valid());
    assert!(warnings(&valid()).iter().all(String::is_empty));
}

#[test]
fn must_not_start_production_without_required_secrets() {
    let found = issues(&AppConfig::default());
    for key in [
        "database.url",
        "auth.bootstrap_admin.password",
        "auth.internal_jwt_key",
        "auth.cookie_key",
        "observability.metrics_token",
    ] {
        assert!(found.mentions(key), "missing {key}: {found}");
    }
    assert!(found.len() >= 5, "all errors come in one pass");
}

#[test]
fn jit_allowlist_is_only_meaningful_in_allowlist_mode() {
    use crate::JitMode;
    let mut c = valid();
    c.auth.jit_mode = JitMode::Allowlist;
    assert!(
        warnings(&c)
            .iter()
            .any(|w| w.contains("auth.jit_allowlist")),
        "an empty list in allowlist mode lets nobody in"
    );
    c.auth.jit_allowlist = vec!["alice".into()];
    assert!(warnings(&c).iter().all(String::is_empty));
    c.auth.jit_mode = JitMode::Jit;
    assert!(
        warnings(&c).iter().any(|w| w.contains("ignored")),
        "a list that nothing reads is reported"
    );
}

#[test]
fn must_not_accept_blank_or_padded_allowlist_entries() {
    for bad in ["", "  ", " alice", "alice "] {
        let mut c = valid();
        c.auth.jit_allowlist = vec![bad.into()];
        assert_fails(&c, "auth.jit_allowlist");
    }
}

#[test]
fn missing_secret_message_tells_how_to_set_it() {
    let text = issues(&AppConfig::default()).to_string();
    assert!(
        text.contains("RUSTRYX_DATABASE_URL or RUSTRYX_DATABASE_URL_FILE"),
        "{text}"
    );
}

#[test]
fn must_not_accept_default_or_weak_admin_password_in_production() {
    for weak in [
        "admin123",
        "admin",
        "Admin123!",
        "Password2024!",
        "changeme",
        "rustryx",
        "nexspence123",
        "aaaaaaaaaaaaaaaa",
        "AdminAdmin",
    ] {
        let mut c = valid();
        c.auth.bootstrap_admin.password = secret(weak);
        assert_fails(&c, "auth.bootstrap_admin.password");
    }
}

#[test]
fn must_not_accept_admin_password_equal_to_username() {
    let mut c = valid();
    c.auth.bootstrap_admin.username = "operator-account".into();
    c.auth.bootstrap_admin.password = secret("operator-account");
    assert_fails(&c, "auth.bootstrap_admin.password");
}

#[test]
fn must_not_accept_empty_admin_password() {
    for empty in ["", "   "] {
        let mut c = valid();
        c.auth.bootstrap_admin.password = secret(empty);
        assert_fails(&c, "auth.bootstrap_admin.password");
        let mut d = dev();
        d.auth.bootstrap_admin.password = secret(empty);
        assert_fails(&d, "auth.bootstrap_admin.password");
    }
}

#[test]
fn must_not_accept_short_admin_password_in_production() {
    let mut c = valid();
    c.auth.bootstrap_admin.password = secret("Xk9#mP2$vL7");
    assert_fails(&c, "auth.bootstrap_admin.password");
}

#[test]
fn development_tolerates_weak_password_with_a_warning() {
    let mut c = dev();
    c.auth.bootstrap_admin.password = secret("admin123");
    assert_ok(&c);
    assert!(warnings(&c).iter().any(|w| w.contains("weak password")));
    c.auth.bootstrap_admin.password = None;
    assert_ok(&c);
}

#[test]
fn bootstrap_username_must_be_safe_for_basic_auth() {
    for bad in ["", "a:b", "has space", "tab\t", &"x".repeat(65)] {
        let mut c = valid();
        c.auth.bootstrap_admin.username = bad.into();
        assert_fails(&c, "auth.bootstrap_admin.username");
    }
}

#[test]
fn key_material_is_checked_for_shape() {
    let mut c = valid();
    c.auth.internal_jwt_key = secret("not a pem");
    assert_fails(&c, "auth.internal_jwt_key");
    let mut c = valid();
    c.auth.cookie_key = secret(&STANDARD.encode([1_u8; 16]));
    assert_fails(&c, "auth.cookie_key");
    c.auth.cookie_key = secret("%%% not base64");
    assert_fails(&c, "auth.cookie_key");
    let mut c = valid();
    c.auth.cookie_key = secret("");
    assert_fails(&c, "auth.cookie_key");
    c.auth.internal_jwt_key = secret(" ");
    assert_fails(&c, "auth.internal_jwt_key");
}

#[test]
fn development_does_not_require_keys_but_checks_them_when_set() {
    let mut c = dev();
    c.auth.internal_jwt_key = None;
    c.auth.cookie_key = None;
    assert_ok(&c);
    c.auth.cookie_key = secret("short");
    assert_fails(&c, "auth.cookie_key");
}

#[test]
fn sessions_cannot_exceed_the_fixed_limits() {
    let mut c = valid();
    c.auth.session.idle_timeout_secs = 1801;
    assert_fails(&c, "auth.session.idle_timeout_secs");
    let mut c = valid();
    c.auth.session.absolute_timeout_secs = 28_801;
    assert_fails(&c, "auth.session.absolute_timeout_secs");
    let mut c = valid();
    c.auth.session.idle_timeout_secs = 1000;
    c.auth.session.absolute_timeout_secs = 900;
    assert_fails(&c, "auth.session.idle_timeout_secs");
}

#[test]
fn anonymous_access_in_production_warns() {
    let mut c = valid();
    c.auth.anonymous_enabled = true;
    assert!(
        warnings(&c)
            .iter()
            .any(|w| w.contains("auth.anonymous_enabled"))
    );
}

// ---- LDAP -----------------------------------------------------------------

#[test]
fn valid_ldap_configuration_passes() {
    assert_ok(&ldap_enabled());
}

#[test]
fn must_not_accept_plaintext_ldap() {
    let mut c = ldap_enabled();
    c.auth.ldap.url = Some("ldap://ldap.example:389".into());
    c.auth.ldap.starttls = false;
    assert_fails(&c, "auth.ldap.url");
}

#[test]
fn must_not_accept_plaintext_ldap_even_while_disabled() {
    let mut c = valid();
    c.auth.ldap.url = Some("ldap://ldap.example".into());
    assert_fails(&c, "auth.ldap.url");
}

#[test]
fn ldap_with_starttls_is_accepted_and_ldaps_with_starttls_is_not() {
    let mut c = ldap_enabled();
    c.auth.ldap.url = Some("ldap://ldap.example:389".into());
    c.auth.ldap.starttls = true;
    assert_ok(&c);
    let mut c = ldap_enabled();
    c.auth.ldap.starttls = true;
    assert_fails(&c, "auth.ldap.starttls");
}

#[test]
fn ldap_url_must_be_a_url_with_ldap_scheme_and_host() {
    for bad in [
        "http://ldap.example",
        "ldaps://",
        "not a url",
        "ldaps:///path",
    ] {
        let mut c = ldap_enabled();
        c.auth.ldap.url = Some(bad.into());
        assert_fails(&c, "auth.ldap.url");
    }
}

#[test]
fn enabled_ldap_requires_its_settings() {
    let mut c = valid();
    c.auth.ldap.enabled = true;
    for key in [
        "auth.ldap.url",
        "auth.ldap.bind_dn",
        "auth.ldap.bind_password",
        "auth.ldap.user_base_dn",
    ] {
        assert_fails(&c, key);
    }
}

#[test]
fn ldap_filters_need_their_placeholders() {
    let mut c = ldap_enabled();
    c.auth.ldap.search_filter = "(uid=fixed)".into();
    assert_fails(&c, "auth.ldap.search_filter");
    let mut c = ldap_enabled();
    c.auth.ldap.group_strategy = GroupStrategy::MemberSearch;
    c.auth.ldap.group_base_dn = Some("ou=groups,dc=example,dc=org".into());
    assert_ok(&c);
    c.auth.ldap.group_filter = "(member=x)".into();
    assert_fails(&c, "auth.ldap.group_filter");
    c.auth.ldap.group_base_dn = None;
    assert_fails(&c, "auth.ldap.group_base_dn");
    let mut c = ldap_enabled();
    c.auth.ldap.group_attribute = " ".into();
    assert_fails(&c, "auth.ldap.group_attribute");
}

#[test]
fn ldap_admin_groups_must_be_dns() {
    let mut c = ldap_enabled();
    c.auth.ldap.admin_groups = vec!["admins".into()];
    assert_fails(&c, "auth.ldap.admin_groups");
    c.auth.ldap.admin_groups = vec!["cn=admins,ou=groups,dc=example,dc=org".into()];
    assert_ok(&c);
}

#[test]
fn must_not_trust_ldap_groups_longer_than_five_minutes() {
    let mut c = ldap_enabled();
    c.auth.ldap.groups_cache_ttl_secs = 301;
    assert_fails(&c, "auth.ldap.groups_cache_ttl_secs");
    c.auth.ldap.groups_cache_ttl_secs = 0;
    assert_fails(&c, "auth.ldap.groups_cache_ttl_secs");
}

#[test]
fn ldap_limits_are_bounded() {
    let mut c = ldap_enabled();
    c.auth.ldap.connect_timeout_secs = 0;
    c.auth.ldap.bind_timeout_secs = 61;
    c.auth.ldap.search_timeout_secs = 0;
    c.auth.ldap.nested_group_depth = 11;
    c.auth.ldap.max_groups = 0;
    for key in [
        "auth.ldap.connect_timeout_secs",
        "auth.ldap.bind_timeout_secs",
        "auth.ldap.search_timeout_secs",
        "auth.ldap.nested_group_depth",
        "auth.ldap.max_groups",
    ] {
        assert_fails(&c, key);
    }
}

// ---- OIDC -----------------------------------------------------------------

#[test]
fn valid_oidc_configuration_passes() {
    assert_ok(&oidc_enabled());
}

#[test]
fn enabled_oidc_requires_its_settings() {
    let mut c = valid();
    c.auth.oidc.enabled = true;
    for key in [
        "auth.oidc.issuer",
        "auth.oidc.client_id",
        "auth.oidc.client_secret",
        "server.base_url",
    ] {
        assert_fails(&c, key);
    }
}

#[test]
fn must_not_make_default_role_nx_admin() {
    let mut c = oidc_enabled();
    c.auth.oidc.default_role = Some("nx-admin".into());
    assert_fails(&c, "auth.oidc.default_role");
    let mut c = valid();
    c.auth.oidc.default_role = Some("nx-admin".into());
    assert_fails(&c, "auth.oidc.default_role");
    let mut c = oidc_enabled();
    c.auth.oidc.default_role = Some("nx-developer".into());
    assert_ok(&c);
    c.auth.oidc.default_role = Some(" ".into());
    assert_fails(&c, "auth.oidc.default_role");
}

#[test]
fn oidc_admin_groups_must_be_full_paths() {
    for bad in ["rustryx/admins", "admins", "/", " /rustryx/admins"] {
        let mut c = oidc_enabled();
        c.auth.oidc.admin_groups = vec![bad.into()];
        assert_fails(&c, "auth.oidc.admin_groups");
    }
}

#[test]
fn must_not_request_offline_access() {
    let mut c = oidc_enabled();
    c.auth.oidc.scopes.push("offline_access".into());
    assert_fails(&c, "auth.oidc.scopes");
    let mut c = oidc_enabled();
    c.auth.oidc.scopes = vec!["profile".into()];
    assert_fails(&c, "auth.oidc.scopes");
    let mut c = oidc_enabled();
    c.auth.oidc.scopes.push("two words".into());
    assert_fails(&c, "auth.oidc.scopes");
}

#[test]
fn must_not_accept_a_plain_http_oidc_issuer_in_any_environment() {
    for env in [Environment::Production, Environment::Development] {
        let mut c = oidc_enabled();
        c.environment = env;
        c.auth.oidc.issuer = Some("http://localhost:8080/realms/main".into());
        assert_fails(&c, "auth.oidc.issuer");
        let mut c = oidc_enabled();
        c.environment = env;
        c.auth.oidc.public_issuer_url = Some("http://public.example/realms/main".into());
        assert_fails(&c, "auth.oidc.public_issuer_url");
    }
}

#[test]
fn production_base_url_must_be_https_but_development_may_use_http_on_localhost() {
    let mut c = oidc_enabled();
    c.server.base_url = Some("http://rustryx.example".into());
    assert_fails(&c, "server.base_url");
    let mut d = oidc_enabled();
    d.environment = Environment::Development;
    d.server.base_url = Some("http://localhost:8081".into());
    assert_ok(&d);
}

#[test]
fn must_not_accept_a_non_loopback_http_base_url_for_the_browser_login_in_development() {
    let mut d = oidc_enabled();
    d.environment = Environment::Development;
    d.server.base_url = Some("http://repo.test:8081".into());
    assert_fails(&d, "server.base_url");
    // Without a client secret there is no browser login and no redirect URI to protect.
    d.auth.oidc.client_secret = None;
    d.server.base_url = Some("http://repo.test:8081".into());
    assert!(
        !issues(&d).mentions("server.base_url"),
        "bearer validation only needs the issuer"
    );
}

#[test]
fn oidc_issuer_shape_is_checked() {
    for bad in [
        "not a url",
        "https://sso.example/realms/main?x=1",
        "ftp://sso.example",
    ] {
        let mut c = oidc_enabled();
        c.auth.oidc.issuer = Some(bad.into());
        assert_fails(&c, "auth.oidc.issuer");
    }
    let mut c = oidc_enabled();
    c.auth.oidc.public_issuer_url = Some("nonsense".into());
    assert_fails(&c, "auth.oidc.public_issuer_url");
    c.auth.oidc.public_issuer_url = Some("https://public.example/realms/main".into());
    assert_ok(&c);
}

#[test]
fn must_not_read_groups_from_role_claims_or_nested_claims() {
    for bad in [
        "realm_access",
        "resource_access",
        "realm_access.roles",
        "",
        " ",
    ] {
        let mut c = oidc_enabled();
        c.auth.oidc.groups_claim = bad.into();
        assert_fails(&c, "auth.oidc.groups_claim");
    }
}

#[test]
fn must_not_allow_clock_skew_above_sixty_seconds() {
    let mut c = oidc_enabled();
    c.auth.oidc.clock_skew_secs = 61;
    assert_fails(&c, "auth.oidc.clock_skew_secs");
}

// ---- server and upstream --------------------------------------------------

#[test]
fn must_not_trust_every_address_as_proxy() {
    for all in ["0.0.0.0/0", "::/0"] {
        let mut c = valid();
        c.server.trusted_proxies = vec![all.parse::<IpNet>().expect("cidr")];
        assert_fails(&c, "server.trusted_proxies");
    }
    let mut c = valid();
    c.server.trusted_proxies = vec!["10.0.0.0/8".parse().expect("cidr")];
    assert_ok(&c);
}

#[test]
fn must_not_allow_wildcard_cors_origin() {
    for bad in [
        "*",
        "https://*.example",
        "https://a.example/path",
        "https://a.example/",
        "nonsense",
        "ftp://a.example",
    ] {
        let mut c = valid();
        c.server.cors_allowed_origins = vec![bad.into()];
        assert_fails(&c, "server.cors_allowed_origins");
    }
    let mut c = valid();
    c.server.cors_allowed_origins = vec![
        "https://ui.example".into(),
        "https://ui.example:8443".into(),
    ];
    assert_ok(&c);
    c.server.cors_allowed_origins = vec!["http://localhost:3000".into()];
    assert!(warnings(&c).iter().any(|w| w.contains("http origin")));
}

#[test]
fn base_url_must_be_a_clean_http_url() {
    for bad in [
        "nonsense",
        "https://u:p@host",
        "https://host?x=1",
        "https://host#f",
        "ftp://host",
    ] {
        let mut c = valid();
        c.server.base_url = Some(bad.into());
        assert_fails(&c, "server.base_url");
    }
}

#[test]
fn server_timeouts_are_bounded() {
    let mut c = valid();
    c.server.shutdown_timeout_secs = 0;
    c.server.drain_delay_secs = 301;
    assert_fails(&c, "server.shutdown_timeout_secs");
    assert_fails(&c, "server.drain_delay_secs");
}

#[test]
fn upstream_schemes_must_include_https_and_pair_http_with_hosts() {
    let mut c = valid();
    c.upstream.allowed_schemes = vec!["http".into()];
    assert_fails(&c, "upstream.allowed_schemes");
    let mut c = valid();
    c.upstream.allowed_schemes = vec!["https".into(), "ftp".into()];
    assert_fails(&c, "upstream.allowed_schemes");
    let mut c = valid();
    c.upstream.allowed_schemes = vec!["https".into(), "http".into()];
    assert_fails(&c, "upstream.allowed_schemes");
    c.upstream.allow_insecure_http_hosts = vec!["mirror.internal".into()];
    assert_ok(&c);
    assert!(warnings(&c).iter().any(|w| w.contains("plain http")));
    let mut c = valid();
    c.upstream.allow_insecure_http_hosts = vec!["mirror.internal".into()];
    assert_fails(&c, "upstream.allow_insecure_http_hosts");
}

#[test]
fn upstream_host_lists_reject_urls_ports_and_wildcards_in_the_wrong_place() {
    let mut c = valid();
    c.upstream.allowed_hosts = vec!["repo.maven.apache.org".into(), "*.pythonhosted.org".into()];
    assert_ok(&c);
    for bad in [
        "https://repo.example",
        "repo.example:443",
        "*",
        "a..b",
        "",
        "*.*.example",
    ] {
        let mut c = valid();
        c.upstream.allowed_hosts = vec![bad.into()];
        assert_fails(&c, "upstream.allowed_hosts");
    }
    let mut c = valid();
    c.upstream.allowed_schemes = vec!["https".into(), "http".into()];
    c.upstream.allow_insecure_http_hosts = vec!["*.internal".into()];
    assert_fails(&c, "upstream.allow_insecure_http_hosts");
}

#[test]
fn must_not_allow_private_cidrs_that_cover_loopback_or_metadata() {
    for bad in [
        "0.0.0.0/0",
        "::/0",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "169.254.169.254/32",
        "::1/128",
        "fd00:ec2::/32",
        "0.0.0.0/1",
    ] {
        let mut c = valid();
        c.upstream.allow_private_cidrs = vec![bad.parse::<IpNet>().expect("cidr")];
        assert_fails(&c, "upstream.allow_private_cidrs");
    }
    let mut c = valid();
    c.upstream.allow_private_cidrs = vec!["10.20.0.0/16".parse().expect("cidr")];
    assert_ok(&c);
}

#[test]
fn upstream_limits_follow_the_requirements() {
    let mut c = valid();
    c.upstream.max_redirects = 4;
    c.upstream.max_attempts = 4;
    c.upstream.connect_timeout_secs = 0;
    assert_fails(&c, "upstream.max_redirects");
    assert_fails(&c, "upstream.max_attempts");
    assert_fails(&c, "upstream.connect_timeout_secs");
    c.upstream.max_attempts = 0;
    assert_fails(&c, "upstream.max_attempts");
}

// ---- storage --------------------------------------------------------------

#[test]
fn database_url_must_be_postgres_with_a_host() {
    for bad in [
        "mysql://u:p@h/db",
        "postgres://",
        "not a url",
        "postgres:///db",
    ] {
        let mut c = valid();
        c.database.url = secret(bad);
        assert_fails(&c, "database.url");
    }
    let mut c = valid();
    c.database.url = secret("postgresql:///db?host=/var/run/postgresql");
    assert_ok(&c);
}

#[test]
fn must_not_accept_blank_database_url() {
    let mut c = valid();
    c.database.url = secret("  ");
    assert_fails(&c, "database.url");
    c.database.url = None;
    assert_fails(&c, "database.url");
}

#[test]
fn direct_url_is_validated_when_set() {
    let mut c = valid();
    c.database.direct_url = secret("postgres://u:p@direct:5432/db?sslmode=verify-full");
    assert_ok(&c);
    c.database.direct_url = secret("mysql://x");
    assert_fails(&c, "database.direct_url");
    c.database.direct_url = secret("");
    assert_fails(&c, "database.direct_url");
}

#[test]
fn must_not_accept_unverified_postgres_tls_in_production() {
    for url in [
        "postgres://u:p@h:5432/db",
        "postgres://u:p@h:5432/db?sslmode=require",
        "postgres://u:p@h:5432/db?sslmode=prefer",
        "postgres://u:p@h:5432/db?sslmode=disable",
        // verify-ca does not check the host name, so it is refused as well.
        "postgres://u:p@h:5432/db?sslmode=verify-ca",
    ] {
        let mut c = valid();
        c.database.url = secret(url);
        assert_fails(&c, "database.url");
        let mut direct = valid();
        direct.database.direct_url = secret(url);
        assert_fails(&direct, "database.direct_url");
    }
    // The message names the rule and never the URL (it carries the password).
    let mut c = valid();
    c.database.url = secret("postgres://u:hunter2-canary@h:5432/db?sslmode=require");
    assert!(!issues(&c).to_string().contains("hunter2-canary"));
}

#[test]
fn verify_full_and_unix_sockets_pass_in_production_and_development_is_lenient() {
    let mut c = valid();
    c.database.url = secret("postgres://u:p@h:5432/db?sslmode=verify-full&connect_timeout=3");
    assert_ok(&c);
    // A Unix socket never crosses the network, TLS does not apply.
    c.database.url = secret("postgres:///db?host=/var/run/postgresql&user=rustryx");
    assert_ok(&c);
    let mut d = dev();
    d.database.url = secret("postgres://u:p@h:5432/db");
    assert_ok(&d);
    assert!(warnings(&d).iter().all(String::is_empty));
}

#[test]
fn database_pool_and_timeouts_are_bounded() {
    let mut c = valid();
    c.database.pool_size = 0;
    c.database.acquire_timeout_secs = 0;
    c.database.connect_timeout_secs = 301;
    c.database.statement_timeout_secs = 0;
    for key in [
        "database.pool_size",
        "database.acquire_timeout_secs",
        "database.connect_timeout_secs",
        "database.statement_timeout_secs",
    ] {
        assert_fails(&c, key);
    }
}

#[test]
fn local_blob_store_needs_an_absolute_path_in_production() {
    let mut c = valid();
    c.blob.local_path = "relative/blobs".into();
    assert_fails(&c, "blob.local_path");
    let mut d = dev();
    d.blob.local_path = "relative/blobs".into();
    assert_ok(&d);
    d.blob.local_path = "".into();
    assert_fails(&d, "blob.local_path");
}

#[test]
fn blob_store_name_and_sizes_are_checked() {
    let mut c = valid();
    c.blob.store_name = "bad name/".into();
    c.blob.s3_part_size_mb = 4;
    c.blob.local_min_free_percent = 95;
    for key in [
        "blob.store_name",
        "blob.s3_part_size_mb",
        "blob.local_min_free_percent",
    ] {
        assert_fails(&c, key);
    }
    c.blob.store_name = String::new();
    assert_fails(&c, "blob.store_name");
}

#[test]
fn s3_blob_store_requires_its_settings() {
    let mut c = valid();
    c.blob.kind = BlobKind::S3;
    for key in [
        "blob.s3_endpoint",
        "blob.s3_bucket",
        "blob.s3_region",
        "blob.s3_access_key",
        "blob.s3_secret_key",
    ] {
        assert_fails(&c, key);
    }
    c.blob.s3_endpoint = Some("https://s3.example".into());
    c.blob.s3_bucket = Some("rustryx".into());
    c.blob.s3_region = Some("us-east-1".into());
    c.blob.s3_access_key = secret("ak");
    c.blob.s3_secret_key = secret("sk");
    assert_ok(&c);
    c.blob.s3_endpoint = Some("http://minio:9000".into());
    assert!(warnings(&c).iter().any(|w| w.contains("blob.s3_endpoint")));
    c.blob.s3_endpoint = Some("ftp://x".into());
    assert_fails(&c, "blob.s3_endpoint");
    c.blob.s3_endpoint = Some("nonsense".into());
    assert_fails(&c, "blob.s3_endpoint");
    c.blob.s3_bucket = Some(" ".into());
    assert_fails(&c, "blob.s3_bucket");
}

#[test]
fn redis_lock_backend_needs_a_redis_url() {
    let mut c = valid();
    c.lock.backend = LockBackendKind::Redis;
    assert_fails(&c, "lock.redis_url");
    c.lock.redis_url = secret("http://redis");
    assert_fails(&c, "lock.redis_url");
    c.lock.redis_url = secret("redis://:pw@redis:6379");
    assert_ok(&c);
    assert!(warnings(&c).iter().any(|w| w.contains("rediss://")));
    c.lock.redis_url = secret("rediss://:pw@redis:6379");
    assert!(warnings(&c).iter().all(String::is_empty));
    c.lock.redis_url = secret("nonsense");
    assert_fails(&c, "lock.redis_url");
}

#[test]
fn postgres_lock_backend_ignores_redis_url_with_a_warning() {
    let mut c = valid();
    c.lock.redis_url = secret("redis://redis");
    assert!(warnings(&c).iter().any(|w| w.contains("ignored")));
    c.lock.ttl_secs = 4;
    assert_fails(&c, "lock.ttl_secs");
}

#[test]
fn must_keep_gc_min_age_above_the_request_timeout() {
    let mut c = valid();
    c.gc.min_age_secs = c.limits.request_timeout_secs;
    assert_fails(&c, "gc.min_age_secs");
    c.gc.min_age_secs = c.limits.request_timeout_secs + 1;
    assert_ok(&c);
    c.gc.min_age_secs = 1;
    c.gc.enabled = false;
    assert_ok(&c);
}

#[test]
fn gc_bounds_apply_when_enabled() {
    let mut c = valid();
    c.gc.interval_secs = 10;
    c.gc.batch_size = 0;
    assert_fails(&c, "gc.interval_secs");
    assert_fails(&c, "gc.batch_size");
}

// ---- limits and observability --------------------------------------------

#[test]
fn limits_are_bounded() {
    let mut c = valid();
    c.limits.max_body_mb = 0;
    c.limits.max_blob_bytes = 0;
    c.limits.request_timeout_secs = 0;
    c.limits.header_read_timeout_secs = 301;
    c.limits.body_idle_timeout_secs = 0;
    c.limits.max_connections = 0;
    c.limits.max_in_flight_requests = 0;
    c.limits.blocking_permits = 5000;
    c.limits.password_permits = 300;
    c.limits.permit_wait_ms = 0;
    for key in [
        "limits.max_body_mb",
        "limits.max_blob_bytes",
        "limits.request_timeout_secs",
        "limits.header_read_timeout_secs",
        "limits.body_idle_timeout_secs",
        "limits.max_connections",
        "limits.max_in_flight_requests",
        "limits.blocking_permits",
        "limits.password_permits",
        "limits.permit_wait_ms",
    ] {
        assert_fails(&c, key);
    }
}

#[test]
fn rate_limit_values_are_checked_only_when_enabled() {
    let mut c = valid();
    c.limits.rate_limit.requests_per_second = 0;
    c.limits.rate_limit.burst = 0;
    assert_fails(&c, "limits.rate_limit.requests_per_second");
    assert_fails(&c, "limits.rate_limit.burst");
    c.limits.rate_limit.enabled = false;
    assert_ok(&c);
}

#[test]
fn log_level_accepts_the_documented_subset_only() {
    for ok in [
        "info",
        "debug,rustryx_api=trace",
        "warn, hyper=error",
        "OFF",
    ] {
        let mut c = valid();
        c.observability.log_level = ok.into();
        assert_ok(&c);
    }
    for bad in [
        "",
        " , ",
        "loud",
        "rustryx=loud",
        "=info",
        "a[b]=info",
        "info,",
    ] {
        let mut c = valid();
        c.observability.log_level = bad.into();
        if bad == "info," {
            assert_ok(&c);
        } else {
            assert_fails(&c, "observability.log_level");
        }
    }
}

#[test]
fn otlp_settings_are_checked() {
    let mut c = valid();
    c.observability.otlp_endpoint = Some("http://collector:4317".into());
    c.observability.otlp_sample_ratio = 0.5;
    assert_ok(&c);
    c.observability.otlp_endpoint = Some("nonsense".into());
    assert_fails(&c, "observability.otlp_endpoint");
    c.observability.otlp_endpoint = Some("ftp://collector".into());
    assert_fails(&c, "observability.otlp_endpoint");
    c.observability.otlp_endpoint = Some("https://u:p@collector".into());
    assert_fails(&c, "observability.otlp_endpoint");
    let mut c = valid();
    c.observability.otlp_sample_ratio = 1.5;
    assert_fails(&c, "observability.otlp_sample_ratio");
    c.observability.otlp_sample_ratio = f64::NAN;
    assert_fails(&c, "observability.otlp_sample_ratio");
    c.observability.otlp_sample_ratio = 0.1;
    c.observability.service_name = " ".into();
    assert_fails(&c, "observability.service_name");
}

#[test]
fn must_not_serve_metrics_without_token_in_production() {
    let mut c = valid();
    c.observability.metrics_token = None;
    assert_fails(&c, "observability.metrics_token");
    c.observability.metrics_public = true;
    assert_ok(&c);
    c.observability.metrics_public = false;
    c.observability.metrics_enabled = false;
    assert_ok(&c);
}

#[test]
fn metrics_token_strength_and_leniency_in_development() {
    let mut c = valid();
    c.observability.metrics_token = secret("short");
    assert_fails(&c, "observability.metrics_token");
    c.observability.metrics_token = secret("  ");
    assert_fails(&c, "observability.metrics_token");
    let mut d = dev();
    d.observability.metrics_token = None;
    assert_ok(&d);
    assert!(warnings(&d).iter().any(|w| w.contains("without a token")));
    let mut c = valid();
    c.observability.metrics_public = true;
    assert!(warnings(&c).iter().any(|w| w.contains("is ignored")));
}

#[test]
fn must_not_leak_configured_secrets_in_validation_messages() {
    let canary = "canary-SECRET-value-42";
    let mut c = AppConfig::default();
    c.database.url = secret(&format!("mysql://u:{canary}@host/db"));
    c.auth.bootstrap_admin.password = secret(canary);
    c.auth.internal_jwt_key = secret(canary);
    c.auth.cookie_key = secret(canary);
    c.auth.ldap.url = Some("ldap://x".into());
    c.auth.ldap.enabled = true;
    c.auth.ldap.bind_password = secret(canary);
    c.blob.kind = BlobKind::S3;
    c.blob.s3_access_key = secret(canary);
    c.blob.s3_secret_key = secret(canary);
    c.lock.backend = LockBackendKind::Redis;
    c.lock.redis_url = secret(&format!("http://:{canary}@redis"));
    c.observability.metrics_token = secret(canary);
    let found = issues(&c);
    assert!(found.len() > 5);
    let text = found.to_string();
    assert!(
        !text.contains(canary),
        "validation output leaked a secret: {text}"
    );
    let all_warnings = validate::run(&c).warnings.join("\n");
    assert!(!all_warnings.contains(canary));
}

#[test]
fn weak_password_detector_accepts_strong_values() {
    use super::auth::is_weak_password;
    assert!(!is_weak_password("Tr0ub4dor&3-horse-battery", "admin"));
    assert!(!is_weak_password("k9#Lm2$vQx7!pZ", "admin"));
    assert!(is_weak_password("", "admin"));
    assert!(is_weak_password("ADMIN123", "someone"));
}

// ---- sealing key ring ----------------------------------------------------------------

#[test]
fn must_not_start_production_without_a_sealing_key() {
    let mut c = valid();
    c.database.sealing_key = None;
    assert_fails(&c, "database.sealing_key");
    assert!(
        issues(&c)
            .to_string()
            .contains("RUSTRYX_DATABASE_SEALING_KEY or RUSTRYX_DATABASE_SEALING_KEY_FILE")
    );
    // Development may run without it; it only warns, because storing the password of a
    // proxy repository then fails.
    let mut d = dev();
    d.database.sealing_key = None;
    assert_ok(&d);
    assert!(
        warnings(&d)
            .iter()
            .any(|w| w.contains("database.sealing_key"))
    );
}

#[test]
fn must_not_accept_a_malformed_or_template_sealing_key() {
    for bad in [
        String::new(),
        "tooshort".to_owned(),
        "zz".repeat(32),
        "00".repeat(32),
        format!("{}0", "ab".repeat(32)),
    ] {
        let mut c = valid();
        c.database.sealing_key = secret(&bad);
        assert_fails(&c, "database.sealing_key");
    }
    let mut c = valid();
    c.database.sealing_key = secret("not-hex-canary-value");
    assert!(!issues(&c).to_string().contains("not-hex-canary-value"));
}

#[test]
fn sealing_key_id_and_previous_keys_are_checked() {
    let mut c = valid();
    c.database.sealing_key_id = "has.dot".into();
    assert_fails(&c, "database.sealing_key_id");
    c.database.sealing_key_id = String::new();
    assert_fails(&c, "database.sealing_key_id");

    let mut c = valid();
    c.database.sealing_previous_keys = secret(&format!("k0:{SEALING_KEY}"));
    assert_ok(&c);

    c.database.sealing_previous_keys = secret("garbage");
    assert_fails(&c, "database.sealing_previous_keys");
    // The current id must name exactly one key.
    c.database.sealing_previous_keys = secret(&format!("k1:{SEALING_KEY}"));
    assert_fails(&c, "database.sealing_previous_keys");
}

#[test]
fn direct_url_defaults_to_the_pooled_url_and_is_never_taken_from_elsewhere() {
    let mut c = valid();
    assert_eq!(
        c.database.effective_direct_url().expect("url").expose(),
        c.database.url.as_ref().expect("url").expose()
    );
    c.database.direct_url = secret("postgres://u:p@direct.example:5432/db?sslmode=verify-full");
    assert!(
        c.database
            .effective_direct_url()
            .expect("url")
            .expose()
            .contains("direct.example")
    );
    c.database.direct_url = secret("   ");
    assert_fails(&c, "database.direct_url");
}

// ---- internal JWT ring ---------------------------------------------------------------

fn pem_block(label: &str) -> String {
    format!("-----BEGIN {label}-----\nAAAA\n-----END {label}-----")
}

#[test]
fn jwt_ring_accepts_previous_keys_and_checks_ids() {
    let mut c = valid();
    c.auth.internal_jwt_key_id = "2026-10".into();
    c.auth.internal_jwt_previous_keys =
        secret(&format!("kid: 2026-09\n{}", pem_block("PUBLIC KEY")));
    assert_ok(&c);
    let ring = c.auth.jwt_ring().expect("parses").expect("configured");
    assert_eq!(ring.current_id, "2026-10");
    assert_eq!(ring.previous.len(), 1);

    c.auth.internal_jwt_key_id = "bad id".into();
    assert_fails(&c, "auth.internal_jwt_key_id");
}

#[test]
fn must_not_reuse_the_current_kid_for_a_previous_key() {
    let mut c = valid();
    c.auth.internal_jwt_previous_keys = secret(&format!("kid: k1\n{}", pem_block("PUBLIC KEY")));
    assert_fails(&c, "auth.internal_jwt_previous_keys");
}

#[test]
fn must_not_accept_a_malformed_previous_jwt_key_list() {
    for bad in [
        pem_block("PUBLIC KEY"),
        "kid: k0".to_owned(),
        format!(
            "kid: k0\n{}\nkid: k0\n{}",
            pem_block("PUBLIC KEY"),
            pem_block("PUBLIC KEY")
        ),
    ] {
        let mut c = valid();
        c.auth.internal_jwt_previous_keys = secret(&bad);
        assert_fails(&c, "auth.internal_jwt_previous_keys");
    }
}

// ---- OIDC authorized parties ---------------------------------------------------------

#[test]
fn authorized_parties_default_to_the_product_client_only() {
    let c = oidc_enabled();
    assert_eq!(c.auth.oidc.authorized_parties, Vec::<String>::new());
    assert_eq!(
        c.auth.oidc.effective_authorized_parties(),
        [c.auth.oidc.client_id.clone().expect("client")]
    );
}

#[test]
fn authorized_parties_add_the_cli_and_never_drop_the_product_client() {
    let mut c = oidc_enabled();
    c.auth.oidc.authorized_parties = vec!["rustryx-cli".into()];
    assert_ok(&c);
    let client = c.auth.oidc.client_id.clone().expect("client");
    assert_eq!(
        c.auth.oidc.effective_authorized_parties(),
        [client.clone(), "rustryx-cli".to_owned()]
    );
    c.auth.oidc.authorized_parties = vec![client.clone(), "rustryx-cli".into()];
    assert_eq!(
        c.auth.oidc.effective_authorized_parties().len(),
        2,
        "no duplicates"
    );
}

#[test]
fn must_not_accept_a_blank_or_joined_authorized_party() {
    for bad in ["", "  ", "a b", "rustryx-ui,rustryx-cli"] {
        let mut c = oidc_enabled();
        c.auth.oidc.authorized_parties = vec![bad.into()];
        assert_fails(&c, "auth.oidc.authorized_parties");
    }
}

// ---- Basic over plain HTTP -----------------------------------------------------------

#[test]
fn require_https_is_on_by_default_and_warns_when_nobody_can_vouch_for_it() {
    assert!(AppConfig::default().server.require_https);
    let mut c = valid();
    c.server.trusted_proxies.clear();
    assert!(
        warnings(&c)
            .iter()
            .any(|w| w.contains("server.require_https") && w.contains("always refused"))
    );
}

#[test]
fn must_warn_when_production_accepts_basic_over_plain_http() {
    let mut c = valid();
    c.server.require_https = false;
    assert_ok(&c);
    assert!(
        warnings(&c)
            .iter()
            .any(|w| w.contains("server.require_https") && w.contains("plain HTTP"))
    );
    let mut d = dev();
    d.server.require_https = false;
    assert!(
        warnings(&d).iter().all(String::is_empty),
        "local work is quiet"
    );
}

// ---- LDAP defaults -------------------------------------------------------------------

#[test]
fn ldap_group_cache_defaults_to_one_minute_inside_the_five_minute_window() {
    let c = AppConfig::default();
    assert_eq!(c.auth.ldap.groups_cache_ttl_secs, 60);
    assert!(c.auth.ldap.groups_cache_ttl_secs <= 300);
}

// ---- wave 2 foundation ---------------------------------------------------------------

#[test]
fn foundation_defaults_follow_the_design() {
    let c = AppConfig::default();
    assert_eq!(
        (
            c.tasks.max_concurrent_runs,
            c.tasks.scheduler_tick_secs,
            c.tasks.run_retention_days,
            c.tasks.log_retention_days
        ),
        (4, 15, 90, 30)
    );
    assert_eq!(
        (c.outbox.retention_days, c.outbox.poll_interval_ms),
        (30, 1000)
    );
    assert_eq!(
        (c.downloads.flush_interval_secs, c.downloads.retention_days),
        (10, 90)
    );
    assert_eq!(c.egress.registries.allowed_ports, [443]);
    assert_eq!(c.egress.smtp.allowed_ports, [465, 587]);
    assert!(
        c.egress.proxy.allowed_hosts.is_empty(),
        "the proxy setting is refused by default"
    );
    assert!(c.egress.webhooks.allow_private_cidrs.is_empty(), "SEC-34");
    assert_eq!(c.tasks.scheduler_tick().as_secs(), 15);
    assert_eq!(c.outbox.poll_interval().as_millis(), 1000);
    assert_eq!(c.downloads.flush_interval().as_secs(), 10);
    assert_ok(&valid());
}

#[test]
fn foundation_values_are_bounded() {
    let mut c = valid();
    c.tasks.max_concurrent_runs = 0;
    c.tasks.scheduler_tick_secs = 301;
    c.tasks.run_retention_days = 0;
    c.outbox.retention_days = 3651;
    c.outbox.poll_interval_ms = 49;
    c.downloads.flush_interval_secs = 0;
    for key in [
        "tasks.max_concurrent_runs",
        "tasks.scheduler_tick_secs",
        "tasks.run_retention_days",
        "outbox.retention_days",
        "outbox.poll_interval_ms",
        "downloads.flush_interval_secs",
    ] {
        assert_fails(&c, key);
    }
}

#[test]
fn must_not_keep_task_logs_longer_than_their_runs() {
    let mut c = valid();
    c.tasks.run_retention_days = 30;
    c.tasks.log_retention_days = 31;
    assert_fails(&c, "tasks.log_retention_days");
    c.tasks.log_retention_days = 30;
    assert_ok(&c);
}

#[test]
fn must_keep_download_counters_for_at_least_a_month_and_a_bit() {
    let mut c = valid();
    c.downloads.retention_days = 34;
    assert_fails(&c, "downloads.retention_days");
    c.downloads.retention_days = 35;
    assert_ok(&c);
}

const HTTP_PURPOSES: [&str; 4] = ["registries", "webhooks", "scanners", "probe"];

fn policy_mut<'a>(c: &'a mut AppConfig, purpose: &str) -> &'a mut crate::EgressPolicyConfig {
    match purpose {
        "registries" => &mut c.egress.registries,
        "webhooks" => &mut c.egress.webhooks,
        "scanners" => &mut c.egress.scanners,
        _ => &mut c.egress.probe,
    }
}

#[test]
fn egress_policies_follow_the_rules_of_the_upstream_policy() {
    for purpose in HTTP_PURPOSES {
        let key = |name: &str| format!("egress.{purpose}.{name}");

        let mut c = valid();
        policy_mut(&mut c, purpose).allowed_schemes = vec!["http".into()];
        assert_fails(&c, &key("allowed_schemes"));

        // `http` needs the hosts it applies to, and the hosts need `http`.
        let mut c = valid();
        policy_mut(&mut c, purpose).allowed_schemes = vec!["https".into(), "http".into()];
        assert_fails(&c, &key("allowed_schemes"));
        policy_mut(&mut c, purpose).allow_insecure_http_hosts = vec!["mirror.internal".into()];
        assert_ok(&c);
        assert!(
            warnings(&c)
                .iter()
                .any(|w| w.contains(&key("allowed_schemes")))
        );
        let mut c = valid();
        policy_mut(&mut c, purpose).allow_insecure_http_hosts = vec!["mirror.internal".into()];
        assert_fails(&c, &key("allow_insecure_http_hosts"));

        let mut c = valid();
        policy_mut(&mut c, purpose).allowed_schemes = vec!["ftp".into(), "https".into()];
        assert_fails(&c, &key("allowed_schemes"));

        let mut c = valid();
        policy_mut(&mut c, purpose).allowed_hosts = vec!["https://x".into()];
        assert_fails(&c, &key("allowed_hosts"));
        policy_mut(&mut c, purpose).allowed_hosts = vec!["*.example.org".into()];
        assert_ok(&c);

        let mut c = valid();
        policy_mut(&mut c, purpose).allowed_ports = Vec::new();
        assert_fails(&c, &key("allowed_ports"));
        policy_mut(&mut c, purpose).allowed_ports = vec![0];
        assert_fails(&c, &key("allowed_ports"));
    }
}

#[test]
fn must_not_allow_private_cidrs_that_cover_loopback_or_metadata_in_any_egress_purpose() {
    for net in ["127.0.0.0/8", "169.254.0.0/16", "0.0.0.0/0", "::/0"] {
        let net: IpNet = net.parse().expect("cidr");
        for purpose in HTTP_PURPOSES {
            let mut c = valid();
            policy_mut(&mut c, purpose).allow_private_cidrs = vec![net];
            assert_fails(&c, &format!("egress.{purpose}.allow_private_cidrs"));
        }
        let mut c = valid();
        c.egress.smtp.allow_private_cidrs = vec![net];
        assert_fails(&c, "egress.smtp.allow_private_cidrs");
    }
    let mut c = valid();
    c.egress.registries.allow_private_cidrs = vec!["10.20.0.0/16".parse().expect("cidr")];
    c.egress.smtp.allow_private_cidrs = vec!["10.30.0.0/24".parse().expect("cidr")];
    assert_ok(&c);
}

#[test]
fn smtp_and_proxy_policies_check_hosts_and_ports() {
    let mut c = valid();
    c.egress.smtp.allowed_hosts = vec!["smtp://relay".into()];
    assert_fails(&c, "egress.smtp.allowed_hosts");
    c.egress.smtp.allowed_hosts = vec!["relay.example.org".into()];
    c.egress.smtp.allowed_ports = Vec::new();
    assert_fails(&c, "egress.smtp.allowed_ports");
    c.egress.smtp.allowed_ports = vec![587];
    assert_ok(&c);

    c.egress.proxy.allowed_hosts = vec!["proxy.example.org:3128".into()];
    assert_fails(&c, "egress.proxy.allowed_hosts");
    c.egress.proxy.allowed_hosts = vec!["proxy.example.org".into()];
    assert_ok(&c);
}
