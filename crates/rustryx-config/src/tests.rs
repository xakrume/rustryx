//! Cross-cutting tests: the key table against the schema, redaction, and the
//! committed example file.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_yaml_ng::Value;

use crate::keys::{KEYS, Kind};
use crate::tree::get_path;
use crate::{AppConfig, ConfigError, MapEnv, REDACTED, load, load_without_rules};

const CANARY: &str = "canary-SECRET-0xDEADBEEF";

fn default_tree() -> Value {
    serde_yaml_ng::to_value(AppConfig::default()).expect("serialize defaults")
}

/// Dotted paths of every non-mapping node.
fn leaves(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Mapping(map) => {
            for (key, child) in map {
                let name = key.as_str().expect("text key");
                let path = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}.{name}")
                };
                leaves(child, &path, out);
            }
        }
        _ => {
            out.insert(prefix.to_owned());
        }
    }
}

fn tree_of(config: &AppConfig) -> Value {
    serde_yaml_ng::to_value(config).expect("serialize")
}

#[test]
fn key_table_equals_the_leaves_of_the_schema() {
    let mut schema = BTreeSet::new();
    leaves(&default_tree(), "", &mut schema);
    let table: BTreeSet<String> = KEYS.iter().map(|k| k.path.to_owned()).collect();
    let missing_from_table: Vec<_> = schema.difference(&table).collect();
    let missing_from_schema: Vec<_> = table.difference(&schema).collect();
    assert!(
        missing_from_table.is_empty(),
        "schema keys without table entry: {missing_from_table:?}"
    );
    assert!(
        missing_from_schema.is_empty(),
        "table entries without schema key: {missing_from_schema:?}"
    );
}

#[test]
fn table_kinds_match_the_shape_of_the_defaults() {
    let defaults = default_tree();
    for spec in KEYS {
        let value = get_path(&defaults, spec.path).expect("default present");
        let ok = match spec.kind {
            Kind::Bool => value.is_bool(),
            Kind::UInt => value.as_u64().is_some(),
            Kind::Float => value.is_number(),
            Kind::Str => value.is_string() || value.is_null(),
            Kind::StrList => value.is_sequence(),
            Kind::Secret => value.is_null(),
            Kind::Enum(variants) => value.as_str().is_some_and(|s| variants.contains(&s)),
        };
        assert!(
            ok,
            "{} has kind {:?} but default {value:?}",
            spec.path, spec.kind
        );
    }
}

#[test]
fn every_enum_variant_in_the_table_deserializes() {
    for spec in KEYS {
        if let Kind::Enum(variants) = spec.kind {
            for variant in variants {
                let env = MapEnv::new().with(spec.env_name(), *variant);
                let config = load_without_rules(None, &env)
                    .unwrap_or_else(|e| panic!("{}={variant} was rejected: {e}", spec.path));
                let shown = get_path(&tree_of(&config), spec.path).cloned();
                assert_eq!(shown, Some(Value::String((*variant).to_owned())));
            }
        }
    }
}

/// A valid sample for a key, different from the default, and what the serialized
/// configuration must show afterwards.
fn sample(spec: &crate::keys::KeySpec, default: &Value) -> (String, Value) {
    match spec.kind {
        Kind::Bool => {
            let flipped = !default.as_bool().expect("bool default");
            (flipped.to_string(), Value::Bool(flipped))
        }
        Kind::UInt => ("7".to_owned(), Value::Number(7_u64.into())),
        Kind::Float => ("0.5".to_owned(), Value::Number(0.5_f64.into())),
        Kind::Enum(variants) => {
            let current = default.as_str().expect("enum default");
            let other = variants
                .iter()
                .find(|v| **v != current)
                .expect("two variants");
            ((*other).to_owned(), Value::String((*other).to_owned()))
        }
        Kind::Secret => (CANARY.to_owned(), Value::String(REDACTED.to_owned())),
        Kind::StrList => match spec.path {
            "server.trusted_proxies" | "upstream.allow_private_cidrs" => (
                "10.0.0.0/8,192.168.0.0/16".to_owned(),
                Value::Sequence(vec!["10.0.0.0/8".into(), "192.168.0.0/16".into()]),
            ),
            _ => (
                "a, b".to_owned(),
                Value::Sequence(vec!["a".into(), "b".into()]),
            ),
        },
        Kind::Str => match spec.path {
            "server.listen_addr" => (
                "127.0.0.1:9".to_owned(),
                Value::String("127.0.0.1:9".into()),
            ),
            _ => ("sample".to_owned(), Value::String("sample".into())),
        },
    }
}

#[test]
fn every_key_can_be_set_from_the_environment() {
    let defaults = default_tree();
    for spec in KEYS {
        let default = get_path(&defaults, spec.path).expect("default");
        let (raw, expected) = sample(spec, default);
        let env = MapEnv::new().with(spec.env_name(), raw);
        let config = load_without_rules(None, &env)
            .unwrap_or_else(|e| panic!("{} via {}: {e}", spec.path, spec.env_name()));
        let shown = get_path(&tree_of(&config), spec.path).cloned();
        assert_eq!(shown, Some(expected), "{}", spec.path);
    }
}

#[test]
fn every_secret_can_be_set_from_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    for spec in KEYS.iter().filter(|k| k.is_secret()) {
        let file = dir.path().join(spec.path);
        std::fs::write(&file, format!("{CANARY}\n")).expect("write");
        let var = spec.env_file_name().expect("file variant");
        let env = MapEnv::new().with(var, file.to_string_lossy().into_owned());
        let config = load_without_rules(None, &env).expect("loads");
        let shown = get_path(&tree_of(&config), spec.path).cloned();
        assert_eq!(shown, Some(Value::String(REDACTED.into())), "{}", spec.path);
    }
}

#[test]
fn every_key_can_be_set_from_the_file_layer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let defaults = default_tree();
    for spec in KEYS {
        let default = get_path(&defaults, spec.path).expect("default");
        let (_, expected) = sample(spec, default);
        let mut root = Value::Mapping(serde_yaml_ng::Mapping::new());
        if let Value::Mapping(map) = &mut root {
            let secret_in_file = spec.is_secret().then(|| Value::String(CANARY.into()));
            crate::tree::set_path(
                map,
                spec.path,
                secret_in_file.unwrap_or_else(|| expected.clone()),
            );
        }
        let path = dir.path().join("c.yaml");
        std::fs::write(&path, serde_yaml_ng::to_string(&root).expect("yaml")).expect("write");
        let config = load_without_rules(Some(&path), &MapEnv::new())
            .unwrap_or_else(|e| panic!("{} from file: {e}", spec.path));
        let shown = get_path(&tree_of(&config), spec.path).cloned();
        assert_eq!(shown, Some(expected), "{}", spec.path);
    }
}

#[test]
fn must_not_print_any_secret() {
    let mut env = MapEnv::new();
    for spec in KEYS.iter().filter(|k| k.is_secret()) {
        env = env.with(spec.env_name(), CANARY);
    }
    let config = load_without_rules(None, &env).expect("loads");

    let yaml = config.to_redacted_yaml().expect("render");
    assert!(!yaml.contains(CANARY), "redacted YAML leaked: {yaml}");
    let secret_count = KEYS.iter().filter(|k| k.is_secret()).count();
    assert_eq!(yaml.matches(REDACTED).count(), secret_count);

    assert!(!format!("{config:?}").contains(CANARY), "Debug leaked");
    assert!(!format!("{:#?}", config.database).contains(CANARY));

    let json_like = serde_yaml_ng::to_string(&tree_of(&config)).expect("render");
    assert!(!json_like.contains(CANARY));
}

#[test]
fn must_not_print_secrets_when_loading_fails() {
    let mut env = MapEnv::new().with("RUSTRYX_SERVER_LISTEN_ADDR", "broken");
    for spec in KEYS.iter().filter(|k| k.is_secret()) {
        env = env.with(spec.env_name(), CANARY);
    }
    let err = load(None, &env).expect_err("invalid");
    assert!(!err.to_string().contains(CANARY));
    assert!(!format!("{err:?}").contains(CANARY));
}

/// Catches a secret field declared with a plain `String` or mislabelled in the
/// table: after setting every text key to a canary, exactly the secret keys must
/// serialize as the placeholder and exactly the other text keys as the canary.
#[test]
fn secret_keys_in_the_table_are_exactly_the_secret_typed_fields() {
    let mut env = MapEnv::new();
    for spec in KEYS
        .iter()
        .filter(|k| matches!(k.kind, Kind::Str | Kind::Secret))
    {
        if spec.path != "server.listen_addr" {
            env = env.with(spec.env_name(), CANARY);
        }
    }
    let config = load_without_rules(None, &env).expect("loads");
    let tree = tree_of(&config);
    for spec in KEYS
        .iter()
        .filter(|k| matches!(k.kind, Kind::Str | Kind::Secret))
    {
        if spec.path == "server.listen_addr" {
            continue;
        }
        let shown = get_path(&tree, spec.path)
            .and_then(Value::as_str)
            .expect("text");
        if spec.is_secret() {
            assert_eq!(shown, REDACTED, "{} should be redacted", spec.path);
        } else {
            assert_eq!(
                shown, CANARY,
                "{} is serialized in clear, table says not secret",
                spec.path
            );
        }
    }
}

#[test]
fn must_not_accept_unknown_keys_in_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("c.yaml");
    std::fs::write(&path, "auth:\n  anonymous_enable: true\n").expect("write");
    let Err(ConfigError::Invalid(issues)) = load_without_rules(Some(&path), &MapEnv::new()) else {
        panic!("typo must be an error");
    };
    assert!(issues.mentions("auth.anonymous_enable"));
    assert!(
        issues
            .to_string()
            .contains("did you mean `anonymous_enabled`")
    );
}

#[test]
fn defaults_follow_the_requirements() {
    let c = AppConfig::default();
    assert!(c.is_production(), "safe default environment");
    assert_eq!(c.database.pool_size, 20);
    assert_eq!(c.database.acquire_timeout_secs, 5);
    assert!(!c.auth.anonymous_enabled);
    assert!(!c.auth.allow_local_role_supplement);
    assert_eq!(c.auth.session.idle_timeout_secs, 1800);
    assert_eq!(c.auth.session.absolute_timeout_secs, 28_800);
    assert_eq!(c.auth.oidc.groups_claim, "groups");
    assert_eq!(c.auth.ldap.connect_timeout_secs, 5);
    assert_eq!(c.auth.ldap.bind_timeout_secs, 5);
    assert_eq!(c.auth.ldap.search_timeout_secs, 5);
    assert_eq!(c.limits.rate_limit.requests_per_second, 50);
    assert_eq!(c.limits.rate_limit.burst, 100);
    assert_eq!(c.server.shutdown_timeout_secs, 30);
    assert_eq!(c.lock.ttl_secs, 30);
    assert_eq!(c.lock.renew_interval().as_secs(), 10);
    assert_eq!(c.server.shutdown_timeout().as_secs(), 30);
    assert_eq!(c.server.drain_delay().as_secs(), 0);
    assert!(c.limits.effective_blocking_permits() >= 2);
    assert!(c.limits.effective_password_permits() >= 1);
}

#[test]
fn explicit_permits_override_the_auto_defaults() {
    let mut c = AppConfig::default();
    c.limits.blocking_permits = 5;
    c.limits.password_permits = 3;
    assert_eq!(c.limits.effective_blocking_permits(), 5);
    assert_eq!(c.limits.effective_password_permits(), 3);
}

#[test]
fn accessors_convert_units() {
    let c = AppConfig::default();
    assert_eq!(c.limits.max_body_bytes(), 10 * 1024 * 1024);
    assert_eq!(c.limits.request_timeout().as_secs(), 1800);
    assert_eq!(c.limits.header_read_timeout().as_secs(), 10);
    assert_eq!(c.limits.body_idle_timeout().as_secs(), 60);
    assert_eq!(c.limits.permit_wait().as_millis(), 1000);
    assert_eq!(c.database.acquire_timeout().as_secs(), 5);
    assert_eq!(c.database.connect_timeout().as_secs(), 5);
    assert_eq!(c.database.statement_timeout().as_secs(), 30);
    assert_eq!(c.auth.ldap.connect_timeout().as_secs(), 5);
    assert_eq!(c.auth.ldap.bind_timeout().as_secs(), 5);
    assert_eq!(c.auth.ldap.search_timeout().as_secs(), 5);
    assert_eq!(c.auth.ldap.groups_cache_ttl().as_secs(), 300);
    assert_eq!(c.upstream.connect_timeout().as_secs(), 3);
    assert_eq!(c.upstream.metadata_timeout().as_secs(), 30);
    assert_eq!(c.upstream.idle_timeout().as_secs(), 60);
    assert_eq!(c.lock.ttl().as_secs(), 30);
}

#[test]
fn direct_url_falls_back_to_the_pooled_url() {
    let mut c = AppConfig::default();
    assert!(c.database.effective_direct_url().is_none());
    c.database.url = Some(crate::ConfigSecret::new("pooled"));
    assert_eq!(
        c.database.effective_direct_url().expect("url").expose(),
        "pooled"
    );
    c.database.direct_url = Some(crate::ConfigSecret::new("direct"));
    assert_eq!(
        c.database.effective_direct_url().expect("url").expose(),
        "direct"
    );
}

// ---- config.example.yaml --------------------------------------------------

fn example_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config.example.yaml")
}

/// Environment supplying what the example deliberately leaves unset (secrets).
fn example_secrets() -> MapEnv {
    MapEnv::new()
        .with(
            "RUSTRYX_DATABASE_URL",
            "postgres://rustryx:pw@db.example.org:5432/rustryx?sslmode=verify-full",
        )
        .with("RUSTRYX_BLOB_S3_ACCESS_KEY", "access")
        .with("RUSTRYX_BLOB_S3_SECRET_KEY", "secret")
        .with(
            "RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD",
            "Tr0ub4dor&3-horse-battery",
        )
        .with(
            "RUSTRYX_AUTH_INTERNAL_JWT_KEY",
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----",
        )
        .with(
            "RUSTRYX_AUTH_COOKIE_KEY",
            "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=",
        )
        .with(
            "RUSTRYX_OBSERVABILITY_METRICS_TOKEN",
            "0123456789abcdef0123",
        )
}

#[test]
fn example_file_parses_against_the_schema_and_covers_every_key() {
    let path = example_path();
    let config = load_without_rules(Some(&path), &MapEnv::new()).expect("example parses strictly");
    assert_eq!(config.server.listen_addr.port(), 8081);

    let text = std::fs::read_to_string(&path).expect("read example");
    let tree: Value = serde_yaml_ng::from_str(&text).expect("yaml");
    for spec in KEYS {
        assert!(
            get_path(&tree, spec.path).is_some(),
            "config.example.yaml does not list {}",
            spec.path
        );
        assert!(
            text.contains(&spec.env_name()),
            "config.example.yaml does not mention {}",
            spec.env_name()
        );
    }
}

#[test]
fn example_file_is_a_valid_production_config_once_secrets_are_supplied() {
    let loaded = load(Some(&example_path()), &example_secrets()).unwrap_or_else(|e| panic!("{e}"));
    assert!(loaded.config.is_production());
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
}

#[test]
fn example_file_without_secrets_fails_with_every_missing_secret() {
    let Err(ConfigError::Invalid(issues)) = load(Some(&example_path()), &MapEnv::new()) else {
        panic!("secrets are required");
    };
    for key in [
        "database.url",
        "blob.s3_access_key",
        "blob.s3_secret_key",
        "auth.bootstrap_admin.password",
        "auth.internal_jwt_key",
        "auth.cookie_key",
        "observability.metrics_token",
    ] {
        assert!(issues.mentions(key), "{key}: {issues}");
    }
}

#[test]
fn must_not_commit_secret_values_in_the_example_file() {
    let tree: Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(example_path()).expect("read"))
            .expect("yaml");
    for spec in KEYS.iter().filter(|k| k.is_secret()) {
        assert_eq!(
            get_path(&tree, spec.path),
            Some(&Value::Null),
            "{} must be null in the example",
            spec.path
        );
    }
}

#[test]
fn example_and_reference_contain_no_dash_characters() {
    let text = std::fs::read_to_string(example_path()).expect("read");
    assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'));
}
