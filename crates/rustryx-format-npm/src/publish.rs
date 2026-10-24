//! The content of a publish document: what a client says it is publishing.
//!
//! [`PublishRequest::parse`] reads the metadata part the [`crate::scan::Scanner`] collected up to
//! the start of the tarball and checks it against the URL: the package named in the document is
//! the package of the URL, exactly one version is being published, the dist-tags point at that
//! version, the attachment is that version's tarball, and the digests the client declares are
//! well formed. Nothing here touches storage: the decisions that need it (does the version exist,
//! may this principal write) are the handler's.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rustryx_format_core::{ExpectedChecksum, FormatError, HashAlgo};
use semver::Version;
use serde_json::{Map, Value};

use crate::name::PackageName;
use crate::version::{parse_version, validate_tag};

/// Longest `dist.integrity` string read. A real one is 150 bytes; an SRI string with many
/// entries is still tiny, and the bound keeps a hostile one from costing anything.
const MAX_INTEGRITY_LEN: usize = 1024;

/// Digests a client declared for its tarball.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DigestClaims {
    /// Lowercase hex SHA-1 (`dist.shasum`, or a `sha1-` entry of `dist.integrity`).
    pub sha1: Option<String>,
    /// SHA-256 from a `sha256-` entry of `dist.integrity`.
    pub sha256: Option<[u8; 32]>,
    /// SHA-512 from a `sha512-` entry of `dist.integrity`.
    pub sha512: Option<[u8; 64]>,
}

impl DigestClaims {
    /// The claim the blob pipeline can verify before it publishes: SHA-1, else SHA-256 (as hex).
    /// SHA-512 is checked by the attachment stream itself, because the blob commit only knows
    /// SHA-1, SHA-256 and MD5.
    #[must_use]
    pub fn pipeline_expectation(&self) -> Option<ExpectedChecksum> {
        if let Some(sha1) = &self.sha1 {
            ExpectedChecksum::new(HashAlgo::Sha1, sha1).ok()
        } else {
            self.sha256.and_then(|sha256| {
                ExpectedChecksum::new(HashAlgo::Sha256, &hex::encode(sha256)).ok()
            })
        }
    }
}

/// A validated publish document head.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishRequest {
    /// The package.
    pub package: PackageName,
    /// The version being published.
    pub version: Version,
    /// The manifest as the client sent it (normalized later, once the digests are known).
    pub manifest: Map<String, Value>,
    /// Dist-tags the client wants on this version; empty means "latest if there is none yet".
    pub tags: Vec<String>,
    /// The key of the attachment, a file name.
    pub attachment: String,
    /// Declared digests of the tarball.
    pub claims: DigestClaims,
}

fn bad(message: &'static str) -> FormatError {
    FormatError::bad_request(message)
}

impl PublishRequest {
    /// Reads the head of a publish document.
    ///
    /// `document` is the metadata scanned so far with its containers closed; `package` is the
    /// package of the URL; `attachment` is the key of the attachment entry.
    ///
    /// # Errors
    /// [`FormatError::BadRequest`] with a client-safe message for anything that does not hold.
    pub fn parse(
        document: &[u8],
        package: &PackageName,
        attachment: &str,
    ) -> Result<Self, FormatError> {
        let value: Value = serde_json::from_slice(document)
            .map_err(|_| bad("the publish document is not valid JSON"))?;
        let Value::Object(mut root) = value else {
            return Err(bad("the publish document must be an object"));
        };
        check_identity(&root, package)?;
        let Some(Value::Object(versions)) = root.remove("versions") else {
            return Err(bad("the publish document has no versions"));
        };
        if versions.len() != 1 {
            return Err(bad("exactly one version can be published at a time"));
        }
        let (key, manifest) = versions
            .into_iter()
            .next()
            .ok_or_else(|| bad("the publish document has no versions"))?;
        let version =
            parse_version(&key).map_err(|_| bad("the version is not a valid semantic version"))?;
        let Value::Object(manifest) = manifest else {
            return Err(bad("the version entry must be an object"));
        };
        check_manifest(&manifest, package, &key)?;
        let tags = read_tags(root.remove("dist-tags"), &key)?;
        check_attachment_name(attachment, package, &version)?;
        let claims = read_claims(&manifest)?;
        Ok(Self {
            package: package.clone(),
            version,
            manifest,
            tags,
            attachment: attachment.to_owned(),
            claims,
        })
    }
}

fn check_identity(root: &Map<String, Value>, package: &PackageName) -> Result<(), FormatError> {
    let full = package.full();
    if root.get("name").and_then(Value::as_str) != Some(full.as_str()) {
        return Err(bad(
            "the name in the document does not match the package of the URL",
        ));
    }
    if let Some(id) = root.get("_id")
        && id.as_str() != Some(full.as_str())
    {
        return Err(bad(
            "the _id in the document does not match the package of the URL",
        ));
    }
    Ok(())
}

fn check_manifest(
    manifest: &Map<String, Value>,
    package: &PackageName,
    key: &str,
) -> Result<(), FormatError> {
    if let Some(name) = manifest.get("name")
        && name.as_str() != Some(package.full().as_str())
    {
        return Err(bad("the name of the version does not match the package"));
    }
    if let Some(version) = manifest.get("version")
        && version.as_str() != Some(key)
    {
        return Err(bad("the version field does not match the version key"));
    }
    Ok(())
}

fn read_tags(value: Option<Value>, key: &str) -> Result<Vec<String>, FormatError> {
    let object = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(object)) => object,
        Some(_) => return Err(bad("dist-tags must be an object")),
    };
    let mut tags = Vec::with_capacity(object.len());
    for (tag, target) in object {
        validate_tag(&tag).map_err(|_| bad("invalid dist-tag name"))?;
        if target.as_str() != Some(key) {
            return Err(bad(
                "a publish can only point its dist-tags at the version it publishes",
            ));
        }
        tags.push(tag);
    }
    Ok(tags)
}

/// The attachment of a version is `<name>-<version>.tgz`, with the scope in the name (what
/// `npm publish` sends) or without it (what other publishers send).
fn check_attachment_name(
    attachment: &str,
    package: &PackageName,
    version: &Version,
) -> Result<(), FormatError> {
    let scoped = format!("{}-{version}.tgz", package.full());
    if attachment == scoped || attachment == package.tarball_filename(version) {
        Ok(())
    } else {
        Err(bad("the attachment is not the tarball of this version"))
    }
}

fn read_claims(manifest: &Map<String, Value>) -> Result<DigestClaims, FormatError> {
    let Some(dist) = manifest.get("dist") else {
        return Ok(DigestClaims::default());
    };
    let Some(dist) = dist.as_object() else {
        return Err(bad("dist must be an object"));
    };
    let mut claims = DigestClaims::default();
    if let Some(shasum) = dist.get("shasum") {
        let text = shasum
            .as_str()
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| bad("dist.shasum is not a SHA-1 digest"))?;
        claims.sha1 = Some(text.to_ascii_lowercase());
    }
    if let Some(integrity) = dist.get("integrity") {
        let text = integrity
            .as_str()
            .ok_or_else(|| bad("dist.integrity must be a string"))?;
        let parsed = parse_integrity(text).ok_or_else(|| bad("dist.integrity is malformed"))?;
        if let Some(from_integrity) = parsed.sha1 {
            let hex = hex::encode(from_integrity);
            if claims
                .sha1
                .as_ref()
                .is_some_and(|declared| *declared != hex)
            {
                return Err(bad("dist.shasum and dist.integrity disagree"));
            }
            claims.sha1 = Some(hex);
        }
        claims.sha256 = parsed.sha256;
        claims.sha512 = parsed.sha512;
    }
    Ok(claims)
}

/// The digests of a Subresource Integrity string (`sha512-<base64> sha1-<base64>`).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Integrity {
    /// SHA-1 entry.
    pub sha1: Option<[u8; 20]>,
    /// SHA-256 entry.
    pub sha256: Option<[u8; 32]>,
    /// SHA-512 entry.
    pub sha512: Option<[u8; 64]>,
}

/// Parses `dist.integrity`. Algorithms other than SHA-1, SHA-256 and SHA-512 are ignored (they
/// cannot be verified here, and refusing them would refuse a client that sends more than we use);
/// a known algorithm with a digest of the wrong size is an error.
#[must_use]
pub fn parse_integrity(text: &str) -> Option<Integrity> {
    if text.len() > MAX_INTEGRITY_LEN {
        return None;
    }
    let mut out = Integrity::default();
    let mut any = false;
    for token in text.split_ascii_whitespace() {
        let (algorithm, rest) = token.split_once('-')?;
        let encoded = rest.split('?').next()?;
        let bytes = STANDARD.decode(encoded).ok()?;
        match algorithm {
            "sha1" => out.sha1 = Some(bytes.try_into().ok()?),
            "sha256" => out.sha256 = Some(bytes.try_into().ok()?),
            "sha512" => out.sha512 = Some(bytes.try_into().ok()?),
            _ => {}
        }
        any = true;
    }
    any.then_some(out)
}

/// Builds the manifest that is stored for a published version: the client's manifest with the
/// fields the registry owns replaced by what it computed.
///
/// `dist.shasum` and `dist.integrity` are the digests of the bytes that were stored, never the
/// declared ones (they were verified to be equal, or the publish was refused); `dist.tarball` is
/// a placeholder that every read overwrites with the address of the repository the client used.
#[must_use]
pub fn normalized_manifest(
    request: &PublishRequest,
    sha1_hex: &str,
    sha512: &[u8; 64],
    publisher: &str,
) -> Map<String, Value> {
    let mut manifest = request.manifest.clone();
    let full = request.package.full();
    manifest.insert("name".to_owned(), Value::String(full.clone()));
    manifest.insert(
        "version".to_owned(),
        Value::String(request.version.to_string()),
    );
    manifest.insert(
        "_id".to_owned(),
        Value::String(format!("{full}@{}", request.version)),
    );
    for registry_owned in ["_rev", "_revisions", "_attachments"] {
        manifest.remove(registry_owned);
    }
    let mut dist = Map::new();
    dist.insert("shasum".to_owned(), Value::String(sha1_hex.to_owned()));
    dist.insert(
        "integrity".to_owned(),
        Value::String(format!("sha512-{}", STANDARD.encode(sha512))),
    );
    dist.insert(
        "tarball".to_owned(),
        Value::String(request.package.tarball_path(&request.version)),
    );
    manifest.insert("dist".to_owned(), Value::Object(dist));
    // Who published is the authenticated principal, never what the client claims.
    let mut user = Map::new();
    user.insert("name".to_owned(), Value::String(publisher.to_owned()));
    manifest.insert("_npmUser".to_owned(), Value::Object(user));
    manifest
}

/// The deprecation messages of a packument-shaped body (`npm deprecate` sends the whole
/// document back with `deprecated` set on versions): version to message, where an absent or
/// empty message means "not deprecated".
///
/// # Errors
/// [`FormatError::BadRequest`] when the body is not a packument of this package.
pub fn deprecation_patch(
    document: &[u8],
    package: &PackageName,
) -> Result<Vec<(Version, Option<String>)>, FormatError> {
    let value: Value =
        serde_json::from_slice(document).map_err(|_| bad("the document is not valid JSON"))?;
    let Value::Object(root) = value else {
        return Err(bad("the document must be an object"));
    };
    check_identity(&root, package)?;
    let versions = match root.get("versions") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(versions)) => versions,
        Some(_) => return Err(bad("versions must be an object")),
    };
    let mut patch = Vec::with_capacity(versions.len());
    for (key, manifest) in versions {
        let version = parse_version(key).map_err(|_| bad("the document has an invalid version"))?;
        let message = manifest
            .get("deprecated")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        patch.push((version, message));
    }
    Ok(patch)
}

/// The versions and the preferred `latest` of a packument-shaped body, for the `-rev` form of a
/// `PUT`, which npm uses to unpublish one version: the versions that are no longer in the body
/// are the ones being removed.
///
/// # Errors
/// [`FormatError::BadRequest`] when the body is not a packument of this package.
pub fn retained_versions(
    document: &[u8],
    package: &PackageName,
) -> Result<(Vec<Version>, Option<Version>), FormatError> {
    let value: Value =
        serde_json::from_slice(document).map_err(|_| bad("the document is not valid JSON"))?;
    let Value::Object(root) = value else {
        return Err(bad("the document must be an object"));
    };
    check_identity(&root, package)?;
    let mut retained = Vec::new();
    match root.get("versions") {
        None | Some(Value::Null) => {}
        Some(Value::Object(versions)) => {
            for key in versions.keys() {
                retained.push(
                    parse_version(key).map_err(|_| bad("the document has an invalid version"))?,
                );
            }
        }
        Some(_) => return Err(bad("versions must be an object")),
    }
    let latest = root
        .get("dist-tags")
        .and_then(|tags| tags.get("latest"))
        .and_then(Value::as_str)
        .and_then(|text| parse_version(text).ok());
    Ok((retained, latest))
}

#[cfg(test)]
mod tests;
