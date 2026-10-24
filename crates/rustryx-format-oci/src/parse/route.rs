//! The routes below `/v2/<rustryx-repo>/`.
//!
//! The HTTP layer hands the engine the path `v2/<image path>/<endpoint>` (the Rustryx
//! repository was already taken from the first segment). This module reads that path
//! from its END, because an image path may itself contain the words `blobs`, `manifests`,
//! `tags` or `referrers`:
//!
//! ```text
//! <name>/manifests/<reference>          <name>/blobs/<digest>
//! <name>/blobs/uploads                  <name>/blobs/uploads/<session>
//! <name>/tags/list                      <name>/referrers/<digest>
//! _catalog
//! ```
//!
//! The upload patterns are tried first: their last three segments are fixed words around
//! the session, so no other endpoint can end that way. `<name>/blobs/uploads` is never a
//! blob (`uploads` is not a digest), so the order of the rest does not matter.
//!
//! [`OciRoute::canonical_path`] gives the text that `classify` stores as the normalized
//! path and that `handle` parses again, so what is authorized is what is served
//! (SEC-19) and nothing decodes twice.

use super::digest::{Digest, DigestError};
use super::name::{NameError, RepositoryName};
use super::reference::{Reference, ReferenceError};
use super::session::{SessionIdError, UploadSessionId};

/// Why a path is not an OCI route.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// The path matches no endpoint.
    #[error("not an OCI Distribution endpoint")]
    NotARoute,
    /// The image name is invalid.
    #[error("invalid repository name")]
    Name(#[from] NameError),
    /// The manifest reference is neither a tag nor a digest.
    #[error("invalid reference")]
    Reference(#[from] ReferenceError),
    /// The digest is invalid.
    #[error("invalid digest")]
    Digest(#[from] DigestError),
    /// The upload session id is invalid.
    #[error("invalid upload session")]
    Session(#[from] SessionIdError),
}

/// An endpoint of the registry API, below the Rustryx repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OciRoute {
    /// `GET /v2/`: the version check.
    Ping,
    /// `GET /v2/_catalog`.
    Catalog,
    /// `GET <name>/tags/list`.
    Tags {
        /// The image.
        name: RepositoryName,
    },
    /// `GET`, `HEAD`, `PUT`, `DELETE` of `<name>/manifests/<reference>`.
    Manifest {
        /// The image.
        name: RepositoryName,
        /// Tag or digest.
        reference: Reference,
    },
    /// `GET`, `HEAD`, `DELETE` of `<name>/blobs/<digest>`.
    Blob {
        /// The image.
        name: RepositoryName,
        /// The blob digest.
        digest: Digest,
    },
    /// `POST <name>/blobs/uploads/`.
    UploadStart {
        /// The image.
        name: RepositoryName,
    },
    /// `GET`, `PATCH`, `PUT`, `DELETE` of `<name>/blobs/uploads/<session>`.
    Upload {
        /// The image.
        name: RepositoryName,
        /// The session.
        session: UploadSessionId,
    },
    /// `GET <name>/referrers/<digest>`.
    Referrers {
        /// The image.
        name: RepositoryName,
        /// The subject digest.
        digest: Digest,
    },
}

impl OciRoute {
    /// Parses a normalized path below `v2/`: decoded, no leading or trailing slash. The
    /// empty path is the ping.
    ///
    /// # Errors
    /// [`RouteError`]; an error about a name, reference, digest or session means the
    /// path has the shape of that endpoint with a bad part, which the client is told.
    pub fn parse(path: &str) -> Result<Self, RouteError> {
        if path.is_empty() {
            return Ok(Self::Ping);
        }
        if path == "_catalog" {
            return Ok(Self::Catalog);
        }
        let segments: Vec<&str> = path.split('/').collect();
        let count = segments.len();
        let at = |index: usize| segments.get(index).copied().unwrap_or_default();
        let name_of = |end: usize| -> Result<RepositoryName, RouteError> {
            let parts = segments.get(..end).ok_or(RouteError::NotARoute)?;
            Ok(RepositoryName::parse(&parts.join("/"))?)
        };
        if count >= 4 && at(count - 3) == "blobs" && at(count - 2) == "uploads" {
            return Ok(Self::Upload {
                name: name_of(count - 3)?,
                session: UploadSessionId::parse(at(count - 1))?,
            });
        }
        if count >= 3 && at(count - 2) == "blobs" && at(count - 1) == "uploads" {
            return Ok(Self::UploadStart {
                name: name_of(count - 2)?,
            });
        }
        if count < 3 {
            return Err(RouteError::NotARoute);
        }
        let last = at(count - 1);
        match at(count - 2) {
            "manifests" => Ok(Self::Manifest {
                name: name_of(count - 2)?,
                reference: Reference::parse(last)?,
            }),
            "blobs" => Ok(Self::Blob {
                name: name_of(count - 2)?,
                digest: Digest::parse(last)?,
            }),
            "referrers" => Ok(Self::Referrers {
                name: name_of(count - 2)?,
                digest: Digest::parse(last)?,
            }),
            "tags" if last == "list" => Ok(Self::Tags {
                name: name_of(count - 2)?,
            }),
            _ => Err(RouteError::NotARoute),
        }
    }

    /// The image the route addresses, if it addresses one.
    #[must_use]
    pub fn image(&self) -> Option<&RepositoryName> {
        match self {
            Self::Ping | Self::Catalog => None,
            Self::Tags { name }
            | Self::Manifest { name, .. }
            | Self::Blob { name, .. }
            | Self::UploadStart { name }
            | Self::Upload { name, .. }
            | Self::Referrers { name, .. } => Some(name),
        }
    }

    /// The tag or digest the route addresses (the "version" of its coordinate).
    #[must_use]
    pub fn version(&self) -> Option<String> {
        match self {
            Self::Manifest { reference, .. } => Some(reference.to_string()),
            Self::Blob { digest, .. } | Self::Referrers { digest, .. } => Some(digest.to_string()),
            _ => None,
        }
    }

    /// The canonical normalized path: parsing it again yields this route.
    #[must_use]
    pub fn canonical_path(&self) -> String {
        match self {
            Self::Ping => String::new(),
            Self::Catalog => "_catalog".to_owned(),
            Self::Tags { name } => format!("{name}/tags/list"),
            Self::Manifest { name, reference } => format!("{name}/manifests/{reference}"),
            Self::Blob { name, digest } => format!("{name}/blobs/{digest}"),
            Self::UploadStart { name } => format!("{name}/blobs/uploads"),
            Self::Upload { name, session } => format!("{name}/blobs/uploads/{session}"),
            Self::Referrers { name, digest } => format!("{name}/referrers/{digest}"),
        }
    }
}

#[cfg(test)]
mod tests;
