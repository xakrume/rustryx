//! Labels: one metadata mechanism with a scope (system or one project).
//!
//! The word "tag" is reserved for OCI tags; the compat tag REST resource is a compatibility view
//! of system scope labels.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{ComponentId, LabelId, ProjectId, UserId};

/// Longest label name.
pub const MAX_LABEL_NAME_LEN: usize = 128;
/// Longest label description in characters.
pub const MAX_LABEL_DESCRIPTION_CHARS: usize = 500;
/// Largest serialized `attributes` object in bytes (the size compat tags allow).
pub const MAX_LABEL_ATTRIBUTES_BYTES: usize = 20 * 1024;
/// Most labels one component may carry.
pub const MAX_LABELS_PER_COMPONENT: usize = 50;
/// Color of a label created without one (the `labels.color` column default).
pub const DEFAULT_LABEL_COLOR: &str = "#6b7280";

/// Validated label name: 1 to 128 ASCII characters of letters, digits, `_`, `-`, `.` and `:`,
/// the first one a letter or digit (the rule of the `labels.name` CHECK constraint).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct LabelName(String);

impl LabelName {
    /// Parses and validates a name.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when a rule is violated.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        if value.is_empty() || value.len() > MAX_LABEL_NAME_LEN {
            return Err(DomainError::invalid("label name length must be 1 to 128"));
        }
        let mut bytes = value.bytes();
        let first_ok = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric());
        let rest_ok =
            bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'));
        if !first_ok || !rest_ok {
            return Err(DomainError::invalid(
                "label name must start with a letter or digit and contain only letters, digits, '_', '-', '.' and ':'",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrows the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LabelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for LabelName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Where a label lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", content = "project_id", rename_all = "snake_case")]
pub enum LabelScope {
    /// Visible everywhere, edited by `nx-admin`.
    System,
    /// Visible and applicable inside one project.
    Project(ProjectId),
}

/// A label.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Label {
    /// Identifier.
    pub id: LabelId,
    /// System or project scope. Names are unique within a scope.
    pub scope: LabelScope,
    /// Name.
    pub name: LabelName,
    /// `#rrggbb` with lowercase hex digits.
    pub color: String,
    /// At most 500 characters.
    pub description: String,
    /// JSON object, at most 20 KiB serialized (the size compat tags allow), no secrets by
    /// convention.
    pub attributes: serde_json::Value,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Label {
    /// A label with the default color, no description, empty attributes and version 1.
    #[must_use]
    pub fn new(id: LabelId, scope: LabelScope, name: LabelName, now: OffsetDateTime) -> Self {
        Self {
            id,
            scope,
            name,
            color: DEFAULT_LABEL_COLOR.to_owned(),
            description: String::new(),
            attributes: serde_json::json!({}),
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Checks color, description and attributes against the limits of the table constraints.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the first field that breaks its rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_color(&self.color)?;
        if self.description.chars().count() > MAX_LABEL_DESCRIPTION_CHARS {
            return Err(DomainError::invalid(
                "label description must be at most 500 characters",
            ));
        }
        if !self.attributes.is_object() {
            return Err(DomainError::invalid(
                "label attributes must be a JSON object",
            ));
        }
        let size = serde_json::to_vec(&self.attributes).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_LABEL_ATTRIBUTES_BYTES {
            return Err(DomainError::invalid(
                "label attributes must be at most 20 KiB",
            ));
        }
        Ok(())
    }
}

/// Checks the `#rrggbb` color form (lowercase hex digits, as the column constraint requires).
///
/// # Errors
/// Returns [`DomainError::Invalid`] for any other text.
pub fn validate_color(color: &str) -> Result<(), DomainError> {
    let bytes = color.as_bytes();
    let ok = bytes.len() == 7
        && bytes[0] == b'#'
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
    if ok {
        Ok(())
    } else {
        Err(DomainError::invalid(
            "label color must be '#' followed by six lowercase hex digits",
        ))
    }
}

/// A label applied to a component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentLabel {
    /// The labelled component.
    pub component_id: ComponentId,
    /// The label.
    pub label_id: LabelId,
    /// Who applied it, `None` for the system or a deleted account.
    pub applied_by: Option<UserId>,
    /// When it was applied.
    #[serde(with = "crate::rfc3339")]
    pub applied_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    #[test]
    fn accepts_typical_label_names() {
        for ok in [
            "prod",
            "team:payments",
            "v1.2-rc_1",
            "A",
            "9lives",
            &"x".repeat(128),
        ] {
            assert!(LabelName::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_malformed_label_names() {
        let long = "x".repeat(129);
        for bad in [
            "",
            "-x",
            ".x",
            ":x",
            "_x",
            "a b",
            "a/b",
            "é",
            "a\n",
            long.as_str(),
        ] {
            assert!(LabelName::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn label_name_deserialization_validates() {
        assert!(serde_json::from_str::<LabelName>("\"ok:name\"").is_ok());
        assert!(serde_json::from_str::<LabelName>("\"no good\"").is_err());
    }

    #[test]
    fn scope_wire_form_is_adjacently_tagged() {
        assert_wire(&LabelScope::System, r#"{"scope":"system"}"#);
        let id = ProjectId::generate();
        assert_wire(
            &LabelScope::Project(id),
            &format!(r#"{{"scope":"project","project_id":"{id}"}}"#),
        );
    }

    #[test]
    fn constructor_defaults_validate_and_serialize_rfc3339() {
        let name = LabelName::parse("prod").expect("name");
        let label = Label::new(LabelId::generate(), LabelScope::System, name, at());
        assert_eq!(label.color, "#6b7280");
        assert_eq!(label.version, 1);
        assert!(
            label
                .attributes
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
        assert!(label.validate().is_ok());
        let json = round_trip_json(&label);
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        // The scope is the adjacently tagged enum, nested under the field name.
        assert_eq!(json["scope"]["scope"], "system");
    }

    #[test]
    fn colors_must_be_lowercase_rrggbb() {
        for ok in ["#000000", "#6b7280", "#abcdef", "#123abc"] {
            assert!(validate_color(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "#fff", "6b7280", "#6B7280", "#6b728g", "#6b72800", "red", "#6b 280",
        ] {
            assert!(validate_color(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validate_enforces_description_and_attribute_limits() {
        let name = LabelName::parse("prod").expect("name");
        let mut label = Label::new(LabelId::generate(), LabelScope::System, name, at());
        label.description = "d".repeat(500);
        assert!(label.validate().is_ok());
        label.description.push('d');
        assert!(label.validate().is_err());
        label.description.clear();
        label.attributes = serde_json::json!([1, 2]);
        assert!(label.validate().is_err(), "attributes must be an object");
        label.attributes = serde_json::json!({ "k": "x".repeat(MAX_LABEL_ATTRIBUTES_BYTES) });
        assert!(label.validate().is_err(), "over 20 KiB");
        label.attributes = serde_json::json!({ "k": "x".repeat(1000) });
        assert!(label.validate().is_ok());
    }

    #[test]
    fn component_label_serializes_the_apply_time() {
        let link = ComponentLabel {
            component_id: ComponentId::generate(),
            label_id: LabelId::generate(),
            applied_by: None,
            applied_at: at(),
        };
        let json = round_trip_json(&link);
        assert_eq!(json["applied_at"], "2026-10-24T10:00:00Z");
        assert!(json["applied_by"].is_null());
    }
}
