//! Configuration errors.
//!
//! Every message here may end up in a terminal or a log, so none of them ever
//! carries a configuration value of a secret key: issues name the key and the
//! rule that failed, not what the operator typed.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Process exit code for an unusable configuration (`EX_CONFIG` from `sysexits.h`).
///
/// SRE-35 fixes this code so that an orchestrator (and the runbook) can tell a
/// configuration failure from a crash.
pub const EXIT_CODE_CONFIG: u8 = 78;

/// Where an issue was detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueOrigin {
    /// A key in the YAML file or a rule over the merged configuration.
    Config,
    /// An environment variable (name given).
    Env(String),
}

/// One problem found while loading or validating the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    /// Dotted key path, for example `auth.ldap.url`. Empty for file-level problems.
    pub key: String,
    /// Where the offending value came from.
    pub origin: IssueOrigin,
    /// What is wrong. Never contains a secret value.
    pub message: String,
}

impl ConfigIssue {
    /// Issue about a key of the merged configuration.
    #[must_use]
    pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            origin: IssueOrigin::Config,
            message: message.into(),
        }
    }

    /// Issue caused by an environment variable.
    #[must_use]
    pub fn from_env(
        key: impl Into<String>,
        var: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            origin: IssueOrigin::Env(var.into()),
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.origin, self.key.is_empty()) {
            (IssueOrigin::Config, true) => f.write_str(&self.message),
            (IssueOrigin::Config, false) => write!(f, "{}: {}", self.key, self.message),
            (IssueOrigin::Env(var), _) => {
                write!(f, "{} ({}): {}", self.key, var, self.message)
            }
        }
    }
}

/// All problems found in one pass. Never empty when wrapped in [`ConfigError::Invalid`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssues(pub Vec<ConfigIssue>);

impl ConfigIssues {
    /// Number of issues.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when there are no issues.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// True when some issue concerns exactly this key.
    #[must_use]
    pub fn mentions(&self, key: &str) -> bool {
        self.0.iter().any(|i| i.key == key)
    }

    /// Iterates over the issues.
    pub fn iter(&self) -> impl Iterator<Item = &ConfigIssue> {
        self.0.iter()
    }
}

impl fmt::Display for ConfigIssues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0.len();
        write!(
            f,
            "invalid configuration, {n} {}:",
            if n == 1 { "error" } else { "errors" }
        )?;
        for issue in &self.0 {
            write!(f, "\n  - {issue}")?;
        }
        Ok(())
    }
}

/// Failure to produce a usable [`crate::AppConfig`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The configuration file could not be read.
    #[error("cannot read configuration file {}", path.display())]
    ReadFile {
        /// Path given by `--config` or `RUSTRYX_CONFIG`.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The configuration file is not usable YAML.
    #[error("configuration file {} is not valid YAML: {message}", path.display())]
    ParseFile {
        /// Path of the file.
        path: PathBuf,
        /// Parser message (position, never a scalar value).
        message: String,
    },
    /// One or more keys are unknown, mistyped, missing or break a rule.
    #[error("{0}")]
    Invalid(ConfigIssues),
    /// The effective configuration could not be rendered.
    #[error("cannot render configuration: {0}")]
    Render(String),
}

impl ConfigError {
    /// Process exit code to use for this error.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        EXIT_CODE_CONFIG
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_display_lists_every_error_with_count() {
        let issues = ConfigIssues(vec![
            ConfigIssue::new("database.url", "is required"),
            ConfigIssue::from_env("server.listen_addr", "RUSTRYX_SERVER_LISTEN_ADDR", "bad"),
        ]);
        let text = issues.to_string();
        assert!(text.starts_with("invalid configuration, 2 errors:"));
        assert!(text.contains("- database.url: is required"));
        assert!(text.contains("- server.listen_addr (RUSTRYX_SERVER_LISTEN_ADDR): bad"));
    }

    #[test]
    fn single_issue_uses_singular() {
        let issues = ConfigIssues(vec![ConfigIssue::new("a", "b")]);
        assert!(issues.to_string().contains("1 error:"));
    }

    #[test]
    fn config_errors_exit_with_ex_config() {
        let err = ConfigError::Invalid(ConfigIssues(vec![ConfigIssue::new("a", "b")]));
        assert_eq!(err.exit_code(), 78);
    }

    #[test]
    fn file_level_issue_has_no_key_prefix() {
        let issue = ConfigIssue::new("", "something");
        assert_eq!(issue.to_string(), "something");
    }

    #[test]
    fn mentions_matches_exact_key() {
        let issues = ConfigIssues(vec![ConfigIssue::new("a.b", "x")]);
        assert!(issues.mentions("a.b"));
        assert!(!issues.mentions("a"));
        assert_eq!(issues.len(), 1);
        assert!(!issues.is_empty());
        assert_eq!(issues.iter().count(), 1);
    }
}
