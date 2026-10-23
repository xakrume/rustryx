//! Secret-valued configuration fields.

use std::fmt;
use std::ops::Deref;

use rustryx_core::Secret;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Text printed instead of a secret anywhere the configuration is rendered.
pub const REDACTED: &str = "[REDACTED]";

/// A configuration value that must never be printed.
///
/// `rustryx_core::Secret` serializes its real value because persistence layers
/// need it. A configuration, on the other hand, is serialized only to be shown
/// to an operator (`print-config`, debugging), so this wrapper serializes the
/// placeholder instead. Making the redaction a property of the type means a new
/// secret field cannot leak by forgetting to add it to a list: the compiler
/// forces the field to be a `ConfigSecret` and the type does the rest.
#[derive(Clone)]
pub struct ConfigSecret(Secret);

impl ConfigSecret {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Secret::new(value))
    }

    /// True when the value is empty or only whitespace. Such a secret is a
    /// configuration mistake, never a usable credential.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.expose().trim().is_empty()
    }

    /// Converts into the core secret type handed to other crates.
    #[must_use]
    pub fn to_secret(&self) -> Secret {
        self.0.clone()
    }
}

impl Deref for ConfigSecret {
    type Target = Secret;

    fn deref(&self) -> &Secret {
        &self.0
    }
}

impl fmt::Debug for ConfigSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigSecret([REDACTED])")
    }
}

impl Serialize for ConfigSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(REDACTED)
    }
}

impl<'de> Deserialize<'de> for ConfigSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn must_not_leak_secret_through_debug() {
        let secret = ConfigSecret::new("hunter2-canary");
        assert!(!format!("{secret:?}").contains("hunter2-canary"));
    }

    #[test]
    fn must_not_leak_secret_through_serialize() {
        let secret = ConfigSecret::new("hunter2-canary");
        let yaml = serde_yaml_ng::to_string(&secret).expect("serialize");
        assert!(!yaml.contains("hunter2-canary"));
        assert!(yaml.contains(REDACTED));
    }

    #[test]
    fn deserialize_keeps_real_value() {
        let secret: ConfigSecret = serde_yaml_ng::from_str("abc").expect("deserialize");
        assert_eq!(secret.expose(), "abc");
        assert_eq!(secret.to_secret().expose(), "abc");
    }

    #[test]
    fn blank_detection_covers_whitespace() {
        assert!(ConfigSecret::new("").is_blank());
        assert!(ConfigSecret::new("  \t").is_blank());
        assert!(!ConfigSecret::new(" x ").is_blank());
    }
}
