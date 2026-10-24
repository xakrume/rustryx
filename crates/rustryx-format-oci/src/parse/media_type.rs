//! Media types: the manifest types this registry stores, syntax checks and `Accept` parsing.
//!
//! Sources: OCI Image Specification 1.1 (<https://github.com/opencontainers/image-spec>,
//! `media-types.md`, `manifest.md`, `image-index.md`) and the Docker image manifest v2.2
//! document (<https://github.com/distribution/distribution/blob/main/docs/content/spec/manifest-v2-2.md>).

/// OCI image manifest.
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
/// OCI image index.
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
/// Docker image manifest, schema 2.
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
/// Docker manifest list (the "fat manifest").
pub const DOCKER_MANIFEST_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
/// OCI image config.
pub const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
/// The OCI empty descriptor media type.
pub const OCI_EMPTY: &str = "application/vnd.oci.empty.v1+json";
/// Content type of a plain blob.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// Every manifest media type the registry accepts, in the order it offers them upstream.
pub const MANIFEST_TYPES: [&str; 4] = [
    OCI_INDEX,
    OCI_MANIFEST,
    DOCKER_MANIFEST_LIST,
    DOCKER_MANIFEST,
];

/// Longest media type or `Accept` header value taken seriously.
const MAX_MEDIA_TYPE_LEN: usize = 255;
const MAX_ACCEPT_LEN: usize = 8 * 1024;
const MAX_ACCEPT_ENTRIES: usize = 64;

/// The two shapes of manifest document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManifestType {
    /// An image manifest: config and layers (OCI or Docker schema 2).
    Image(Flavor),
    /// An index: child manifests (OCI index or Docker manifest list).
    Index(Flavor),
}

/// Which family a manifest type belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flavor {
    /// `application/vnd.oci.*`
    Oci,
    /// `application/vnd.docker.*`
    Docker,
}

impl ManifestType {
    /// Classifies a media type (parameters and case are ignored).
    #[must_use]
    pub fn of(media_type: &str) -> Option<Self> {
        match essence(media_type)?.as_str() {
            OCI_MANIFEST => Some(Self::Image(Flavor::Oci)),
            OCI_INDEX => Some(Self::Index(Flavor::Oci)),
            DOCKER_MANIFEST => Some(Self::Image(Flavor::Docker)),
            DOCKER_MANIFEST_LIST => Some(Self::Index(Flavor::Docker)),
            _ => None,
        }
    }

    /// The canonical media type string.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Image(Flavor::Oci) => OCI_MANIFEST,
            Self::Index(Flavor::Oci) => OCI_INDEX,
            Self::Image(Flavor::Docker) => DOCKER_MANIFEST,
            Self::Index(Flavor::Docker) => DOCKER_MANIFEST_LIST,
        }
    }

    /// True for an index or a manifest list.
    #[must_use]
    pub const fn is_index(self) -> bool {
        matches!(self, Self::Index(_))
    }
}

/// True when `media_type` is one of the manifest types.
#[must_use]
pub fn is_manifest_type(media_type: &str) -> bool {
    ManifestType::of(media_type).is_some()
}

/// The lowercase `type/subtype` of a media type without its parameters, when the syntax
/// is valid (RFC 6838 restricted names, at most 127 characters each).
#[must_use]
pub fn essence(media_type: &str) -> Option<String> {
    if media_type.len() > MAX_MEDIA_TYPE_LEN {
        return None;
    }
    let essence = media_type.split(';').next()?.trim();
    let (kind, subtype) = essence.split_once('/')?;
    (restricted_name(kind) && restricted_name(subtype)).then(|| essence.to_ascii_lowercase())
}

/// True when `media_type` is a syntactically valid media type (no parameters needed).
#[must_use]
pub fn is_valid_media_type(media_type: &str) -> bool {
    essence(media_type).is_some()
}

/// `restricted-name = restricted-name-first *126restricted-name-chars` (RFC 6838, 4.2).
fn restricted_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if bytes.len() > 127 || !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes.iter().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#' | b'$' | b'&' | b'-' | b'^' | b'_' | b'.' | b'+'
            )
    })
}

/// One entry of an `Accept` header.
#[derive(Debug, Clone, PartialEq)]
struct AcceptEntry {
    kind: String,
    subtype: String,
    quality_zero: bool,
}

/// A parsed `Accept` header.
///
/// Only what a registry needs is kept: which media types are acceptable at all. Quality
/// values other than zero are not ranked (the registry has one representation of a
/// manifest, so there is nothing to choose between); `q=0` excludes a type, as RFC 9110
/// says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Accept {
    entries: Vec<AcceptEntry>,
}

impl Accept {
    /// Parses the values of all `Accept` headers of a request. Malformed entries are
    /// skipped, an oversized header is ignored as a whole: a client that sends garbage
    /// gets no filtering rather than a failure.
    #[must_use]
    pub fn parse<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let mut entries = Vec::new();
        for value in values {
            if value.len() > MAX_ACCEPT_LEN {
                continue;
            }
            for part in value.split(',') {
                if entries.len() >= MAX_ACCEPT_ENTRIES {
                    break;
                }
                if let Some(entry) = parse_entry(part) {
                    entries.push(entry);
                }
            }
        }
        Self { entries }
    }

    /// True when the client sent no usable `Accept`: every type is acceptable.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        self.entries.is_empty()
    }

    /// True when `media_type` is acceptable to the client.
    #[must_use]
    pub fn accepts(&self, media_type: &str) -> bool {
        if self.entries.is_empty() {
            return true;
        }
        let Some(target) = essence(media_type) else {
            return false;
        };
        let Some((kind, subtype)) = target.split_once('/') else {
            return false;
        };
        let mut allowed = false;
        for entry in &self.entries {
            let matches = (entry.kind == "*" || entry.kind == kind)
                && (entry.subtype == "*" || entry.subtype == subtype);
            if !matches {
                continue;
            }
            // An exact entry with q=0 beats a wildcard that allows it.
            let exact = entry.kind == kind && entry.subtype == subtype;
            if entry.quality_zero {
                if exact {
                    return false;
                }
            } else {
                allowed = true;
            }
        }
        allowed
    }

    /// The manifest types the client accepts, in the canonical order of
    /// [`MANIFEST_TYPES`]. Used to build the `Accept` header sent upstream and to key the
    /// cached copy: the same set always yields the same list, so equal clients share one
    /// cached manifest.
    #[must_use]
    pub fn manifest_types(&self) -> Vec<&'static str> {
        MANIFEST_TYPES
            .iter()
            .copied()
            .filter(|media_type| self.accepts(media_type))
            .collect()
    }
}

fn parse_entry(part: &str) -> Option<AcceptEntry> {
    let mut pieces = part.split(';');
    let range = pieces.next()?.trim().to_ascii_lowercase();
    let (kind, subtype) = range.split_once('/')?;
    let token_or_star = |s: &str| s == "*" || restricted_name(s);
    if !token_or_star(kind) || !token_or_star(subtype) || (kind == "*" && subtype != "*") {
        return None;
    }
    let quality_zero = pieces.any(|parameter| {
        let parameter = parameter.trim();
        parameter
            .strip_prefix("q=")
            .or_else(|| parameter.strip_prefix("Q="))
            .is_some_and(|q| q.trim().parse::<f32>().is_ok_and(|q| q <= 0.0))
    });
    Some(AcceptEntry {
        kind: kind.to_owned(),
        subtype: subtype.to_owned(),
        quality_zero,
    })
}

#[cfg(test)]
mod tests;
