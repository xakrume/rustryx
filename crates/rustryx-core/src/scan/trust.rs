//! Trust policies for signature verification and the trust material they refer to.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::policy::GateMode;
use super::{MAX_TRUST_PEM_BYTES, is_sha256_hex};
use crate::error::DomainError;
use crate::ids::{TrustMaterialId, TrustPolicyId, UserId};
use crate::lifecycle::PolicyScope;

/// Which signature schemes a trust policy requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredSignatures {
    /// No signature required.
    None,
    /// A verifying Cosign signature.
    Cosign,
    /// A verifying Notation signature.
    Notation,
    /// Both must verify.
    Both,
}

/// Notation verification level. There is no `skip`: it is the presence-only check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotationLevel {
    /// Every check must pass.
    Strict,
    /// Revocation and expiry problems are logged, the rest must pass.
    Permissive,
    /// Nothing is enforced, results are recorded.
    Audit,
}

/// A trusted Cosign identity (issuer and subject, exact or as a pattern).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosignIdentity {
    /// OIDC issuer.
    pub issuer: String,
    /// Certificate subject.
    pub subject: String,
    /// `issuer` is a regular expression.
    pub issuer_is_pattern: bool,
    /// `subject` is a regular expression.
    pub subject_is_pattern: bool,
}

/// A trust policy. A policy that only checks that a signature exists is not representable:
/// every accepted signature has to verify against the material listed here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrustPolicy {
    /// Identifier.
    pub id: TrustPolicyId,
    /// Where it applies.
    pub scope: PolicyScope,
    /// Required schemes.
    pub required: RequiredSignatures,
    /// Enforce or audit only.
    pub mode: GateMode,
    /// Cosign public keys.
    pub cosign_keys: Vec<TrustMaterialId>,
    /// Cosign roots (Fulcio).
    pub cosign_roots: Vec<TrustMaterialId>,
    /// Trusted Cosign identities.
    pub cosign_identities: Vec<CosignIdentity>,
    /// Require a transparency log entry.
    pub cosign_require_transparency_log: bool,
    /// Notation trust stores.
    pub notation_stores: Vec<TrustMaterialId>,
    /// Trusted Notation identities (certificate subjects).
    pub notation_identities: Vec<String>,
    /// Notation verification level.
    pub notation_level: NotationLevel,
    /// For an index: also verify every child image. Default true.
    pub verify_children: bool,
    /// Optimistic concurrency version.
    pub version: i64,
}

impl TrustPolicy {
    /// A policy that requires nothing, in audit mode, that verifies the children of an index
    /// and uses the `Strict` Notation level once schemes are required.
    #[must_use]
    pub fn new(id: TrustPolicyId, scope: PolicyScope) -> Self {
        Self {
            id,
            scope,
            required: RequiredSignatures::None,
            mode: GateMode::AuditOnly,
            cosign_keys: Vec::new(),
            cosign_roots: Vec::new(),
            cosign_identities: Vec::new(),
            cosign_require_transparency_log: false,
            notation_stores: Vec::new(),
            notation_identities: Vec::new(),
            notation_level: NotationLevel::Strict,
            verify_children: true,
            version: 1,
        }
    }

    /// Checks that a required scheme has material to verify against. Without it every
    /// signature would be unverifiable, which is a policy nobody can satisfy rather than the
    /// "a signature exists" check the type refuses to represent.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when a required scheme has no key, root or store.
    pub fn validate(&self) -> Result<(), DomainError> {
        let needs_cosign = matches!(
            self.required,
            RequiredSignatures::Cosign | RequiredSignatures::Both
        );
        let needs_notation = matches!(
            self.required,
            RequiredSignatures::Notation | RequiredSignatures::Both
        );
        if needs_cosign && self.cosign_keys.is_empty() && self.cosign_roots.is_empty() {
            return Err(DomainError::invalid(
                "required cosign signatures need at least one public key or root",
            ));
        }
        if needs_notation && self.notation_stores.is_empty() {
            return Err(DomainError::invalid(
                "required notation signatures need at least one trust store",
            ));
        }
        Ok(())
    }
}

/// What kind of trust material a record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustMaterialKind {
    /// A Cosign public key.
    CosignPublicKey,
    /// A Cosign (Fulcio) root certificate.
    CosignRoot,
    /// A Notation CA certificate.
    NotationCa,
}

/// Public material only. A PEM that contains a private key block is refused at upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrustMaterial {
    /// Identifier.
    pub id: TrustMaterialId,
    /// Key, root or store certificate.
    pub kind: TrustMaterialKind,
    /// Unique name.
    pub name: String,
    /// The PEM text.
    pub pem: String,
    /// Lowercase hex SHA-256 of the material.
    pub fingerprint_sha256: String,
    /// Expiry of a certificate.
    #[serde(default, with = "crate::rfc3339::option")]
    pub not_after: Option<OffsetDateTime>,
    /// Who uploaded it.
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl TrustMaterial {
    /// A record without expiry or uploader.
    #[must_use]
    pub fn new(
        id: TrustMaterialId,
        kind: TrustMaterialKind,
        name: impl Into<String>,
        pem: impl Into<String>,
        fingerprint_sha256: impl Into<String>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            pem: pem.into(),
            fingerprint_sha256: fingerprint_sha256.into(),
            not_after: None,
            created_by: None,
            created_at: now,
        }
    }

    /// Checks name, PEM and fingerprint against the table constraints: a PEM of at most 64 KiB
    /// that contains no private key block, and a lowercase hex SHA-256.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.name.is_empty() || self.name.chars().count() > 128 {
            return Err(DomainError::invalid(
                "trust material name must be 1 to 128 characters",
            ));
        }
        if self.pem.len() > MAX_TRUST_PEM_BYTES {
            return Err(DomainError::invalid(
                "trust material must be at most 64 KiB",
            ));
        }
        if self.pem.contains("PRIVATE KEY") {
            return Err(DomainError::invalid("private key material is not accepted"));
        }
        if !is_sha256_hex(&self.fingerprint_sha256) {
            return Err(DomainError::invalid(
                "fingerprint must be 64 lowercase hex digits",
            ));
        }
        Ok(())
    }
}
