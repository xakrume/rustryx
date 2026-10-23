//! Scanners: the kinds, their credentials, registration records, probe results and the advisory
//! data sets they use.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{ScanKind, is_sha256_hex};
use crate::error::DomainError;
use crate::ids::{BlobId, ScannerDataSetId, ScannerId, TrustedCertificateId};
use crate::model::RepoFormat;
use crate::secret::Secret;

/// The kind of a scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScannerKind {
    /// The Trivy binary, run as a subprocess.
    Trivy,
    /// The `osv-scanner` binary, run as a subprocess.
    Osv,
    /// An external scanner behind the HTTP contract.
    Http,
}

/// How an HTTP scanner is authenticated. Secrets are sealed at rest and never returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScannerAuth {
    /// No authentication.
    None,
    /// HTTP Basic.
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
    /// A key in a header of the caller's choosing.
    ApiKey {
        /// Header name.
        header: String,
        /// Header value. Redacted in `Debug`.
        value: Secret,
    },
}

/// A registered scanner. There is no "skip certificate verification" switch: a private CA goes
/// through `ca_certificate_ids`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScannerRecord {
    /// Identifier.
    pub id: ScannerId,
    /// Unique name.
    pub name: String,
    /// Trivy, OSV or HTTP.
    pub kind: ScannerKind,
    /// `https` URL for `Http`; for `Trivy` and `Osv` always `None` (they are local processes).
    pub endpoint: Option<String>,
    /// Credentials for an HTTP scanner.
    pub auth: ScannerAuth,
    /// Extra CA certificates from the managed trust store.
    pub ca_certificate_ids: Vec<TrustedCertificateId>,
    /// What it can produce.
    pub capabilities: Vec<ScanKind>,
    /// Formats it handles (empty means all).
    pub formats: Vec<RepoFormat>,
    /// Exactly one default scanner per scan kind and format family (partial unique index).
    pub is_default: bool,
    /// A disabled scanner is not used and not probed.
    pub enabled: bool,
    /// Result of the last probe.
    pub last_probe: Option<ProbeResult>,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl ScannerRecord {
    /// An enabled, non default scanner of vulnerabilities for all formats, with no endpoint,
    /// credentials or extra CA.
    #[must_use]
    pub fn new(
        id: ScannerId,
        name: impl Into<String>,
        kind: ScannerKind,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            endpoint: None,
            auth: ScannerAuth::None,
            ca_certificate_ids: Vec::new(),
            capabilities: vec![ScanKind::Vulnerability],
            formats: Vec::new(),
            is_default: false,
            enabled: true,
            last_probe: None,
            version: 1,
            created_at: now,
        }
    }

    /// Checks the name, that an endpoint exists exactly for `Http` and is an `https` URL, and
    /// that at least one capability is set. Whether the endpoint passes the egress policy is
    /// decided by `rustryx-egress`.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.name.is_empty() || self.name.chars().count() > 128 {
            return Err(DomainError::invalid(
                "scanner name must be 1 to 128 characters",
            ));
        }
        let is_http = self.kind == ScannerKind::Http;
        if is_http != self.endpoint.is_some() {
            return Err(DomainError::invalid(
                "an endpoint is required for an http scanner and not allowed for a local one",
            ));
        }
        if self
            .endpoint
            .as_ref()
            .is_some_and(|e| !e.starts_with("https://"))
        {
            return Err(DomainError::invalid(
                "a scanner endpoint must be an https URL",
            ));
        }
        if self.capabilities.is_empty() {
            return Err(DomainError::invalid(
                "a scanner needs at least one capability",
            ));
        }
        Ok(())
    }
}

/// Result of probing a scanner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// When the probe ran.
    #[serde(with = "crate::rfc3339")]
    pub at: OffsetDateTime,
    /// Whether the scanner answered as expected.
    pub ok: bool,
    /// The scanner's version, when it reports one.
    pub version: Option<String>,
    /// Short text, no secrets and no internal addresses.
    pub message: String,
}

/// A downloaded advisory data set (Trivy database, OSV ecosystem dump), stored as a blob so
/// every stateless node can sync it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScannerDataSet {
    /// Identifier.
    pub id: ScannerDataSetId,
    /// The scanner family the data belongs to.
    pub scanner_kind: ScannerKind,
    /// `trivy-db`, `trivy-java-db`, `osv-<ecosystem>`.
    pub dataset: String,
    /// Version string of the data.
    pub version: String,
    /// The blob holding the data.
    pub blob_id: BlobId,
    /// Lowercase hex SHA-256 of the data.
    pub sha256: String,
    /// Where it came from (a registry reference or a mirror URL host).
    pub source: String,
    /// When it was fetched.
    #[serde(with = "crate::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// True for the version nodes sync.
    pub is_current: bool,
}

impl ScannerDataSet {
    /// A non current data set with an empty source (assign `source` afterwards).
    #[must_use]
    pub fn new(
        id: ScannerDataSetId,
        scanner_kind: ScannerKind,
        dataset: impl Into<String>,
        version: impl Into<String>,
        blob_id: BlobId,
        sha256: impl Into<String>,
        fetched_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            scanner_kind,
            dataset: dataset.into(),
            version: version.into(),
            blob_id,
            sha256: sha256.into(),
            source: String::new(),
            fetched_at,
            is_current: false,
        }
    }

    /// Checks that the digest is a lowercase hex SHA-256.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for any other digest text.
    pub fn validate(&self) -> Result<(), DomainError> {
        if is_sha256_hex(&self.sha256) {
            Ok(())
        } else {
            Err(DomainError::invalid(
                "data set sha256 must be 64 lowercase hex digits",
            ))
        }
    }
}
