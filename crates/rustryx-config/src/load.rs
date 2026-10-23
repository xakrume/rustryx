//! Layered loading: defaults < YAML file < environment.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_path_to_error::Segment;
use serde_yaml_ng::{Mapping, Value};

use crate::env::{EnvLookup, EnvSource};
use crate::error::{ConfigError, ConfigIssue, ConfigIssues, IssueOrigin};
use crate::keys::{KEYS, KeyIndex};
use crate::overlay::{OverlayOutcome, apply_env};
use crate::tree::{check_structure, remove_path};
use crate::{AppConfig, validate};

/// Name of the variable that points at the configuration file.
pub const CONFIG_PATH_VAR: &str = "RUSTRYX_CONFIG";
/// Largest configuration file accepted. A real one is a few kilobytes.
const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

/// A loaded and validated configuration.
#[derive(Debug)]
pub struct Loaded {
    /// The effective configuration.
    pub config: AppConfig,
    /// Non-fatal findings to log once logging is up (unknown variables, risky but legal settings).
    pub warnings: Vec<String>,
}

/// Picks the configuration file: `--config` wins over `RUSTRYX_CONFIG`; an empty
/// variable counts as unset.
#[must_use]
pub fn resolve_config_path(cli: Option<&Path>, env: &dyn EnvSource) -> Option<PathBuf> {
    if let Some(path) = cli {
        return Some(path.to_path_buf());
    }
    match env.get(CONFIG_PATH_VAR) {
        EnvLookup::Present(value) if !value.trim().is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// Loads, merges (env > file > defaults) and validates the configuration.
///
/// All problems found in one pass are returned together.
///
/// # Errors
/// [`ConfigError::ReadFile`] and [`ConfigError::ParseFile`] when the file is unusable,
/// [`ConfigError::Invalid`] with every issue otherwise.
pub fn load(file: Option<&Path>, env: &dyn EnvSource) -> Result<Loaded, ConfigError> {
    let assembled = assemble(file, env)?;
    let mut issues = assembled.issues;
    let mut warnings = assembled.warnings;

    let report = validate::run(&assembled.config);
    // A key that already failed to load fell back to its default; validation
    // would then complain about the default and double-report the same key.
    let failed: Vec<String> = issues.iter().map(|i| i.key.clone()).collect();
    issues.extend(
        report
            .issues
            .into_iter()
            .filter(|i| !failed.contains(&i.key)),
    );
    issues.extend(file_checks(&assembled.config));
    warnings.extend(report.warnings);

    if issues.is_empty() {
        Ok(Loaded {
            config: assembled.config,
            warnings,
        })
    } else {
        Err(ConfigError::Invalid(ConfigIssues(issues)))
    }
}

/// Like [`load`] but skips the semantic rules (`print-config --no-validate`).
/// Unknown keys, wrong types and bad environment values are still errors, because
/// without them the printed configuration would not be the effective one.
///
/// # Errors
/// As [`load`].
pub fn load_without_rules(
    file: Option<&Path>,
    env: &dyn EnvSource,
) -> Result<AppConfig, ConfigError> {
    let assembled = assemble(file, env)?;
    if assembled.issues.is_empty() {
        Ok(assembled.config)
    } else {
        Err(ConfigError::Invalid(ConfigIssues(assembled.issues)))
    }
}

struct Assembled {
    config: AppConfig,
    issues: Vec<ConfigIssue>,
    warnings: Vec<String>,
}

fn assemble(file: Option<&Path>, env: &dyn EnvSource) -> Result<Assembled, ConfigError> {
    let mut root = match file {
        Some(path) => read_file_tree(path)?,
        None => Mapping::new(),
    };
    let index = KeyIndex::new();
    let mut issues = check_structure(&mut root, &index);

    let OverlayOutcome {
        issues: env_issues,
        sources,
        unknown,
    } = apply_env(&mut root, env);
    issues.extend(env_issues);

    let config = deserialize_lenient(root, &sources, &mut issues);
    let warnings = unknown
        .into_iter()
        .map(|name| format!("ignoring environment variable {name}: no such configuration key"))
        .collect();
    Ok(Assembled {
        config,
        issues,
        warnings,
    })
}

fn read_file_tree(path: &Path) -> Result<Mapping, ConfigError> {
    let read_error = |source| ConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(read_error)?;
    let mut text = String::new();
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(read_error)?;
    let parse_error = |message: String| ConfigError::ParseFile {
        path: path.to_path_buf(),
        message,
    };
    if text.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(parse_error("the file is larger than 1 MiB".to_owned()));
    }
    match serde_yaml_ng::from_str::<Value>(&text) {
        Ok(Value::Mapping(map)) => Ok(map),
        Ok(Value::Null) => Ok(Mapping::new()),
        Ok(_) => Err(parse_error(
            "the top level must be a mapping of sections".to_owned(),
        )),
        Err(e) => Err(parse_error(e.to_string())),
    }
}

/// Deserializes the tree, collecting every error instead of the first.
///
/// `serde` stops at the first failure. Each failing key is recorded, removed from
/// the tree (so its default applies) and deserialization is retried; the loop is
/// bounded by the number of keys because every round removes at least one.
fn deserialize_lenient(
    mut root: Mapping,
    sources: &std::collections::BTreeMap<String, String>,
    issues: &mut Vec<ConfigIssue>,
) -> AppConfig {
    for _ in 0..=KEYS.len() {
        match serde_path_to_error::deserialize::<_, AppConfig>(Value::Mapping(root.clone())) {
            Ok(config) => return config,
            Err(err) => {
                let key = leading_map_path(err.path());
                let message = err.inner().to_string();
                let origin = sources
                    .get(&key)
                    .cloned()
                    .map_or(IssueOrigin::Config, IssueOrigin::Env);
                issues.push(ConfigIssue {
                    key: key.clone(),
                    origin,
                    message,
                });
                if key.is_empty() || !remove_path(&mut root, &key) {
                    break;
                }
            }
        }
    }
    AppConfig::default()
}

/// Dotted path made of the leading map segments of a serde path.
fn leading_map_path(path: &serde_path_to_error::Path) -> String {
    path.iter()
        .map_while(|segment| match segment {
            Segment::Map { key } => Some(key.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Checks that need the filesystem. Kept out of [`validate::run`] so that rule
/// validation stays pure and unit-testable.
fn file_checks(config: &AppConfig) -> Vec<ConfigIssue> {
    let ldap = &config.auth.ldap;
    let Some(ca) = ldap.ca_file.as_ref().filter(|_| ldap.enabled) else {
        return Vec::new();
    };
    match std::fs::metadata(ca) {
        Ok(meta) if meta.is_file() => Vec::new(),
        Ok(_) => vec![ConfigIssue::new(
            "auth.ldap.ca_file",
            "is not a regular file",
        )],
        Err(e) => vec![ConfigIssue::new(
            "auth.ldap.ca_file",
            format!("cannot be read: {}", e.kind()),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    fn write(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, text).expect("write");
        path
    }

    #[test]
    fn config_path_prefers_cli_then_env_and_ignores_empty() {
        let env = MapEnv::new().with(CONFIG_PATH_VAR, "/etc/env.yaml");
        assert_eq!(
            resolve_config_path(Some(Path::new("/cli.yaml")), &env),
            Some(PathBuf::from("/cli.yaml"))
        );
        assert_eq!(
            resolve_config_path(None, &env),
            Some(PathBuf::from("/etc/env.yaml"))
        );
        let empty = MapEnv::new().with(CONFIG_PATH_VAR, " ");
        assert_eq!(resolve_config_path(None, &empty), None);
        assert_eq!(resolve_config_path(None, &MapEnv::new()), None);
    }

    #[test]
    fn missing_file_is_a_read_error() {
        let err = load(Some(Path::new("/nonexistent/rustryx.yaml")), &MapEnv::new())
            .expect_err("missing");
        assert!(matches!(err, ConfigError::ReadFile { .. }));
        assert_eq!(err.exit_code(), 78);
    }

    #[test]
    fn broken_yaml_is_a_parse_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(&dir, "c.yaml", "server: [unclosed\n");
        assert!(matches!(
            load(Some(&path), &MapEnv::new()),
            Err(ConfigError::ParseFile { .. })
        ));
    }

    #[test]
    fn top_level_list_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(&dir, "c.yaml", "- a\n- b\n");
        let err = load_without_rules(Some(&path), &MapEnv::new()).expect_err("list");
        assert!(err.to_string().contains("top level must be a mapping"));
    }

    #[test]
    fn empty_file_equals_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(&dir, "c.yaml", "# nothing\n");
        let config = load_without_rules(Some(&path), &MapEnv::new()).expect("loads");
        assert_eq!(config.server.listen_addr.port(), 8081);
    }

    #[test]
    fn oversized_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(&dir, "c.yaml", &"#".repeat(1024 * 1024 + 5));
        let err = load_without_rules(Some(&path), &MapEnv::new()).expect_err("big");
        assert!(err.to_string().contains("larger than 1 MiB"));
    }

    #[test]
    fn precedence_is_env_over_file_over_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(
            &dir,
            "c.yaml",
            "server:\n  shutdown_timeout_secs: 11\n  drain_delay_secs: 4\n",
        );
        let env = MapEnv::new().with("RUSTRYX_SERVER_SHUTDOWN_TIMEOUT_SECS", "22");
        let config = load_without_rules(Some(&path), &env).expect("loads");
        assert_eq!(config.server.shutdown_timeout_secs, 22, "env beats file");
        assert_eq!(config.server.drain_delay_secs, 4, "file beats default");
        assert_eq!(config.limits.max_body_mb, 10, "default stays");
    }

    #[test]
    fn every_problem_is_reported_in_one_pass() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(
            &dir,
            "c.yaml",
            "serverr:\n  x: 1\nlimits:\n  max_body_mb: lots\nlock:\n  backend: etcd\n",
        );
        let env = MapEnv::new().with("RUSTRYX_SERVER_LISTEN_ADDR", "not-an-address");
        let Err(ConfigError::Invalid(issues)) = load(Some(&path), &env) else {
            panic!("expected issues");
        };
        for key in [
            "serverr",
            "limits.max_body_mb",
            "lock.backend",
            "server.listen_addr",
        ] {
            assert!(issues.mentions(key), "missing {key}: {issues}");
        }
        assert!(
            issues.mentions("database.url"),
            "rule errors are included: {issues}"
        );
    }

    #[test]
    fn serde_level_errors_name_the_env_variable_that_caused_them() {
        let env = MapEnv::new().with("RUSTRYX_SERVER_LISTEN_ADDR", "not-an-address");
        let Err(ConfigError::Invalid(issues)) = load(None, &env) else {
            panic!("expected issues");
        };
        let issue = issues
            .iter()
            .find(|i| i.key == "server.listen_addr")
            .expect("issue");
        assert_eq!(
            issue.origin,
            IssueOrigin::Env("RUSTRYX_SERVER_LISTEN_ADDR".into())
        );
    }

    #[test]
    fn failed_key_is_not_double_reported_by_the_rules() {
        let env = MapEnv::new().with("RUSTRYX_SERVER_TRUSTED_PROXIES", "10.0.0.0/8,garbage");
        let Err(ConfigError::Invalid(issues)) = load(None, &env) else {
            panic!("expected issues");
        };
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.key == "server.trusted_proxies")
                .count(),
            1
        );
    }

    #[test]
    fn unknown_env_variables_become_warnings_not_errors() {
        let env = MapEnv::new().with("RUSTRYX_NOPE", "1");
        let assembled = assemble(None, &env).expect("assembles");
        assert_eq!(assembled.warnings.len(), 1);
        assert!(assembled.warnings[0].contains("RUSTRYX_NOPE"));
    }

    #[test]
    fn missing_ldap_ca_file_is_reported_only_when_ldap_is_enabled() {
        let mut config = AppConfig::default();
        config.auth.ldap.ca_file = Some(PathBuf::from("/nonexistent/ca.pem"));
        assert_eq!(file_checks(&config), Vec::new());
        config.auth.ldap.enabled = true;
        let issues = file_checks(&config);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "auth.ldap.ca_file");

        let dir = tempfile::tempdir().expect("tempdir");
        config.auth.ldap.ca_file = Some(dir.path().to_path_buf());
        assert!(
            file_checks(&config)[0]
                .message
                .contains("not a regular file")
        );
        let ca = write(&dir, "ca.pem", "x");
        config.auth.ldap.ca_file = Some(ca);
        assert_eq!(file_checks(&config), Vec::new());
    }
}
