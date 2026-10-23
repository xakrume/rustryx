//! The upload and delete steps of the hosted pipeline.

use axum_core::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use rustryx_core::audit::ActorRef;
use rustryx_core::error::{MutationError, PolicyViolation};
use rustryx_core::ids::BlobId;
use rustryx_core::model::{
    BlobRecord, NewAsset, NewComponent, PublishMode, WriteOptions, WritePolicy,
};
use rustryx_core::ports::{BlobCommit, BlobKey};

use super::quota::{self, Budget};
use super::{OverwriteRule, UploadOutcome, UploadReservation, UploadSpec};
use crate::blobio::{BlobSink, SinkError, discard_committed};
use crate::checksum::ExpectedChecksum;
use crate::context::{ContentEvent, FormatContext, OrphanCause};
use crate::error::FormatError;
use crate::metrics;
use crate::mime::resolve_content_type;
use crate::path::NormalizedPath;
use crate::policy::{DeleteIntent, WriteIntent};

pub(super) async fn run(
    cx: &FormatContext,
    spec: UploadSpec,
    body: Body,
) -> Result<UploadOutcome, FormatError> {
    let format = cx.repository().format;
    let result = run_inner(cx, spec, body).await;
    match &result {
        Ok(_) => metrics::upload_result(format, "ok"),
        Err(error) => metrics::upload_result(format, error.code()),
    }
    result
}

async fn run_inner(
    cx: &FormatContext,
    spec: UploadSpec,
    body: Body,
) -> Result<UploadOutcome, FormatError> {
    let repository = cx.repository();
    let existing = cx
        .ports()
        .assets
        .get_by_path(repository.id, spec.path.as_str())
        .await?;

    let restricted = restricted_for(cx, &spec, existing.is_some())?;
    precheck_write(cx, &spec, existing.as_ref(), spec.content_length).await?;
    let reservation = reserve(cx, &spec, restricted)?;

    let replaced_size = existing.as_ref().map_or(0, |a| a.size_bytes);
    let budget = quota::budget(cx, replaced_size).await?;
    budget.check_declared(spec.content_length)?;

    let store = cx.store().await?;
    let id = BlobId::from_uuid(cx.ports().ids.next_uuid());
    let key = BlobKey::new(id.to_string());
    // A client that declared a length gets no more room than that: a body longer than its
    // own Content-Length is refused as soon as it crosses it, not after it was stored.
    let limit = spec
        .content_length
        .map_or(budget.bytes, |d| d.min(budget.bytes));
    let mut sink = BlobSink::begin(store.as_ref(), key.clone(), spec.content_length, limit).await?;

    if let Err(error) = pump(cx, &mut sink, body, &budget, spec.content_length).await {
        sink.abort().await;
        return Err(error);
    }

    let job = Finalize {
        cx: cx.clone(),
        spec,
        sink,
        id,
        key,
        restricted,
        _reservation: reservation,
    };
    // Commit and publish must not be cut in half by a dropped request future: a
    // committed blob without its database row is an object nobody can ever collect.
    // The context's tracker owns the task, so shutdown can still wait for it.
    match cx.tasks().spawn(job.run()).await {
        Ok(result) => result,
        Err(join_error) => Err(FormatError::internal(format!(
            "upload finalizer failed: {join_error}"
        ))),
    }
}

/// Applies the write policy to this upload: whether the path is immutable (and so written
/// create-only), or the refusal.
pub(super) fn restricted_for(
    cx: &FormatContext,
    spec: &UploadSpec,
    exists: bool,
) -> Result<bool, FormatError> {
    check_write_policy(cx.repository().write_policy, spec.overwrite, exists)
        .map_err(|violation| counted(cx, violation))
}

/// Refuses every write to a repository whose policy is `deny`.
pub(super) fn deny_writes(cx: &FormatContext) -> Result<(), FormatError> {
    if cx.repository().write_policy == WritePolicy::Deny {
        return Err(counted(cx, PolicyViolation::WritePolicy(WritePolicy::Deny)));
    }
    Ok(())
}

/// Reserves the path of an immutable upload on this node. Only paths that must stay
/// immutable need it; plain overwrites may race freely.
///
/// The reservation is an optimisation, not the guarantee: it turns the second of two
/// concurrent uploads on one node into an immediate 409 before its body is read. Across
/// nodes the guarantee is the create-only publish, which the database decides.
pub(super) fn reserve(
    cx: &FormatContext,
    spec: &UploadSpec,
    restricted: bool,
) -> Result<Option<UploadReservation>, FormatError> {
    if !restricted {
        return Ok(None);
    }
    cx.shared
        .reservations
        .try_reserve(cx.repository().id, spec.path.as_str())
        .map(Some)
        .ok_or_else(|| FormatError::conflict("the path is being written by another request"))
}

/// Runs the policy hook of the pipeline before the body is read. A refusal costs no byte and
/// no staged blob; an undecidable hook fails the request closed.
pub(super) async fn precheck_write(
    cx: &FormatContext,
    spec: &UploadSpec,
    existing: Option<&rustryx_core::model::Asset>,
    declared_length: Option<u64>,
) -> Result<(), FormatError> {
    let mut intent = WriteIntent::new(cx.repository(), cx.principal(), &spec.path);
    intent.declared_length = declared_length;
    intent.existing = existing;
    intent.component = spec.component.as_ref();
    match cx.ports().policy.check_write(&intent).await {
        Ok(()) => Ok(()),
        Err(error) => Err(refused(cx, &spec.path, error).await),
    }
}

/// Counts and audits a refusal and turns it into the client error. Every refusal of a hook or of
/// the transaction goes through here, so `policy_violations_total{reason}` and the
/// `asset.rejected` audit record have one definition. A failure of the store behind the hook is
/// not a refusal and is neither counted nor audited as one.
pub(super) async fn refused(
    cx: &FormatContext,
    path: &NormalizedPath,
    error: MutationError,
) -> FormatError {
    if let MutationError::Violation(violation) = &error {
        metrics::policy_violation(cx.repository().format, violation.label());
        cx.audit_rejection(target(cx, path), violation).await;
    }
    error.into()
}

fn counted(cx: &FormatContext, violation: PolicyViolation) -> FormatError {
    // The routine refusals (`deny`, `allow_once`, a taken path) are counted, not audited: a
    // client that retries an immutable upload would otherwise fill the audit log.
    metrics::policy_violation(cx.repository().format, violation.label());
    violation.into()
}

/// The options of a state changing port call made for this request: who acts and which
/// request, for the events and the audit batch row, and the publish mode.
pub(super) fn write_options(cx: &FormatContext, restricted: bool) -> WriteOptions {
    let mut options = WriteOptions::new()
        .with_actor(ActorRef::of(cx.principal()))
        .with_mode(if restricted {
            PublishMode::CreateOnly
        } else {
            PublishMode::Upsert
        });
    if let Some(request_id) = cx.request_id() {
        options = options.with_request_id(request_id);
    }
    options
}

/// Moves the request body into the sink, chunk by chunk.
async fn pump(
    cx: &FormatContext,
    sink: &mut BlobSink,
    body: Body,
    budget: &Budget,
    content_length: Option<u64>,
) -> Result<(), FormatError> {
    let idle = cx.limits().body_idle_timeout;
    let mut stream = body.into_data_stream();
    loop {
        let next = tokio::time::timeout(idle, stream.next())
            .await
            .map_err(|_| FormatError::RequestTimeout)?;
        let Some(item) = next else { break };
        let chunk: Bytes =
            item.map_err(|_| FormatError::bad_request("request body could not be read"))?;
        sink.write(chunk)
            .await
            .map_err(|e| map_sink_error(e, budget, content_length))?;
    }
    if content_length.is_some_and(|declared| declared != sink.written()) {
        return Err(FormatError::bad_request(
            "body length does not match Content-Length",
        ));
    }
    Ok(())
}

fn map_sink_error(error: SinkError, budget: &Budget, content_length: Option<u64>) -> FormatError {
    match error {
        // The declared length fitted the budget (it was checked up front), so the sink
        // limit that was crossed is the declared length itself.
        SinkError::LimitExceeded { .. } if content_length.is_some_and(|d| d <= budget.bytes) => {
            FormatError::bad_request("body is longer than Content-Length")
        }
        SinkError::LimitExceeded { .. } => budget.exceeded(),
        SinkError::Blob(e) => FormatError::Blob(e),
    }
}

/// Applies the write policy. Returns whether the path must be treated as immutable
/// (and therefore written create-only and reserved against concurrent writers on this node).
///
/// This is the cheap look at the state the request saw; the transaction of `publish_with`
/// decides again, so a path that appeared in between is still refused.
fn check_write_policy(
    policy: WritePolicy,
    overwrite: OverwriteRule,
    exists: bool,
) -> Result<bool, PolicyViolation> {
    if policy == WritePolicy::Deny {
        return Err(PolicyViolation::WritePolicy(WritePolicy::Deny));
    }
    let immutable = policy == WritePolicy::AllowOnce || overwrite == OverwriteRule::Never;
    if immutable && exists {
        return Err(if policy == WritePolicy::AllowOnce {
            PolicyViolation::WritePolicy(WritePolicy::AllowOnce)
        } else {
            PolicyViolation::AlreadyExists
        });
    }
    Ok(immutable)
}

/// The part of an upload that must run to completion once the body is in.
struct Finalize {
    cx: FormatContext,
    spec: UploadSpec,
    sink: BlobSink,
    id: BlobId,
    key: BlobKey,
    restricted: bool,
    _reservation: Option<UploadReservation>,
}

impl Finalize {
    async fn run(self) -> Result<UploadOutcome, FormatError> {
        let Self {
            cx,
            spec,
            sink,
            id,
            key,
            restricted,
            _reservation,
        } = self;
        let commit = match sink.commit().await {
            Ok(commit) => commit,
            Err(error) => return Err(error.into()),
        };
        publish_committed(&cx, &spec, &commit, id, &key, restricted).await
    }
}

/// Publishes a blob that is already committed, or removes it. Shared by the single request
/// upload and the finalize step of a chunked upload, so both end the same way.
pub(super) async fn publish_committed(
    cx: &FormatContext,
    spec: &UploadSpec,
    commit: &BlobCommit,
    id: BlobId,
    key: &BlobKey,
    restricted: bool,
) -> Result<UploadOutcome, FormatError> {
    match publish(cx, spec, commit, id, key, restricted).await {
        Ok(outcome) => {
            after_publish(cx, spec, commit, &outcome, restricted).await;
            Ok(outcome)
        }
        Err(error) => {
            // Committed but not published: the object must not stay behind. A refusal of the
            // transaction (create-only found the path taken, a guard said no) lands here too,
            // and leaves no asset, no blob row and no outbox row behind by contract.
            discard_committed(cx, commit, key).await;
            if matches!(error, FormatError::DigestMismatch) {
                cx.audit_content(
                    "asset.rejected",
                    target(cx, &spec.path),
                    "failure",
                    serde_json::json!({ "reason": "digest_mismatch" }),
                )
                .await;
            }
            Err(error)
        }
    }
}

async fn publish(
    cx: &FormatContext,
    spec: &UploadSpec,
    commit: &BlobCommit,
    id: BlobId,
    key: &BlobKey,
    restricted: bool,
) -> Result<UploadOutcome, FormatError> {
    verify_expected(spec.expected.as_ref(), commit)?;
    let repository = cx.repository();
    let component_id = match &spec.component {
        Some(component) => {
            let mut new = NewComponent::new(repository.id, repository.format, &component.name);
            new.group_id.clone_from(&component.group);
            new.version.clone_from(&component.version);
            new.created_by = cx.principal().user_id;
            Some(cx.ports().components.find_or_create(&new).await?.id)
        }
        None => None,
    };
    let mut new = NewAsset::new(
        repository.id,
        spec.path.as_str(),
        resolve_content_type(spec.content_type.as_deref(), spec.path.as_str()),
        BlobRecord {
            id,
            // The member that took the bytes, when the store says so: the row must name the
            // concrete store, otherwise a read would have to probe the members of a group.
            blob_store_id: cx.blob_store_id_of(commit),
            key: key.as_str().to_owned(),
            size_bytes: commit.size_bytes,
            checksums: commit.checksums.clone(),
            created_at: cx.now(),
        },
    );
    new.component_id = component_id;
    // The uploader id, never the client address (the audit record carries that).
    new.created_by = cx.principal().user_id;
    let outcome = match cx
        .ports()
        .assets
        .publish_with(&new, &write_options(cx, restricted))
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => return Err(refused(cx, &spec.path, error).await),
    };
    Ok(UploadOutcome {
        created: outcome.created,
        asset: outcome.asset,
        replaced: outcome.replaced,
    })
}

fn verify_expected(
    expected: Option<&ExpectedChecksum>,
    commit: &BlobCommit,
) -> Result<(), FormatError> {
    match expected {
        Some(expected) if !expected.matches(&commit.checksums) => Err(FormatError::DigestMismatch),
        _ => Ok(()),
    }
}

async fn after_publish(
    cx: &FormatContext,
    spec: &UploadSpec,
    commit: &BlobCommit,
    outcome: &UploadOutcome,
    restricted: bool,
) {
    let repository = cx.repository();
    let replaced = !outcome.created;
    if restricted && replaced {
        // Create-only publishes cannot replace, on any node: the transaction answers
        // `AlreadyExists`. Reaching this line means a port ignored `PublishMode::CreateOnly`,
        // so it is a tripwire for a broken implementation, and it must stay at zero.
        metrics::count(metrics::ALLOW_ONCE_VIOLATION_TOTAL);
        tracing::error!(
            repository = %repository.name,
            "an immutable path was replaced although the publish was create-only; the metadata port ignores the mode"
        );
    }
    if let Some(blob) = &outcome.replaced {
        cx.ports().events.emit(ContentEvent::BlobOrphaned {
            repository: repository.id,
            blob: blob.clone(),
            cause: OrphanCause::Replaced,
        });
    }
    cx.ports().events.emit(ContentEvent::AssetPublished {
        repository: repository.id,
        asset: outcome.asset.clone(),
        replaced,
    });
    metrics::upload_bytes(repository.format, commit.size_bytes);
    cx.audit_content(
        if replaced {
            "asset.overwrite"
        } else {
            "asset.publish"
        },
        target(cx, &spec.path),
        "success",
        serde_json::json!({
            "size_bytes": commit.size_bytes,
            "sha256": commit.checksums.sha256,
            "replaced": replaced,
        }),
    )
    .await;
}

pub(super) fn target(cx: &FormatContext, path: &NormalizedPath) -> String {
    format!("{}:{}", cx.repository().name, path)
}

/// Deletes the asset at `path`. Returns the blob that lost its last reference, `None` when
/// the blob is still referenced by another asset (a linked copy).
pub(super) async fn delete(
    cx: &FormatContext,
    path: &NormalizedPath,
) -> Result<Option<BlobRecord>, FormatError> {
    let repository = cx.repository();
    deny_writes(cx)?;
    let asset = cx
        .ports()
        .assets
        .get_by_path(repository.id, path.as_str())
        .await?
        .ok_or(FormatError::NotFound)?;
    if let Err(error) = cx
        .ports()
        .policy
        .check_delete(&DeleteIntent::new(repository, cx.principal(), path, &asset))
        .await
    {
        return Err(refused(cx, path, error).await);
    }
    // The transaction decides again (immutability, hold, read only mode): the hook above only
    // refuses early. `None` here is not "not found": the asset existed a moment ago, so it is
    // either gone through a racing delete or its blob is still referenced elsewhere.
    let removed = match cx
        .ports()
        .assets
        .delete_by_path_checked(repository.id, path.as_str(), &write_options(cx, false))
        .await
    {
        Ok(removed) => removed,
        Err(error) => return Err(refused(cx, path, error).await),
    };
    if let Some(blob) = &removed {
        cx.ports().events.emit(ContentEvent::BlobOrphaned {
            repository: repository.id,
            blob: blob.clone(),
            cause: OrphanCause::Deleted,
        });
    }
    cx.audit_content(
        "asset.delete",
        target(cx, path),
        "success",
        serde_json::json!({ "size_bytes": asset.size_bytes }),
    )
    .await;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a client sees: the violation converted the way the pipeline converts it.
    fn check(policy: WritePolicy, rule: OverwriteRule, exists: bool) -> Result<bool, FormatError> {
        check_write_policy(policy, rule, exists).map_err(FormatError::from)
    }

    #[test]
    fn write_policy_matrix() {
        use OverwriteRule::{FromRepositoryPolicy as Policy, Never};
        use WritePolicy::{Allow, AllowOnce, Deny};
        // (policy, rule, exists) -> Ok(immutable) | Err
        assert_eq!(check(Allow, Policy, false).unwrap(), false);
        assert_eq!(check(Allow, Policy, true).unwrap(), false);
        assert_eq!(check(AllowOnce, Policy, false).unwrap(), true);
        assert!(matches!(
            check(AllowOnce, Policy, true),
            Err(FormatError::Conflict(_))
        ));
        assert_eq!(check(Allow, Never, false).unwrap(), true);
        assert!(matches!(
            check(Allow, Never, true),
            Err(FormatError::Conflict(_))
        ));
        for exists in [false, true] {
            for rule in [Policy, Never] {
                assert!(matches!(
                    check(Deny, rule, exists),
                    Err(FormatError::Forbidden)
                ));
            }
        }
    }

    #[test]
    fn the_pre_check_names_the_violation_the_transaction_would_name() {
        use OverwriteRule::{FromRepositoryPolicy as Policy, Never};
        use WritePolicy::{Allow, AllowOnce, Deny};
        assert_eq!(
            check_write_policy(AllowOnce, Policy, true),
            Err(PolicyViolation::WritePolicy(AllowOnce))
        );
        assert_eq!(
            check_write_policy(Allow, Never, true),
            Err(PolicyViolation::AlreadyExists)
        );
        assert_eq!(
            check_write_policy(Deny, Policy, false),
            Err(PolicyViolation::WritePolicy(Deny))
        );
    }
}
