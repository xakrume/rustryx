//! Manifest documents: Docker schema 2, OCI image manifest, OCI image index, Docker
//! manifest list.
//!
//! Sources: OCI Image Specification 1.1 `manifest.md`, `image-index.md`, `descriptor.md`
//! (<https://github.com/opencontainers/image-spec/tree/v1.1.0>) and the Docker image
//! manifest v2.2 document. Docker schema 1 is refused: it is deprecated and carries
//! signatures this registry would have to keep byte for byte.
//!
//! The parser is strict about what the registry acts on (the type, the digests it will
//! look up, the sizes it will compare) and lenient about what it only stores (unknown
//! top level fields, annotations, platform data), because the registry must store the
//! exact bytes a client sent and must not fail on fields it does not know. It only ever
//! reads; nothing here indexes a slice with data from the document.
//!
//! Client text: [`ManifestError`] messages contain no content of the document.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::digest::{Digest, DigestError};
use super::media_type::{self, ManifestType, essence};

/// Why a manifest was refused. The messages are safe to show to the client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// Not JSON, or not the expected shape of JSON.
    #[error("the manifest is not a valid JSON document of the expected shape")]
    NotJson,
    /// `schemaVersion` is missing.
    #[error("the manifest has no schemaVersion")]
    MissingSchemaVersion,
    /// `schemaVersion` is not 2.
    #[error("manifest schema version {0} is not supported, use schema version 2")]
    UnsupportedSchema(i64),
    /// Neither `mediaType` nor `Content-Type` names a supported manifest type.
    #[error("the manifest media type is missing or not supported")]
    UnsupportedMediaType,
    /// `Content-Type` and `mediaType` disagree.
    #[error("the Content-Type header does not match the mediaType of the manifest")]
    MediaTypeMismatch,
    /// A required field is missing.
    #[error("the manifest is missing the required field {0}")]
    MissingField(&'static str),
    /// A field is present but invalid.
    #[error("the manifest field {0} is invalid")]
    InvalidField(&'static str),
    /// A descriptor names a digest algorithm the registry does not support.
    #[error("a descriptor uses an unsupported digest algorithm")]
    UnsupportedDigest,
    /// More descriptors than the registry accepts.
    #[error("the manifest has more than {0} descriptors")]
    TooManyDescriptors(usize),
    /// An OCI artifact with an empty config has no `artifactType`.
    #[error("artifactType is required when the config is the empty descriptor")]
    MissingArtifactType,
}

/// A content descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// Media type of the referenced content.
    pub media_type: String,
    /// Digest of the referenced content.
    pub digest: Digest,
    /// Size of the referenced content in bytes.
    pub size: u64,
    /// Alternative fetch locations: a layer with URLs is a foreign layer the registry does
    /// not have to hold.
    pub urls: Vec<String>,
    /// `artifactType`, when the descriptor carries one.
    pub artifact_type: Option<String>,
    /// Annotations (empty when absent).
    pub annotations: BTreeMap<String, String>,
}

impl Descriptor {
    /// The descriptor as the JSON object of a referrers response.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut object = json!({
            "mediaType": self.media_type,
            "digest": self.digest.to_string(),
            "size": self.size,
        });
        if let Some(artifact_type) = &self.artifact_type {
            object["artifactType"] = Value::String(artifact_type.clone());
        }
        if !self.annotations.is_empty() {
            object["annotations"] = json!(self.annotations);
        }
        object
    }
}

/// A validated manifest document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedManifest {
    /// Which of the four manifest types this is.
    pub manifest_type: ManifestType,
    /// `artifactType`, when present.
    pub artifact_type: Option<String>,
    /// Annotations of the manifest.
    pub annotations: BTreeMap<String, String>,
    /// The `subject` descriptor.
    pub subject: Option<Descriptor>,
    /// The config descriptor (image manifests).
    pub config: Option<Descriptor>,
    /// Layers (image manifests).
    pub layers: Vec<Descriptor>,
    /// Child manifests (indexes).
    pub manifests: Vec<Descriptor>,
}

impl ParsedManifest {
    /// Parses and validates a manifest.
    ///
    /// `content_type` is the `Content-Type` of the `PUT`. It must agree with the
    /// `mediaType` of the document when both are present, and it names the type when the
    /// document has no `mediaType`.
    ///
    /// # Errors
    /// A [`ManifestError`] naming the first problem found.
    pub fn parse(
        body: &[u8],
        content_type: Option<&str>,
        max_descriptors: usize,
    ) -> Result<Self, ManifestError> {
        let raw: RawManifest = serde_json::from_slice(body).map_err(|_| ManifestError::NotJson)?;
        match raw.schema_version {
            None => return Err(ManifestError::MissingSchemaVersion),
            Some(2) => {}
            Some(other) => return Err(ManifestError::UnsupportedSchema(other)),
        }
        let manifest_type = resolve_type(raw.media_type.as_deref(), content_type)?;
        let counted =
            raw.layers.as_ref().map_or(0, Vec::len) + raw.manifests.as_ref().map_or(0, Vec::len);
        if counted > max_descriptors {
            return Err(ManifestError::TooManyDescriptors(max_descriptors));
        }
        let artifact_type = match raw.artifact_type.as_deref() {
            None | Some("") => None,
            Some(value) if media_type::is_valid_media_type(value) => Some(value.to_owned()),
            Some(_) => return Err(ManifestError::InvalidField("artifactType")),
        };
        let subject = raw
            .subject
            .map(|descriptor| descriptor.validate("subject"))
            .transpose()?;
        let mut parsed = Self {
            manifest_type,
            artifact_type,
            annotations: raw.annotations.unwrap_or_default(),
            subject,
            config: None,
            layers: Vec::new(),
            manifests: Vec::new(),
        };
        if manifest_type.is_index() {
            let manifests = raw
                .manifests
                .ok_or(ManifestError::MissingField("manifests"))?;
            parsed.manifests = validate_all(manifests, "manifests")?;
        } else {
            let config = raw.config.ok_or(ManifestError::MissingField("config"))?;
            let layers = raw.layers.ok_or(ManifestError::MissingField("layers"))?;
            parsed.config = Some(config.validate("config")?);
            parsed.layers = validate_all(layers, "layers")?;
            let empty_config = parsed
                .config
                .as_ref()
                .is_some_and(|c| c.media_type.eq_ignore_ascii_case(media_type::OCI_EMPTY));
            if empty_config && parsed.artifact_type.is_none() {
                return Err(ManifestError::MissingArtifactType);
            }
        }
        Ok(parsed)
    }

    /// The media type to serve this manifest with.
    #[must_use]
    pub fn media_type(&self) -> &'static str {
        self.manifest_type.media_type()
    }

    /// The blobs a client must have uploaded before the manifest: the config and every
    /// layer that has no `urls` (a foreign layer is fetched from its URLs, not from here).
    pub fn required_blobs(&self) -> impl Iterator<Item = &Descriptor> {
        self.config
            .iter()
            .chain(self.layers.iter().filter(|layer| layer.urls.is_empty()))
    }

    /// The child manifests of an index that must exist as manifests of the repository.
    /// Entries of a type the registry does not know are not required to exist: the
    /// specification forbids failing on them.
    pub fn required_children(&self) -> impl Iterator<Item = &Descriptor> {
        self.manifests
            .iter()
            .filter(|child| media_type::is_manifest_type(&child.media_type))
    }

    /// What the referrers API reports as the `artifactType` of this manifest: the
    /// `artifactType` field, else the media type of the config of an image manifest.
    #[must_use]
    pub fn referrer_artifact_type(&self) -> Option<String> {
        self.artifact_type
            .clone()
            .or_else(|| self.config.as_ref().map(|c| c.media_type.clone()))
    }

    /// The descriptor under which this manifest appears in a referrers response.
    #[must_use]
    pub fn as_referrer(&self, digest: &Digest, size: u64) -> Descriptor {
        Descriptor {
            media_type: self.media_type().to_owned(),
            digest: digest.clone(),
            size,
            urls: Vec::new(),
            artifact_type: self.referrer_artifact_type(),
            annotations: self.annotations.clone(),
        }
    }
}

/// Decides the manifest type from the document and the `Content-Type` header.
fn resolve_type(
    body_type: Option<&str>,
    header_type: Option<&str>,
) -> Result<ManifestType, ManifestError> {
    let from_body = match body_type {
        None => None,
        Some(text) => Some(ManifestType::of(text).ok_or(ManifestError::UnsupportedMediaType)?),
    };
    let header_essence = header_type.and_then(essence);
    match (from_body, header_essence) {
        (Some(body), Some(header)) if header != body.media_type() => {
            Err(ManifestError::MediaTypeMismatch)
        }
        (Some(body), _) => Ok(body),
        (None, Some(header)) => {
            ManifestType::of(&header).ok_or(ManifestError::UnsupportedMediaType)
        }
        (None, None) => Err(ManifestError::UnsupportedMediaType),
    }
}

fn validate_all(
    raw: Vec<RawDescriptor>,
    field: &'static str,
) -> Result<Vec<Descriptor>, ManifestError> {
    raw.into_iter()
        .map(|descriptor| descriptor.validate(field))
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawManifest {
    schema_version: Option<i64>,
    media_type: Option<String>,
    artifact_type: Option<String>,
    config: Option<RawDescriptor>,
    layers: Option<Vec<RawDescriptor>>,
    manifests: Option<Vec<RawDescriptor>>,
    subject: Option<RawDescriptor>,
    annotations: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDescriptor {
    media_type: Option<String>,
    digest: Option<String>,
    size: Option<u64>,
    urls: Option<Vec<String>>,
    artifact_type: Option<String>,
    annotations: Option<BTreeMap<String, String>>,
}

/// Longest URL of a foreign layer and most URLs per descriptor.
const MAX_URL_LEN: usize = 2048;
const MAX_URLS: usize = 16;

impl RawDescriptor {
    fn validate(self, field: &'static str) -> Result<Descriptor, ManifestError> {
        let media_type = self.media_type.ok_or(ManifestError::InvalidField(field))?;
        if !media_type::is_valid_media_type(&media_type) {
            return Err(ManifestError::InvalidField(field));
        }
        let digest = self.digest.ok_or(ManifestError::InvalidField(field))?;
        let digest = Digest::parse(&digest).map_err(|error| match error {
            DigestError::UnsupportedAlgorithm => ManifestError::UnsupportedDigest,
            DigestError::Malformed => ManifestError::InvalidField(field),
        })?;
        let size = self.size.ok_or(ManifestError::InvalidField(field))?;
        let urls = self.urls.unwrap_or_default();
        if urls.len() > MAX_URLS || urls.iter().any(|url| url.len() > MAX_URL_LEN) {
            return Err(ManifestError::InvalidField(field));
        }
        let artifact_type = match self.artifact_type.as_deref() {
            None | Some("") => None,
            Some(value) if media_type::is_valid_media_type(value) => Some(value.to_owned()),
            Some(_) => return Err(ManifestError::InvalidField(field)),
        };
        Ok(Descriptor {
            media_type,
            digest,
            size,
            urls,
            artifact_type,
            annotations: self.annotations.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests;
