//! `RawLikeHandler`: the smallest possible format, for testing the harness itself and anything
//! that sits on top of the engine without a real format crate.
//!
//! It is the default flow of `classify_path_addressed` and `serve_default` plus the optional
//! hooks of a real format: `*.index` paths are index documents (merged as the union of their
//! lines in a group), the first path segment is the package name (a hosted member claims it
//! against later proxy members), and errors are rendered in a protocol-shaped body so a test
//! can tell that `render_error` ran. A copy of the handler of the format-core integration
//! tests, which is not public.

use async_trait::async_trait;
use axum::body::Body;
use bytes::Bytes;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::auth::Coordinate;
use rustryx_format_core::{
    ArtifactRequest, ComponentSpec, ContentKind, ErrorInfo, FormatContext, FormatError,
    FormatHandler, GroupIndexMerger, GroupNameClaimer, MergeInput, MergedDocument, NameClaim,
    ProxyFetch, RequestHead, UploadSpec, classify_path_addressed,
};
use std::collections::HashSet;

/// Union of lines, in group order, without duplicates.
struct LineMerger;

impl GroupIndexMerger for LineMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let mut seen = HashSet::new();
        let mut lines = Vec::new();
        for document in &input.documents {
            let text = std::str::from_utf8(&document.body)
                .map_err(|_| FormatError::unprocessable("member document is not utf-8"))?;
            for line in text.lines() {
                if seen.insert(line.to_owned()) {
                    lines.push(line.to_owned());
                }
            }
        }
        Ok(MergedDocument {
            content_type: "text/plain".into(),
            body: Bytes::from(lines.join("\n")),
        })
    }
}

/// The first path segment is the "package name".
struct FirstSegmentClaimer;

impl GroupNameClaimer for FirstSegmentClaimer {
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim> {
        request
            .path
            .segments()
            .next()
            .map(|name| NameClaim::package(name.to_owned()))
    }
}

/// A path addressed test format, registered for [`RepoFormat::Raw`] unless
/// [`RawLikeHandler::for_format`] says otherwise.
///
/// `GET __whoami` on a hosted repository answers `origin=<addressed> repo=<serving>`, which
/// shows through a group which repository the client addressed and which one answered.
#[derive(Debug, Clone, Copy)]
pub struct RawLikeHandler {
    format: RepoFormat,
}

impl Default for RawLikeHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl RawLikeHandler {
    /// The handler for the `raw` format.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            format: RepoFormat::Raw,
        }
    }

    /// The same path addressed behaviour under another format label, to test what the
    /// harness does per format without a real format crate: the `Bearer realm="pub"`
    /// challenges for `Pub` (which also answers 403 for a repository the caller may not see,
    /// like the real pub handler), the registry challenges for `Docker` and `Oci` (both
    /// labels are registered, as the real OCI handler does).
    #[must_use]
    pub const fn for_format(format: RepoFormat) -> Self {
        Self { format }
    }
}

#[async_trait]
impl FormatHandler for RawLikeHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        match self.format {
            RepoFormat::Raw => &[RepoFormat::Raw],
            RepoFormat::Maven2 => &[RepoFormat::Maven2],
            RepoFormat::Npm => &[RepoFormat::Npm],
            RepoFormat::Pypi => &[RepoFormat::Pypi],
            RepoFormat::Docker | RepoFormat::Oci => &[RepoFormat::Docker, RepoFormat::Oci],
            RepoFormat::Helm => &[RepoFormat::Helm],
            RepoFormat::Pub => &[RepoFormat::Pub],
            // A format added to core later has no label here; `start` then fails with the
            // registry's "declares no formats" instead of serving the wrong one.
            _ => &[],
        }
    }

    fn hide_inaccessible_repository(&self) -> bool {
        // pub clients erase their stored token on 401 and keep it on 403, so a hidden
        // repository must answer 403 there (README 8.5).
        self.format != RepoFormat::Pub
    }

    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        let mut request = classify_path_addressed(repository, head)?;
        let first = request.path.segments().next().map(str::to_owned);
        if let Some(name) = first {
            request = request.with_coordinate(Coordinate::new(None, Some(name), None));
        }
        if std::path::Path::new(request.path.as_str())
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("index"))
        {
            request = request.index();
        }
        Ok(request)
    }

    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        match (cx.repository().repo_type, req.action, req.kind) {
            (RepoType::Hosted, Action::Write, _) => {
                let mut spec = UploadSpec::from_request(&req);
                spec.component = req.path.segments().next().map(|name| ComponentSpec {
                    group: None,
                    name: name.to_owned(),
                    version: None,
                });
                let outcome = cx.hosted().upload(spec, body).await?;
                let mut response = Response::new(Body::empty());
                *response.status_mut() = if outcome.created() {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                };
                Ok(response)
            }
            (RepoType::Hosted, Action::Read, _) if req.path.as_str() == "__whoami" => {
                Ok(Response::new(Body::from(format!(
                    "origin={} repo={}",
                    cx.origin_repository().name,
                    cx.repository().name
                ))))
            }
            (RepoType::Proxy, Action::Read, ContentKind::Index) => {
                let document = cx.proxy().fetch_document(ProxyFetch::new(&req)).await?;
                let mut response = Response::new(Body::from(document.body.clone()));
                response.headers_mut().extend(document.response_headers());
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                Ok(response)
            }
            _ => cx.serve_default(req, body).await,
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&LineMerger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&FirstSegmentClaimer)
    }

    fn render_error(&self, error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        // A protocol-shaped body, to prove the hook replaces the generic envelope.
        let mut response = Response::new(Body::from(format!("RAW-ERROR {}", error.code())));
        *response.status_mut() = error.status();
        response.headers_mut().extend(error.headers());
        if let Some(id) = info.request_id
            && let Ok(value) = HeaderValue::from_str(id)
        {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    }
}
