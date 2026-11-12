//! Named remote registries that proxy repositories, replication rules and scanners reference.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{RegistryId, TrustedCertificateId};
use crate::secret::Secret;

/// Longest registry name, in characters.
pub const MAX_REGISTRY_NAME_CHARS: usize = 128;

/// Which replication dialect a registry speaks.
///
/// `Rustryx` additionally negotiates the replication protocol version with the peer; `Oci` is
/// any distribution compatible registry. Vendor token exchange adapters are not part of wave 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryProvider {
    /// Another Rustryx node.
    Rustryx,
    /// A generic OCI distribution registry.
    Oci,
}

/// How a registry is authenticated. Secrets are sealed at rest and never returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RegistryAuth {
    /// Anonymous.
    None,
    /// Also used for the Docker token handshake: the client follows the
    /// `WWW-Authenticate: Bearer` challenge itself.
    Basic {
        /// User name.
        username: String,
        /// Password. Redacted in `Debug`.
        password: Secret,
    },
    /// A static bearer token.
    Bearer {
        /// The token. Redacted in `Debug`.
        token: Secret,
    },
}

/// A named, reusable remote registry. There is no "verify certificate" switch: a private CA
/// goes through `ca_certificate_ids` (the managed trust store).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Registry {
    /// Identifier.
    pub id: RegistryId,
    /// Unique name.
    pub name: String,
    /// Dialect.
    pub provider: RegistryProvider,
    /// `https` origin only: no path, query, fragment or user info. `http` only for hosts of
    /// `egress.registries.allow_insecure_http_hosts`.
    pub url: String,
    /// Credentials.
    pub auth: RegistryAuth,
    /// Extra CA certificates from the managed trust store.
    pub ca_certificate_ids: Vec<TrustedCertificateId>,
    /// Result of the last connection test.
    pub last_test: Option<RegistryTest>,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Registry {
    /// An anonymous registry without extra CA or test result.
    #[must_use]
    pub fn new(
        id: RegistryId,
        name: impl Into<String>,
        provider: RegistryProvider,
        url: impl Into<String>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            provider,
            url: url.into(),
            auth: RegistryAuth::None,
            ca_certificate_ids: Vec::new(),
            last_test: None,
            version: 1,
            created_at: now,
        }
    }

    /// Checks the name and that the URL is an origin: `http` or `https`, a host, and nothing
    /// after it (no path, query, fragment or user info; the `registries.url` constraint).
    /// Whether the host is reachable and whether `http` is allowed for it is decided by the
    /// `Registry` egress policy, not here.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.name.is_empty() || self.name.chars().count() > MAX_REGISTRY_NAME_CHARS {
            return Err(DomainError::invalid(
                "registry name must be 1 to 128 characters",
            ));
        }
        validate_origin(&self.url)
    }
}

fn validate_origin(url: &str) -> Result<(), DomainError> {
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| DomainError::invalid("registry URL must start with https:// or http://"))?;
    let bad = host.is_empty()
        || host
            .chars()
            .any(|c| matches!(c, '/' | '@' | '?' | '#') || c.is_whitespace() || c.is_control());
    if bad {
        return Err(DomainError::invalid(
            "registry URL must be an origin without path, query, fragment or user info",
        ));
    }
    Ok(())
}

/// Result of testing a registry connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryTest {
    /// When the test ran.
    #[serde(with = "crate::rfc3339")]
    pub at: OffsetDateTime,
    /// Whether it succeeded.
    pub ok: bool,
    /// Replication protocol version of a Rustryx peer.
    pub protocol_version: Option<u32>,
    /// Short text, no secrets and no internal addresses.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn registry(url: &str) -> Registry {
        Registry::new(
            RegistryId::generate(),
            "origin",
            RegistryProvider::Oci,
            url,
            at(),
        )
    }

    #[test]
    fn providers_and_auth_use_snake_case_tags() {
        assert_wire(&RegistryProvider::Rustryx, "\"rustryx\"");
        assert_wire(&RegistryProvider::Oci, "\"oci\"");
        assert_eq!(round_trip_json(&RegistryAuth::None)["type"], "none");
        let basic = RegistryAuth::Basic {
            username: "u".into(),
            password: Secret::new("p"),
        };
        assert_eq!(round_trip_json(&basic)["type"], "basic");
        assert_eq!(
            round_trip_json(&RegistryAuth::Bearer {
                token: Secret::new("t")
            })["type"],
            "bearer"
        );
    }

    #[test]
    fn must_not_leak_a_registry_password_through_debug() {
        let mut registry = registry("https://registry.example.org");
        registry.auth = RegistryAuth::Basic {
            username: "robot".into(),
            password: Secret::new("canary-pw"),
        };
        let rendered = format!("{registry:?} {:?}", registry.auth);
        assert!(!rendered.contains("canary-pw"), "{rendered}");
        registry.auth = RegistryAuth::Bearer {
            token: Secret::new("canary-token"),
        };
        assert!(!format!("{registry:?}").contains("canary-token"));
    }

    #[test]
    fn constructor_defaults() {
        let registry = registry("https://registry.example.org");
        assert!(matches!(registry.auth, RegistryAuth::None));
        assert!(registry.ca_certificate_ids.is_empty() && registry.last_test.is_none());
        assert_eq!(registry.version, 1);
        assert!(registry.validate().is_ok());
    }

    #[test]
    fn url_must_be_an_origin() {
        for ok in [
            "https://registry.example.org",
            "https://registry.example.org:5000",
            "http://10.0.0.5:8080",
            "https://[2001:db8::1]:443",
        ] {
            assert!(registry(ok).validate().is_ok(), "{ok}");
        }
        for bad in [
            "",
            "registry.example.org",
            "ftp://registry.example.org",
            "https://",
            "https://registry.example.org/",
            "https://registry.example.org/v2",
            "https://registry.example.org?x=1",
            "https://registry.example.org#frag",
            "https://user@registry.example.org",
            "https://user:pw@registry.example.org",
            "https://registry .example.org",
            "https://registry\n.example.org",
            "file:///etc/passwd",
        ] {
            assert!(registry(bad).validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn name_limits_and_wire_form() {
        let mut registry = registry("https://registry.example.org");
        registry.name = String::new();
        assert!(registry.validate().is_err());
        registry.name = "n".repeat(129);
        assert!(registry.validate().is_err());
        registry.name = "n".repeat(128);
        assert!(registry.validate().is_ok());
        registry.last_test = Some(RegistryTest {
            at: at(),
            ok: true,
            protocol_version: Some(1),
            message: "ok".into(),
        });
        let json = round_trip_json(&registry);
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["last_test"]["at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["provider"], "oci");
    }
}
