//! [`FormatEngine`]: the one entry point from the HTTP layer into the formats.
//!
//! It owns the shared state (ports, registry, upstream executor, singleflight map,
//! negative cache, task tracker) and enforces the order that makes the security
//! properties hold, so that no format has to remember it:
//!
//! 1. the handler for the repository's format is looked up;
//! 2. `classify` runs **before the body is read**;
//! 3. writes to proxy and group repositories are refused (405) before any byte of
//!    the body is consumed;
//! 4. authorization runs on the classified request: deny by default, anonymous
//!    callers need the repository flag, a caller who may not see the repository gets
//!    404 (or 403 if the format asks, see `hide_inaccessible_repository`), an
//!    authorization backend failure fails closed;
//! 5. routing rules, then `handle`;
//! 6. any error is logged once, here, and rendered by the format;
//! 7. the response leaves with its [`RequestLabels`] (`class`, `cache`) as an extension, for the
//!    latency histogram of the HTTP layer.

use std::sync::Arc;

use axum_core::body::Body;
use http::Response;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{Action, AuthMethod, Principal};
use rustryx_core::model::{DownloadSource, RepoType, Repository};
use rustryx_core::ports::{Decision, ResourceRef};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::context::{FormatContext, Limits, Ports, RequestMeta, RoutingDecision, Shared};
use crate::error::FormatError;
use crate::handler::{ErrorInfo, FormatHandler};
use crate::hosted::{CacheStatus, UploadReservations, X_RUSTRYX_CACHE};
use crate::metrics::{CacheLabel, RequestClass, RequestLabels};
use crate::proxy::executor::{ExecutorConfig, UpstreamExecutor};
use crate::proxy::flight::Flights;
use crate::proxy::negative::NegativeCache;
use crate::proxy::ssrf::UpstreamUrlPolicy;
use crate::proxy::upstream::{
    ReqwestClientConfig, ReqwestUpstreamClient, UpstreamClient, UpstreamError,
};
use crate::proxy::validators::ValidatorCache;
use crate::registry::FormatRegistry;
use crate::request::{ArtifactRequest, ContentKind, RequestHead};

/// Builder of a [`FormatEngine`].
pub struct FormatEngineBuilder {
    ports: Ports,
    registry: FormatRegistry,
    limits: Limits,
    policy: UpstreamUrlPolicy,
    client: Option<Arc<dyn UpstreamClient>>,
    client_config: ReqwestClientConfig,
    executor: ExecutorConfig,
    shutdown: CancellationToken,
}

impl FormatEngineBuilder {
    /// Overrides the limits.
    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets the SSRF policy for upstream URLs. The default denies every host: a
    /// proxy repository cannot reach anything until the policy allows it.
    #[must_use]
    pub fn url_policy(mut self, policy: UpstreamUrlPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Replaces the HTTP client (tests use [`crate::testing::FakeUpstream`]). The
    /// replacement is responsible for its own DNS guard; the URL policy is still
    /// applied to every URL and redirect by the executor.
    #[must_use]
    pub fn upstream_client(mut self, client: Arc<dyn UpstreamClient>) -> Self {
        self.client = Some(client);
        self
    }

    /// Settings of the default reqwest client (connect timeout, resolver, user agent).
    #[must_use]
    pub fn client_config(mut self, config: ReqwestClientConfig) -> Self {
        self.client_config = config;
        self
    }

    /// Timeouts, retry policy and circuit breaker tuning.
    #[must_use]
    pub fn executor_config(mut self, config: ExecutorConfig) -> Self {
        self.executor = config;
        self
    }

    /// The token cancelled at shutdown. Cache fills select on it.
    #[must_use]
    pub fn shutdown_token(mut self, token: CancellationToken) -> Self {
        self.shutdown = token;
        self
    }

    /// Builds the engine.
    ///
    /// # Errors
    /// [`UpstreamError::Protocol`] when the default HTTP client cannot be built.
    pub fn build(self) -> Result<FormatEngine, UpstreamError> {
        let policy = Arc::new(self.policy);
        let client = match self.client {
            Some(client) => client,
            None => Arc::new(ReqwestUpstreamClient::new(
                Arc::clone(&policy),
                &self.client_config,
            )?),
        };
        let shared = Shared {
            upstream: UpstreamExecutor::new(client, policy, self.executor),
            flights: Flights::new(),
            negative: NegativeCache::new(self.limits.negative_cache_capacity),
            validators: ValidatorCache::new(
                self.limits.validator_cache_capacity,
                self.limits.stale_max_age,
            ),
            reservations: UploadReservations::new(),
            blocking: Arc::new(Semaphore::new(self.limits.blocking_permits.max(1))),
            tasks: TaskTracker::new(),
            shutdown: self.shutdown,
            ports: self.ports,
            registry: self.registry,
            limits: self.limits,
        };
        Ok(FormatEngine {
            shared: Arc::new(shared),
        })
    }
}

/// The format layer, ready to serve. Cheap to clone.
#[derive(Clone)]
pub struct FormatEngine {
    shared: Arc<Shared>,
}

impl FormatEngine {
    /// Starts building an engine over `ports` and the registered `registry`.
    #[must_use]
    pub fn builder(ports: Ports, registry: FormatRegistry) -> FormatEngineBuilder {
        FormatEngineBuilder {
            ports,
            registry,
            limits: Limits::default(),
            policy: UpstreamUrlPolicy::default(),
            client: None,
            client_config: ReqwestClientConfig::default(),
            executor: ExecutorConfig::default(),
            shutdown: CancellationToken::new(),
        }
    }

    /// The registered handlers.
    #[must_use]
    pub fn registry(&self) -> &FormatRegistry {
        &self.shared.registry
    }

    /// The tracker of the engine's background tasks, for work that must end with the engine (the
    /// download counter flusher): [`FormatEngine::drain`] waits for what runs on it.
    #[must_use]
    pub fn tasks(&self) -> &TaskTracker {
        &self.shared.tasks
    }

    /// The shutdown token the engine's tasks listen to.
    #[must_use]
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.shared.shutdown
    }

    /// Stops accepting tracked tasks and waits for the running ones (cache fills that
    /// outlived their client, finalizing uploads). Cancel the shutdown token first so
    /// fills stop streaming; commit and publish phases still run to completion.
    pub async fn drain(&self) {
        self.shared.tasks.close();
        self.shared.tasks.wait().await;
    }

    /// Number of tracked background tasks that have not finished.
    #[must_use]
    pub fn running_tasks(&self) -> usize {
        self.shared.tasks.len()
    }

    /// Number of cache fills in progress (singleflight keys). Exposed for tests and
    /// diagnostics: it must be zero when the system is idle.
    #[must_use]
    pub fn fills_in_flight(&self) -> usize {
        self.shared.flights.in_flight()
    }

    /// A context for `repository` and the caller of `head`, for code that calls a
    /// handler directly (tests, the API layer's own endpoints).
    #[must_use]
    pub fn context(&self, repository: Arc<Repository>, head: &RequestHead) -> FormatContext {
        FormatContext::new(
            Arc::clone(&self.shared),
            repository,
            Arc::clone(&head.principal),
            RequestMeta {
                request_id: head.client.request_id.clone(),
                client_ip: head.client.ip,
                download_source: Some(
                    head.client
                        .download_source
                        .unwrap_or_else(|| download_source_of(&head.principal)),
                ),
            },
        )
    }

    /// Serves one request on `repository`.
    ///
    /// Never fails: every error is rendered into a response by the format. The
    /// request body is not touched before authorization has succeeded.
    ///
    /// Cancel safe: dropping the future (client disconnect) aborts a staged upload,
    /// while a cache fill started by it keeps running in its own task.
    pub async fn serve(
        &self,
        repository: Arc<Repository>,
        head: RequestHead,
        body: Body,
    ) -> Response<Body> {
        let Some(handler) = self.shared.registry.get(repository.format).cloned() else {
            let error = FormatError::internal("no handler is registered for the repository format");
            Self::log(&error, &repository, head.client.request_id.as_deref());
            let mut response = error.to_generic_response(head.client.request_id.as_deref());
            RequestLabels::default().attach(&mut response);
            return response;
        };
        // Set by `dispatch` as soon as the request is classified, so that a failure after that
        // point still reports the right class.
        let mut class = RequestClass::Other;
        match self
            .dispatch(&handler, &repository, &head, body, &mut class)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let request_id = head.client.request_id.as_deref();
                Self::log(&error, &repository, request_id);
                let mut response = handler.render_error(
                    &error,
                    &ErrorInfo {
                        request_id,
                        repository: &repository,
                        anonymous: head.principal.is_anonymous(),
                    },
                );
                RequestLabels::new(class, CacheLabel::None).attach(&mut response);
                response
            }
        }
    }

    async fn dispatch(
        &self,
        handler: &Arc<dyn FormatHandler>,
        repository: &Arc<Repository>,
        head: &RequestHead,
        body: Body,
        class: &mut RequestClass,
    ) -> Result<Response<Body>, FormatError> {
        if !repository.online {
            return Err(FormatError::Unavailable {
                reason: "the repository is offline",
                retry_after: None,
            });
        }
        let request = handler.classify(repository, head)?;
        let (action, kind) = (request.action, request.kind);
        *class = class_of(action, kind);
        let writes = matches!(request.action, Action::Write | Action::Delete);
        if writes && repository.repo_type != RepoType::Hosted {
            // Refused before authorization and before the body is read: nothing about
            // such a request can succeed, and an upload body must not be consumed for it.
            return Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" });
        }
        self.authorize(handler.as_ref(), repository, &request)
            .await?;
        if repository.repo_type != RepoType::Hosted {
            let gate = self
                .shared
                .ports
                .routing
                .check(repository, &request)
                .await?;
            if gate == RoutingDecision::Block {
                return Err(FormatError::NotFound);
            }
        }
        let cx = self.context(Arc::clone(repository), head);
        let mut response = handler.handle(cx, request, body).await?;
        RequestLabels::new(
            *class,
            cache_of(action, kind, repository.repo_type, &response),
        )
        .attach(&mut response);
        Ok(response)
    }

    async fn authorize(
        &self,
        handler: &dyn FormatHandler,
        repository: &Repository,
        request: &ArtifactRequest,
    ) -> Result<(), FormatError> {
        let principal = &request.head.principal;
        let rbac = &self.shared.ports.rbac;
        // Anonymous callers need the repository's own flag on top of whatever the
        // authorizer says (the global switch is the HTTP layer's: it only builds an
        // anonymous principal when the switch is on).
        let anonymous_blocked = principal.is_anonymous() && !repository.allow_anonymous;
        if !anonymous_blocked {
            let mut resource = ResourceRef::for_repository(repository, request.path.as_str());
            if let Some(coordinate) = &request.coordinate {
                resource = resource.with_coordinate(coordinate.clone());
            }
            match rbac.check(principal, request.action, &resource).await {
                Ok(Decision::Allow) => return Ok(()),
                Ok(Decision::Deny) => {}
                Err(AuthError::Overloaded) => {
                    return Err(FormatError::Unavailable {
                        reason: "authorization capacity exhausted, retry",
                        retry_after: Some(std::time::Duration::from_secs(1)),
                    });
                }
                Err(error) => {
                    // Group lookup failure, provider outage: never "no groups", never allow.
                    tracing::warn!(%error, repository = %repository.name, "authorization failed closed");
                    return Err(FormatError::Forbidden);
                }
            }
        }
        if principal.is_anonymous() {
            // Anonymous callers must learn that credentials are needed (maven and pip
            // only send them after a 401), so they are never answered with a 404.
            return Err(FormatError::Unauthenticated);
        }
        if handler.hide_inaccessible_repository() {
            let visible = rbac
                .can_see_repository(principal, repository.name.as_str(), repository.format)
                .await
                .unwrap_or(false);
            if !visible {
                return Err(FormatError::NotFound);
            }
        }
        Err(FormatError::Forbidden)
    }

    fn log(error: &FormatError, repository: &Repository, request_id: Option<&str>) {
        // The one place an error is logged. Expected client errors are not alarms.
        if error.is_server_error() {
            tracing::error!(
                repository = %repository.name,
                request_id,
                code = error.code(),
                error = %error,
                "format request failed"
            );
        } else {
            tracing::info!(
                repository = %repository.name,
                request_id,
                code = error.code(),
                "format request rejected"
            );
        }
    }
}

/// Who caused the read when the HTTP layer did not say: a scanner's short lived token is the
/// scanner, everything else is a client. Replication workers and the node itself call the
/// engine with an explicit `ClientInfo::download_source`.
fn download_source_of(principal: &Principal) -> DownloadSource {
    if principal.auth_method == AuthMethod::ScanToken {
        DownloadSource::Scanner
    } else {
        DownloadSource::Client
    }
}

/// The `class` label of a classified request.
fn class_of(action: Action, kind: ContentKind) -> RequestClass {
    match action {
        Action::Write | Action::Delete => RequestClass::Upload,
        _ if kind == ContentKind::Index => RequestClass::Metadata,
        _ => RequestClass::Download,
    }
}

/// The `cache` label of a response. The proxy and the group already say how they produced a
/// copy in `X-Rustryx-Cache`; a response without it is a read from local storage (`hit`: the
/// request did not wait for anything), or, for a merged group index, one that was composed
/// without a cache (`bypass`). Failures and writes carry no cache relation.
fn cache_of(
    action: Action,
    kind: ContentKind,
    repo_type: RepoType,
    response: &Response<Body>,
) -> CacheLabel {
    if let Some(value) = response.headers().get(X_RUSTRYX_CACHE) {
        return match value.as_bytes() {
            v if v == CacheStatus::Hit.as_str().as_bytes() => CacheLabel::Hit,
            v if v == CacheStatus::Miss.as_str().as_bytes() => CacheLabel::Miss,
            v if v == CacheStatus::Stale.as_str().as_bytes() => CacheLabel::Stale,
            _ => CacheLabel::None,
        };
    }
    let read = matches!(action, Action::Read | Action::Browse);
    let served =
        response.status().is_success() || response.status() == http::StatusCode::NOT_MODIFIED;
    if !read || !served {
        return CacheLabel::None;
    }
    if repo_type == RepoType::Group && kind == ContentKind::Index {
        CacheLabel::Bypass
    } else {
        CacheLabel::Hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::identity::UserSource;

    #[test]
    fn a_scan_token_is_a_scanner_and_everything_else_a_client() {
        let scan = Principal::new("scan", UserSource::Local, AuthMethod::ScanToken);
        assert_eq!(download_source_of(&scan), DownloadSource::Scanner);
        for method in [
            AuthMethod::Anonymous,
            AuthMethod::BasicLocal,
            AuthMethod::ApiToken,
            AuthMethod::RobotToken,
            AuthMethod::OidcBearer,
        ] {
            let principal = Principal::new("u", UserSource::Local, method);
            assert_eq!(
                download_source_of(&principal),
                DownloadSource::Client,
                "{method:?}"
            );
        }
    }
}
