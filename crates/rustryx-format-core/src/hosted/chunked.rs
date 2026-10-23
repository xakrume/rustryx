//! Chunked uploads that span requests and nodes (OCI blob uploads: `POST`, `PATCH`..., `PUT`).
//!
//! This is the pipeline hook for `BlobStore::resumable`, the contract of core revision 2
//! (gap 29): each chunk is staged as its own object, `finalize` joins and hashes them and
//! commits the result. A staged chunk lives in the blob store, not in this process, so any
//! node can take the next `PATCH` or the closing `PUT`, which is what an OCI registry behind a
//! load balancer needs. A store that cannot do it says so through
//! `capabilities().resumable`; [`HostedPipeline::supports_chunked`] is the probe, and a format
//! must use it before it hands out an upload location.
//!
//! What this module adds on top of the store contract, and why:
//!
//! * **Bounded memory.** The body of a `PATCH` can be gigabytes. It is cut into segments of
//!   `Limits::chunk_segment_bytes` and each segment is one `append`; nothing larger than a
//!   segment is ever held.
//! * **The same limits as a single upload.** Blob size cap and quota are checked against the
//!   offset before the body is read, and the same budget is the byte limit while it streams.
//! * **The same ending.** `finish_chunked` ends in the code path of a single request upload:
//!   write policy, policy hook, create-only publish, digest check, events, audit, cleanup of
//!   the committed object when the publish is refused.
//! * **A token that names the repository.** The session token carries the repository id, so
//!   a session opened through one repository cannot be finished through another.
//!
//! Not cancel safe, by the store contract: `append` after a cancelled call leaves an unknown
//! length, and `finalize` may or may not have committed. The pipeline reads the length again
//! on an offset mismatch, and runs finalize and publish in a task the context tracks, like an
//! upload, so a client that disconnects at the last moment cannot leave a committed object
//! without its row.

use std::fmt;
use std::str::FromStr;

use axum_core::body::Body;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use rustryx_core::error::BlobError;
use rustryx_core::ids::{BlobId, RepositoryId};
use rustryx_core::ports::{BlobKey, BlobStore, ResumableBlobStore, UploadHandle};
use uuid::Uuid;

use super::quota::{self, Budget};
use super::upload::{deny_writes, precheck_write, publish_committed, reserve, restricted_for};
use super::{HostedPipeline, UploadOutcome, UploadSpec};
use crate::context::FormatContext;
use crate::error::FormatError;
use crate::metrics;

/// A chunked upload in progress: the blob it will become, the store handle that stages its
/// chunks and the repository it belongs to.
///
/// The string form ([`ChunkedSession::token`], [`ChunkedSession::parse`]) is what a format
/// puts into the client visible upload location. It is `repository-uuid.blob-uuid.handle`
/// (the handle is URL safe by the core contract), not a secret: knowing it lets a caller who
/// may write to the repository continue the upload, nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedSession {
    repository: RepositoryId,
    blob: BlobId,
    handle: UploadHandle,
}

impl ChunkedSession {
    /// The token for a client visible URL.
    #[must_use]
    pub fn token(&self) -> String {
        format!("{}.{}.{}", self.repository, self.blob, self.handle.as_str())
    }

    /// Parses a token and checks that it belongs to `repository`.
    ///
    /// # Errors
    /// [`FormatError::NotFound`] for a malformed token or one of another repository: the
    /// client's view is "no such upload", and it learns nothing about which part was wrong.
    pub fn parse(token: &str, repository: RepositoryId) -> Result<Self, FormatError> {
        let mut parts = token.splitn(3, '.');
        let (Some(repo), Some(blob), Some(handle)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(FormatError::NotFound);
        };
        let repo = Uuid::from_str(repo).map_err(|_| FormatError::NotFound)?;
        let blob = Uuid::from_str(blob).map_err(|_| FormatError::NotFound)?;
        if RepositoryId::from_uuid(repo) != repository {
            return Err(FormatError::NotFound);
        }
        // A malformed handle is `BlobError::NotFound` by the core contract; the client's view
        // is the same "no such upload" as for every other malformed part.
        let handle = UploadHandle::new(handle).map_err(|_| FormatError::NotFound)?;
        Ok(Self {
            repository,
            blob: BlobId::from_uuid(blob),
            handle,
        })
    }

    /// The id the finished blob will have.
    #[must_use]
    pub fn blob_id(&self) -> BlobId {
        self.blob
    }

    fn key(&self) -> BlobKey {
        BlobKey::new(self.blob.to_string())
    }
}

impl fmt::Display for ChunkedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.token())
    }
}

/// The resumable side of a store, or the error a caller gets for a store without it.
fn resumable_of(store: &dyn BlobStore) -> Result<&dyn ResumableBlobStore, FormatError> {
    store
        .resumable()
        .ok_or(FormatError::Blob(BlobError::Unsupported("resumable")))
}

impl HostedPipeline<'_> {
    /// True when the store of this repository can stage chunked uploads. A format asks this
    /// before it answers `POST .../blobs/uploads/`, and falls back to a single request upload
    /// (or refuses) when the answer is no.
    ///
    /// # Errors
    /// [`FormatError::Blob`] when the registry cannot provide the store.
    pub async fn supports_chunked(&self) -> Result<bool, FormatError> {
        let store = self.cx.store().await?;
        Ok(store.capabilities().resumable && store.resumable().is_some())
    }

    /// Starts a chunked upload.
    ///
    /// The target path is not needed yet: an OCI blob upload learns its digest, and so its
    /// path, only at the closing request. The write policy `deny` is refused here, because
    /// nothing can ever be published.
    ///
    /// # Errors
    /// `Forbidden` for a read-only repository, `MethodNotAllowed` for a proxy or group,
    /// `Blob(Unsupported)` when the store cannot do chunked uploads and the caller did not
    /// probe, store errors.
    pub async fn begin_chunked(&self) -> Result<ChunkedSession, FormatError> {
        self.require_hosted()?;
        let cx = self.cx;
        deny_writes(cx)?;
        let store = cx.store().await?;
        let resumable = resumable_of(store.as_ref())?;
        let blob = BlobId::from_uuid(cx.ports().ids.next_uuid());
        let handle = resumable
            .begin_resumable(&BlobKey::new(blob.to_string()))
            .await?;
        Ok(ChunkedSession {
            repository: cx.repository().id,
            blob,
            handle,
        })
    }

    /// Bytes staged so far. This is what a client resumes from.
    ///
    /// # Errors
    /// `NotFound` for an unknown or finished session, store errors.
    pub async fn chunked_length(&self, session: &ChunkedSession) -> Result<u64, FormatError> {
        self.require_session(session)?;
        let store = self.cx.store().await?;
        Ok(resumable_of(store.as_ref())?
            .length(&session.handle)
            .await?)
    }

    /// Stages the body as the next chunk(s), starting at `offset`. Returns the new length.
    ///
    /// The body is cut into segments of `Limits::chunk_segment_bytes`, one staged object each.
    /// A declared length that does not fit the blob size cap or the quota is refused before
    /// the body is read. A body that breaks halfway leaves the segments that were already
    /// staged in place: the length tells the client where to resume.
    ///
    /// # Errors
    /// `RangeNotSatisfiable { total }` when `offset` is not the staged length (`total` is the
    /// length, for the `Range` header of the answer), `PayloadTooLarge`, `QuotaExceeded`,
    /// `RequestTimeout` (stalled client), `BadRequest` (unreadable body or a length that does
    /// not match), store errors.
    pub async fn append_chunk(
        &self,
        session: &ChunkedSession,
        offset: u64,
        body: Body,
        declared_length: Option<u64>,
    ) -> Result<u64, FormatError> {
        self.require_session(session)?;
        let cx = self.cx;
        let store = cx.store().await?;
        let resumable = resumable_of(store.as_ref())?;
        // The staged bytes count against the budget too: the budget is for the whole blob.
        let budget = quota::budget(cx, 0).await?;
        let room = Budget {
            bytes: budget.bytes.saturating_sub(offset),
            ..budget
        };
        room.check_declared(declared_length)?;
        let limit = declared_length.map_or(room.bytes, |declared| declared.min(room.bytes));
        let result = stream_segments(
            cx,
            resumable,
            session,
            offset,
            body,
            &room,
            limit,
            declared_length,
        )
        .await;
        match result {
            Err(FormatError::Blob(BlobError::RangeNotSatisfiable)) => {
                // The store refused the offset: the client is out of step. Report the truth.
                let staged = resumable.length(&session.handle).await?;
                Err(FormatError::RangeNotSatisfiable { total: staged })
            }
            other => other,
        }
    }

    /// Joins the chunks, hashes and commits them, and publishes the blob under `spec.path`
    /// with the same checks as a single request upload: write policy, policy hook,
    /// create-only publish for immutable paths, expected digest, events and audit.
    ///
    /// `spec.content_length`, when set, must equal the staged length. A refused publish
    /// removes the committed object; the staged chunks are consumed by the finalize, so the
    /// client has to upload again. A refusal that comes before the join (write policy, policy
    /// hook, a length that does not match, the quota) leaves the session in place, so the client
    /// can abort it or fix the request.
    ///
    /// # Errors
    /// As [`HostedPipeline::upload`], plus `NotFound` for an unknown session.
    pub async fn finish_chunked(
        &self,
        session: ChunkedSession,
        spec: UploadSpec,
    ) -> Result<UploadOutcome, FormatError> {
        self.require_session(&session)?;
        let format = self.cx.repository().format;
        let result = finish(self.cx, session, spec).await;
        match &result {
            Ok(_) => metrics::upload_result(format, "ok"),
            Err(error) => metrics::upload_result(format, error.code()),
        }
        result
    }

    /// Discards the staged chunks. Idempotent: an unknown session is not an error, so a client
    /// that retries a `DELETE` gets the same answer.
    ///
    /// # Errors
    /// Store errors other than "unknown session".
    pub async fn abort_chunked(&self, session: ChunkedSession) -> Result<(), FormatError> {
        self.require_session(&session)?;
        let store = self.cx.store().await?;
        match resumable_of(store.as_ref())?.abort(session.handle).await {
            Ok(()) | Err(BlobError::NotFound) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// A session belongs to the repository it was opened on. `parse` already checks the token
    /// of a client; this guards sessions that were built by other means.
    fn require_session(&self, session: &ChunkedSession) -> Result<(), FormatError> {
        self.require_hosted()?;
        if session.repository == self.cx.repository().id {
            Ok(())
        } else {
            Err(FormatError::NotFound)
        }
    }
}

/// Moves the body into the store segment by segment. Returns the new staged length.
#[allow(
    clippy::too_many_arguments,
    reason = "one parameter per fact of the request; a parameter struct would only rename them"
)]
async fn stream_segments(
    cx: &FormatContext,
    resumable: &dyn ResumableBlobStore,
    session: &ChunkedSession,
    offset: u64,
    body: Body,
    budget: &Budget,
    limit: u64,
    declared_length: Option<u64>,
) -> Result<u64, FormatError> {
    let idle = cx.limits().body_idle_timeout;
    let segment = cx.limits().chunk_segment_bytes.max(1);
    let mut stream = body.into_data_stream();
    let mut buffer = BytesMut::new();
    let mut received: u64 = 0;
    let mut length = offset;
    loop {
        let next = tokio::time::timeout(idle, stream.next())
            .await
            .map_err(|_| FormatError::RequestTimeout)?;
        let Some(item) = next else { break };
        let chunk: Bytes =
            item.map_err(|_| FormatError::bad_request("request body could not be read"))?;
        received = received.saturating_add(chunk.len() as u64);
        if received > limit {
            // Nothing of the offending frame is staged. A declared length that the body
            // outgrows is the client's mistake, a budget that runs out is the system's.
            return Err(if declared_length.is_some_and(|d| d <= budget.bytes) {
                FormatError::bad_request("body is longer than Content-Length")
            } else {
                budget.exceeded()
            });
        }
        buffer.extend_from_slice(&chunk);
        if buffer.len() >= segment {
            length = flush(resumable, session, length, &mut buffer).await?;
        }
    }
    if declared_length.is_some_and(|declared| declared != received) {
        return Err(FormatError::bad_request(
            "body length does not match Content-Length",
        ));
    }
    if !buffer.is_empty() {
        length = flush(resumable, session, length, &mut buffer).await?;
    }
    Ok(length)
}

/// Stages the buffered bytes as one chunk and empties the buffer.
async fn flush(
    resumable: &dyn ResumableBlobStore,
    session: &ChunkedSession,
    offset: u64,
    buffer: &mut BytesMut,
) -> Result<u64, FormatError> {
    let chunk = buffer.split().freeze();
    let size = chunk.len() as u64;
    resumable.append(&session.handle, offset, chunk).await?;
    Ok(offset.saturating_add(size))
}

async fn finish(
    cx: &FormatContext,
    session: ChunkedSession,
    spec: UploadSpec,
) -> Result<UploadOutcome, FormatError> {
    let repository = cx.repository();
    let existing = cx
        .ports()
        .assets
        .get_by_path(repository.id, spec.path.as_str())
        .await?;
    let restricted = restricted_for(cx, &spec, existing.is_some())?;
    let store = cx.store().await?;
    let staged = resumable_of(store.as_ref())?
        .length(&session.handle)
        .await?;
    if spec
        .content_length
        .is_some_and(|declared| declared != staged)
    {
        return Err(FormatError::bad_request(
            "the staged length does not match Content-Length",
        ));
    }
    // The hook sees the real total here; at `begin` there was no path and no size.
    precheck_write(cx, &spec, existing.as_ref(), Some(staged)).await?;
    let reservation = reserve(cx, &spec, restricted)?;
    let replaced_size = existing.as_ref().map_or(0, |asset| asset.size_bytes);
    quota::budget(cx, replaced_size)
        .await?
        .check_declared(Some(staged))?;

    let task_cx = cx.clone();
    let job = async move {
        // Held until the end of the task: the path stays reserved through the publish.
        let _reservation = reservation;
        let store = task_cx.store().await?;
        let key = session.key();
        let commit = resumable_of(store.as_ref())?
            .finalize(session.handle.clone())
            .await?;
        publish_committed(&task_cx, &spec, &commit, session.blob, &key, restricted).await
    };
    // Finalize is not cancel safe and a committed object without a row is a leak: the task is
    // tracked by the context and survives a client that disconnects now.
    match cx.tasks().spawn(job).await {
        Ok(result) => result,
        Err(join_error) => Err(FormatError::internal(format!(
            "chunked upload finalizer failed: {join_error}"
        ))),
    }
}
