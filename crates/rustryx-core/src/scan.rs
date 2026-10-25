//! Supply chain entities: scanners, scan reports and findings, CVE allowlists, the pull gate,
//! quarantine, trust policies and advisory data sets.
//!
//! The scanned subject is a component (for OCI one manifest digest; an index is judged by its
//! children). A scan that cannot run leaves the state "not scanned", never "clean": the absence
//! of a result is a first class state and the gate treats it as such.

use serde::{Deserialize, Serialize};

/// Longest scan error text, in characters.
pub const MAX_SCAN_ERROR_CHARS: usize = 500;
/// Longest finding title, in characters.
pub const MAX_FINDING_TITLE_CHARS: usize = 200;
/// Longest vulnerability id, in characters.
pub const MAX_VULN_ID_LEN: usize = 64;
/// Most findings one report may hold; more fails the scan with `too_many_findings`.
pub const MAX_FINDINGS_PER_REPORT: usize = 50_000;
/// Longest reason of an allowlist entry or a hold, in characters.
pub const MAX_ALLOWLIST_REASON_CHARS: usize = 500;
/// Largest PEM of a trust material, in bytes.
pub const MAX_TRUST_PEM_BYTES: usize = 65_536;

mod policy;
mod report;
mod scanner;
mod trust;

pub use policy::{
    AllowlistMode, AllowlistScope, CveAllowlistEntry, GateDecision, GateMode, GateReason,
    PullGatePolicy, ScanPolicy,
};
pub use report::{
    ComponentSecurityState, Finding, LicenseBuckets, QuarantineState, RepositoryHealth,
    ScanReport, ScanStatus, ScanSummary, ScanTrigger, VulnBucket, cvss_bucket, normalize_vuln_id,
};
pub use scanner::{ProbeResult, ScannerAuth, ScannerDataSet, ScannerKind, ScannerRecord};
pub use trust::{
    CosignIdentity, NotationLevel, RequiredSignatures, TrustMaterial, TrustMaterialKind,
    TrustPolicy,
};

/// Severity of a finding, ordered from the least to the most severe.
///
/// `Negligible` never blocks a pull. `Unknown` is a finding without a usable severity; it
/// counts as `Medium` for the gate (a gate must not be bypassed by a missing score) and is
/// shown as unknown. The derived order is the declaration order, and `Severity as usize` is the
/// index into [`ScanSummary::by_severity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// No usable severity.
    Unknown,
    /// Negligible.
    Negligible,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// Critical.
    Critical,
}

impl Severity {
    /// Every severity, in order.
    pub const ALL: [Self; 6] = [
        Self::Unknown,
        Self::Negligible,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Critical,
    ];

    /// Index into [`ScanSummary::by_severity`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The snake case name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Negligible => "negligible",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Parses a severity name as scanners write it (any ASCII case). An unrecognized word is
    /// `Unknown`, never an error and never a lower severity: the gate then still counts it.
    #[must_use]
    pub fn from_scanner_label(label: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str().eq_ignore_ascii_case(label.trim()))
            .unwrap_or(Self::Unknown)
    }

    /// The severity the gate compares: `Unknown` counts as `Medium`.
    #[must_use]
    pub const fn gate_equivalent(self) -> Self {
        match self {
            Self::Unknown => Self::Medium,
            other => other,
        }
    }

    /// True when a finding of this severity blocks a pull at `threshold`: `Negligible` never
    /// does, `Unknown` is compared as `Medium`.
    #[must_use]
    pub fn blocks_at(self, threshold: Self) -> bool {
        self != Self::Negligible && self.gate_equivalent() >= threshold
    }
}

/// What a scan produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanKind {
    /// Vulnerabilities.
    Vulnerability,
    /// A software bill of materials.
    Sbom,
}

pub(crate) fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests;
