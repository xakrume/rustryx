//! Rules for the non-provider part of `auth`: sessions, keys, bootstrap admin.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::Report;
use crate::{AppConfig, keyring};

/// Passwords refused in production. All lowercase; comparison lowercases the input.
const WEAK_PASSWORDS: &[&str] = &[
    "admin",
    "admin123",
    "admin1234",
    "administrator",
    "password",
    "password1",
    "password123",
    "passw0rd",
    "changeme",
    "change-me",
    "change_me",
    "changeit",
    "secret",
    "letmein",
    "welcome",
    "qwerty",
    "qwerty123",
    "rustryx",
    "rustryx123",
    "compat",
    "compat123",
    "nexspence",
    "nexspence123",
    "root",
    "toor",
    "test",
    "test1234",
    "default",
    "demo",
    "12345678",
    "123456789",
    "1234567890",
    "00000000",
    "11111111",
];

/// Words that make a password weak even with digits or punctuation appended
/// (`Admin123!`, `Password2024`).
const WEAK_STEMS: &[&str] = &[
    "admin",
    "administrator",
    "password",
    "passw0rd",
    "changeme",
    "secret",
    "letmein",
    "welcome",
    "qwerty",
    "rustryx",
    "compat",
    "nexspence",
    "root",
    "default",
];

/// Minimum length of the bootstrap password in production.
const MIN_PRODUCTION_PASSWORD_CHARS: usize = 12;

pub(super) fn auth(config: &AppConfig, report: &mut Report) {
    let auth = &config.auth;
    report.range(
        "auth.session.idle_timeout_secs",
        auth.session.idle_timeout_secs,
        1,
        1800,
    );
    report.range(
        "auth.session.absolute_timeout_secs",
        auth.session.absolute_timeout_secs,
        1,
        28_800,
    );
    if auth.session.idle_timeout_secs > auth.session.absolute_timeout_secs {
        report.err(
            "auth.session.idle_timeout_secs",
            "must not exceed auth.session.absolute_timeout_secs",
        );
    }
    if config.is_production() && auth.anonymous_enabled {
        report.warn(
            "auth.anonymous_enabled",
            "anonymous access is enabled in production",
        );
    }
    jit_allowlist(config, report);
    bootstrap_admin(config, report);
    keys(config, report);
}

/// The allowlist is only read in `allowlist` mode. An empty one in that mode lets nobody in,
/// which is safe but is almost always a forgotten key, so it is reported.
fn jit_allowlist(config: &AppConfig, report: &mut Report) {
    let auth = &config.auth;
    let key = "auth.jit_allowlist";
    if auth
        .jit_allowlist
        .iter()
        .any(|name| name.trim().is_empty() || name.trim() != name)
    {
        report.err(
            key,
            "entries must be non-empty usernames without leading or trailing whitespace",
        );
    }
    match auth.jit_mode {
        crate::JitMode::Allowlist if auth.jit_allowlist.is_empty() => report.warn(
            key,
            "auth.jit_mode is allowlist and the list is empty: no identity-provider user can sign in",
        ),
        crate::JitMode::Allowlist => {}
        _ if !auth.jit_allowlist.is_empty() => report.warn(
            key,
            "ignored: it is only read when auth.jit_mode is allowlist",
        ),
        _ => {}
    }
}

fn bootstrap_admin(config: &AppConfig, report: &mut Report) {
    let admin = &config.auth.bootstrap_admin;
    let username = admin.username.as_str();
    if username.trim().is_empty() || username.chars().count() > 64 {
        report.err(
            "auth.bootstrap_admin.username",
            "must be 1 to 64 characters",
        );
    } else if username
        .chars()
        .any(|c| c == ':' || c.is_control() || c.is_whitespace())
    {
        // ':' would split a Basic credential in the wrong place.
        report.err(
            "auth.bootstrap_admin.username",
            "must not contain ':', whitespace or control characters",
        );
    }

    let key = "auth.bootstrap_admin.password";
    match admin.password.as_ref() {
        None if config.is_production() => {
            report.require_secret(key, None);
        }
        None => report.warn(key, "not set, no bootstrap administrator will be created"),
        Some(password) if password.is_blank() => report.err(key, "must not be blank"),
        Some(password) => {
            let weak = is_weak_password(password.expose(), username);
            let short = password.expose().chars().count() < MIN_PRODUCTION_PASSWORD_CHARS;
            if config.is_production() {
                if weak {
                    report.err(
                        key,
                        "is a default or well-known weak password, which production refuses",
                    );
                }
                if short {
                    report.err(
                        key,
                        format!("must be at least {MIN_PRODUCTION_PASSWORD_CHARS} characters in production"),
                    );
                }
            } else if weak {
                report.warn(
                    key,
                    "is a default or well-known weak password (refused in production)",
                );
            }
        }
    }
}

/// True for well-known, default or trivially derived passwords.
pub(crate) fn is_weak_password(password: &str, username: &str) -> bool {
    let lowered = password.trim().to_lowercase();
    if lowered.is_empty() || WEAK_PASSWORDS.contains(&lowered.as_str()) {
        return true;
    }
    if lowered == username.trim().to_lowercase() {
        return true;
    }
    let mut chars = lowered.chars();
    if let Some(first) = chars.next()
        && chars.all(|c| c == first)
    {
        return true;
    }
    let stem = lowered.trim_end_matches(|c: char| !c.is_alphabetic());
    WEAK_STEMS.contains(&stem)
}

/// The ids and the retired keys of the internal JWT ring.
fn jwt_ring(config: &AppConfig, report: &mut Report) {
    let auth = &config.auth;
    if !keyring::valid_jwt_id(&auth.internal_jwt_key_id) {
        report.err(
            "auth.internal_jwt_key_id",
            "must be 1 to 64 characters of letters, digits, '.', '_' or '-'",
        );
    }
    let Some(previous) = auth.internal_jwt_previous_keys.as_ref() else {
        return;
    };
    match keyring::parse_jwt_previous(previous.expose()) {
        Ok(entries) => {
            if entries
                .iter()
                .any(|(kid, _)| *kid == auth.internal_jwt_key_id)
            {
                // Two keys under one `kid` would make the verifier pick one of them at random
                // from the token's point of view: the id has to name exactly one key.
                report.err(
                    "auth.internal_jwt_previous_keys",
                    "must not reuse auth.internal_jwt_key_id: the current key id names one key only",
                );
            }
        }
        Err(error) => report.err("auth.internal_jwt_previous_keys", error.to_string()),
    }
}

fn keys(config: &AppConfig, report: &mut Report) {
    let auth = &config.auth;
    let jwt = "auth.internal_jwt_key";
    match auth.internal_jwt_key.as_ref() {
        None if config.is_production() => {
            report.require_secret(jwt, None);
        }
        None => {}
        Some(key) if key.is_blank() => report.err(jwt, "must not be blank"),
        Some(key) => {
            let pem = key.expose();
            if !(pem.contains("-----BEGIN") && pem.contains("PRIVATE KEY-----")) {
                report.err(jwt, "must be a PEM encoded private key");
            }
        }
    }

    jwt_ring(config, report);

    let cookie = "auth.cookie_key";
    match auth.cookie_key.as_ref() {
        None if config.is_production() => {
            report.require_secret(cookie, None);
        }
        None => {}
        Some(key) if key.is_blank() => report.err(cookie, "must not be blank"),
        Some(key) => {
            let decoded = STANDARD.decode(key.expose().trim());
            if decoded.map(|bytes| bytes.len()) != Ok(32) {
                report.err(
                    cookie,
                    "must be base64 of exactly 32 bytes (openssl rand -base64 32)",
                );
            }
        }
    }
}
