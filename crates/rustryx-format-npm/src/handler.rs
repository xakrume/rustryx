//! [`NpmHandler`]: the npm registry format.
//!
//! `classify` turns the URL into a [`Route`] (before the body is read) so that authorization and
//! routing rules see the package, the scope as group and the version of the request. `handle`
//! dispatches the route by repository type to [`crate::hosted`], [`crate::proxied`] or
//! [`crate::grouped`]; the routes that concern the caller and not the content (`ping`, `whoami`,
//! login, audit) are answered the same on every repository type.

use std::sync::Arc;

use async_trait::async_trait;
use axum_core::body::Body;
use http::Response;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_format_core::{
    ArtifactRequest, ErrorInfo, FormatContext, FormatError, FormatHandler, GroupIndexMerger,
    GroupNameClaimer, NormalizedPath, PathRules, RequestHead,
};

use crate::blocking::Blocking;
use crate::group::{NpmClaims, NpmMerger};
use crate::respond;
use crate::route::Route;
use crate::session::TokenIssuer;

/// Tunable limits of the npm format.
#[derive(Debug, Clone)]
pub struct NpmConfig {
    /// Largest package document that is parsed: stored documents and upstream documents. A
    /// larger one is refused (`413` on a write, `502` for an upstream document).
    pub max_packument_bytes: u64,
    /// Largest metadata part of a publish or a document `PUT` (everything but the tarball).
    pub max_publish_metadata_bytes: usize,
    /// Permits of the pool that parses documents (see [`crate::blocking`]).
    pub blocking_permits: usize,
    /// Components a search reads before it stops looking for more packages.
    pub search_scan_limit: usize,
    /// Most results one search returns (the registry's own maximum is 250; every result of a
    /// hosted search costs a document read).
    pub search_max_results: usize,
}

impl Default for NpmConfig {
    fn default() -> Self {
        Self {
            max_packument_bytes: 32 * 1024 * 1024,
            max_publish_metadata_bytes: 16 * 1024 * 1024,
            blocking_permits: 4,
            search_scan_limit: 2_000,
            search_max_results: 50,
        }
    }
}

/// The npm format handler. Register one instance per engine.
pub struct NpmHandler {
    pub(crate) config: NpmConfig,
    pub(crate) blocking: Blocking,
    pub(crate) tokens: Option<Arc<dyn TokenIssuer>>,
    merger: NpmMerger,
    claims: NpmClaims,
}

impl Default for NpmHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl NpmHandler {
    /// A handler with the default limits and without a token issuer.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(NpmConfig::default())
    }

    /// A handler with explicit limits.
    #[must_use]
    pub fn with_config(config: NpmConfig) -> Self {
        Self {
            blocking: Blocking::new(config.blocking_permits),
            config,
            tokens: None,
            merger: NpmMerger,
            claims: NpmClaims,
        }
    }

    /// Lets the legacy `npm login` and `npm logout` routes mint and revoke API tokens through
    /// `issuer`. Without one they answer `501`.
    #[must_use]
    pub fn with_token_issuer(mut self, issuer: Arc<dyn TokenIssuer>) -> Self {
        self.tokens = Some(issuer);
        self
    }

    /// The limits in force.
    #[must_use]
    pub fn config(&self) -> &NpmConfig {
        &self.config
    }
}

/// Path rules of the registry: `%2f` is the slash of a scoped name.
fn path_rules() -> PathRules {
    PathRules {
        allow_encoded_slash: true,
        allow_empty: true,
        ..PathRules::default()
    }
}

#[async_trait]
impl FormatHandler for NpmHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Npm]
    }

    fn classify(
        &self,
        _repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        let path = NormalizedPath::parse_with(&head.path, &path_rules())?;
        let route = Route::parse(&head.method, &path)?;
        let mut request = ArtifactRequest::new(head, path, route.action(&head.method));
        if let Some(coordinate) = route.coordinate() {
            request = request.with_coordinate(coordinate);
        }
        if route.is_index(&head.method) {
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
        let route = Route::parse(req.method(), &req.path)?;
        match &route {
            Route::Ping => return Ok(respond::json(http::StatusCode::OK, &serde_json::json!({}))),
            Route::Whoami => return Self::whoami(&cx),
            Route::Audit => return Ok(crate::session::audit_answer(&req)),
            Route::Login { user } => return self.login(&cx, user, body).await,
            Route::Logout { token } => return self.logout(&cx, token).await,
            _ => {}
        }
        match cx.repository().repo_type {
            RepoType::Hosted => self.hosted(&cx, &req, route, body).await,
            RepoType::Proxy => self.proxied(&cx, &req, route).await,
            RepoType::Group => self.grouped(&cx, &req, &route).await,
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&self.merger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&self.claims)
    }

    fn render_error(&self, error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        respond::render_error(error, info)
    }
}
