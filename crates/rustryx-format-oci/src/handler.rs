//! `OciHandler`: the `FormatHandler` of the `docker` and `oci` labels.

use std::sync::Arc;

use async_trait::async_trait;
use axum_core::body::Body;
use http::{Method, Response, StatusCode};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_format_core::{
    ArtifactRequest, ErrorInfo, FormatContext, FormatError, FormatHandler, GroupIndexMerger,
    GroupNameClaimer, RequestHead,
};

use crate::config::OciConfig;
use crate::error::{OciError, OciErrorCode};
use crate::group::{OciClaimer, OciMerger, finish_listing};
use crate::ops::{Op, Shared};
use crate::parse::route::OciRoute;
use crate::proxy::UpstreamTokens;
use crate::token::ping_ok;
use crate::{blobs, listing, manifests, proxy, uploads};

/// One handler for both repository labels. Register it once:
/// `registry.register(Arc::new(OciHandler::new(OciConfig::default())))`.
pub struct OciHandler {
    shared: Arc<Shared>,
    merger: OciMerger,
    claimer: OciClaimer,
}

impl OciHandler {
    /// A handler with the given tunables.
    #[must_use]
    pub fn new(config: OciConfig) -> Self {
        Self {
            merger: OciMerger::new(config.clone()),
            claimer: OciClaimer,
            shared: Arc::new(Shared {
                tokens: UpstreamTokens::new(&config),
                config,
            }),
        }
    }

    /// The tunables of this handler.
    #[must_use]
    pub fn config(&self) -> &OciConfig {
        &self.shared.config
    }
}

impl Default for OciHandler {
    fn default() -> Self {
        Self::new(OciConfig::default())
    }
}

#[async_trait]
impl FormatHandler for OciHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Docker, RepoFormat::Oci]
    }

    fn classify(
        &self,
        _repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        crate::classify::classify_request(head)
    }

    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        // `classify` produced this path, so it parses; a failure here is a bug.
        let route = OciRoute::parse(req.path.as_str())
            .map_err(|_| FormatError::internal("a classified registry path failed to parse"))?;
        let op = Op {
            cx: &cx,
            req: &req,
            shared: &self.shared,
        };
        match dispatch(&op, &route, body).await {
            Ok(response) => Ok(response),
            // A 5xx goes back to the engine as a `FormatError`: logged once, rendered by
            // `render_error`, and skipped (not forwarded) by a group.
            Err(error) if error.is_server_error() => Err(error.into_format_error()),
            // Everything else is a protocol answer and is returned as one.
            Err(error) => Ok(error.into_response()),
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&self.merger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&self.claimer)
    }

    fn render_error(&self, error: &FormatError, _info: &ErrorInfo<'_>) -> Response<Body> {
        OciError::describe(error).into_response()
    }
}

async fn dispatch(op: &Op<'_>, route: &OciRoute, body: Body) -> Result<Response<Body>, OciError> {
    match op.cx.repository().repo_type {
        RepoType::Hosted => hosted(op, route, body).await,
        RepoType::Proxy => proxied(op, route).await,
        RepoType::Group => grouped(op, route).await,
    }
}

fn not_allowed() -> OciError {
    OciError::unsupported(
        StatusCode::METHOD_NOT_ALLOWED,
        "the operation is not supported on this endpoint",
    )
}

async fn hosted(op: &Op<'_>, route: &OciRoute, body: Body) -> Result<Response<Body>, OciError> {
    let method = op.req.method().clone();
    match route {
        OciRoute::Ping => Ok(ping_ok()),
        OciRoute::Catalog => listing::catalog(op).await,
        OciRoute::Tags { name } => listing::tags(op, name).await,
        OciRoute::Referrers { name, digest } => listing::referrers(op, name, digest).await,
        OciRoute::Manifest { name, reference } => match method {
            Method::GET | Method::HEAD => manifests::read(op).await,
            Method::PUT => manifests::put(op, name, reference, body).await,
            Method::DELETE => manifests::delete(op, reference).await,
            _ => Err(not_allowed()),
        },
        OciRoute::Blob { digest, .. } => match method {
            Method::GET | Method::HEAD => blobs::read(op, digest).await,
            Method::DELETE => blobs::delete(op).await,
            _ => Err(not_allowed()),
        },
        OciRoute::UploadStart { name } => uploads::start(op, name, body).await,
        OciRoute::Upload { name, session } => match method {
            Method::GET => uploads::status(op, name, session).await,
            Method::PATCH => uploads::patch(op, name, session, body).await,
            Method::PUT => uploads::put(op, name, session, body).await,
            Method::DELETE => uploads::cancel(op, session).await,
            _ => Err(not_allowed()),
        },
    }
}

async fn proxied(op: &Op<'_>, route: &OciRoute) -> Result<Response<Body>, OciError> {
    match route {
        OciRoute::Ping => Ok(ping_ok()),
        OciRoute::Catalog => listing::catalog(op).await,
        OciRoute::Tags { name } => proxy::tags(op, name).await,
        OciRoute::Referrers { name, digest } => proxy::referrers(op, name, digest).await,
        OciRoute::Manifest { name, reference } => proxy::read_manifest(op, name, reference).await,
        OciRoute::Blob { name, digest } => proxy::read_blob(op, name, digest).await,
        // The engine refuses writes to a proxy before it gets here.
        OciRoute::UploadStart { .. } | OciRoute::Upload { .. } => Err(not_allowed()),
    }
}

async fn grouped(op: &Op<'_>, route: &OciRoute) -> Result<Response<Body>, OciError> {
    if matches!(route, OciRoute::Ping) {
        return Ok(ping_ok());
    }
    let (code, message) = missing(route);
    let response = op
        .cx
        .group()
        .resolve(op.req)
        .await
        .map_err(|error| OciError::from(error).not_found_as(code, message))?;
    Ok(finish_listing(op.cx, op.req, route, op.config(), response).await)
}

/// The code a missing answer gets on a route.
fn missing(route: &OciRoute) -> (OciErrorCode, &'static str) {
    match route {
        OciRoute::Manifest { .. } => (
            OciErrorCode::ManifestUnknown,
            "manifest unknown to registry",
        ),
        OciRoute::Blob { .. } => (OciErrorCode::BlobUnknown, "blob unknown to registry"),
        OciRoute::Upload { .. } => (
            OciErrorCode::BlobUploadUnknown,
            "blob upload unknown to registry",
        ),
        _ => (
            OciErrorCode::NameUnknown,
            "repository name not known to registry",
        ),
    }
}

#[cfg(test)]
mod tests;
