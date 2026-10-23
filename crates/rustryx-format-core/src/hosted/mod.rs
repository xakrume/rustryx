//! Hosted repositories: content is uploaded to this repository.
//!
//! The upload path is a straight line with one visibility commit:
//!
//! ```text
//! policy hook -> body stream -> size limit -> 64 KiB slices -> BlobWriter (hashes) -> commit
//!             -> AssetRepo::publish_with   <- the only moment the artifact becomes visible
//! ```
//!
//! Nothing is collected into memory. Any error, a client that disconnects, or a
//! cancelled request aborts the staged blob; a published artifact is never half
//! there. See [`UploadSpec`] for what a format controls.
//!
//! Uploads that span requests (OCI chunked blob uploads) use the same publish step through
//! [`HostedPipeline::begin_chunked`] and friends, on stores whose
//! `BlobStoreCapabilities::resumable` is set.

mod chunked;
mod download;
pub(crate) mod quota;
mod reservation;
mod upload;

use axum_core::body::Body;
use http::Response;
use rustryx_core::model::{Asset, BlobRecord, RepoType};

pub use chunked::ChunkedSession;
pub use download::{CacheStatus, X_RUSTRYX_CACHE};
pub(crate) use download::{ServeOptions, serve_asset};
pub use reservation::{UploadReservation, UploadReservations};

use crate::checksum::ExpectedChecksum;
use crate::context::FormatContext;
use crate::error::FormatError;
use crate::path::NormalizedPath;
use crate::request::{ArtifactRequest, ContentKind};

/// Whether an existing path may be overwritten, beyond the repository write policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverwriteRule {
    /// Follow the repository: `allow` overwrites, `allow_once` conflicts, `deny` refuses.
    #[default]
    FromRepositoryPolicy,
    /// Never overwrite, whatever the policy says. For immutable things such as an
    /// npm version, a pub version, a maven release.
    Never,
}

/// Component coordinate to create or reuse together with the asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentSpec {
    /// Namespace (maven groupId, npm scope).
    pub group: Option<String>,
    /// Package name.
    pub name: String,
    /// Version.
    pub version: Option<String>,
}

/// What a format tells the hosted pipeline about one upload.
#[derive(Debug, Clone)]
pub struct UploadSpec {
    /// Normalized storage path. Taken from [`ArtifactRequest::path`].
    pub path: NormalizedPath,
    /// `Content-Type` as declared by the client; sanitized by
    /// [`crate::mime::resolve_content_type`], falling back to the extension.
    pub content_type: Option<String>,
    /// Exact body length when known (`Content-Length`). It is checked against the
    /// limits before the body is read and against the bytes actually received after.
    pub content_length: Option<u64>,
    /// Component to attach, if the format knows the coordinate.
    pub component: Option<ComponentSpec>,
    /// Digest the client declared (OCI blob digest, pub `archive_sha256`). A mismatch
    /// rejects the upload and nothing is published.
    pub expected: Option<ExpectedChecksum>,
    /// Overwrite rule on top of the write policy.
    pub overwrite: OverwriteRule,
}

impl UploadSpec {
    /// A spec for `path` with nothing else declared.
    #[must_use]
    pub fn new(path: NormalizedPath) -> Self {
        Self {
            path,
            content_type: None,
            content_length: None,
            component: None,
            expected: None,
            overwrite: OverwriteRule::FromRepositoryPolicy,
        }
    }

    /// The default spec of a `PUT`: content type and length from the request headers.
    #[must_use]
    pub fn from_request(req: &ArtifactRequest) -> Self {
        let mut spec = Self::new(req.path.clone());
        spec.content_type = req
            .head
            .header(&http::header::CONTENT_TYPE)
            .map(str::to_owned);
        spec.content_length = req.head.content_length();
        spec
    }
}

/// Result of a successful upload.
#[derive(Debug, Clone)]
pub struct UploadOutcome {
    /// The asset after the commit.
    pub asset: Asset,
    /// The blob this upload replaced. It is already unreferenced in the database
    /// and has been announced through [`crate::ContentEvents`]; the GC removes it.
    /// `None` for a new path, and also when the replaced blob is still referenced by another
    /// asset (a linked copy), so it is not a test for "the path existed": use `created`.
    pub replaced: Option<BlobRecord>,
    /// True when the path did not exist before (the port's own answer).
    pub created: bool,
}

impl UploadOutcome {
    /// True when the path did not exist before.
    #[must_use]
    pub fn created(&self) -> bool {
        self.created
    }
}

/// Hosted operations on the repository of a [`FormatContext`].
pub struct HostedPipeline<'a> {
    cx: &'a FormatContext,
}

impl<'a> HostedPipeline<'a> {
    pub(crate) fn new(cx: &'a FormatContext) -> Self {
        Self { cx }
    }

    fn require_hosted(&self) -> Result<(), FormatError> {
        if self.cx.repository().repo_type == RepoType::Hosted {
            Ok(())
        } else {
            Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" })
        }
    }

    /// Streams `body` into the blob store and publishes it under `spec.path`.
    ///
    /// Order of events: write policy, declared size and quota, body streamed with the
    /// byte limit enforced on actual bytes, commit, digest check, component, publish,
    /// event and audit. The final phase (commit and publish) runs in a task tracked
    /// by the context, so a client that disconnects right after the last byte cannot
    /// leave a committed blob without a database row.
    ///
    /// Cancel safe: dropping the future before the last body byte aborts the staged
    /// blob and releases the path reservation; nothing is published.
    ///
    /// # Errors
    /// `Forbidden` (read-only repository), `Conflict` (write policy, concurrent
    /// upload of the same path), `PayloadTooLarge`, `QuotaExceeded`, `RequestTimeout`,
    /// `BadRequest` (unreadable body, length mismatch), `DigestMismatch`, and store errors.
    pub async fn upload(&self, spec: UploadSpec, body: Body) -> Result<UploadOutcome, FormatError> {
        self.require_hosted()?;
        upload::run(self.cx, spec, body).await
    }

    /// Serves the asset at the request path: `GET` and `HEAD`, conditional requests
    /// and single byte ranges, streamed from the blob store with backpressure.
    ///
    /// Cancel safe.
    ///
    /// # Errors
    /// `NotFound`, `RangeNotSatisfiable`, `PreconditionFailed`, store and blob errors.
    pub async fn download(&self, req: &ArtifactRequest) -> Result<Response<Body>, FormatError> {
        let repository = self.cx.repository();
        let asset = self
            .cx
            .ports()
            .assets
            .get_by_path(repository.id, req.path.as_str())
            .await?
            .ok_or(FormatError::NotFound)?;
        serve_asset(
            self.cx,
            &req.head,
            &asset,
            ServeOptions {
                cache: None,
                // Only artifacts count: an index document is read once per member of a group.
                count_download: req.kind == ContentKind::Artifact,
            },
        )
        .await
    }

    /// Deletes the asset at `path` and announces its blob for garbage collection. Returns the
    /// blob that lost its last reference, `None` when another asset still references it.
    ///
    /// The policy hook runs first and the transaction (`delete_by_path_checked`) decides
    /// again, so an immutable or held asset stays whatever the hook said.
    ///
    /// Cancel safe: the delete is one statement.
    ///
    /// # Errors
    /// `Forbidden` for a read-only repository, `NotFound` when there is no such asset,
    /// `Policy` (409, or 503 in read only mode) when immutability, a hold or the system state
    /// refuses the delete.
    pub async fn delete(&self, path: &NormalizedPath) -> Result<Option<BlobRecord>, FormatError> {
        self.require_hosted()?;
        upload::delete(self.cx, path).await
    }
}
