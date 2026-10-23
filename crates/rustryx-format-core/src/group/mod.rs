//! Group repositories: an ordered union of other repositories.
//!
//! Two behaviours, chosen by [`ContentKind`]:
//!
//! * **Artifacts**: the first member that has the path answers, and its body is
//!   streamed through untouched. A group never buffers an artifact.
//! * **Index documents** (when the format provides a [`GroupIndexMerger`]): every
//!   authorized member is asked, the documents are read into memory under a shared
//!   cap (64 MiB in total) and merged on the blocking pool.
//!
//! Rules that hold for both:
//!
//! * **Authorization per member.** The caller needs rights on the group (checked by
//!   the engine) *and* on each member whose data is returned. A member the caller
//!   may not read is skipped silently, so its content cannot leak through the
//!   group, and its existence is indistinguishable from a 404.
//! * **A member's 404 is discarded**, and so are its 401 and 403: a group never
//!   forwards a member's (or an upstream's) authentication errors.
//! * **A member's 5xx is skipped but remembered.** If no member has the content and
//!   at least one failed, the group answers with that failure (502/504/503), not
//!   404: "not found" would be a lie when a member that may have it was down.
//! * **Name claims** protect against dependency confusion, see [`GroupNameClaimer`].
//! * Nested groups are resolved with a depth limit and cycle detection.

mod claim;
mod merge;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum_core::body::Body;
use bytes::{Bytes, BytesMut};
use futures::{StreamExt, stream};
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoType, Repository};
use rustryx_core::ports::{Decision, ResourceRef};
use sha2::{Digest, Sha256};

pub use claim::{GroupNameClaimer, NameClaim};
pub use merge::{GroupIndexMerger, MemberDocument, MemberRef, MergeInput, MergedDocument};

use crate::conditional::if_none_match_hits;
use crate::context::{FormatContext, RoutingDecision};
use crate::error::{FormatError, UpstreamFailureKind};
use crate::metrics;
use crate::request::{ArtifactRequest, ContentKind};

/// Header carrying the number of members that failed or could not be consulted.
/// Present only when the answer may be incomplete.
pub const X_RUSTRYX_GROUP_SKIPPED: http::HeaderName =
    http::HeaderName::from_static("x-rustryx-group-skipped");

/// Validators and range headers that refer to one member's representation. They must
/// not be forwarded when documents of several members are merged: a member would
/// answer 304 for a document that is only one ingredient of the result.
const REPRESENTATION_HEADERS: [http::HeaderName; 6] = [
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::IF_MATCH,
    header::IF_UNMODIFIED_SINCE,
    header::IF_RANGE,
    header::RANGE,
];

/// A member that passed authorization, routing and claim filtering.
struct Candidate {
    repository: Arc<Repository>,
    holds_claim: bool,
}

/// What asking one member produced.
enum MemberOutcome {
    /// The member has it.
    Hit(Response<Body>),
    /// The member does not have it, or the caller may not see it.
    Miss,
    /// A client error that applies to the request itself (400, 412, 416): returned as is.
    Terminal(Result<Response<Body>, FormatError>),
    /// The member is broken.
    Failed(FormatError),
}

/// Group operations of a [`FormatContext`].
pub struct GroupPipeline<'a> {
    cx: &'a FormatContext,
}

impl<'a> GroupPipeline<'a> {
    pub(crate) fn new(cx: &'a FormatContext) -> Self {
        Self { cx }
    }

    /// Resolves a read against the members of the group.
    ///
    /// Cancel safe: no state outlives the call except member fills, which are
    /// independent tasks (see the proxy pipeline).
    ///
    /// # Errors
    /// `MethodNotAllowed` for anything but `GET` and `HEAD`, `NotFound` when no
    /// member has the content, the first member failure when none has it and one
    /// failed, `Forbidden` when an authorization lookup failed (fail closed),
    /// `IndexTooLarge` when a merge would exceed the memory cap.
    pub async fn resolve(&self, req: &ArtifactRequest) -> Result<Response<Body>, FormatError> {
        let cx = self.cx;
        if !req.head.is_read_method() {
            return Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" });
        }
        let group = cx.repository();
        let handler = cx
            .shared
            .registry
            .get(group.format)
            .ok_or_else(|| FormatError::internal("no handler registered for the group's format"))?
            .clone();
        let members = self.load_members().await?;
        let claim = handler.group_claims().and_then(|c| c.claim_for(req));
        let held = match &claim {
            Some(claim) => claim::holders(cx, &members, claim).await,
            None => vec![false; members.len()],
        };
        let (candidates, lookup_failures) = self.filter(req, &members, &held).await;
        let merge = req.kind == ContentKind::Index && handler.group_merge().is_some();
        if merge {
            self.merge(req, candidates, claim, lookup_failures).await
        } else {
            self.first_hit(req, candidates, lookup_failures).await
        }
    }

    /// Looks the members up and drops those that cannot take part: unknown, offline,
    /// of another format, or already being resolved (cycle).
    async fn load_members(&self) -> Result<Vec<Arc<Repository>>, FormatError> {
        let cx = self.cx;
        let group = cx.repository();
        let limits = cx.limits();
        if cx.trail().len() >= limits.max_group_depth {
            return Err(FormatError::internal("group nesting is too deep"));
        }
        let config = group
            .group
            .as_ref()
            .ok_or_else(|| FormatError::internal("group repository without members"))?;
        let lookups = config
            .members
            .iter()
            .take(limits.max_group_members)
            .map(|name| cx.ports().repositories.get_by_name(name));
        let found = futures::future::join_all(lookups).await;
        let mut members = Vec::with_capacity(found.len());
        for result in found {
            let Some(member) = result? else {
                metrics::group_member_skipped("missing");
                continue;
            };
            if member.format != group.format {
                metrics::group_member_skipped("format");
                continue;
            }
            if !member.online {
                metrics::group_member_skipped("offline");
                continue;
            }
            if member.id == group.id || cx.trail().contains(&member.id) {
                metrics::group_member_skipped("cycle");
                continue;
            }
            members.push(Arc::new(member));
        }
        Ok(members)
    }

    /// Applies claims, routing rules and authorization. Returns the candidates and
    /// the number of members that could not be judged (a failing lookup).
    async fn filter(
        &self,
        req: &ArtifactRequest,
        members: &[Arc<Repository>],
        held: &[bool],
    ) -> (Vec<Candidate>, usize) {
        let mut candidates = Vec::new();
        let mut failures = 0;
        let mut claimed = false;
        for (member, holds_claim) in members.iter().zip(held) {
            if claimed && member.repo_type != RepoType::Hosted {
                metrics::group_member_skipped("claimed");
                continue;
            }
            // The claim stands even when the caller may not read the claiming member.
            claimed |= *holds_claim;
            match self.authorize(member, req).await {
                Ok(true) => candidates.push(Candidate {
                    repository: Arc::clone(member),
                    holds_claim: *holds_claim,
                }),
                Ok(false) => {}
                Err(()) => failures += 1,
            }
        }
        (candidates, failures)
    }

    /// `Ok(true)` when the caller may read from the member, `Ok(false)` to skip it,
    /// `Err(())` when the decision could not be made (fail closed, counted as a failure).
    async fn authorize(&self, member: &Repository, req: &ArtifactRequest) -> Result<bool, ()> {
        let cx = self.cx;
        let principal = cx.principal();
        if principal.is_anonymous() && !member.allow_anonymous {
            metrics::group_member_skipped("denied");
            return Ok(false);
        }
        match cx.ports().routing.check(member, req).await {
            Ok(RoutingDecision::Allow) => {}
            Ok(RoutingDecision::Block) => {
                metrics::group_member_skipped("routing");
                return Ok(false);
            }
            Err(error) => {
                tracing::warn!(%error, member = %member.name, "routing check failed; skipping member");
                return Err(());
            }
        }
        let mut resource = ResourceRef::for_repository(member, req.path.as_str());
        if let Some(coordinate) = &req.coordinate {
            resource = resource.with_coordinate(coordinate.clone());
        }
        let action = if req.action == Action::Browse {
            Action::Browse
        } else {
            Action::Read
        };
        match cx.ports().rbac.check(principal, action, &resource).await {
            Ok(Decision::Allow) => Ok(true),
            Ok(Decision::Deny) => {
                metrics::group_member_skipped("denied");
                Ok(false)
            }
            Err(error) => {
                tracing::warn!(%error, member = %member.name, "authorization lookup failed; skipping member (fail closed)");
                Err(())
            }
        }
    }

    async fn call_member(&self, member: &Arc<Repository>, req: &ArtifactRequest) -> MemberOutcome {
        let Some(handler) = self.cx.shared.registry.get(member.format).cloned() else {
            return MemberOutcome::Failed(FormatError::internal(
                "no handler for a member's format",
            ));
        };
        let member_cx = self.cx.for_member(Arc::clone(member));
        match handler.handle(member_cx, req.clone(), Body::empty()).await {
            Ok(response) => classify_response(response),
            Err(error) => classify_error(error),
        }
    }

    async fn first_hit(
        &self,
        req: &ArtifactRequest,
        candidates: Vec<Candidate>,
        lookup_failures: usize,
    ) -> Result<Response<Body>, FormatError> {
        let mut first_failure: Option<FormatError> = None;
        let mut failed = lookup_failures;
        for candidate in candidates {
            match self.call_member(&candidate.repository, req).await {
                MemberOutcome::Hit(mut response) => {
                    mark_skipped(response.headers_mut(), failed);
                    return Ok(response);
                }
                MemberOutcome::Terminal(result) => return result,
                MemberOutcome::Miss => {}
                MemberOutcome::Failed(error) => {
                    metrics::group_member_skipped("failed");
                    failed += 1;
                    first_failure.get_or_insert(error);
                }
            }
        }
        Err(no_hit_error(first_failure, lookup_failures))
    }

    async fn merge(
        &self,
        req: &ArtifactRequest,
        candidates: Vec<Candidate>,
        claim: Option<NameClaim>,
        lookup_failures: usize,
    ) -> Result<Response<Body>, FormatError> {
        let cx = self.cx;
        let member_req = merge_request(req);
        let cap = cx.limits().group_merge_cap_bytes;
        // One counter across all members: the memory held by a merge is bounded by
        // the cap however many members answer at once.
        let consumed = Arc::new(AtomicU64::new(0));
        let results: Vec<_> = stream::iter(candidates)
            .map(|candidate| {
                let consumed = Arc::clone(&consumed);
                let member_req = &member_req;
                async move {
                    let outcome = self.call_member(&candidate.repository, member_req).await;
                    self.read_document(candidate, outcome, &consumed, cap).await
                }
            })
            .buffered(cx.limits().group_fanout.max(1))
            .collect()
            .await;

        let mut documents = Vec::new();
        let mut first_failure: Option<FormatError> = None;
        let mut failed = lookup_failures;
        for result in results {
            match result {
                DocumentResult::Document(document) => documents.push(document),
                DocumentResult::Miss => {}
                DocumentResult::Terminal(result) => return result,
                DocumentResult::Failed(error) => {
                    metrics::group_member_skipped("failed");
                    failed += 1;
                    first_failure.get_or_insert(error);
                }
            }
        }
        if documents.is_empty() {
            return Err(no_hit_error(first_failure, lookup_failures));
        }
        let merged = self.run_merge(req, documents, claim).await?;
        Ok(merged_response(req, &merged, failed))
    }

    /// Turns a member outcome into a bounded in-memory document.
    async fn read_document(
        &self,
        candidate: Candidate,
        outcome: MemberOutcome,
        consumed: &AtomicU64,
        cap: u64,
    ) -> DocumentResult {
        match outcome {
            MemberOutcome::Miss => DocumentResult::Miss,
            MemberOutcome::Terminal(result) => DocumentResult::Terminal(result),
            MemberOutcome::Failed(error) => DocumentResult::Failed(error),
            MemberOutcome::Hit(response) => {
                let content_type = response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let mut data = response.into_body().into_data_stream();
                let mut buffer = BytesMut::new();
                while let Some(chunk) = data.next().await {
                    let Ok(chunk) = chunk else {
                        return DocumentResult::Failed(FormatError::upstream(
                            UpstreamFailureKind::Unreachable,
                            "a member's document could not be read",
                        ));
                    };
                    let total = consumed.fetch_add(chunk.len() as u64, Ordering::SeqCst)
                        + chunk.len() as u64;
                    if total > cap {
                        return DocumentResult::Terminal(Err(FormatError::IndexTooLarge {
                            limit: cap,
                        }));
                    }
                    buffer.extend_from_slice(&chunk);
                }
                DocumentResult::Document(MemberDocument {
                    member: MemberRef {
                        id: candidate.repository.id,
                        name: candidate.repository.name.clone(),
                        repo_type: candidate.repository.repo_type,
                    },
                    content_type,
                    body: buffer.freeze(),
                    holds_claim: candidate.holds_claim,
                })
            }
        }
    }

    /// Runs the format's merger on the blocking pool. The permit is taken before the
    /// spawn; without one in time the client gets 503 and the queue does not grow.
    async fn run_merge(
        &self,
        req: &ArtifactRequest,
        documents: Vec<MemberDocument>,
        claim: Option<NameClaim>,
    ) -> Result<MergedDocument, FormatError> {
        let cx = self.cx;
        let permit = tokio::time::timeout(
            cx.limits().blocking_permit_wait,
            Arc::clone(&cx.shared.blocking).acquire_owned(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(FormatError::Unavailable {
            reason: "the server is busy merging indexes, retry",
            retry_after: Some(std::time::Duration::from_secs(1)),
        })?;
        let handler = cx
            .shared
            .registry
            .get(cx.repository().format)
            .cloned()
            .ok_or_else(|| FormatError::internal("no handler registered for the group's format"))?;
        let request = req.clone();
        let group = cx.repository().clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let merger = handler
                .group_merge()
                .ok_or_else(|| FormatError::internal("the format has no index merger"))?;
            merger.merge(MergeInput {
                request: &request,
                group: &group,
                documents,
                claim,
            })
        })
        .await
        .map_err(|e| FormatError::internal(format!("index merge task failed: {e}")))?
    }
}

enum DocumentResult {
    Document(MemberDocument),
    Miss,
    Terminal(Result<Response<Body>, FormatError>),
    Failed(FormatError),
}

/// The request as members see it during a merge: always a `GET`, without validators
/// or ranges of the client, which refer to the merged result and not to one member.
fn merge_request(req: &ArtifactRequest) -> ArtifactRequest {
    let mut head = (*req.head).clone();
    head.method = Method::GET;
    for name in &REPRESENTATION_HEADERS {
        head.headers.remove(name);
    }
    ArtifactRequest {
        head: Arc::new(head),
        ..req.clone()
    }
}

/// What the group answers when no member had the content.
fn no_hit_error(first_failure: Option<FormatError>, lookup_failures: usize) -> FormatError {
    match first_failure {
        Some(error) => error,
        // A lookup that failed closed may have hidden a member that has the content.
        None if lookup_failures > 0 => FormatError::Forbidden,
        None => FormatError::NotFound,
    }
}

fn mark_skipped(headers: &mut HeaderMap, failed: usize) {
    if failed > 0 {
        headers.insert(X_RUSTRYX_GROUP_SKIPPED, HeaderValue::from(failed as u64));
    }
}

/// Maps a member's response status to what the group does with it.
fn classify_response(response: Response<Body>) -> MemberOutcome {
    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        return MemberOutcome::Hit(response);
    }
    match status.as_u16() {
        // Absent, hidden or unauthorized at the member: the group never forwards these.
        401 | 403 | 404 | 405 | 410 => MemberOutcome::Miss,
        // The request itself is wrong (bad range, failed precondition, malformed): the
        // next member would say the same.
        400..=499
            if status != StatusCode::TOO_MANY_REQUESTS && status != StatusCode::REQUEST_TIMEOUT =>
        {
            MemberOutcome::Terminal(Ok(response))
        }
        _ => MemberOutcome::Failed(FormatError::upstream(
            UpstreamFailureKind::BadStatus,
            format!("a group member answered {status}"),
        )),
    }
}

fn classify_error(error: FormatError) -> MemberOutcome {
    let status = error.status();
    match status.as_u16() {
        401 | 403 | 404 | 405 | 410 => MemberOutcome::Miss,
        400..=499
            if status != StatusCode::TOO_MANY_REQUESTS && status != StatusCode::REQUEST_TIMEOUT =>
        {
            MemberOutcome::Terminal(Err(error))
        }
        _ => MemberOutcome::Failed(error),
    }
}

fn merged_response(
    req: &ArtifactRequest,
    merged: &MergedDocument,
    failed: usize,
) -> Response<Body> {
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(&merged.body)));
    let mut response = if if_none_match_hits(&req.head.headers, &etag) {
        let mut not_modified = Response::new(Body::empty());
        *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
        not_modified
    } else {
        let length = merged.body.len() as u64;
        let body = if req.head.method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(Bytes::clone(&merged.body))
        };
        let mut ok = Response::new(body);
        let headers = ok.headers_mut();
        if let Ok(value) = HeaderValue::from_str(&merged.content_type) {
            headers.insert(header::CONTENT_TYPE, value);
        }
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        ok
    };
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    if failed > 0 {
        mark_skipped(headers, failed);
        // An incomplete merge must not be cached by clients or shared caches.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16) -> Response<Body> {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::from_u16(status).unwrap();
        r
    }

    #[test]
    fn member_response_classification() {
        for code in [200u16, 206, 304] {
            assert!(
                matches!(classify_response(response(code)), MemberOutcome::Hit(_)),
                "{code}"
            );
        }
        for code in [401u16, 403, 404, 405, 410] {
            assert!(
                matches!(classify_response(response(code)), MemberOutcome::Miss),
                "{code}"
            );
        }
        for code in [400u16, 412, 416, 422] {
            assert!(
                matches!(
                    classify_response(response(code)),
                    MemberOutcome::Terminal(Ok(_))
                ),
                "{code}"
            );
        }
        for code in [429u16, 408, 500, 502, 503, 504, 301] {
            assert!(
                matches!(classify_response(response(code)), MemberOutcome::Failed(_)),
                "{code}"
            );
        }
    }

    #[test]
    fn member_error_classification() {
        assert!(matches!(
            classify_error(FormatError::NotFound),
            MemberOutcome::Miss
        ));
        assert!(matches!(
            classify_error(FormatError::Forbidden),
            MemberOutcome::Miss
        ));
        assert!(matches!(
            classify_error(FormatError::Unauthenticated),
            MemberOutcome::Miss
        ));
        assert!(matches!(
            classify_error(FormatError::RangeNotSatisfiable { total: 1 }),
            MemberOutcome::Terminal(Err(_))
        ));
        assert!(matches!(
            classify_error(FormatError::upstream(UpstreamFailureKind::Timeout, "t")),
            MemberOutcome::Failed(_)
        ));
        assert!(matches!(
            classify_error(FormatError::Unavailable {
                reason: "x",
                retry_after: None
            }),
            MemberOutcome::Failed(_)
        ));
        assert!(matches!(
            classify_error(FormatError::TooManyRequests {
                retry_after: std::time::Duration::from_secs(1)
            }),
            MemberOutcome::Failed(_)
        ));
    }

    #[test]
    fn must_not_forward_a_members_auth_failure() {
        // 401 and 403 from a member must never reach the client through the group.
        for code in [401u16, 403] {
            assert!(matches!(
                classify_response(response(code)),
                MemberOutcome::Miss
            ));
        }
        for error in [FormatError::Unauthenticated, FormatError::Forbidden] {
            assert!(matches!(classify_error(error), MemberOutcome::Miss));
        }
    }

    #[test]
    fn no_hit_error_prefers_failures_then_fail_closed_then_not_found() {
        assert!(matches!(
            no_hit_error(
                Some(FormatError::upstream(UpstreamFailureKind::Timeout, "")),
                0
            ),
            FormatError::Upstream { .. }
        ));
        assert!(matches!(no_hit_error(None, 1), FormatError::Forbidden));
        assert!(matches!(no_hit_error(None, 0), FormatError::NotFound));
    }

    #[test]
    fn skipped_marker_only_when_something_was_skipped() {
        let mut headers = HeaderMap::new();
        mark_skipped(&mut headers, 0);
        assert!(headers.is_empty());
        mark_skipped(&mut headers, 2);
        assert_eq!(headers.get(&X_RUSTRYX_GROUP_SKIPPED).unwrap(), "2");
    }
}
