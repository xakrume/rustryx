//! Helpers over the untyped YAML tree that sits between the file/env layers and
//! the typed [`crate::AppConfig`].
//!
//! The typed deserializer reports one error and stops. To satisfy "all errors in
//! one pass" the tree is checked here against the key table first (unknown keys,
//! wrong types), each offender is reported and removed so that the default
//! applies, and only then is the tree deserialized.

use serde_yaml_ng::{Mapping, Value};

use crate::error::ConfigIssue;
use crate::keys::{KeyIndex, KeySpec, Kind};

/// Sets `path` (dotted) to `value`, creating intermediate mappings.
pub(crate) fn set_path(root: &mut Mapping, path: &str, value: Value) {
    match path.split_once('.') {
        None => {
            root.insert(Value::String(path.to_owned()), value);
        }
        Some((head, rest)) => {
            let key = Value::String(head.to_owned());
            if !matches!(root.get(&key), Some(Value::Mapping(_))) {
                root.insert(key.clone(), Value::Mapping(Mapping::new()));
            }
            if let Some(Value::Mapping(child)) = root.get_mut(&key) {
                set_path(child, rest, value);
            }
        }
    }
}

/// Removes `path` (dotted). Returns true when something was removed.
pub(crate) fn remove_path(root: &mut Mapping, path: &str) -> bool {
    match path.split_once('.') {
        None => root.remove(path).is_some(),
        Some((head, rest)) => match root.get_mut(head) {
            Some(Value::Mapping(child)) => remove_path(child, rest),
            _ => false,
        },
    }
}

/// Reads `path` (dotted) from a value tree.
pub(crate) fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(root, |node, segment| match node {
        Value::Mapping(map) => map.get(segment),
        _ => None,
    })
}

/// Checks keys and value types of `root` against the table. Offending entries are
/// reported and removed; `Null` values are removed silently (null means "unset",
/// which Helm and templating produce for empty values).
pub(crate) fn check_structure(root: &mut Mapping, index: &KeyIndex) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    check_node("", root, index, &mut issues);
    issues
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

fn check_node(prefix: &str, node: &mut Mapping, index: &KeyIndex, issues: &mut Vec<ConfigIssue>) {
    let keys: Vec<Value> = node.keys().cloned().collect();
    for key in keys {
        let Some(name) = key.as_str().map(str::to_owned) else {
            issues.push(ConfigIssue::new(
                prefix,
                "keys must be text, found a non-text key",
            ));
            node.remove(&key);
            continue;
        };
        let path = join(prefix, &name);
        if index.is_section(&path) {
            check_section(&path, &key, node, index, issues);
        } else if let Some(spec) = index.leaf(&path) {
            check_leaf(spec, &key, node, issues);
        } else {
            issues.push(ConfigIssue::new(
                path,
                unknown_key_message(prefix, &name, index),
            ));
            node.remove(&key);
        }
    }
}

fn check_section(
    path: &str,
    key: &Value,
    node: &mut Mapping,
    index: &KeyIndex,
    issues: &mut Vec<ConfigIssue>,
) {
    match node.get_mut(key) {
        Some(Value::Mapping(child)) => check_node(path, child, index, issues),
        Some(Value::Null) => {
            node.remove(key);
        }
        _ => {
            issues.push(ConfigIssue::new(
                path,
                "expected a section (a mapping of keys)",
            ));
            node.remove(key);
        }
    }
}

fn check_leaf(spec: &KeySpec, key: &Value, node: &mut Mapping, issues: &mut Vec<ConfigIssue>) {
    let verdict = match node.get(key) {
        Some(Value::Null) => Err(None),
        Some(value) => check_kind(spec, value).map_err(Some),
        None => return,
    };
    match verdict {
        Ok(()) => {}
        Err(None) => {
            node.remove(key);
        }
        Err(Some(message)) => {
            issues.push(ConfigIssue::new(spec.path, message));
            node.remove(key);
        }
    }
}

/// Checks that a YAML value has the shape the key expects. Messages never echo
/// the offending value (it may be a secret put under the wrong key).
fn check_kind(spec: &KeySpec, value: &Value) -> Result<(), String> {
    let accepted = match (spec.kind, value) {
        (Kind::Bool, Value::Bool(_))
        | (Kind::Float, Value::Number(_))
        | (Kind::Str | Kind::Secret, Value::String(_)) => true,
        (Kind::UInt, Value::Number(n)) => n.as_u64().is_some(),
        (Kind::StrList, Value::Sequence(items)) => items.iter().all(Value::is_string),
        (Kind::Enum(variants), Value::String(s)) => variants.contains(&s.as_str()),
        _ => false,
    };
    if accepted {
        return Ok(());
    }
    Err(match spec.kind {
        Kind::Enum(variants) => format!("must be one of: {}", variants.join(", ")),
        Kind::Bool => "expected true or false".to_owned(),
        Kind::UInt => "expected a non-negative integer".to_owned(),
        Kind::Float => "expected a number".to_owned(),
        Kind::Str | Kind::Secret => "expected text (quote numeric-looking values)".to_owned(),
        Kind::StrList => "expected a list of text values".to_owned(),
    })
}

fn unknown_key_message(prefix: &str, name: &str, index: &KeyIndex) -> String {
    let nearest = index
        .children(prefix)
        .into_iter()
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance);
    match nearest {
        Some((_, candidate)) => format!("unknown key, did you mean `{candidate}`?"),
        None => "unknown key".to_owned(),
    }
}

/// Levenshtein distance, enough for typo hints on short ASCII key names.
fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b_chars.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != *cb);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current.push(substitution.min(insertion).min(deletion));
        }
        previous = current;
    }
    previous[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(yaml: &str) -> Mapping {
        match serde_yaml_ng::from_str::<Value>(yaml).expect("yaml") {
            Value::Mapping(m) => m,
            other => panic!("not a mapping: {other:?}"),
        }
    }

    #[test]
    fn set_and_get_create_nested_sections() {
        let mut root = Mapping::new();
        set_path(&mut root, "auth.oidc.issuer", Value::String("x".into()));
        let value = Value::Mapping(root);
        assert_eq!(
            get_path(&value, "auth.oidc.issuer"),
            Some(&Value::String("x".into()))
        );
        assert!(get_path(&value, "auth.ldap").is_none());
    }

    #[test]
    fn remove_path_reports_presence() {
        let mut root = tree("a:\n  b: 1\n");
        assert!(remove_path(&mut root, "a.b"));
        assert!(!remove_path(&mut root, "a.b"));
        assert!(!remove_path(&mut root, "x.y"));
    }

    #[test]
    fn unknown_keys_get_a_typo_hint() {
        let mut root = tree("server:\n  listen_adr: x\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "server.listen_adr");
        assert!(issues[0].message.contains("did you mean `listen_addr`"));
    }

    #[test]
    fn far_unknown_key_has_no_hint() {
        let mut root = tree("totally_unrelated: 1\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert_eq!(issues[0].message, "unknown key");
    }

    #[test]
    fn wrong_types_are_each_reported_and_removed() {
        let mut root = tree(
            "limits:\n  max_body_mb: lots\nauth:\n  anonymous_enabled: 3\n  oidc:\n    admin_groups: x\n",
        );
        let issues = check_structure(&mut root, &KeyIndex::new());
        let keys: Vec<&str> = issues.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "limits.max_body_mb",
                "auth.anonymous_enabled",
                "auth.oidc.admin_groups"
            ]
        );
        assert!(get_path(&Value::Mapping(root), "limits.max_body_mb").is_none());
    }

    #[test]
    fn must_not_echo_value_of_mistyped_secret() {
        let mut root = tree("database:\n  url: [canary-secret]\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert_eq!(issues.len(), 1);
        assert!(!issues[0].message.contains("canary-secret"));
    }

    #[test]
    fn enum_values_are_checked_against_the_variants() {
        let mut root = tree("lock:\n  backend: etcd\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert!(issues[0].message.contains("postgres, redis"));
    }

    #[test]
    fn null_means_unset_and_is_dropped() {
        let mut root = tree("database:\n  url: null\nauth:\n  oidc:\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert_eq!(issues, Vec::new());
        let value = Value::Mapping(root);
        assert!(get_path(&value, "database.url").is_none());
        assert!(get_path(&value, "auth.oidc").is_none());
    }

    #[test]
    fn section_given_as_scalar_is_reported() {
        let mut root = tree("server: nope\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert_eq!(issues[0].key, "server");
    }

    #[test]
    fn non_text_keys_are_reported() {
        let mut root = tree("1: x\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("non-text key"));
    }

    #[test]
    fn negative_integers_are_rejected_for_unsigned_keys() {
        let mut root = tree("gc:\n  batch_size: -1\n");
        let issues = check_structure(&mut root, &KeyIndex::new());
        assert_eq!(issues[0].key, "gc.batch_size");
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
