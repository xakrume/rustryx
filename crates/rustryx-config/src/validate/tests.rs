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
    c
}

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
fn production_oidc_urls_must_be_https_but_development_may_use_http() {
    let mut c = oidc_enabled();
    c.auth.oidc.issuer = Some("http://sso.example/realms/main".into());
    assert_fails(&c, "auth.oidc.issuer");
    let mut c = oidc_enabled();
    c.server.base_url = Some("http://rustryx.example".into());
    assert_fails(&c, "server.base_url");
    let mut d = oidc_enabled();
    d.environment = Environment::Development;
    d.auth.oidc.issuer = Some("http://localhost:8080/realms/main".into());
    d.server.base_url = Some("http://localhost:8081".into());
    assert_ok(&d);
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
fn production_warns_when_postgres_tls_is_not_verified() {
    let mut c = valid();
    c.database.url = secret("postgres://u:p@h:5432/db?sslmode=require");
    assert!(
        warnings(&c)
            .iter()
            .any(|w| w.contains("sslmode=verify-full"))
    );
    let mut d = dev();
    d.database.url = secret("postgres://u:p@h:5432/db");
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
