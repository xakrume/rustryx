//! Semantic rules over a typed configuration.
//!
//! The rules are pure (no filesystem, no network) and never stop at the first
//! failure: every rule appends to one [`Report`], so an operator sees the whole
//! list at once. Messages name the key and the rule, never a configured value.

mod auth;
mod foundation;
mod idp;
mod network;
mod runtime;
mod storage;
#[cfg(test)]
mod tests;

use url::Url;

use crate::AppConfig;
use crate::error::ConfigIssue;
use crate::keys::{KEYS, env_name_for};
use crate::secret::ConfigSecret;

/// Findings of one validation run.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Rule violations (fatal).
    pub issues: Vec<ConfigIssue>,
    /// Risky but legal settings.
    pub warnings: Vec<String>,
}

impl Report {
    pub(crate) fn err(&mut self, key: &str, message: impl Into<String>) {
        self.issues.push(ConfigIssue::new(key, message));
    }

    pub(crate) fn warn(&mut self, key: &str, message: impl AsRef<str>) {
        self.warnings.push(format!("{key}: {}", message.as_ref()));
    }

    /// Requires `value` to be within `min..=max`.
    pub(crate) fn range(&mut self, key: &str, value: u64, min: u64, max: u64) {
        if value < min || value > max {
            self.err(key, format!("must be between {min} and {max}"));
        }
    }

    /// Requires a non-blank text value; `hint` is appended to the message.
    pub(crate) fn require_text(&mut self, key: &str, value: Option<&str>) -> Option<String> {
        match value {
            Some(text) if !text.trim().is_empty() => Some(text.to_owned()),
            Some(_) => {
                self.err(key, "must not be blank");
                None
            }
            None => {
                self.err(key, format!("is required ({})", how_to_set(key)));
                None
            }
        }
    }

    /// Requires a non-blank secret.
    pub(crate) fn require_secret<'a>(
        &mut self,
        key: &str,
        value: Option<&'a ConfigSecret>,
    ) -> Option<&'a ConfigSecret> {
        match value {
            Some(secret) if !secret.is_blank() => Some(secret),
            Some(_) => {
                self.err(key, "must not be blank");
                None
            }
            None => {
                self.err(key, format!("is required ({})", how_to_set(key)));
                None
            }
        }
    }

    /// Parses an absolute URL, reporting a generic message on failure.
    pub(crate) fn url(&mut self, key: &str, raw: &str) -> Option<Url> {
        if let Ok(url) = Url::parse(raw) {
            Some(url)
        } else {
            self.err(key, "is not a valid absolute URL");
            None
        }
    }
}

/// Tells the operator which variables set a key.
fn how_to_set(key: &str) -> String {
    let env = env_name_for(key);
    match KEYS.iter().find(|spec| spec.path == key) {
        Some(spec) if spec.is_secret() => format!("set {env} or {env}_FILE"),
        _ => format!("set {env}"),
    }
}

/// Runs every rule.
pub(crate) fn run(config: &AppConfig) -> Report {
    let mut report = Report::default();
    network::server(config, &mut report);
    network::upstream(config, &mut report);
    storage::database(config, &mut report);
    storage::blob(config, &mut report);
    storage::lock(config, &mut report);
    storage::gc(config, &mut report);
    auth::auth(config, &mut report);
    idp::oidc(config, &mut report);
    idp::ldap(config, &mut report);
    runtime::limits(config, &mut report);
    runtime::observability(config, &mut report);
    foundation::foundation(config, &mut report);
    report
}
