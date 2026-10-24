//! What every operation needs from the request, in one place.

use http::header;
use rustryx_core::audit::ActorRef;
use rustryx_core::model::WriteOptions;
use rustryx_format_core::{ArtifactRequest, FormatContext, NormalizedPath};

use crate::config::OciConfig;
use crate::error::OciError;
use crate::parse::name::RepositoryName;
use crate::proxy::UpstreamTokens;

/// State shared by all requests of one handler.
pub(crate) struct Shared {
    pub(crate) config: OciConfig,
    pub(crate) tokens: UpstreamTokens,
}

/// One classified request with its context.
pub(crate) struct Op<'a> {
    pub(crate) cx: &'a FormatContext,
    pub(crate) req: &'a ArtifactRequest,
    pub(crate) shared: &'a Shared,
}

impl Op<'_> {
    /// The configuration of the engine.
    pub(crate) fn config(&self) -> &OciConfig {
        &self.shared.config
    }

    /// The name a client uses for the repository it addressed (for a group request served
    /// by a member, the group: documents and URLs must keep pointing at it).
    pub(crate) fn origin_name(&self) -> &str {
        self.cx.origin_repository().name.as_str()
    }

    /// `<repo>/<image>`: the name in documents (`tags/list`, the catalog).
    pub(crate) fn docker_name(&self, image: &RepositoryName) -> String {
        format!("{}/{image}", self.origin_name())
    }

    /// An absolute URL below `/v2/<repo>/`, for `Location` headers and the like.
    pub(crate) fn url(&self, tail: &str) -> String {
        format!(
            "{}/v2/{}/{tail}",
            self.req.head.base_url.as_str().trim_end_matches('/'),
            self.origin_name()
        )
    }

    /// A path-absolute reference below `/v2/<repo>/`, for `Link` headers.
    pub(crate) fn path_ref(&self, tail: &str) -> String {
        format!("/v2/{}/{tail}", self.origin_name())
    }

    /// The options of a state changing port call made for this request.
    pub(crate) fn write_options(&self) -> WriteOptions {
        let mut options = WriteOptions::new().with_actor(ActorRef::of(self.cx.principal()));
        if let Some(id) = self.cx.request_id() {
            options = options.with_request_id(id);
        }
        options
    }

    /// The values of a header, as text.
    pub(crate) fn header_values(&self, name: &header::HeaderName) -> Vec<&str> {
        self.req
            .head
            .headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect()
    }

    /// A normalized path built from text this crate made from validated parts.
    pub(crate) fn path(text: &str) -> Result<NormalizedPath, OciError> {
        NormalizedPath::parse(text).map_err(|_| {
            OciError::internal("a registry path built by the crate failed to normalize")
        })
    }
}
