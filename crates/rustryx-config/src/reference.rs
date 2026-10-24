//! Generated key reference for `docs/config.md`.
//!
//! The table in the docs is produced from [`crate::keys::KEYS`] and the serialized
//! default configuration, and a test compares it with the committed file, so the
//! documentation cannot miss a key or show a stale default.

use serde_yaml_ng::Value;

use crate::AppConfig;
use crate::keys::{KEYS, KeySpec, Kind};
use crate::tree::get_path;

/// Marker that opens the generated block in `docs/config.md`.
pub const BEGIN_MARKER: &str = "<!-- BEGIN GENERATED KEY REFERENCE -->";
/// Marker that closes the generated block in `docs/config.md`.
pub const END_MARKER: &str = "<!-- END GENERATED KEY REFERENCE -->";

/// Renders the key reference as a Markdown table.
#[must_use]
pub fn key_reference_markdown() -> String {
    let defaults = serde_yaml_ng::to_value(AppConfig::default()).unwrap_or(Value::Null);
    let header = "| Key | Environment variable | Type | Default | Sensitivity | Description |\n\
                  |---|---|---|---|---|---|\n";
    let rows: Vec<String> = KEYS
        .iter()
        .map(|spec| {
            let default =
                get_path(&defaults, spec.path).map_or_else(|| "unset".to_owned(), render_default);
            format!(
                "| `{}` | `{}` | {} | {} | {} | {} |\n",
                spec.path,
                spec.env_name(),
                type_label(spec),
                default,
                sensitivity(spec),
                spec.doc
            )
        })
        .collect();
    format!("{header}{}", rows.concat())
}

fn type_label(spec: &KeySpec) -> String {
    match spec.kind {
        Kind::Bool => "bool".to_owned(),
        Kind::UInt => "integer".to_owned(),
        Kind::Float => "number".to_owned(),
        Kind::Str | Kind::Secret => "string".to_owned(),
        Kind::StrList => "list".to_owned(),
        Kind::UIntList => "list of integers".to_owned(),
        Kind::Enum(variants) => format!("`{}`", variants.join("` / `")),
    }
}

fn sensitivity(spec: &KeySpec) -> String {
    match spec.env_file_name() {
        Some(file_var) => format!("secret (also `{file_var}`)"),
        None => "-".to_owned(),
    }
}

fn render_default(value: &Value) -> String {
    match value {
        Value::Null => "unset".to_owned(),
        Value::Bool(b) => format!("`{b}`"),
        Value::Number(n) => format!("`{n}`"),
        Value::String(s) => format!("`\"{s}\"`"),
        Value::Sequence(items) if items.is_empty() => "`[]`".to_owned(),
        Value::Sequence(items) => {
            let inner: Vec<String> = items
                .iter()
                .map(|item| match item {
                    Value::String(s) => format!("\"{s}\""),
                    Value::Number(n) => n.to_string(),
                    other => format!("{other:?}"),
                })
                .collect();
            format!("`[{}]`", inner.join(", "))
        }
        _ => "-".to_owned(),
    }
}

/// Replaces the text between the markers with `table`. Returns `None` when a
/// marker is missing or they are out of order.
#[must_use]
pub fn splice(document: &str, table: &str) -> Option<String> {
    let begin = document.find(BEGIN_MARKER)?;
    let end = document.find(END_MARKER)?;
    if end < begin {
        return None;
    }
    let head = &document[..begin + BEGIN_MARKER.len()];
    let tail = &document[end..];
    Some(format!("{head}\n\n{table}\n{tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/config.md")
    }

    #[test]
    fn reference_lists_every_key_once() {
        let table = key_reference_markdown();
        for spec in KEYS {
            let cell = format!("| `{}` |", spec.path);
            assert_eq!(table.matches(&cell).count(), 1, "{}", spec.path);
        }
    }

    #[test]
    fn secrets_show_no_default_value() {
        let table = key_reference_markdown();
        let row = table
            .lines()
            .find(|l| l.starts_with("| `database.url`"))
            .expect("row");
        assert!(row.contains("| unset |"));
        assert!(row.contains("RUSTRYX_DATABASE_URL_FILE"));
    }

    #[test]
    fn splice_requires_both_markers_in_order() {
        assert!(splice("no markers", "t").is_none());
        let reversed = format!("{END_MARKER}\n{BEGIN_MARKER}");
        assert!(splice(&reversed, "t").is_none());
        let doc = format!("a\n{BEGIN_MARKER}\nold\n{END_MARKER}\nz\n");
        let out = splice(&doc, "NEW\n").expect("spliced");
        assert!(out.contains("NEW") && !out.contains("old"));
        assert!(out.starts_with("a\n") && out.ends_with("z\n"));
    }

    /// Run with `RUSTRYX_BLESS_DOCS=1 cargo test -p rustryx-config docs_` to rewrite
    /// the generated block after changing a key.
    #[test]
    fn docs_config_md_key_reference_is_up_to_date() {
        let path = docs_path();
        let current = std::fs::read_to_string(&path).expect("docs/config.md exists");
        let expected = splice(&current, &key_reference_markdown())
            .expect("docs/config.md contains the generated block markers");
        if std::env::var_os("RUSTRYX_BLESS_DOCS").is_some() {
            std::fs::write(&path, &expected).expect("rewrite docs/config.md");
            return;
        }
        assert!(
            current == expected,
            "docs/config.md key reference is stale: run `RUSTRYX_BLESS_DOCS=1 cargo test -p rustryx-config docs_`"
        );
    }
}
