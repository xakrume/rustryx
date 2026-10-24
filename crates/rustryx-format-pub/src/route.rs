//! The URL space of a pub repository, parsed once.
//!
//! `classify` turns the raw request path into a [`NormalizedPath`] exactly once (format-core
//! decodes it and refuses traversal in any encoding) and then into a [`Route`] here. `handle`
//! and the group claimer re-read the same normalised path with the same function, so what is
//! authorised, what is claimed and what is served is one value (SEC-19).
//!
//! ```text
//! GET  api/packages/<package>                                list of versions
//! GET  api/packages/<package>/versions/<version>             one version (deprecated, Dart < 2.8)
//! GET  api/packages/<package>/advisories                     not supported in phase 1
//! GET  packages/<package>/versions/<version>.tar.gz          the archive (also the archive_url)
//! GET  api/packages/versions/new                             publish step 1
//! POST api/packages/versions/upload/<id>[/<name>/<version>]  publish step 2 (multipart)
//! GET  api/packages/versions/finish/<id>[/<name>/<version>]  publish step 3 (finalize)
//! ```
//!
//! The literal `versions` after `api/packages/` is not ambiguous with a package of that name:
//! a package route has three segments (`api/packages/versions`), the publish routes have a
//! fourth segment (`new`, `upload`, `finish`) that is never `versions`, and the single version
//! route puts `versions` in the fourth place after a package name. A version text is never
//! `new`, so `api/packages/<package>/versions/new` cannot be a version either.

use std::fmt;

use rustryx_format_core::{FormatError, NormalizedPath};
use uuid::Uuid;

use crate::name::{NameError, PackageName};
use crate::version::{PubVersion, VersionError};

/// Suffix of an archive file name.
const ARCHIVE_SUFFIX: &str = ".tar.gz";

/// Identifier of one publication in progress. It is a canonical lowercase hyphenated UUID, so
/// the same upload has one spelling in the URL and in the staging path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UploadId(Uuid);

impl UploadId {
    /// Wraps a freshly generated UUID.
    #[must_use]
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Parses the canonical text form and nothing else (no braces, no `urn:`, no upper case,
    /// no missing hyphens).
    ///
    /// # Errors
    /// [`RouteError::UploadId`] for any other spelling.
    pub fn parse(raw: &str) -> Result<Self, RouteError> {
        let id = Uuid::parse_str(raw).map_err(|_| RouteError::UploadId)?;
        if id.hyphenated().to_string() == raw {
            Ok(Self(id))
        } else {
            Err(RouteError::UploadId)
        }
    }
}

impl fmt::Display for UploadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

/// The coordinate a publisher declared when it asked for an upload URL
/// (`GET api/packages/versions/new?package=<name>&version=<version>`). The standard Dart client
/// does not send it, and then the coordinate comes from the `pubspec.yaml` of the archive. A
/// client that declares it gets the authorization of its publication decided before a byte of
/// the archive is read, and a refusal when the archive says something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// Package name.
    pub name: PackageName,
    /// Version.
    pub version: PubVersion,
}

/// What a request path addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// List of all versions of a package.
    List(PackageName),
    /// One version document (deprecated endpoint, kept for old clients).
    Version(PackageName, PubVersion),
    /// The archive of one version.
    Archive(PackageName, PubVersion),
    /// The advisories endpoint: recognised, answered 404 in phase 1.
    Advisories(PackageName),
    /// Publish step 1: ask for an upload URL.
    NewUpload,
    /// Publish step 2: the multipart upload.
    Upload {
        /// The publication.
        id: UploadId,
        /// The declared coordinate, if any.
        declared: Option<Declared>,
    },
    /// Publish step 3: finalize.
    Finish {
        /// The publication.
        id: UploadId,
        /// The declared coordinate, if any.
        declared: Option<Declared>,
    },
}

/// Why a path is not a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// The path is not part of the protocol.
    #[error("not found")]
    NotFound,
    /// A package name segment is not a valid package name.
    #[error(transparent)]
    Name(#[from] NameError),
    /// A version segment is not a valid version.
    #[error(transparent)]
    Version(#[from] VersionError),
    /// An upload id segment is not a canonical UUID.
    #[error("invalid upload id")]
    UploadId,
}

impl From<RouteError> for FormatError {
    fn from(error: RouteError) -> Self {
        match error {
            RouteError::NotFound => Self::NotFound,
            // The texts of the name and version errors are fixed strings that name the rule,
            // never the input, so they are safe to show.
            other => Self::bad_request(other.to_string()),
        }
    }
}

impl Route {
    /// Reads the route of a normalised path.
    ///
    /// # Errors
    /// [`RouteError::NotFound`] for a path outside the protocol, and the specific error for a
    /// segment that is in the right place but not valid.
    pub fn parse(path: &NormalizedPath) -> Result<Self, RouteError> {
        let segments: Vec<&str> = path.segments().collect();
        match segments.as_slice() {
            ["api", "packages", "versions", "new"] => Ok(Self::NewUpload),
            ["api", "packages", package] => Ok(Self::List(PackageName::parse(package)?)),
            ["api", "packages", package, "advisories"] => {
                Ok(Self::Advisories(PackageName::parse(package)?))
            }
            ["api", "packages", package, "versions", version] => Ok(Self::Version(
                PackageName::parse(package)?,
                PubVersion::parse(version)?,
            )),
            [
                "api",
                "packages",
                "versions",
                step @ ("upload" | "finish"),
                rest @ ..,
            ] => {
                let (id, declared) = publication(rest)?;
                Ok(if *step == "upload" {
                    Self::Upload { id, declared }
                } else {
                    Self::Finish { id, declared }
                })
            }
            ["packages", package, "versions", file] => {
                let version = file
                    .strip_suffix(ARCHIVE_SUFFIX)
                    .ok_or(RouteError::NotFound)?;
                Ok(Self::Archive(
                    PackageName::parse(package)?,
                    PubVersion::parse(version)?,
                ))
            }
            _ => Err(RouteError::NotFound),
        }
    }

    /// The package this route addresses, for claims and coordinates.
    #[must_use]
    pub fn package(&self) -> Option<&PackageName> {
        match self {
            Self::List(name)
            | Self::Version(name, _)
            | Self::Archive(name, _)
            | Self::Advisories(name) => Some(name),
            Self::Upload { declared, .. } | Self::Finish { declared, .. } => {
                declared.as_ref().map(|d| &d.name)
            }
            Self::NewUpload => None,
        }
    }

    /// The version this route addresses, if it names one.
    #[must_use]
    pub fn version(&self) -> Option<&PubVersion> {
        match self {
            Self::Version(_, version) | Self::Archive(_, version) => Some(version),
            Self::Upload { declared, .. } | Self::Finish { declared, .. } => {
                declared.as_ref().map(|d| &d.version)
            }
            _ => None,
        }
    }

    /// The path of this route, the inverse of [`Route::parse`].
    #[must_use]
    pub fn to_path(&self) -> String {
        match self {
            Self::List(name) => list_path(name),
            Self::Version(name, version) => version_path(name, version),
            Self::Archive(name, version) => archive_path(name, version),
            Self::Advisories(name) => format!("api/packages/{name}/advisories"),
            Self::NewUpload => "api/packages/versions/new".to_owned(),
            Self::Upload { id, declared } => publication_path("upload", *id, declared.as_ref()),
            Self::Finish { id, declared } => publication_path("finish", *id, declared.as_ref()),
        }
    }
}

fn publication(rest: &[&str]) -> Result<(UploadId, Option<Declared>), RouteError> {
    match rest {
        [id] => Ok((UploadId::parse(id)?, None)),
        [id, name, version] => Ok((
            UploadId::parse(id)?,
            Some(Declared {
                name: PackageName::parse(name)?,
                version: PubVersion::parse(version)?,
            }),
        )),
        _ => Err(RouteError::NotFound),
    }
}

fn publication_path(step: &str, id: UploadId, declared: Option<&Declared>) -> String {
    match declared {
        Some(d) => format!("api/packages/versions/{step}/{id}/{}/{}", d.name, d.version),
        None => format!("api/packages/versions/{step}/{id}"),
    }
}

/// `api/packages/<name>`: the list document, also the proxy cache path of the upstream list.
#[must_use]
pub fn list_path(name: &PackageName) -> String {
    format!("api/packages/{name}")
}

/// `api/packages/<name>/versions/<version>`: the single version route and, in a hosted
/// repository, the path of the stored version document (the authorised path is the stored path).
#[must_use]
pub fn version_path(name: &PackageName, version: &PubVersion) -> String {
    format!("api/packages/{name}/versions/{version}")
}

/// `packages/<name>/versions/<version>.tar.gz`: the archive path and the path it is stored under.
#[must_use]
pub fn archive_path(name: &PackageName, version: &PubVersion) -> String {
    format!("packages/{name}/versions/{version}{ARCHIVE_SUFFIX}")
}

/// Prefix under which the version documents of one hosted package are stored.
#[must_use]
pub fn version_prefix(name: &PackageName) -> String {
    format!("api/packages/{name}/versions/")
}

/// Prefix under which staged uploads are stored.
pub const STAGING_PREFIX: &str = "api/packages/versions/upload/";

#[cfg(test)]
mod tests {
    use super::*;

    fn route(path: &str) -> Result<Route, RouteError> {
        Route::parse(&NormalizedPath::parse(path).unwrap())
    }

    const ID: &str = "0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10";

    #[test]
    fn reads_every_route() {
        let name = PackageName::parse("http").unwrap();
        let version = PubVersion::parse("1.2.3").unwrap();
        let id = UploadId::parse(ID).unwrap();
        assert_eq!(
            route("api/packages/http").unwrap(),
            Route::List(name.clone())
        );
        assert_eq!(
            route("api/packages/http/versions/1.2.3").unwrap(),
            Route::Version(name.clone(), version.clone())
        );
        assert_eq!(
            route("api/packages/http/advisories").unwrap(),
            Route::Advisories(name.clone())
        );
        assert_eq!(
            route("packages/http/versions/1.2.3.tar.gz").unwrap(),
            Route::Archive(name.clone(), version.clone())
        );
        assert_eq!(
            route("api/packages/versions/new").unwrap(),
            Route::NewUpload
        );
        assert_eq!(
            route(&format!("api/packages/versions/upload/{ID}")).unwrap(),
            Route::Upload { id, declared: None }
        );
        assert_eq!(
            route(&format!("api/packages/versions/finish/{ID}/http/1.2.3")).unwrap(),
            Route::Finish {
                id,
                declared: Some(Declared { name, version })
            }
        );
    }

    #[test]
    fn a_package_called_versions_is_not_confused_with_the_publish_routes() {
        let versions = PackageName::parse("versions").unwrap();
        assert_eq!(
            route("api/packages/versions").unwrap(),
            Route::List(versions.clone())
        );
        assert_eq!(
            route("api/packages/versions/versions/1.0.0").unwrap(),
            Route::Version(versions.clone(), PubVersion::parse("1.0.0").unwrap())
        );
        assert_eq!(
            route("packages/versions/versions/1.0.0.tar.gz").unwrap(),
            Route::Archive(versions, PubVersion::parse("1.0.0").unwrap())
        );
        // `new` is not a version, so it can only be the publish route.
        assert_eq!(
            route("api/packages/versions/new").unwrap(),
            Route::NewUpload
        );
        assert!(matches!(
            route("api/packages/http/versions/new"),
            Err(RouteError::Version(_))
        ));
    }

    #[test]
    fn unknown_paths_are_not_found() {
        for path in [
            "",
            "api",
            "api/packages",
            "api/packages/http/other",
            "api/packages/http/versions",
            "api/archives/http-1.0.0.tar.gz",
            "packages/http/versions/1.0.0.zip",
            "packages/http/versions/1.0.0",
            "packages/http",
            "x/api/packages/http",
            "api/packages/versions/upload",
            "api/packages/versions/finish/a/b",
            "api/packages/versions/other/x",
        ] {
            assert_eq!(route(path), Err(RouteError::NotFound), "{path:?}");
        }
    }

    #[test]
    fn invalid_segments_are_named() {
        assert!(matches!(
            route("api/packages/Http"),
            Err(RouteError::Name(_))
        ));
        assert!(matches!(
            route("packages/http/versions/1.0.tar.gz"),
            Err(RouteError::Version(_))
        ));
        assert_eq!(
            route("api/packages/versions/upload/not-a-uuid"),
            Err(RouteError::UploadId)
        );
        // Another spelling of a valid UUID is not the canonical one.
        let upper = ID.to_uppercase();
        assert_eq!(
            route(&format!("api/packages/versions/upload/{upper}")),
            Err(RouteError::UploadId)
        );
        let simple = ID.replace('-', "");
        assert_eq!(
            route(&format!("api/packages/versions/finish/{simple}")),
            Err(RouteError::UploadId)
        );
    }

    #[test]
    fn route_errors_become_client_errors() {
        assert!(matches!(
            FormatError::from(RouteError::NotFound),
            FormatError::NotFound
        ));
        for error in [
            RouteError::Name(NameError::BadChar),
            RouteError::Version(VersionError::Invalid),
            RouteError::UploadId,
        ] {
            assert!(matches!(
                FormatError::from(error),
                FormatError::BadRequest(_)
            ));
        }
    }

    #[test]
    fn paths_round_trip() {
        let name = PackageName::parse("http").unwrap();
        let version = PubVersion::parse("1.2.3-beta.1+7").unwrap();
        let id = UploadId::parse(ID).unwrap();
        let declared = Some(Declared {
            name: name.clone(),
            version: version.clone(),
        });
        let routes = [
            Route::List(name.clone()),
            Route::Version(name.clone(), version.clone()),
            Route::Archive(name.clone(), version.clone()),
            Route::Advisories(name.clone()),
            Route::NewUpload,
            Route::Upload { id, declared: None },
            Route::Upload {
                id,
                declared: declared.clone(),
            },
            Route::Finish { id, declared: None },
            Route::Finish { id, declared },
        ];
        for original in routes {
            let reparsed = route(&original.to_path()).unwrap();
            assert_eq!(reparsed, original);
        }
    }

    #[test]
    fn accessors() {
        let name = PackageName::parse("http").unwrap();
        let version = PubVersion::parse("1.0.0").unwrap();
        let id = UploadId::parse(ID).unwrap();
        assert_eq!(Route::List(name.clone()).package(), Some(&name));
        assert_eq!(Route::List(name.clone()).version(), None);
        assert_eq!(
            Route::Archive(name.clone(), version.clone()).version(),
            Some(&version)
        );
        assert_eq!(Route::NewUpload.package(), None);
        let declared = Route::Upload {
            id,
            declared: Some(Declared {
                name: name.clone(),
                version: version.clone(),
            }),
        };
        assert_eq!(declared.package(), Some(&name));
        assert_eq!(declared.version(), Some(&version));
        assert_eq!(Route::Finish { id, declared: None }.package(), None);
        assert_eq!(id.to_string(), ID);
        assert_eq!(UploadId::new(Uuid::parse_str(ID).unwrap()), id);
    }

    #[test]
    fn prefixes_are_prefixes_of_the_stored_paths() {
        let name = PackageName::parse("http").unwrap();
        let version = PubVersion::parse("1.0.0").unwrap();
        assert!(version_path(&name, &version).starts_with(&version_prefix(&name)));
        let id = UploadId::parse(ID).unwrap();
        let upload = Route::Upload { id, declared: None }.to_path();
        assert!(upload.starts_with(STAGING_PREFIX));
    }
}
