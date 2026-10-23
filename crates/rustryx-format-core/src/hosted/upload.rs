//! The upload and delete steps of the hosted pipeline.

use axum_core::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use rustryx_core::ids::BlobId;
use rustryx_core::model::{BlobRecord, NewAsset, NewComponent, WritePolicy};
use rustryx_core::ports::{BlobCommit, BlobKey, BlobStore};

use super::quota::{self, Budget};
use super::{OverwriteRule, UploadOutcome, UploadReservation, UploadSpec};
use crate::blobio::{BlobSink, SinkError, delete_orphan};
use crate::checksum::ExpectedChecksum;
use crate::context::{ContentEvent, FormatContext, OrphanCause};
use crate::error::FormatError;
use crate::metrics;
use crate::mime::resolve_content_type;
use crate::path::NormalizedPath;

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

    let restricted =
        check_write_policy(repository.write_policy, spec.overwrite, existing.is_some())?;
    // Only paths that must stay immutable need a reservation; plain overwrites may race freely.
    let reservation = if restricted {
        Some(
            cx.shared
                .reservations
                .try_reserve(repository.id, spec.path.as_str())
                .ok_or_else(|| {
                    FormatError::conflict("the path is being written by another request")
                })?,
        )
    } else {
        None
    };

    let replaced_size = existing.as_ref().map_or(0, |a| a.size_bytes);
    let budget = quota::budget(cx, replaced_size).await?;
    budget.check_declared(spec.content_length)?;

    let store = cx.store()?;
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
        store,
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
/// (and therefore reserved against concurrent writers).
fn check_write_policy(
    policy: WritePolicy,
    overwrite: OverwriteRule,
    exists: bool,
) -> Result<bool, FormatError> {
    if policy == WritePolicy::Deny {
        return Err(FormatError::Forbidden);
    }
    let immutable = policy == WritePolicy::AllowOnce || overwrite == OverwriteRule::Never;
    if immutable && exists {
        return Err(FormatError::conflict(
            "the path already exists and cannot be overwritten",
        ));
    }
    Ok(immutable)
}

/// The part of an upload that must run to completion once the body is in.
struct Finalize {
    cx: FormatContext,
    spec: UploadSpec,
    sink: BlobSink,
    store: std::sync::Arc<dyn BlobStore>,
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
            store,
            id,
            key,
            restricted,
            _reservation,
        } = self;
        let commit = match sink.commit().await {
            Ok(commit) => commit,
            Err(error) => return Err(error.into()),
        };
        match publish(&cx, &spec, &commit, id, &key).await {
            Ok(outcome) => {
                after_publish(&cx, &spec, &commit, &outcome, restricted).await;
                Ok(outcome)
            }
            Err(error) => {
                // Committed but not published: the object must not stay behind.
                delete_orphan(store.as_ref(), &key).await;
                if matches!(error, FormatError::DigestMismatch) {
                    cx.audit_content(
                        "asset.rejected",
                        target(&cx, &spec.path),
                        "failure",
                        serde_json::json!({ "reason": "digest_mismatch" }),
                    )
                    .await;
                }
                Err(error)
            }
        }
    }
}

async fn publish(
    cx: &FormatContext,
    spec: &UploadSpec,
    commit: &BlobCommit,
    id: BlobId,
    key: &BlobKey,
) -> Result<UploadOutcome, FormatError> {
    verify_expected(spec.expected.as_ref(), commit)?;
    let repository = cx.repository();
    let component_id = match &spec.component {
        Some(component) => Some(
            cx.ports()
                .components
                .find_or_create(&NewComponent {
                    repository_id: repository.id,
                    format: repository.format,
                    group_id: component.group.clone(),
                    name: component.name.clone(),
                    version: component.version.clone(),
                })
                .await?
                .id,
        ),
        None => None,
    };
    let new = NewAsset {
        repository_id: repository.id,
        component_id,
        path: spec.path.as_str().to_owned(),
        content_type: resolve_content_type(spec.content_type.as_deref(), spec.path.as_str()),
        blob: BlobRecord {
            id,
            blob_store_id: repository.blob_store_id,
            key: key.as_str().to_owned(),
            size_bytes: commit.size_bytes,
            checksums: commit.checksums.clone(),
            created_at: cx.now(),
        },
    };
    let outcome = cx.ports().assets.publish(&new).await?;
    Ok(UploadOutcome {
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
    let replaced = outcome.replaced.is_some();
    if restricted && replaced {
        // The in-process reservation cannot see another node. The overwrite is done;
        // all that is left is to make it loud.
        metrics::count(metrics::ALLOW_ONCE_VIOLATION_TOTAL);
        tracing::warn!(
            repository = %repository.name,
            "an immutable path was overwritten by a concurrent upload on another node"
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

fn target(cx: &FormatContext, path: &NormalizedPath) -> String {
    format!("{}:{}", cx.repository().name, path)
}

pub(super) async fn delete(
    cx: &FormatContext,
    path: &NormalizedPath,
) -> Result<BlobRecord, FormatError> {
    let repository = cx.repository();
    if repository.write_policy == WritePolicy::Deny {
        return Err(FormatError::Forbidden);
    }
    let removed = cx
        .ports()
        .assets
        .delete_by_path(repository.id, path.as_str())
        .await?
        .ok_or(FormatError::NotFound)?;
    cx.ports().events.emit(ContentEvent::BlobOrphaned {
        repository: repository.id,
        blob: removed.clone(),
        cause: OrphanCause::Deleted,
    });
    cx.audit_content(
        "asset.delete",
        target(cx, path),
        "success",
        serde_json::json!({ "size_bytes": removed.size_bytes }),
    )
    .await;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_policy_matrix() {
        use OverwriteRule::{FromRepositoryPolicy as Policy, Never};
        use WritePolicy::{Allow, AllowOnce, Deny};
        // (policy, rule, exists) -> Ok(immutable) | Err
        assert_eq!(check_write_policy(Allow, Policy, false).unwrap(), false);
        assert_eq!(check_write_policy(Allow, Policy, true).unwrap(), false);
        assert_eq!(check_write_policy(AllowOnce, Policy, false).unwrap(), true);
        assert!(matches!(
            check_write_policy(AllowOnce, Policy, true),
            Err(FormatError::Conflict(_))
        ));
        assert_eq!(check_write_policy(Allow, Never, false).unwrap(), true);
        assert!(matches!(
            check_write_policy(Allow, Never, true),
            Err(FormatError::Conflict(_))
        ));
        for exists in [false, true] {
            for rule in [Policy, Never] {
                assert!(matches!(
                    check_write_policy(Deny, rule, exists),
                    Err(FormatError::Forbidden)
                ));
            }
        }
    }
}
