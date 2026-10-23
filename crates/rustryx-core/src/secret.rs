//! A string that never shows up in `Debug` output or logs.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Secret text (password, token, client secret).
///
/// `Debug` prints a fixed placeholder. `Serialize` writes the real value because
/// persistence layers need it; API response DTOs must therefore never embed a
/// `Secret` directly (there is a test per API crate for that).
#[derive(Clone)]
pub struct Secret(SecretString);

impl Secret {
    /// Wraps a value as secret.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// Returns the secret text. Call sites should be few and obvious.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }

    /// True when the secret is the empty string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.expose().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn must_not_leak_secret_in_debug() {
        let secret = Secret::new("hunter2");
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("REDACTED"));
    }

    #[test]
    fn serde_round_trip_keeps_value() {
        let json = serde_json::to_string(&Secret::new("abc")).expect("serialize");
        let back: Secret = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.expose(), "abc");
    }
}
