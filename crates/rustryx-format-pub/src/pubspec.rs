//! Reading `pubspec.yaml` into the JSON object that the version listing carries.
//!
//! The manifest comes from an untrusted publisher, so the YAML is not deserialised into a
//! generic value and trusted: a small visitor builds the JSON itself and counts as it goes.
//!
//! * **Alias bombs.** The YAML library resolves an alias (`*a`) by replaying the anchored events
//!   each time it is used, so a few hundred bytes can describe billions of values. The visitor
//!   charges one unit per value it is handed, replayed or not, and stops at `max_nodes`.
//!   Nothing is allocated for a value that was refused.
//! * **Depth.** Nesting past `max_depth` is refused (the library has its own, larger limit).
//! * **Ambiguity.** A mapping with a repeated key, a key that is not a string, a number that
//!   JSON cannot hold (`.nan`, `.inf`) and a value with a custom tag (`!custom`) are refused
//!   rather than guessed at, so the JSON the server stores is what the Dart client parses. The
//!   tags of the YAML core schema (`!!str`, `!!int`) are resolved by the library itself.
//! * **Size.** The text is limited before it is parsed, and a string value is limited too.
//!
//! Only the YAML subset that real manifests use is needed, and that subset is what survives these
//! rules. Multi-document files are refused by the library (one document per file).

use std::cell::{Cell, RefCell};
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use crate::name::PackageName;
use crate::version::PubVersion;

/// Limits of [`parse`].
#[derive(Debug, Clone)]
pub struct PubspecLimits {
    /// Largest manifest, in bytes.
    pub max_bytes: usize,
    /// Most values (scalars, sequences and mappings) the document may produce, aliases included.
    pub max_nodes: usize,
    /// Deepest nesting.
    pub max_depth: usize,
    /// Longest string value or key, in bytes.
    pub max_string_bytes: usize,
}

impl Default for PubspecLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_nodes: 50_000,
            max_depth: 32,
            max_string_bytes: 64 * 1024,
        }
    }
}

/// Why a manifest was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PubspecError {
    /// Larger than the limit.
    #[error("pubspec.yaml is larger than the limit")]
    TooLarge,
    /// Not UTF-8 text, or containing a NUL byte.
    #[error("pubspec.yaml is not valid UTF-8 text")]
    Encoding,
    /// A YAML syntax error. Only the position is reported, never the content.
    #[error("pubspec.yaml is not valid YAML{}", location_suffix(*.line, *.column))]
    Yaml {
        /// Line, when the parser knows it.
        line: Option<usize>,
        /// Column, when the parser knows it.
        column: Option<usize>,
    },
    /// More values than the budget allows (an alias bomb or an enormous file).
    #[error("pubspec.yaml is too complex")]
    Complexity,
    /// Nested deeper than the limit.
    #[error("pubspec.yaml is nested too deeply")]
    Depth,
    /// A string longer than the limit.
    #[error("pubspec.yaml contains a string longer than the limit")]
    StringTooLong,
    /// A mapping with the same key twice.
    #[error("pubspec.yaml repeats a key")]
    DuplicateKey,
    /// A mapping key that is not a string.
    #[error("pubspec.yaml has a key that is not a string")]
    NonStringKey,
    /// A float that is not finite.
    #[error("pubspec.yaml contains a number that is not finite")]
    NonFiniteNumber,
    /// The top level is not a mapping.
    #[error("pubspec.yaml must be a mapping")]
    NotAMapping,
    /// No `name` string.
    #[error("pubspec.yaml has no package name")]
    MissingName,
    /// `name` is not a valid package name.
    #[error("pubspec.yaml: {0}")]
    InvalidName(crate::name::NameError),
    /// No `version` string. A bare `version: 1.0` is a YAML number, not a string.
    #[error("pubspec.yaml has no version string")]
    MissingVersion,
    /// `version` is not a valid version.
    #[error("pubspec.yaml: {0}")]
    InvalidVersion(crate::version::VersionError),
}

fn location_suffix(line: Option<usize>, column: Option<usize>) -> String {
    match (line, column) {
        (Some(line), Some(column)) => format!(" (line {line}, column {column})"),
        _ => String::new(),
    }
}

impl PubspecError {
    /// A short, bounded label for metrics.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::TooLarge => "pubspec_too_large",
            Self::Encoding | Self::Yaml { .. } => "pubspec_syntax",
            Self::Complexity | Self::Depth | Self::StringTooLong => "pubspec_complexity",
            Self::DuplicateKey | Self::NonStringKey | Self::NonFiniteNumber => "pubspec_ambiguous",
            Self::NotAMapping | Self::MissingName | Self::MissingVersion => "pubspec_incomplete",
            Self::InvalidName(_) => "pubspec_name",
            Self::InvalidVersion(_) => "pubspec_version",
        }
    }
}

/// A validated manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Pubspec {
    /// The package name.
    pub name: PackageName,
    /// The version.
    pub version: PubVersion,
    /// The whole manifest as the JSON object the version listing carries.
    pub json: Map<String, Value>,
}

/// Parses and validates a manifest.
///
/// # Errors
/// A [`PubspecError`]; none of its texts contains manifest content.
pub fn parse(bytes: &[u8], limits: &PubspecLimits) -> Result<Pubspec, PubspecError> {
    if bytes.len() > limits.max_bytes {
        return Err(PubspecError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PubspecError::Encoding)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.contains('\0') {
        return Err(PubspecError::Encoding);
    }
    let value = to_json(text, limits)?;
    let Value::Object(mut json) = value else {
        return Err(PubspecError::NotAMapping);
    };
    let name = match json.get("name") {
        Some(Value::String(name)) => PackageName::parse(name).map_err(PubspecError::InvalidName)?,
        _ => return Err(PubspecError::MissingName),
    };
    let version = match json.get("version") {
        Some(Value::String(version)) => {
            PubVersion::parse(version).map_err(PubspecError::InvalidVersion)?
        }
        _ => return Err(PubspecError::MissingVersion),
    };
    // Both strings were checked to be canonical, so this changes nothing; it states that the
    // document carries the same name and version as the coordinate.
    json.insert("name".to_owned(), Value::String(name.to_string()));
    json.insert("version".to_owned(), Value::String(version.to_string()));
    Ok(Pubspec {
        name,
        version,
        json,
    })
}

/// YAML text to JSON under the budget.
fn to_json(text: &str, limits: &PubspecLimits) -> Result<Value, PubspecError> {
    let budget = Budget {
        nodes: Cell::new(0),
        refusal: RefCell::new(None),
        limits,
    };
    let deserializer = serde_yaml_ng::Deserializer::from_str(text);
    match (ValueSeed {
        budget: &budget,
        depth: 0,
    })
    .deserialize(deserializer)
    {
        Ok(value) => Ok(value),
        Err(error) => Err(budget.refusal.take().unwrap_or_else(|| {
            let location = error.location();
            PubspecError::Yaml {
                line: location.as_ref().map(serde_yaml_ng::Location::line),
                column: location.as_ref().map(serde_yaml_ng::Location::column),
            }
        })),
    }
}

/// What the visitor has consumed, and why it refused, if it did.
struct Budget<'a> {
    nodes: Cell<usize>,
    refusal: RefCell<Option<PubspecError>>,
    limits: &'a PubspecLimits,
}

impl Budget<'_> {
    /// Records the reason and produces the error that stops the library.
    fn refuse<E: de::Error>(&self, reason: PubspecError) -> E {
        let mut slot = self.refusal.borrow_mut();
        // The first refusal is the cause; later ones are fallout of unwinding.
        slot.get_or_insert(reason);
        E::custom("refused")
    }

    fn charge<E: de::Error>(&self, depth: usize) -> Result<(), E> {
        if depth > self.limits.max_depth {
            return Err(self.refuse(PubspecError::Depth));
        }
        let used = self.nodes.get().saturating_add(1);
        self.nodes.set(used);
        if used > self.limits.max_nodes {
            return Err(self.refuse(PubspecError::Complexity));
        }
        Ok(())
    }

    fn check_string<E: de::Error>(&self, text: &str) -> Result<(), E> {
        if text.len() > self.limits.max_string_bytes {
            return Err(self.refuse(PubspecError::StringTooLong));
        }
        Ok(())
    }
}

struct ValueSeed<'a> {
    budget: &'a Budget<'a>,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ValueSeed<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a YAML value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        self.budget.charge(self.depth)?;
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        self.budget.charge(self.depth)?;
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        self.budget.charge(self.depth)?;
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        self.budget.charge(self.depth)?;
        // JSON has no NaN and no infinity.
        match Number::from_f64(v) {
            Some(number) => Ok(Value::Number(number)),
            None => Err(self.budget.refuse(PubspecError::NonFiniteNumber)),
        }
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        self.budget.charge(self.depth)?;
        self.budget.check_string(v)?;
        Ok(Value::String(v.to_owned()))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        self.budget.charge(self.depth)?;
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        self.visit_unit()
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        self.budget.charge(self.depth)?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(ValueSeed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        self.budget.charge(self.depth)?;
        let mut object = Map::new();
        while let Some(key) = map.next_key_seed(KeySeed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            let value = map.next_value_seed(ValueSeed {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            if object.contains_key(&key) {
                return Err(self.budget.refuse(PubspecError::DuplicateKey));
            }
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

/// A mapping key: a string, nothing else.
struct KeySeed<'a> {
    budget: &'a Budget<'a>,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = String;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for KeySeed<'_> {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string key")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
        self.budget.charge(self.depth)?;
        self.budget.check_string(v)?;
        Ok(v.to_owned())
    }

    // A scalar that is not a string, a collection used as a key, an alias to one: all of them
    // reach the default methods of `Visitor`, which return a type error. That error is turned
    // into the specific refusal here, because the library would report it as a syntax error.
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<String, E> {
        Err(self.budget.refuse(PubspecError::NonStringKey))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<String, E> {
        Err(self.budget.refuse(PubspecError::NonStringKey))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<String, E> {
        Err(self.budget.refuse(PubspecError::NonStringKey))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<String, E> {
        Err(self.budget.refuse(PubspecError::NonStringKey))
    }

    fn visit_unit<E: de::Error>(self) -> Result<String, E> {
        Err(self.budget.refuse(PubspecError::NonStringKey))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<String, A::Error> {
        Err(self.budget.refuse(PubspecError::NonStringKey))
    }

    fn visit_map<A: MapAccess<'de>>(self, _: A) -> Result<String, A::Error> {
        Err(self.budget.refuse(PubspecError::NonStringKey))
    }
}

#[cfg(test)]
mod tests;
