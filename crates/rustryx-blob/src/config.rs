//! JSON configuration of blob stores (`BlobStoreDef::config`) and build errors.
//!
//! The JSON holds settings and *references* to secrets, never secret values. Both
//! structs reject unknown fields, so a secret pasted into the wrong place
//! (`"secret_access_key": "..."`) fails loudly instead of being silently ignored.

use std::path::PathBuf;
use std::time::Duration;

use rustryx_core::BlobError;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::key::{KeyError, validate_prefix};
use crate::secrets::SecretError;

const MIN_PART_MIB: u32 = 5;

/// S3 requires every part but the last to be at least this large.
pub const MIN_PART_BYTES: usize = MIN_PART_MIB as usize * 1024 * 1024;

/// Default part size (architecture 1.4: parts of 16 MiB).
pub const DEFAULT_PART_BYTES: usize = 16 * 1024 * 1024;

/// S3 allows at most this many parts per upload.
pub const MAX_PARTS: usize = 10_000;

const MAX_PART_MIB: u32 = 128;
const MAX_CONCURRENT_UPLOADS: usize = 1024;

/// Longest lifetime of a presigned download URL (`BLOB-16`). A URL is a bearer
/// capability; the shorter it lives, the less a leaked one is worth.
pub const MAX_PRESIGN_TTL_SECS: u32 = 900;

/// Default free space floor of a local store, in percent (SRE-34).
pub const DEFAULT_MIN_FREE_PERCENT: u8 = 10;

/// The floor cannot exceed this: above it a store would refuse writes on a healthy disk.
/// Same limit as `blob.local_min_free_percent` in `rustryx-config`.
const MAX_MIN_FREE_PERCENT: u8 = 90;

/// Why a store could not be built from its definition.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The definition is wrong. The message never contains configuration values
    /// (they might be misplaced secrets), only field names and limits.
    #[error("invalid blob store configuration: {0}")]
    Invalid(String),
    /// A referenced secret is missing or unreadable.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The S3 client could not be constructed.
    #[error("could not initialise the S3 client")]
    Client(#[source] object_store::Error),
    /// The local store root could not be opened.
    #[error("could not open the local blob store")]
    Open(#[source] BlobError),
    /// The definition names a kind this build cannot construct (Azure and groups arrive
    /// with stream F).
    #[error("blob store kind `{0}` is not supported by this build")]
    UnsupportedKind(&'static str),
}

/// Parses a definition's JSON into `T`. Serde reports unknown or missing fields by
/// name only, but a type mismatch quotes the offending value, which could be a
/// secret that landed in a numeric field; those messages are replaced.
pub(crate) fn parse_config<T: DeserializeOwned>(
    kind: &'static str,
    config: &serde_json::Value,
) -> Result<T, BuildError> {
    serde_json::from_value(config.clone()).map_err(|err| {
        let text = err.to_string();
        let field_only = text.starts_with("unknown field") || text.starts_with("missing field");
        if field_only {
            BuildError::Invalid(format!("{kind}: {text}"))
        } else {
            BuildError::Invalid(format!(
                "{kind}: a field has the wrong type or an invalid value"
            ))
        }
    })
}

/// Settings of a local store.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalConfig {
    /// Root directory. Must be absolute: a relative path would silently depend on
    /// the working directory of whichever process built the store.
    pub path: PathBuf,
    /// Writes are refused with `InsufficientStorage` below this percentage of free space
    /// (0 to 90, default 10; 0 switches the refusal off, the free space gauges stay on).
    #[serde(default = "default_min_free_percent")]
    pub min_free_percent: u8,
}

const fn default_min_free_percent() -> u8 {
    DEFAULT_MIN_FREE_PERCENT
}

impl LocalConfig {
    pub(crate) fn validate(&self) -> Result<(), BuildError> {
        if !self.path.is_absolute() {
            return Err(BuildError::Invalid(
                "local: `path` must be an absolute path".into(),
            ));
        }
        if self.min_free_percent > MAX_MIN_FREE_PERCENT {
            return Err(BuildError::Invalid(
                "local: `min_free_percent` must be between 0 and 90".into(),
            ));
        }
        Ok(())
    }
}

/// Settings of an S3 store.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    /// Bucket name.
    pub bucket: String,
    /// Region used for request signing. `MinIO` and Ceph accept any non-empty value.
    #[serde(default = "default_region")]
    pub region: String,
    /// Custom endpoint (`MinIO`, Ceph RGW). Absent means AWS.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Path-style addressing (`https://host/bucket/key`). Defaults to true when an
    /// endpoint is set (`MinIO`, Ceph) and to false for AWS.
    #[serde(default)]
    pub path_style: Option<bool>,
    /// Permit a plain `http://` endpoint. Off by default; only for a trusted network.
    #[serde(default)]
    pub allow_http: bool,
    /// Object name prefix inside the bucket, without leading or trailing slash.
    #[serde(default)]
    pub prefix: String,
    /// Reference (not value) of the access key id.
    #[serde(default)]
    pub access_key_id_ref: Option<String>,
    /// Reference (not value) of the secret access key.
    #[serde(default)]
    pub secret_access_key_ref: Option<String>,
    /// Reference (not value) of an optional session token.
    #[serde(default)]
    pub session_token_ref: Option<String>,
    /// Multipart part size in MiB (5 to 128). Memory per upload is at most this.
    #[serde(default = "default_part_mib")]
    pub part_size_mib: u32,
    /// Maximum concurrent uploads (each may buffer one part).
    #[serde(default = "default_max_uploads")]
    pub max_concurrent_uploads: usize,
    /// Presigned download URLs (`BLOB-16`). Off by default.
    #[serde(default)]
    pub presign: PresignConfig,
}

/// The `presign` block of an S3 definition: `{"enabled": false, "ttl_secs": 300}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresignConfig {
    /// Whether `presign_get` may return a URL. When false it answers `Ok(None)` and the
    /// node serves the bytes itself.
    #[serde(default)]
    pub enabled: bool,
    /// Longest lifetime of a URL in seconds (1 to 900). A caller may ask for less, never
    /// for more.
    #[serde(default = "default_presign_ttl_secs")]
    pub ttl_secs: u32,
}

const fn default_presign_ttl_secs() -> u32 {
    300
}

impl Default for PresignConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: default_presign_ttl_secs(),
        }
    }
}

fn default_region() -> String {
    "us-east-1".to_owned()
}

const fn default_part_mib() -> u32 {
    16
}

const fn default_max_uploads() -> usize {
    32
}

impl S3Config {
    /// Runtime tuning derived from this configuration.
    #[must_use]
    pub fn settings(&self) -> S3Settings {
        S3Settings {
            part_size: self.part_size_mib as usize * 1024 * 1024,
            max_concurrent_uploads: self.max_concurrent_uploads,
            presign: PresignSettings {
                enabled: self.presign.enabled,
                ttl: Duration::from_secs(u64::from(self.presign.ttl_secs)),
            },
            ..S3Settings::default()
        }
    }

    /// Checks everything that can be checked without the network.
    pub fn validate(&self) -> Result<(), BuildError> {
        let invalid = |what: &str| Err(BuildError::Invalid(format!("s3: {what}")));
        if self.bucket.is_empty() || self.bucket.contains(['/', ' ']) {
            return invalid("`bucket` must be a non-empty name without slashes or spaces");
        }
        if self.region.is_empty() {
            return invalid("`region` must not be empty");
        }
        if let Err(err) = validate_prefix(&self.prefix) {
            return invalid(&format!("`prefix` is not valid ({err})"));
        }
        if !(MIN_PART_MIB..=MAX_PART_MIB).contains(&self.part_size_mib) {
            return invalid("`part_size_mib` must be between 5 and 128");
        }
        if !(1..=MAX_CONCURRENT_UPLOADS).contains(&self.max_concurrent_uploads) {
            return invalid("`max_concurrent_uploads` must be between 1 and 1024");
        }
        if !(1..=MAX_PRESIGN_TTL_SECS).contains(&self.presign.ttl_secs) {
            return invalid("`presign.ttl_secs` must be between 1 and 900");
        }
        if self.access_key_id_ref.is_some() != self.secret_access_key_ref.is_some() {
            return invalid(
                "`access_key_id_ref` and `secret_access_key_ref` must be set together \
                 (set neither to use the ambient credential chain)",
            );
        }
        if self.session_token_ref.is_some() && self.access_key_id_ref.is_none() {
            return invalid("`session_token_ref` needs static credentials");
        }
        if let Some(endpoint) = &self.endpoint {
            self.validate_endpoint(endpoint)?;
        }
        Ok(())
    }

    fn validate_endpoint(&self, endpoint: &str) -> Result<(), BuildError> {
        let invalid = |what: &str| Err(BuildError::Invalid(format!("s3: {what}")));
        let Ok(url) = Url::parse(endpoint) else {
            return invalid("`endpoint` is not a valid URL");
        };
        if url.host_str().is_none() {
            return invalid("`endpoint` has no host");
        }
        if !url.username().is_empty() || url.password().is_some() {
            return invalid("`endpoint` must not embed credentials");
        }
        if url.query().is_some() || url.fragment().is_some() {
            return invalid("`endpoint` must not have a query or fragment");
        }
        match url.scheme() {
            "https" => Ok(()),
            "http" if self.allow_http => Ok(()),
            "http" => invalid("`endpoint` uses http but `allow_http` is not set"),
            _ => invalid("`endpoint` must use http or https"),
        }
    }
}

/// Runtime tuning of an S3 store. Not part of the stored JSON except for the part
/// size and the concurrency limit (see [`S3Config::settings`]).
#[derive(Debug, Clone)]
pub struct S3Settings {
    /// Multipart part size in bytes; also the most memory one upload buffers.
    /// [`S3Config::validate`] enforces the real-S3 minimum of 5 MiB; this struct
    /// accepts anything above zero for stores that allow smaller parts.
    pub part_size: usize,
    /// Number of uploads that may be open at once.
    pub max_concurrent_uploads: usize,
    /// How long `begin` waits for a free upload slot before failing as
    /// `Unavailable` (maps to 503). Short on purpose: a long wait is a queue.
    pub permit_timeout: Duration,
    /// Deadline for metadata requests (head, delete, create/abort multipart).
    pub metadata_timeout: Duration,
    /// Deadline for one part, a single-request upload, and for completing a multipart
    /// upload. Streaming bodies are covered by the client's 60 s idle timeout, but a
    /// stalled upload has no idle signal, so it needs an explicit bound.
    pub part_timeout: Duration,
    /// Presigned downloads.
    pub presign: PresignSettings,
    /// Value of the `store` label on every series of this store.
    pub store_name: String,
}

/// Presigned download settings of a store. The URL lifetime is capped at
/// [`MAX_PRESIGN_TTL_SECS`] whatever is configured here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresignSettings {
    /// Whether `presign_get` may return a URL.
    pub enabled: bool,
    /// Longest lifetime of a URL.
    pub ttl: Duration,
}

impl Default for PresignSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl: Duration::from_secs(u64::from(default_presign_ttl_secs())),
        }
    }
}

impl Default for S3Settings {
    fn default() -> Self {
        Self {
            part_size: DEFAULT_PART_BYTES,
            max_concurrent_uploads: default_max_uploads(),
            permit_timeout: Duration::from_secs(10),
            metadata_timeout: Duration::from_secs(30),
            part_timeout: Duration::from_secs(300),
            presign: PresignSettings::default(),
            store_name: "s3".to_owned(),
        }
    }
}

impl S3Settings {
    pub(crate) fn validate(&self) -> Result<(), BuildError> {
        if self.part_size == 0 || self.max_concurrent_uploads == 0 {
            return Err(BuildError::Invalid(
                "s3: part size and upload limit must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

impl From<KeyError> for BuildError {
    fn from(err: KeyError) -> Self {
        Self::Invalid(format!("s3: `prefix` is not valid ({err})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid() -> S3Config {
        parse_config("s3", &json!({"bucket": "artifacts"})).unwrap()
    }

    fn rejected(config: &S3Config) -> String {
        config.validate().unwrap_err().to_string()
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let config = valid();
        assert_eq!(config.region, "us-east-1");
        assert_eq!(config.part_size_mib, 16);
        assert_eq!(config.max_concurrent_uploads, 32);
        assert!(!config.allow_http);
        assert!(config.endpoint.is_none() && config.path_style.is_none());
        config.validate().unwrap();
        assert_eq!(config.settings().part_size, DEFAULT_PART_BYTES);
    }

    #[test]
    fn must_not_accept_secret_values_in_the_json() {
        let err = parse_config::<S3Config>(
            "s3",
            &json!({"bucket": "b", "secret_access_key": "AKIA-super-secret"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field"));
        assert!(!err.contains("AKIA-super-secret"));
    }

    #[test]
    fn must_not_echo_values_of_wrongly_typed_fields() {
        let err = parse_config::<S3Config>(
            "s3",
            &json!({"bucket": "b", "max_concurrent_uploads": "AKIA-super-secret"}),
        )
        .unwrap_err()
        .to_string();
        assert!(!err.contains("AKIA-super-secret"), "{err}");
    }

    #[test]
    fn missing_required_field_is_named() {
        let err = parse_config::<S3Config>("s3", &json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing field `bucket`"));
    }

    #[test]
    fn bucket_region_prefix_and_limits_are_validated() {
        let mut config = valid();
        config.bucket = String::new();
        assert!(rejected(&config).contains("bucket"));
        config.bucket = "a/b".into();
        assert!(rejected(&config).contains("bucket"));

        let mut config = valid();
        config.region = String::new();
        assert!(rejected(&config).contains("region"));

        let mut config = valid();
        config.prefix = "../x".into();
        assert!(rejected(&config).contains("prefix"));

        let mut config = valid();
        config.part_size_mib = 4;
        assert!(rejected(&config).contains("part_size_mib"));
        config.part_size_mib = 129;
        assert!(rejected(&config).contains("part_size_mib"));

        let mut config = valid();
        config.max_concurrent_uploads = 0;
        assert!(rejected(&config).contains("max_concurrent_uploads"));
    }

    #[test]
    fn credentials_refs_come_as_a_pair() {
        let mut config = valid();
        config.access_key_id_ref = Some("A".into());
        assert!(rejected(&config).contains("together"));
        config.secret_access_key_ref = Some("B".into());
        config.validate().unwrap();

        let mut config = valid();
        config.session_token_ref = Some("T".into());
        assert!(rejected(&config).contains("session_token_ref"));
    }

    #[test]
    fn must_not_allow_plain_http_unless_configured() {
        let mut config = valid();
        config.endpoint = Some("http://minio:9000".into());
        assert!(rejected(&config).contains("allow_http"));
        config.allow_http = true;
        config.validate().unwrap();
        config.endpoint = Some("https://s3.example.com".into());
        config.validate().unwrap();
    }

    #[test]
    fn endpoint_shape_is_validated() {
        for (endpoint, needle) in [
            ("not a url", "valid URL"),
            ("ftp://host", "http or https"),
            ("https://user:pw@host", "credentials"),
            ("https://host?x=1", "query"),
            ("https://host#frag", "query or fragment"),
        ] {
            let mut config = valid();
            config.endpoint = Some(endpoint.into());
            assert!(rejected(&config).contains(needle), "{endpoint}");
        }
    }

    #[test]
    fn presign_is_off_by_default_and_bounded() {
        let config = valid();
        assert_eq!(config.presign, PresignConfig::default());
        assert!(!config.settings().presign.enabled);
        assert_eq!(config.settings().presign.ttl, Duration::from_secs(300));

        let mut config = valid();
        config.presign = PresignConfig {
            enabled: true,
            ttl_secs: 900,
        };
        config.validate().unwrap();
        assert!(config.settings().presign.enabled);
        for bad in [0, 901] {
            config.presign.ttl_secs = bad;
            assert!(rejected(&config).contains("presign.ttl_secs"), "{bad}");
        }
    }

    #[test]
    fn the_presign_block_rejects_unknown_fields() {
        let outcome = parse_config::<S3Config>(
            "s3",
            &json!({"bucket": "b", "presign": {"enabled": true, "secret": "x"}}),
        );
        assert!(outcome.unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn the_free_space_floor_defaults_to_ten_percent_and_is_bounded() {
        let config: LocalConfig = parse_config("local", &json!({"path": "/x"})).unwrap();
        assert_eq!(config.min_free_percent, 10);
        for ok in [0, 10, 90] {
            let config: LocalConfig =
                parse_config("local", &json!({"path": "/x", "min_free_percent": ok})).unwrap();
            assert!(config.validate().is_ok(), "{ok}");
        }
        let too_high: LocalConfig =
            parse_config("local", &json!({"path": "/x", "min_free_percent": 91})).unwrap();
        assert!(too_high.validate().is_err());
        assert!(
            parse_config::<LocalConfig>("local", &json!({"path": "/x", "min_free_percent": 300}))
                .is_err()
        );
    }

    #[test]
    fn local_path_must_be_absolute() {
        let relative: LocalConfig = parse_config("local", &json!({"path": "data/blobs"})).unwrap();
        assert!(relative.validate().is_err());
        let absolute: LocalConfig = parse_config("local", &json!({"path": "/var/lib/x"})).unwrap();
        assert!(absolute.validate().is_ok());
        assert!(parse_config::<LocalConfig>("local", &json!({"path": "/x", "extra": 1})).is_err());
    }

    #[test]
    fn settings_reject_zero_values() {
        assert!(
            S3Settings {
                part_size: 0,
                ..S3Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            S3Settings {
                max_concurrent_uploads: 0,
                ..S3Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(S3Settings::default().validate().is_ok());
    }
}
