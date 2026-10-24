//! Bounded YAML for documents that come from outside: `Chart.yaml` out of an uploaded archive
//! and `index.yaml` out of an upstream repository or a group member.
//!
//! **Parser choice.** `serde-saphyr` (MIT or Apache-2.0) is pure Rust, maintained, and ships
//! the resource budget this format needs. The workspace's other YAML crate, `serde_yaml_ng`,
//! sits on `unsafe-libyaml` (advisory `RUSTSEC-2024-0449`, a warning) and has no budget at all
//! (an alias bomb has to be stopped by counting before the parse, which libyaml cannot do).
//!
//! **What is bounded**, per document and before any typed value exists:
//!
//! * the input size, checked here before the parser sees a byte;
//! * parser events and nodes, nesting depth and flow nesting (a deeply nested or huge
//!   document);
//! * anchors, aliases and the replayed events of an alias (the billion laughs family);
//! * the total size of scalars.
//!
//! Merge keys (`<<`) are refused, not expanded: they are an amplification tool and no real
//! `index.yaml` or `Chart.yaml` needs them. Duplicate keys are an error: two definitions of
//! `version` in one entry are two readings of one chart.
//!
//! The parser's error text names positions and quotes input, so it goes to the log and never
//! to a client: callers get [`YamlError`], whose `Display` is generic.

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Options, SerializerOptions};

/// Limits of one parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlLimits {
    /// Largest input in bytes.
    pub max_bytes: usize,
    /// Most parser events (every token of the document counts).
    pub max_events: usize,
    /// Most nodes (mappings, sequences and scalars).
    pub max_nodes: usize,
    /// Deepest nesting of collections.
    pub max_depth: usize,
    /// Most aliases (`*name`).
    pub max_aliases: usize,
    /// Most anchors (`&name`).
    pub max_anchors: usize,
    /// Most events an alias expansion may replay in total.
    pub max_replayed_events: usize,
    /// Total bytes of scalar text.
    pub max_scalar_bytes: usize,
}

impl YamlLimits {
    /// Limits for a `Chart.yaml`: small, hand written, no reason for a large graph.
    pub const CHART: Self = Self {
        max_bytes: 1024 * 1024,
        max_events: 200_000,
        max_nodes: 100_000,
        max_depth: 32,
        max_aliases: 64,
        max_anchors: 64,
        max_replayed_events: 10_000,
        max_scalar_bytes: 1024 * 1024,
    };

    /// Limits for an `index.yaml` of at most `max_bytes` bytes. The graph limits scale with the
    /// size: a megabyte of index holds at most this many events, whatever it says, so the
    /// counts cannot be reached by an honest document and cannot be exceeded by a hostile one.
    #[must_use]
    pub const fn index(max_bytes: usize) -> Self {
        // One event needs at least one input byte, so `max_bytes` is already an upper bound on
        // a document without aliases; the factor leaves room for the few aliases that are
        // allowed below. Aliases themselves are capped far lower than that.
        Self {
            max_bytes,
            max_events: max_bytes.saturating_add(max_bytes / 2),
            max_nodes: max_bytes / 2 + 1,
            max_depth: 32,
            max_aliases: 1024,
            max_anchors: 1024,
            max_replayed_events: 100_000,
            max_scalar_bytes: max_bytes,
        }
    }

    fn options(self) -> Options {
        let mut options = Options::default();
        let mut budget = serde_saphyr::Budget::default();
        budget.max_events = self.max_events;
        budget.max_nodes = self.max_nodes;
        budget.max_depth = self.max_depth;
        budget.max_aliases = self.max_aliases;
        budget.max_anchors = self.max_anchors;
        budget.max_recorded_anchor_events = self.max_replayed_events;
        budget.max_total_scalar_bytes = self.max_scalar_bytes;
        budget.max_merge_keys = 0;
        budget.max_documents = 1;
        budget.max_reader_input_bytes = Some(self.max_bytes);
        options.budget = Some(budget);
        let mut aliases = serde_saphyr::options::AliasLimits::default();
        aliases.max_total_replayed_events = self.max_replayed_events;
        aliases.max_replay_stack_depth = 8;
        options.alias_limits = aliases;
        options.merge_keys = MergeKeyPolicy::Error;
        options.duplicate_keys = DuplicateKeyPolicy::Error;
        // The error text is never shown to a client, so a snippet of the input would only
        // add log volume.
        options.with_snippet = false;
        options
    }
}

/// Why a document was refused. The parser's own message is kept for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlError {
    /// The input is longer than the limit.
    TooLarge {
        /// The limit that was exceeded, in bytes.
        limit: usize,
    },
    /// The input is not valid UTF-8.
    NotUtf8,
    /// Not valid YAML, not the expected shape, or over a budget. The detail is for the log.
    Invalid(String),
    /// A value could not be written as YAML.
    Serialize(String),
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "document is larger than {limit} bytes"),
            Self::NotUtf8 => f.write_str("document is not valid UTF-8"),
            Self::Invalid(_) => f.write_str("document is not valid or exceeds its limits"),
            Self::Serialize(_) => f.write_str("document could not be written"),
        }
    }
}

impl std::error::Error for YamlError {}

impl YamlError {
    /// The parser's text, for the log. Never put it in a response.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Invalid(detail) | Self::Serialize(detail) => detail,
            Self::TooLarge { .. } => "too large",
            Self::NotUtf8 => "not utf-8",
        }
    }
}

/// Parses `bytes` as one YAML document into `T` under `limits`.
///
/// Never panics, whatever the input. CPU bound: callers on an async runtime run it on the
/// blocking pool behind a permit.
pub fn parse<T: DeserializeOwned>(bytes: &[u8], limits: YamlLimits) -> Result<T, YamlError> {
    if bytes.len() > limits.max_bytes {
        return Err(YamlError::TooLarge {
            limit: limits.max_bytes,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| YamlError::NotUtf8)?;
    // A byte order mark in front of the document is tolerated, the way editors write it.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_saphyr::from_str_with_options(text, limits.options())
        .map_err(|error| YamlError::Invalid(error.to_string()))
}

/// Writes `value` as a YAML document: block style, empty collections as `[]` and `{}`, strings
/// that read as another type (`"1.0"`, `"true"`, `"~"`) quoted, and no folded or literal
/// blocks, so a description with a newline stays one quoted scalar.
pub fn render<T: Serialize>(value: &T) -> Result<String, YamlError> {
    let mut options = SerializerOptions::default();
    options.prefer_block_scalars = false;
    serde_saphyr::to_string_with_options(value, options)
        .map_err(|error| YamlError::Serialize(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
    struct Doc {
        name: String,
        #[serde(default)]
        version: String,
        #[serde(default)]
        items: Vec<String>,
        #[serde(default)]
        map: BTreeMap<String, String>,
    }

    fn doc(text: &str) -> Result<Doc, YamlError> {
        parse(text.as_bytes(), YamlLimits::CHART)
    }

    #[test]
    fn reads_a_plain_document_and_ignores_unknown_keys() {
        let parsed = doc("name: web\nversion: 1.2.3\nextra: [1, 2]\nitems: [a, b]\n").unwrap();
        assert_eq!(parsed.name, "web");
        assert_eq!(parsed.version, "1.2.3");
        assert_eq!(parsed.items, ["a", "b"]);
    }

    #[test]
    fn numbers_and_booleans_are_read_as_text_for_a_string_field() {
        // `version: 1.20` and `name: 123` are what a hand written Chart.yaml contains.
        let parsed = doc("name: 123\nversion: 1.20\n").unwrap();
        assert_eq!(parsed.name, "123");
        assert_eq!(parsed.version, "1.20");
    }

    #[test]
    fn rendering_quotes_strings_that_would_read_as_another_type() {
        let value = Doc {
            name: "true".into(),
            version: "1.0".into(),
            items: vec![
                "null".into(),
                "~".into(),
                "12".into(),
                "a: b".into(),
                String::new(),
            ],
            map: BTreeMap::from([("k".into(), "line one\nline two".into())]),
        };
        let text = render(&value).unwrap();
        let back: Doc = parse(text.as_bytes(), YamlLimits::CHART).unwrap();
        assert_eq!(back, value, "{text}");
    }

    #[test]
    fn must_not_expand_an_alias_bomb() {
        // The classic billion laughs: each level multiplies the previous one by ten.
        let mut text = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for level in 1..12 {
            let previous = level - 1;
            let refs = vec![format!("*a{previous}"); 10].join(", ");
            writeln!(text, "a{level}: &a{level} [{refs}]").unwrap();
        }
        text.push_str("name: bomb\n");
        let result = doc(&text);
        assert!(matches!(result, Err(YamlError::Invalid(_))), "{result:?}");
    }

    #[test]
    fn must_not_accept_more_aliases_than_the_limit() {
        let mut text = String::from("base: &b value\nname: x\nitems:\n");
        for _ in 0..200 {
            text.push_str("  - *b\n");
        }
        assert!(matches!(doc(&text), Err(YamlError::Invalid(_))));
    }

    #[test]
    fn must_not_accept_merge_keys_or_duplicate_keys() {
        assert!(doc("base: &b {k: v}\nname: x\nmap:\n  <<: *b\n").is_err());
        assert!(doc("name: a\nname: b\n").is_err());
    }

    #[test]
    fn must_not_accept_a_document_nested_beyond_the_limit() {
        let deep = format!("name: x\nmap: {}v{}\n", "[".repeat(60), "]".repeat(60));
        assert!(doc(&deep).is_err());
        let block = (0..200)
            .map(|i| format!("{}k{i}:", " ".repeat(i * 2)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(doc(&format!("name: x\nmap:\n{block}\n")).is_err());
    }

    #[test]
    fn must_not_read_an_oversized_document() {
        let big = format!("name: x\nitems: [{}]\n", "a, ".repeat(400_000));
        assert!(matches!(
            parse::<Doc>(big.as_bytes(), YamlLimits::CHART),
            Err(YamlError::TooLarge { .. })
        ));
        let limits = YamlLimits::index(64);
        assert!(matches!(
            parse::<Doc>(&[b'a'; 65], limits),
            Err(YamlError::TooLarge { limit: 64 })
        ));
    }

    #[test]
    fn refuses_non_utf8_and_tolerates_a_byte_order_mark() {
        assert_eq!(
            parse::<Doc>(&[0xff, 0xfe, 0x00], YamlLimits::CHART),
            Err(YamlError::NotUtf8)
        );
        let with_bom = "\u{feff}name: x\n";
        assert_eq!(doc(with_bom).unwrap().name, "x");
    }

    #[test]
    fn errors_have_a_generic_display_and_a_separate_detail() {
        let error = doc("name: [unterminated").unwrap_err();
        assert_eq!(
            error.to_string(),
            "document is not valid or exceeds its limits"
        );
        assert_ne!(error.detail(), "");
        assert!(
            YamlError::Serialize("x".into())
                .to_string()
                .contains("could not be written")
        );
        assert!(YamlError::TooLarge { limit: 5 }.to_string().contains('5'));
        assert_eq!(YamlError::NotUtf8.detail(), "not utf-8");
    }

    #[test]
    fn wrong_shapes_and_empty_input_are_errors_not_panics() {
        for text in [
            "",
            "- a\n- b\n",
            "just a string",
            "name:\n  nested: 1\n",
            "\u{0}",
            "name: &a\n",
        ] {
            let _ = doc(text);
        }
        assert!(doc("").is_err());
    }
}
