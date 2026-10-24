//! The `FormatHandler` of the pub format.

use std::sync::Arc;

use async_trait::async_trait;
use axum_core::body::Body;
use http::{Method, Response, StatusCode};
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::auth::Coordinate;
use rustryx_format_core::{
    ArtifactRequest, ErrorInfo, FormatContext, FormatError, FormatHandler, GroupIndexMerger,
    GroupNameClaimer, NormalizedPath, RequestHead,
};

use crate::accept::accepts_v2;
use crate::blocking::BlockingPool;
use crate::config::PubConfig;
use crate::error;
use crate::hosted::{finalize, publish, read};
use crate::merge::{PubClaimer, PubMerger};
use crate::name::PackageName;
use crate::proxy;
use crate::route::Route;
use crate::version::PubVersion;

/// Serves hosted, proxy and group repositories of the `pub` format.
///
/// One handler serves `dart pub` and `flutter pub` alike: the Flutter tool runs the Dart client,
/// so there is no second protocol.
#[derive(Debug)]
pub struct PubHandler {
    config: Arc<PubConfig>,
    pool: BlockingPool,
    merger: PubMerger,
    claimer: PubClaimer,
}

impl PubHandler {
    /// A handler with the given settings.
    #[must_use]
    pub fn new(config: PubConfig) -> Self {
        Self {
            pool: BlockingPool::new(config.blocking_permits, config.blocking_permit_wait),
            merger: PubMerger::new(config.max_versions_per_package),
            claimer: PubClaimer,
            config: Arc::new(config),
        }
    }

    /// The settings in force.
    #[must_use]
    pub fn config(&self) -> &PubConfig {
        &self.config
    }
}

impl Default for PubHandler {
    fn default() -> Self {
        Self::new(PubConfig::default())
    }
}

/// The methods a route supports and the action they need.
fn action_of(route: &Route, method: &Method) -> Result<Action, FormatError> {
    let read = *method == Method::GET || *method == Method::HEAD;
    match route {
        Route::List(_) | Route::Version(..) | Route::Archive(..) | Route::Advisories(_) => {
            if read {
                Ok(Action::Read)
            } else {
                Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" })
            }
        }
        // Starting and finishing a publication are `GET`s that change state, so they need the
        // write privilege, and `HEAD` (which must not) is refused.
        Route::NewUpload | Route::Finish { .. } => {
            if *method == Method::GET {
                Ok(Action::Write)
            } else {
                Err(FormatError::MethodNotAllowed { allow: "GET" })
            }
        }
        Route::Upload { .. } => {
            if *method == Method::POST {
                Ok(Action::Write)
            } else {
                Err(FormatError::MethodNotAllowed { allow: "POST" })
            }
        }
    }
}

fn coordinate_of(name: &PackageName, version: Option<&PubVersion>) -> Coordinate {
    Coordinate::new(
        None,
        Some(name.to_string()),
        version.map(ToString::to_string),
    )
}

/// True for the routes whose answer is a JSON document the client negotiated.
fn negotiates(route: &Route) -> bool {
    matches!(
        route,
        Route::List(_) | Route::Version(..) | Route::NewUpload | Route::Finish { .. }
    )
}

#[async_trait]
impl FormatHandler for PubHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Pub]
    }

    fn classify(
        &self,
        _repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        let path = NormalizedPath::parse(&head.path)?;
        let route = Route::parse(&path)?;
        let action = action_of(&route, &head.method)?;
        // The coordinate that RBAC and routing rules see, derived from the path before the body
        // is read. A publication names its package only inside the archive, so the steps of a
        // standard client carry none (and are decided again with the real coordinate when the
        // archive has been read); a client that declared one gets it here.
        let coordinate = match &route {
            Route::List(name) | Route::Advisories(name) => Some(coordinate_of(name, None)),
            Route::Version(name, version) | Route::Archive(name, version) => {
                Some(coordinate_of(name, Some(version)))
            }
            Route::NewUpload => publish::declared_from_head(head)?
                .map(|declared| coordinate_of(&declared.name, Some(&declared.version))),
            Route::Upload { declared, .. } | Route::Finish { declared, .. } => declared
                .as_ref()
                .map(|declared| coordinate_of(&declared.name, Some(&declared.version))),
        };
        let mut request = ArtifactRequest::new(head, path, action);
        if let Some(coordinate) = coordinate {
            request = request.with_coordinate(coordinate);
        }
        if matches!(route, Route::List(_) | Route::Version(..)) {
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
        let route = Route::parse(&req.path)?;
        if negotiates(&route) && !accepts_v2(&req.head.headers) {
            return Ok(error::respond(
                StatusCode::NOT_ACCEPTABLE,
                "not_acceptable",
                "this repository serves application/vnd.pub.v2+json",
            ));
        }
        let cfg = self.config.as_ref();
        match (cx.repository().repo_type, route) {
            // Advisories are not part of phase 1. The answer is an honest "not found": an empty
            // list would claim that the package is free of known vulnerabilities.
            (_, Route::Advisories(_)) => Err(FormatError::NotFound),

            (RepoType::Hosted, Route::List(name)) => read::list(&cx, cfg, &req, &name).await,
            (RepoType::Hosted, Route::Version(name, version)) => {
                read::version(&cx, cfg, &req, &name, &version).await
            }
            (RepoType::Hosted, Route::Archive(..)) => read::archive(&cx, &req).await,
            (RepoType::Hosted, Route::NewUpload) => publish::begin(&cx, cfg, &req),
            (RepoType::Hosted, Route::Upload { .. }) => {
                publish::receive(&cx, cfg, &req, body).await
            }
            (RepoType::Hosted, Route::Finish { .. }) => {
                finalize::run(&cx, &self.config, &self.pool, &req).await
            }

            (RepoType::Proxy, Route::List(name)) => {
                proxy::list(&cx, cfg, &self.pool, &req, &name).await
            }
            (RepoType::Proxy, Route::Version(name, version)) => {
                proxy::version(&cx, cfg, &self.pool, &req, &name, &version).await
            }
            (RepoType::Proxy, Route::Archive(name, version)) => {
                proxy::archive(&cx, cfg, &self.pool, &req, &name, &version).await
            }

            (RepoType::Group, Route::List(_) | Route::Version(..) | Route::Archive(..)) => {
                cx.group().resolve(&req).await
            }

            // The engine refuses writes to proxy and group repositories before it gets here;
            // this arm only keeps the match total.
            _ => Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" }),
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&self.merger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&self.claimer)
    }

    fn render_error(&self, failure: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        error::render(failure, info.anonymous)
    }

    /// pub clients erase their stored token on `401` and keep it on `403`. A repository that the
    /// caller may not see is therefore answered `403` with the pub challenge, not `404`.
    fn hide_inaccessible_repository(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests;
