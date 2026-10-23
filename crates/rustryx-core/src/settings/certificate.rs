//! A certificate of the managed outbound trust store.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{TrustedCertificateId, UserId};

/// Largest PEM of a trusted certificate, in bytes.
pub const MAX_CERTIFICATE_PEM_BYTES: usize = 16 * 1024;

/// A certificate in the managed outbound trust store. Public data, one certificate per record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrustedCertificate {
    /// Identifier.
    pub id: TrustedCertificateId,
    /// Unique name.
    pub name: String,
    /// The certificate in PEM form.
    pub pem: String,
    /// Subject, as parsed.
    pub subject: String,
    /// Issuer, as parsed.
    pub issuer: String,
    /// Lowercase hex SHA-256 of the DER form. Unique.
    pub fingerprint_sha256: String,
    /// Start of validity.
    #[serde(with = "crate::rfc3339")]
    pub not_before: OffsetDateTime,
    /// End of validity.
    #[serde(with = "crate::rfc3339")]
    pub not_after: OffsetDateTime,
    /// basicConstraints CA flag.
    pub is_ca: bool,
    /// A leaf (a pinned self-signed server certificate) is accepted only when the uploader
    /// says so explicitly.
    pub allow_leaf: bool,
    /// Who uploaded it.
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl TrustedCertificate {
    /// A CA certificate record with empty subject and issuer (assign the parsed values
    /// afterwards) and no leaf permission.
    #[must_use]
    pub fn new(
        id: TrustedCertificateId,
        name: impl Into<String>,
        pem: impl Into<String>,
        fingerprint_sha256: impl Into<String>,
        not_before: OffsetDateTime,
        not_after: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            pem: pem.into(),
            subject: String::new(),
            issuer: String::new(),
            fingerprint_sha256: fingerprint_sha256.into(),
            not_before,
            not_after,
            is_ca: true,
            allow_leaf: false,
            created_by: None,
            created_at: now,
        }
    }

    /// True while the certificate is inside its validity window. An expired certificate is not
    /// trusted, and not trusting it happens at snapshot build time, so a certificate that
    /// expires at midnight stops being trusted at midnight.
    #[must_use]
    pub fn is_valid_at(&self, now: OffsetDateTime) -> bool {
        self.not_before <= now && now < self.not_after
    }

    /// Checks the table constraints: a name of 1 to 128 characters, a PEM of at most 16 KiB
    /// without a private key block, subject and issuer of at most 512 characters, a lowercase
    /// hex SHA-256 fingerprint, a validity window that is not empty, and that a leaf has the
    /// explicit flag.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.name.is_empty() || self.name.chars().count() > 128 {
            return Err(DomainError::invalid(
                "certificate name must be 1 to 128 characters",
            ));
        }
        if self.pem.len() > MAX_CERTIFICATE_PEM_BYTES || self.pem.contains("PRIVATE KEY") {
            return Err(DomainError::invalid(
                "certificate PEM must be at most 16 KiB and must not contain a private key",
            ));
        }
        if self.subject.chars().count() > 512 || self.issuer.chars().count() > 512 {
            return Err(DomainError::invalid(
                "subject and issuer must be at most 512 characters",
            ));
        }
        let fingerprint_ok = self.fingerprint_sha256.len() == 64
            && self
                .fingerprint_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !fingerprint_ok {
            return Err(DomainError::invalid(
                "fingerprint must be 64 lowercase hex digits",
            ));
        }
        if self.not_after <= self.not_before {
            return Err(DomainError::invalid("certificate validity window is empty"));
        }
        if !self.is_ca && !self.allow_leaf {
            return Err(DomainError::invalid(
                "a leaf certificate needs the explicit allow_leaf flag",
            ));
        }
        Ok(())
    }
}
