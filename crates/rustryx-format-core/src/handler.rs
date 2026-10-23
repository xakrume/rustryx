//! The contract a package format implements.

use async_trait::async_trait;
use axum_core::body::Body;
use http::Response;
use rustryx_core::model::{RepoFormat, Repository};

use crate::context::FormatContext;
use crate::error::FormatError;
use crate::group::{GroupIndexMerger, GroupNameClaimer};
use crate::request::{ArtifactRequest, RequestHead};

/// What [`FormatHandler::render_error`] may use to build a protocol error body.
#[derive(Debug, Clone, Copy)]
pub struct ErrorInfo<'a> {
    /// Request id, for the generic envelope and for log correlation.
    pub request_id: Option<&'a str>,
    /// The repository that was addressed.
    pub repository: &'a Repository,
    /// True when the caller presented no credentials.
    pub anonymous: bool,
}

/// One package format (maven, npm, pypi, oci, helm, raw, pub).
///
/// ## Request life cycle
///
/// 1. The HTTP layer builds a [`RequestHead`] and calls [`crate::FormatEngine::serve`].
/// 2. The engine calls [`FormatHandler::classify`] **before the body is read**. The
///    returned [`ArtifactRequest`] carries the normalized path, the action and an
///    optional component coordinate. Authorization and routing rules run on exactly
///    that value, so what is authorized is what is read (SEC-19).
/// 3. Only then does the engine call [`FormatHandler::handle`] with the body.
/// 4. Any [`FormatError`] goes through [`FormatHandler::render_error`].
///
/// ## Implementing a format
///
/// * `classify` is pure and cheap: no I/O. Normalize the path with
///   [`crate::NormalizedPath`] exactly once, choose the [`rustryx_core::identity::Action`], set
///   [`crate::ContentKind::Index`] for documents that a group must merge or that the
///   proxy caches as metadata.
/// * `handle` dispatches on `cx.repository().repo_type`. For plain path-addressed
///   content call `cx.serve_default(req, body)`; for format specific documents
///   use the pipelines directly: `cx.hosted()`, `cx.proxy()`, `cx.group()`.
/// * Never read `req.head.path` in `handle`; read `req.path`.
/// * Build every URL you write into a document from `cx.origin_repository()`.
///
/// Cancel safety: `handle` may be dropped at any await point (client disconnect).
/// The pipelines are written for that; a handler must not add state that outlives
/// a cancelled call without a guard.
#[async_trait]
pub trait FormatHandler: Send + Sync + 'static {
    /// The repository format labels this handler serves. The OCI handler returns both
    /// `Docker` and `Oci`.
    fn formats(&self) -> &'static [RepoFormat];

    /// Normalizes the request and decides what it needs, before the body is read.
    ///
    /// # Errors
    /// [`FormatError::BadRequest`] for a malformed path (use `?` on a
    /// [`crate::PathError`]), [`FormatError::MethodNotAllowed`] for an unsupported method.
    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError>;

    /// Serves the classified request. `body` is the request body (empty for reads).
    ///
    /// # Errors
    /// Any [`FormatError`]; the engine renders it via [`FormatHandler::render_error`].
    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError>;

    /// Merger for index documents when a group of this format is read. `None`
    /// means a group returns the first member's document.
    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        None
    }

    /// Dependency-confusion protection for groups of this format: which package
    /// name a request addresses, so that a hosted member that has the name claims
    /// it against later proxy members. `None` disables claiming.
    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        None
    }

    /// Renders an error in the format's own protocol shape (OCI `errors` array, pub
    /// `error` object, npm `{"error": ...}`). The default is the generic envelope.
    ///
    /// Use [`FormatError::status`], [`FormatError::code`], [`FormatError::public_message`]
    /// and copy [`FormatError::headers`] so `Retry-After`, `Allow` and `Content-Range`
    /// are not lost. Never put anything but those into the body.
    fn render_error(&self, error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        error.to_generic_response(info.request_id)
    }

    /// Whether a repository the caller may not see answers 404 (default, so its
    /// existence is not revealed) or 403. pub overrides this: its clients erase the
    /// stored token on 401 and keep it on 403, and the spec of this project wants
    /// 403 for hidden repositories there.
    fn hide_inaccessible_repository(&self) -> bool {
        true
    }
}
