//! Environment layer: `RUSTRYX_<SECTION>_<KEY>` and `<NAME>_FILE` for secrets.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_yaml_ng::{Mapping, Number, Value};

use crate::env::{EnvLookup, EnvSource};
use crate::error::ConfigIssue;
use crate::keys::{ENV_PREFIX, KEYS, KeySpec, Kind};
use crate::tree::{remove_path, set_path};

/// `RUSTRYX_ENV` is the name the delivery requirements (OPS-53) use for the
/// environment switch; it is accepted as an alias of `RUSTRYX_ENVIRONMENT`.
pub(crate) const ENV_ALIAS_ENVIRONMENT: &str = "RUSTRYX_ENV";
/// Names that are not configuration keys but are read by the process.
const NON_KEY_VARIABLES: &[&str] = &["RUSTRYX_CONFIG"];
/// Variables Kubernetes injects for a Service called `rustryx`
/// (`RUSTRYX_SERVICE_HOST`, `RUSTRYX_PORT`, `RUSTRYX_PORT_8081_TCP_ADDR`, ...).
/// They are not configuration, so they are skipped instead of warned about.
const SERVICE_LINK_PREFIXES: &[&str] = &["RUSTRYX_SERVICE_", "RUSTRYX_PORT"];
/// Upper bound for a secret file; a mounted secret is a few hundred bytes.
const MAX_SECRET_FILE_BYTES: u64 = 1024 * 1024;

/// What the overlay produced besides the mutated tree.
#[derive(Debug, Default)]
pub(crate) struct OverlayOutcome {
    /// Problems with environment values.
    pub issues: Vec<ConfigIssue>,
    /// Which variable supplied each key, to name it in later errors.
    pub sources: BTreeMap<String, String>,
    /// Variables with the `RUSTRYX_` prefix that match no key.
    pub unknown: Vec<String>,
}

/// Applies environment overrides to `root` (environment wins over the file).
pub(crate) fn apply_env(root: &mut Mapping, env: &dyn EnvSource) -> OverlayOutcome {
    let mut out = OverlayOutcome::default();
    let optional = optional_keys();
    for spec in KEYS {
        if spec.path == "environment" {
            apply_environment_key(spec, root, env, &mut out);
        } else if let Some(file_var) = spec.env_file_name() {
            apply_secret(spec, &file_var, root, env, &mut out);
        } else {
            apply_plain(spec, optional.contains(spec.path), root, env, &mut out);
        }
    }
    out.unknown = unknown_variables(env);
    out
}

/// Keys whose default is unset; an empty environment value unsets them (Helm renders
/// empty strings for values that are not provided).
fn optional_keys() -> BTreeSet<&'static str> {
    let default = serde_yaml_ng::to_value(crate::AppConfig::default()).unwrap_or(Value::Null);
    KEYS.iter()
        .filter(|spec| {
            matches!(
                crate::tree::get_path(&default, spec.path),
                Some(Value::Null)
            )
        })
        .map(|spec| spec.path)
        .collect()
}

fn apply_plain(
    spec: &KeySpec,
    optional: bool,
    root: &mut Mapping,
    env: &dyn EnvSource,
    out: &mut OverlayOutcome,
) {
    let name = spec.env_name();
    match env.get(&name) {
        EnvLookup::Missing => {}
        EnvLookup::NotUnicode => out.issues.push(not_unicode(spec, &name)),
        EnvLookup::Present(raw) if optional && raw.is_empty() => {
            remove_path(root, spec.path);
            out.sources.insert(spec.path.to_owned(), name);
        }
        EnvLookup::Present(raw) => match parse_value(spec, &raw) {
            Ok(value) => {
                set_path(root, spec.path, value);
                out.sources.insert(spec.path.to_owned(), name);
            }
            Err(message) => out
                .issues
                .push(ConfigIssue::from_env(spec.path, name, message)),
        },
    }
}

fn apply_secret(
    spec: &KeySpec,
    file_var: &str,
    root: &mut Mapping,
    env: &dyn EnvSource,
    out: &mut OverlayOutcome,
) {
    let name = spec.env_name();
    match (env.get(&name), env.get(file_var)) {
        (EnvLookup::Missing, EnvLookup::Missing) => {}
        (EnvLookup::NotUnicode, _) => out.issues.push(not_unicode(spec, &name)),
        (_, EnvLookup::NotUnicode) => out.issues.push(not_unicode(spec, file_var)),
        (EnvLookup::Present(_), EnvLookup::Present(_)) => out.issues.push(ConfigIssue::from_env(
            spec.path,
            name.clone(),
            format!("both {name} and {file_var} are set; use only one"),
        )),
        (EnvLookup::Present(value), EnvLookup::Missing) => {
            set_path(root, spec.path, Value::String(value));
            out.sources.insert(spec.path.to_owned(), name);
        }
        (EnvLookup::Missing, EnvLookup::Present(path)) => match read_secret_file(&path) {
            Ok(value) => {
                set_path(root, spec.path, Value::String(value));
                out.sources
                    .insert(spec.path.to_owned(), file_var.to_owned());
            }
            Err(message) => out
                .issues
                .push(ConfigIssue::from_env(spec.path, file_var, message)),
        },
    }
}

/// `RUSTRYX_ENV` as alias of `RUSTRYX_ENVIRONMENT`; a conflict is an error because
/// guessing which one wins could silently turn a production node into development.
fn apply_environment_key(
    spec: &KeySpec,
    root: &mut Mapping,
    env: &dyn EnvSource,
    out: &mut OverlayOutcome,
) {
    let primary = spec.env_name();
    let (var, lookup) = match (env.get(&primary), env.get(ENV_ALIAS_ENVIRONMENT)) {
        (EnvLookup::Missing, alias) => (ENV_ALIAS_ENVIRONMENT.to_owned(), alias),
        (primary_value, EnvLookup::Missing) => (primary, primary_value),
        (EnvLookup::Present(a), EnvLookup::Present(b))
            if a.trim().eq_ignore_ascii_case(b.trim()) =>
        {
            (primary, EnvLookup::Present(a))
        }
        _ => {
            out.issues.push(ConfigIssue::from_env(
                spec.path,
                primary.clone(),
                format!("{primary} and {ENV_ALIAS_ENVIRONMENT} are both set and differ"),
            ));
            return;
        }
    };
    match lookup {
        EnvLookup::Missing => {}
        EnvLookup::NotUnicode => out.issues.push(not_unicode(spec, &var)),
        EnvLookup::Present(raw) => match parse_value(spec, &raw) {
            Ok(value) => {
                set_path(root, spec.path, value);
                out.sources.insert(spec.path.to_owned(), var);
            }
            Err(message) => out
                .issues
                .push(ConfigIssue::from_env(spec.path, var, message)),
        },
    }
}

fn not_unicode(spec: &KeySpec, var: &str) -> ConfigIssue {
    ConfigIssue::from_env(spec.path, var, "value is not valid UTF-8")
}

/// Parses one environment value by the key's kind. Messages never echo the value.
fn parse_value(spec: &KeySpec, raw: &str) -> Result<Value, String> {
    match spec.kind {
        Kind::Bool => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err("expected true, false, 1 or 0".to_owned()),
        },
        Kind::UInt => raw
            .trim()
            .parse::<u64>()
            .map(|n| Value::Number(Number::from(n)))
            .map_err(|_| "expected a non-negative integer".to_owned()),
        Kind::Float => raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(|f| Value::Number(Number::from(f)))
            .ok_or_else(|| "expected a number".to_owned()),
        Kind::Str | Kind::Secret => Ok(Value::String(raw.to_owned())),
        Kind::StrList => Ok(Value::Sequence(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| Value::String(item.to_owned()))
                .collect(),
        )),
        Kind::Enum(variants) => {
            let lowered = raw.trim().to_ascii_lowercase();
            if variants.contains(&lowered.as_str()) {
                Ok(Value::String(lowered))
            } else {
                Err(format!("must be one of: {}", variants.join(", ")))
            }
        }
    }
}

/// Reads a mounted secret. One trailing newline is removed because secrets
/// created with `echo` or `kubectl create secret --from-file` usually end in one;
/// no other whitespace is touched.
fn read_secret_file(path: &str) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("the file path is empty".to_owned());
    }
    let path = Path::new(path);
    let file =
        File::open(path).map_err(|e| format!("cannot open {}: {}", path.display(), e.kind()))?;
    let mut text = String::new();
    file.take(MAX_SECRET_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e.kind()))?;
    if text.len() as u64 > MAX_SECRET_FILE_BYTES {
        return Err(format!("{} is larger than 1 MiB", path.display()));
    }
    if text.ends_with("\r\n") {
        text.truncate(text.len() - 2);
    } else if text.ends_with('\n') {
        text.truncate(text.len() - 1);
    }
    Ok(text)
}

fn unknown_variables(env: &dyn EnvSource) -> Vec<String> {
    let known: BTreeSet<String> = KEYS
        .iter()
        .flat_map(|spec| std::iter::once(spec.env_name()).chain(spec.env_file_name()))
        .chain(NON_KEY_VARIABLES.iter().map(|s| (*s).to_owned()))
        .chain(std::iter::once(ENV_ALIAS_ENVIRONMENT.to_owned()))
        .collect();
    let mut unknown: Vec<String> = env
        .names()
        .into_iter()
        .filter(|name| name.starts_with(ENV_PREFIX))
        .filter(|name| !known.contains(name))
        .filter(|name| !SERVICE_LINK_PREFIXES.iter().any(|p| name.starts_with(p)))
        .collect();
    unknown.sort();
    unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::tree::get_path;

    fn run(env: &MapEnv) -> (Value, OverlayOutcome) {
        let mut root = Mapping::new();
        let out = apply_env(&mut root, env);
        (Value::Mapping(root), out)
    }

    #[test]
    fn values_are_parsed_by_kind() {
        let env = MapEnv::new()
            .with("RUSTRYX_SERVER_SHUTDOWN_TIMEOUT_SECS", " 45 ")
            .with("RUSTRYX_AUTH_ANONYMOUS_ENABLED", "TRUE")
            .with("RUSTRYX_OBSERVABILITY_OTLP_SAMPLE_RATIO", "0.25")
            .with("RUSTRYX_LOCK_BACKEND", "Redis")
            .with(
                "RUSTRYX_SERVER_TRUSTED_PROXIES",
                "10.0.0.0/8, 192.168.0.0/16,,",
            );
        let (tree, out) = run(&env);
        assert!(out.issues.is_empty(), "{:?}", out.issues);
        assert_eq!(
            get_path(&tree, "server.shutdown_timeout_secs"),
            Some(&Value::Number(Number::from(45_u64)))
        );
        assert_eq!(
            get_path(&tree, "auth.anonymous_enabled"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            get_path(&tree, "lock.backend"),
            Some(&Value::String("redis".into()))
        );
        let proxies = get_path(&tree, "server.trusted_proxies").expect("list");
        assert_eq!(proxies.as_sequence().map(Vec::len), Some(2));
    }

    #[test]
    fn unparsable_values_are_reported_with_the_variable_name() {
        let env = MapEnv::new()
            .with("RUSTRYX_SERVER_SHUTDOWN_TIMEOUT_SECS", "soon")
            .with("RUSTRYX_AUTH_ANONYMOUS_ENABLED", "maybe")
            .with("RUSTRYX_LOCK_BACKEND", "etcd")
            .with("RUSTRYX_OBSERVABILITY_OTLP_SAMPLE_RATIO", "NaN");
        let (_, out) = run(&env);
        assert_eq!(out.issues.len(), 4);
        assert!(
            out.issues
                .iter()
                .all(|i| matches!(&i.origin, crate::error::IssueOrigin::Env(_)))
        );
    }

    #[test]
    fn empty_value_unsets_optional_keys_only() {
        let env = MapEnv::new()
            .with("RUSTRYX_SERVER_BASE_URL", "")
            .with("RUSTRYX_BLOB_STORE_NAME", "");
        let (tree, out) = run(&env);
        assert!(get_path(&tree, "server.base_url").is_none());
        assert_eq!(
            get_path(&tree, "blob.store_name"),
            Some(&Value::String(String::new()))
        );
        assert_eq!(out.sources.len(), 2);
    }

    #[test]
    fn secret_from_env_and_from_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("pw");
        std::fs::write(&file, "from-file\n").expect("write");
        let env = MapEnv::new()
            .with("RUSTRYX_DATABASE_URL", "postgres://env")
            .with(
                "RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD_FILE",
                file.to_string_lossy().into_owned(),
            );
        let (tree, out) = run(&env);
        assert_eq!(out.issues, Vec::new());
        assert_eq!(
            get_path(&tree, "database.url"),
            Some(&Value::String("postgres://env".into()))
        );
        assert_eq!(
            get_path(&tree, "auth.bootstrap_admin.password"),
            Some(&Value::String("from-file".into()))
        );
    }

    #[test]
    fn crlf_and_single_newline_are_stripped_but_not_other_whitespace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s");
        std::fs::write(&file, " pad \r\n").expect("write");
        assert_eq!(
            read_secret_file(&file.to_string_lossy()).as_deref(),
            Ok(" pad ")
        );
        std::fs::write(&file, "x\n\n").expect("write");
        assert_eq!(
            read_secret_file(&file.to_string_lossy()).as_deref(),
            Ok("x\n")
        );
    }

    #[test]
    fn both_direct_and_file_variants_conflict() {
        let env = MapEnv::new()
            .with("RUSTRYX_DATABASE_URL", "a")
            .with("RUSTRYX_DATABASE_URL_FILE", "/nonexistent");
        let (tree, out) = run(&env);
        assert_eq!(out.issues.len(), 1);
        assert!(out.issues[0].message.contains("use only one"));
        assert!(get_path(&tree, "database.url").is_none());
    }

    #[test]
    fn missing_secret_file_is_an_issue_without_contents() {
        let env = MapEnv::new().with("RUSTRYX_DATABASE_URL_FILE", "/nonexistent/secret");
        let (_, out) = run(&env);
        assert_eq!(out.issues.len(), 1);
        assert!(out.issues[0].message.contains("cannot open"));
    }

    #[test]
    fn empty_secret_file_path_is_an_issue() {
        let env = MapEnv::new().with("RUSTRYX_DATABASE_URL_FILE", "  ");
        let (_, out) = run(&env);
        assert!(out.issues[0].message.contains("path is empty"));
    }

    #[test]
    fn oversized_secret_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("big");
        std::fs::write(&file, vec![b'a'; 1024 * 1024 + 10]).expect("write");
        assert!(
            read_secret_file(&file.to_string_lossy())
                .expect_err("too large")
                .contains("larger than 1 MiB")
        );
    }

    #[test]
    fn environment_alias_is_accepted_and_conflicts_are_errors() {
        let (tree, out) = run(&MapEnv::new().with("RUSTRYX_ENV", "development"));
        assert_eq!(out.issues, Vec::new());
        assert_eq!(
            get_path(&tree, "environment"),
            Some(&Value::String("development".into()))
        );
        assert_eq!(
            out.sources.get("environment").map(String::as_str),
            Some("RUSTRYX_ENV")
        );

        let same = MapEnv::new()
            .with("RUSTRYX_ENV", "Production")
            .with("RUSTRYX_ENVIRONMENT", "production");
        assert_eq!(run(&same).1.issues, Vec::new());

        let conflict = MapEnv::new()
            .with("RUSTRYX_ENV", "development")
            .with("RUSTRYX_ENVIRONMENT", "production");
        let (tree, out) = run(&conflict);
        assert_eq!(out.issues.len(), 1);
        assert!(get_path(&tree, "environment").is_none());
    }

    #[test]
    fn unknown_prefixed_variables_are_listed_but_service_links_are_not() {
        let env = MapEnv::new()
            .with("RUSTRYX_SERVR_LISTEN_ADDR", "x")
            .with("RUSTRYX_CONFIG", "/etc/c.yaml")
            .with("RUSTRYX_SERVICE_HOST", "10.0.0.1")
            .with("RUSTRYX_PORT_8081_TCP_ADDR", "10.0.0.1")
            .with("RUSTRYX_DATABASE_URL_FILE", "/nonexistent")
            .with("OTHER_VAR", "1");
        let (_, out) = run(&env);
        assert_eq!(out.unknown, vec!["RUSTRYX_SERVR_LISTEN_ADDR".to_owned()]);
    }
}
