//! Rules for the non-provider part of `auth`: sessions, keys, bootstrap admin.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::Report;
use crate::AppConfig;

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
    bootstrap_admin(config, report);
    keys(config, report);
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
