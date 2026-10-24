//! Rules for `auth.oidc` and `auth.ldap`.

use rustryx_core::identity::ROLE_NX_ADMIN;

use super::Report;
use crate::AppConfig;
use crate::schema::GroupStrategy;

pub(super) fn oidc(config: &AppConfig, report: &mut Report) {
    let oidc = &config.auth.oidc;
    // Rules that make a setting dangerous apply even while OIDC is disabled, so
    // that enabling it later through one variable cannot introduce them.
    if oidc
        .default_role
        .as_deref()
        .is_some_and(|r| r.trim() == ROLE_NX_ADMIN)
    {
        // Everyone who can log in would become an administrator. Administrators come
        // only from an exact match against admin_groups.
        report.err(
            "auth.oidc.default_role",
            format!("must not be `{ROLE_NX_ADMIN}`: administrators come only from admin_groups"),
        );
    }
    for party in &oidc.authorized_parties {
        // `azp` is compared as a whole string; a blank entry or one with a separator inside
        // can only be a mistake (a comma-joined list pasted into one entry).
        if party.trim().is_empty() || party.contains(char::is_whitespace) || party.contains(',') {
            report.err(
                "auth.oidc.authorized_parties",
                "entries must be client ids without whitespace or commas",
            );
        }
    }
    if !oidc.enabled {
        return;
    }

    issuer("auth.oidc.issuer", oidc.issuer.as_deref(), true, report);
    if oidc.public_issuer_url.is_some() {
        issuer(
            "auth.oidc.public_issuer_url",
            oidc.public_issuer_url.as_deref(),
            false,
            report,
        );
    }
    browser_login_redirect(config, report);
    report.require_text("auth.oidc.client_id", oidc.client_id.as_deref());
    // Only confidential clients are supported: a public client with a secret, the
    // password grant and the implicit flow are not accepted.
    report.require_secret("auth.oidc.client_secret", oidc.client_secret.as_ref());
    report.range("auth.oidc.clock_skew_secs", oidc.clock_skew_secs, 0, 60);

    if oidc
        .default_role
        .as_deref()
        .is_some_and(|r| r.trim().is_empty())
    {
        report.err(
            "auth.oidc.default_role",
            "must not be blank; leave it unset to refuse users without a group",
        );
    }
    claim(config, report);
    scopes(config, report);
    for group in &oidc.admin_groups {
        if !group.starts_with('/') || group.len() < 2 || group.trim() != group {
            // Matching is exact and keeps the leading slash, so a bare name would
            // never match and the intended administrators would silently get nothing.
            report.err(
                "auth.oidc.admin_groups",
                "entries must be full group paths starting with '/', for example /rustryx/admins",
            );
        }
    }
}

/// The redirect URI of the browser login is `server.base_url` plus the callback path, and
/// `rustryx-auth` accepts plain http for it only on a loopback host. A development base URL such
/// as `http://repo.test:8081` would otherwise validate and then stop the node at startup.
fn browser_login_redirect(config: &AppConfig, report: &mut Report) {
    if config.auth.oidc.client_secret.is_none() {
        // No client secret means no browser login, and no redirect URI.
        return;
    }
    let Some(url) = config
        .server
        .base_url
        .as_deref()
        .and_then(|raw| url::Url::parse(raw).ok())
    else {
        return;
    };
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() == "http" && !loopback {
        report.err(
            "server.base_url",
            "must be https, or http on localhost, while auth.oidc has a client secret: it is the base of the browser login redirect URI",
        );
    }
}

fn issuer(key: &str, raw: Option<&str>, required: bool, report: &mut Report) {
    let text = if required {
        report.require_text(key, raw)
    } else {
        raw.map(str::to_owned)
    };
    let Some(text) = text else {
        return;
    };
    let Some(url) = report.url(key, &text) else {
        return;
    };
    // JWKS are fetched from the discovery document, and signing keys are accepted only from
    // HTTPS endpoints with certificate verification. `rustryx-auth` refuses to build a provider
    // from any other scheme in EVERY environment, so a development allowance here would be a
    // configuration that validates and then stops the node at startup.
    if url.scheme() != "https" || url.host_str().is_none() {
        report.err(key, "must be an https URL with a host");
    }
    if url.query().is_some() || url.fragment().is_some() {
        report.err(key, "must not contain a query or fragment");
    }
}

fn claim(config: &AppConfig, report: &mut Report) {
    let claim = config.auth.oidc.groups_claim.as_str();
    let key = "auth.oidc.groups_claim";
    if claim.trim().is_empty() {
        report.err(key, "must not be blank");
    } else if claim.contains('.') {
        report.err(
            key,
            "must name a top-level claim, nested claims are not supported",
        );
    } else if matches!(claim, "realm_access" | "resource_access") {
        // Keycloak role claims never assign product roles.
        report.err(
            key,
            "role claims do not carry group paths and are not used for authorization",
        );
    }
}

fn scopes(config: &AppConfig, report: &mut Report) {
    let scopes = &config.auth.oidc.scopes;
    let key = "auth.oidc.scopes";
    if !scopes.iter().any(|s| s == "openid") {
        report.err(key, "must include `openid`");
    }
    if scopes.iter().any(|s| s == "offline_access") {
        report.err(
            key,
            "`offline_access` is refused: offline tokens are not allowed",
        );
    }
    if scopes
        .iter()
        .any(|s| s.trim().is_empty() || s.contains(char::is_whitespace))
    {
        report.err(key, "entries must be single words");
    }
}

pub(super) fn ldap(config: &AppConfig, report: &mut Report) {
    let ldap = &config.auth.ldap;
    // The transport rule applies whenever a URL is present, enabled or not: a
    // plaintext URL waiting behind `enabled: false` is one variable away from
    // sending service-account and user passwords in the clear.
    if let Some(raw) = ldap.url.as_deref() {
        transport(config, raw, report);
    }
    if !ldap.enabled {
        return;
    }
    if ldap.url.is_none() {
        report.require_text("auth.ldap.url", None);
    }
    report.require_text("auth.ldap.bind_dn", ldap.bind_dn.as_deref());
    report.require_secret("auth.ldap.bind_password", ldap.bind_password.as_ref());
    report.require_text("auth.ldap.user_base_dn", ldap.user_base_dn.as_deref());

    if !ldap.search_filter.contains("{username}") {
        report.err(
            "auth.ldap.search_filter",
            "must contain the {username} placeholder",
        );
    }
    group_settings(config, report);

    report.range(
        "auth.ldap.connect_timeout_secs",
        ldap.connect_timeout_secs,
        1,
        60,
    );
    report.range("auth.ldap.bind_timeout_secs", ldap.bind_timeout_secs, 1, 60);
    report.range(
        "auth.ldap.search_timeout_secs",
        ldap.search_timeout_secs,
        1,
        60,
    );
    // Confirmed membership is trusted for at most five minutes; a longer value would
    // keep access alive after the user was removed from the group.
    report.range(
        "auth.ldap.groups_cache_ttl_secs",
        ldap.groups_cache_ttl_secs,
        1,
        300,
    );
    report.range(
        "auth.ldap.nested_group_depth",
        u64::from(ldap.nested_group_depth),
        0,
        10,
    );
    report.range(
        "auth.ldap.max_groups",
        u64::from(ldap.max_groups),
        1,
        10_000,
    );
}

fn transport(config: &AppConfig, raw: &str, report: &mut Report) {
    let key = "auth.ldap.url";
    let ldap = &config.auth.ldap;
    let Some(url) = report.url(key, raw) else {
        return;
    };
    if url.host_str().is_none_or(str::is_empty) {
        report.err(key, "must name a host");
    }
    match url.scheme() {
        "ldaps" if ldap.starttls => {
            report.err("auth.ldap.starttls", "cannot be combined with an ldaps:// URL");
        }
        "ldaps" => {}
        "ldap" if ldap.starttls => {}
        "ldap" => report.err(
            key,
            "plain ldap:// is not allowed: use ldaps://, or ldap:// together with auth.ldap.starttls: true",
        ),
        _ => report.err(key, "must use the ldaps:// or ldap:// scheme"),
    }
}

fn group_settings(config: &AppConfig, report: &mut Report) {
    let ldap = &config.auth.ldap;
    match ldap.group_strategy {
        GroupStrategy::MemberOf => {
            if ldap.group_attribute.trim().is_empty() {
                report.err(
                    "auth.ldap.group_attribute",
                    "must not be blank for the member_of strategy",
                );
            }
        }
        GroupStrategy::MemberSearch => {
            report.require_text("auth.ldap.group_base_dn", ldap.group_base_dn.as_deref());
            if !ldap.group_filter.contains("{dn}") {
                report.err(
                    "auth.ldap.group_filter",
                    "must contain the {dn} placeholder",
                );
            }
        }
    }
    for group in &ldap.admin_groups {
        if !group.contains('=') {
            report.err(
                "auth.ldap.admin_groups",
                "entries must be group DNs such as cn=admins,ou=groups,dc=example,dc=org",
            );
        }
    }
}
