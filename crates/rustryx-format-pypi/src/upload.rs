//! The legacy upload API: `POST /` with `multipart/form-data`, as twine, uv, poetry and flit
//! send it.
//!
//! Order of events, chosen so that nothing a client sent can be stored before it has been
//! checked, and the file itself is never held in memory:
//!
//! 1. the form fields before the file are read (bounded, see [`crate::form`]) and validated;
//! 2. the file name is parsed and must agree with the `name`, `version` and `filetype` fields;
//! 3. authorization is repeated with the real coordinate (the engine could only judge the URL,
//!    the project of an upload is in the body): a selector that allows or denies by project name
//!    is applied here, before a byte of the file is read;
//! 4. an existing file name is answered 409 before the body is read;
//! 5. the file part is streamed into the blob writer through a hasher that verifies the declared
//!    `md5_digest` and `blake2_256_digest` (the declared `sha256_digest` is verified by the
//!    hosted pipeline); a mismatch makes the stream fail at its end, which aborts the staged blob,
//!    so nothing is published. A body that ends before the closing delimiter fails the same way;
//! 6. for a wheel, the `METADATA` inside it is read by ranged reads, name and version must agree
//!    with the file name, and the metadata becomes the PEP 658 sidecar; the records of the file
//!    are written. This phase runs in a task tracked by the context, so a client that
//!    disconnects after the file was committed cannot leave a file without its records;
//! 7. any failure in step 6 deletes the file again (a compensating delete): the repository never
//!    keeps a wheel whose metadata contradicts its name.
//!
//! Known limit: between the commit of step 5 and the delete of step 7 the file is visible. The
//! alternative (staging the file under another path and moving it) would announce an event and an
//! audit record for a path that never existed, and a failed wheel check would still have written
//! them.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use axum_core::body::Body;
use blake2::{Blake2b256, Digest};
use bytes::Bytes;
use futures::Stream;
use http::{HeaderValue, Response, StatusCode, header};
use md5::Md5;
use rustryx_core::error::AuthError;
use rustryx_core::identity::Action;
use rustryx_core::ports::auth::Coordinate;
use rustryx_core::ports::{BlobKey, BlobStore, ByteRange, Decision, ResourceRef};
use rustryx_format_core::stream::{collect_bounded, hash_slices};
use rustryx_format_core::{
    ArtifactRequest, ExpectedChecksum, FormatContext, FormatError, HashAlgo, NormalizedPath,
    OverwriteRule, UploadOutcome, UploadSpec,
};

use crate::filename::{DistFilename, DistKind};
use crate::form::{DeclaredDigests, FormError, MAX_DROPPED_FORM_BYTES, UploadFields, read_prefix};
use crate::handler::PypiHandler;
use crate::hosted::{component_spec, delete_if_present};
use crate::metadata::{CoreMetadata, MetadataError};
use crate::metrics;
use crate::multipart::{MultipartError, MultipartReader, boundary_from_content_type};
use crate::name::ProjectName;
use crate::route::{PACKAGES, file_path, info_path, metadata_path, yank_path};
use crate::sidecar::FileInfo;
use crate::zipmeta::{RangeRead, ZipError, read_metadata};

/// What went wrong inside the file stream, kept so that the handler can answer precisely: the
/// hosted pipeline only sees an I/O error on the body.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamProblem {
    DigestMismatch,
    Multipart(MultipartError),
}

type ProblemSlot = Arc<Mutex<Option<StreamProblem>>>;

fn record(slot: &ProblemSlot, problem: StreamProblem) {
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(problem);
}

fn take(slot: &ProblemSlot) -> Option<StreamProblem> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Digests that are verified while the file streams past.
struct Verifier {
    md5: Option<(Md5, [u8; 16])>,
    blake2: Option<(Blake2b256, [u8; 32])>,
}

impl Verifier {
    fn new(declared: &DeclaredDigests) -> Self {
        Self {
            md5: declared.md5.map(|expected| (Md5::new(), expected)),
            blake2: declared
                .blake2_256
                .map(|expected| (Blake2b256::new(), expected)),
        }
    }

    /// Hashes in slices of at most 64 KiB, in the task that moves the bytes: hashing is not
    /// moved to the blocking pool (README 8.1).
    fn update(&mut self, chunk: &Bytes) {
        for slice in hash_slices(chunk.clone()) {
            if let Some((hasher, _)) = &mut self.md5 {
                hasher.update(&slice);
            }
            if let Some((hasher, _)) = &mut self.blake2 {
                hasher.update(&slice);
            }
        }
    }

    fn matches(self) -> bool {
        let md5_ok = self
            .md5
            .is_none_or(|(hasher, expected)| hasher.finalize().as_slice() == expected);
        let blake2_ok = self
            .blake2
            .is_none_or(|(hasher, expected)| hasher.finalize().as_slice() == expected);
        md5_ok && blake2_ok
    }
}

struct FileStream<S> {
    reader: MultipartReader<S>,
    verifier: Verifier,
    problem: ProblemSlot,
}

/// The body handed to the hosted pipeline: the bytes of the file part and nothing else.
fn file_body<S, E>(
    reader: MultipartReader<S>,
    declared: &DeclaredDigests,
    problem: ProblemSlot,
) -> Body
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let state = Some(FileStream {
        reader,
        verifier: Verifier::new(declared),
        problem,
    });
    Body::from_stream(futures::stream::unfold(state, |state| async move {
        let mut state = state?;
        match state.reader.next_chunk().await {
            Ok(Some(chunk)) => {
                state.verifier.update(&chunk);
                Some((Ok::<Bytes, io::Error>(chunk), Some(state)))
            }
            Ok(None) => {
                if state.verifier.matches() {
                    None
                } else {
                    record(&state.problem, StreamProblem::DigestMismatch);
                    Some((Err(io::Error::other("digest mismatch")), None))
                }
            }
            Err(error) => {
                let text = error.to_string();
                record(&state.problem, StreamProblem::Multipart(error));
                Some((Err(io::Error::other(text)), None))
            }
        }
    }))
}

fn form_error(error: FormError) -> FormatError {
    match error {
        FormError::Multipart(MultipartError::Stalled) => FormatError::RequestTimeout,
        FormError::TooLarge => FormatError::PayloadTooLarge {
            limit: MAX_DROPPED_FORM_BYTES,
        },
        other => FormatError::bad_request(other.to_string()),
    }
}

fn problem_error(problem: &StreamProblem) -> FormatError {
    match problem {
        StreamProblem::DigestMismatch => FormatError::DigestMismatch,
        StreamProblem::Multipart(MultipartError::Stalled) => FormatError::RequestTimeout,
        StreamProblem::Multipart(_) => {
            FormatError::bad_request("the upload body is incomplete or malformed")
        }
    }
}

/// Ranged reads of a stored blob, for the wheel reader.
struct BlobRange {
    store: Arc<dyn BlobStore>,
    key: BlobKey,
}

#[async_trait]
impl RangeRead for BlobRange {
    async fn read_range(&self, offset: u64, len: u64) -> Result<Bytes, ZipError> {
        let Some(last) = len.checked_sub(1) else {
            return Ok(Bytes::new());
        };
        let end = offset
            .checked_add(last)
            .ok_or_else(|| ZipError::Io("range overflow".to_owned()))?;
        let read = self
            .store
            .get(
                &self.key,
                Some(ByteRange {
                    start: offset,
                    end: Some(end),
                }),
            )
            .await
            .map_err(|error| ZipError::Io(error.to_string()))?;
        let bytes = collect_bounded(read.stream, len)
            .await
            .map_err(|error| ZipError::Io(error.to_string()))?;
        if bytes.len() as u64 == len {
            Ok(bytes)
        } else {
            Err(ZipError::Io("short read".to_owned()))
        }
    }
}

fn storage_unavailable() -> FormatError {
    FormatError::Unavailable {
        reason: "storage is temporarily unavailable, retry",
        retry_after: Some(std::time::Duration::from_secs(5)),
    }
}

fn zip_failure(error: &ZipError) -> FormatError {
    match error {
        ZipError::Io(_) => {
            metrics::wheel_metadata("unavailable");
            storage_unavailable()
        }
        other => {
            metrics::wheel_metadata("invalid");
            FormatError::unprocessable(format!("the wheel is not valid: {other}"))
        }
    }
}

fn metadata_failure(error: MetadataError) -> FormatError {
    metrics::wheel_metadata("invalid");
    FormatError::unprocessable(format!("the wheel METADATA is not valid: {error}"))
}

impl PypiHandler {
    /// Serves `POST /`. See the module documentation for the order of events.
    pub(crate) async fn upload(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let result = Self::upload_inner(&cx, &req, body).await;
        if let Err(error) = &result {
            metrics::upload_rejected(rejection_reason(error));
        }
        result
    }

    async fn upload_inner(
        cx: &FormatContext,
        req: &ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let content_type = req
            .head
            .header(&header::CONTENT_TYPE)
            .ok_or_else(|| FormatError::bad_request("a multipart/form-data body is required"))?;
        let boundary = boundary_from_content_type(content_type)
            .map_err(|error| FormatError::bad_request(error.to_string()))?;
        let reader = MultipartReader::new(
            body.into_data_stream(),
            &boundary,
            cx.limits().body_idle_timeout,
        );
        let prefix = read_prefix(reader).await.map_err(form_error)?;
        let fields = UploadFields::from_raw(&prefix.fields).map_err(form_error)?;
        let filename = prefix
            .content
            .filename
            .as_deref()
            .ok_or_else(|| FormatError::bad_request("the content part has no file name"))?;
        let dist = DistFilename::parse(filename).map_err(|_| {
            FormatError::bad_request("the file name is not a wheel or an sdist name")
        })?;
        check_agreement(&fields, &dist)?;

        let path = NormalizedPath::from_segments([PACKAGES, fields.name.as_str(), dist.as_str()])?;
        authorize_upload(cx, &fields, &dist, &path).await?;

        let repository = cx.repository();
        if cx
            .ports()
            .assets
            .get_by_path(repository.id, path.as_str())
            .await?
            .is_some()
        {
            return Err(already_exists(&dist));
        }

        let mut spec = UploadSpec::new(path);
        spec.component = Some(component_spec(&fields.name, &dist));
        spec.overwrite = OverwriteRule::Never;
        spec.expected = fields
            .digests
            .sha256
            .and_then(|digest| ExpectedChecksum::new(HashAlgo::Sha256, &hex::encode(digest)).ok());

        let problem: ProblemSlot = Arc::default();
        let stream = file_body(prefix.reader, &fields.digests, Arc::clone(&problem));
        let outcome = match cx.hosted().upload(spec, stream).await {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(take(&problem).map_or_else(
                    || exists_or(error, &dist),
                    |problem| problem_error(&problem),
                ));
            }
        };

        let task_cx = cx.clone();
        let handle = cx
            .tasks()
            .spawn(async move { finalize(task_cx, outcome, fields, dist).await });
        match handle.await {
            Ok(result) => result?,
            Err(error) => {
                return Err(FormatError::internal(format!(
                    "upload finalizer failed: {error}"
                )));
            }
        }
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::OK;
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(0u64));
        Ok(response)
    }
}

/// The 409 of a file name that is taken.
fn already_exists(dist: &DistFilename) -> FormatError {
    FormatError::conflict(format!(
        "File already exists: {}. A file name can be used once; upload a new version instead.",
        dist.as_str()
    ))
}

/// A refusal of the pipeline for an existing path is the same 409 as the early check.
fn exists_or(error: FormatError, dist: &DistFilename) -> FormatError {
    match error {
        FormatError::Conflict(_) => already_exists(dist),
        other => other,
    }
}

fn rejection_reason(error: &FormatError) -> &'static str {
    match error {
        FormatError::Conflict(_) => "exists",
        FormatError::DigestMismatch => "digest",
        FormatError::Forbidden | FormatError::Unauthenticated => "forbidden",
        FormatError::BadRequest(message) if message.contains("does not match") => "mismatch",
        FormatError::BadRequest(message) if message.contains("file name") => "filename",
        FormatError::Unprocessable(_) => "metadata",
        FormatError::BadRequest(_) | FormatError::PayloadTooLarge { .. } => "form",
        _ => "other",
    }
}

/// The form and the file name must say the same: a client that claims one project and uploads a
/// file of another is refused, not stored under either.
fn check_agreement(fields: &UploadFields, dist: &DistFilename) -> Result<(), FormatError> {
    if dist.kind() != fields.filetype {
        return Err(FormatError::bad_request(
            "the filetype does not match the file name",
        ));
    }
    if dist.project() != &fields.name || dist.version() != &fields.version {
        return Err(FormatError::bad_request(
            "the file name does not match the name and version of the form",
        ));
    }
    Ok(())
}

/// Authorizes the write with the coordinate the engine could not know.
async fn authorize_upload(
    cx: &FormatContext,
    fields: &UploadFields,
    dist: &DistFilename,
    path: &NormalizedPath,
) -> Result<(), FormatError> {
    let mut coordinate = Coordinate::new(
        None,
        Some(fields.name.to_string()),
        Some(fields.version.to_string()),
    );
    coordinate.extension = Some(dist.extension().to_owned());
    let resource =
        ResourceRef::for_repository(cx.repository(), path.as_str()).with_coordinate(coordinate);
    match cx
        .ports()
        .rbac
        .check(cx.principal(), Action::Write, &resource)
        .await
    {
        Ok(Decision::Allow) => Ok(()),
        Ok(Decision::Deny) => Err(FormatError::Forbidden),
        Err(AuthError::Overloaded) => Err(FormatError::Unavailable {
            reason: "authorization capacity exhausted, retry",
            retry_after: Some(std::time::Duration::from_secs(1)),
        }),
        Err(error) => {
            tracing::warn!(%error, "authorization of an upload failed closed");
            Err(FormatError::Forbidden)
        }
    }
}

/// Step 6 and 7: validate, write the records, or take the file back.
async fn finalize(
    cx: FormatContext,
    outcome: UploadOutcome,
    fields: UploadFields,
    dist: DistFilename,
) -> Result<(), FormatError> {
    match write_records(&cx, &outcome, &fields, &dist).await {
        Ok(()) => Ok(()),
        Err(error) => {
            take_back(&cx, &fields.name, &dist).await;
            Err(error)
        }
    }
}

async fn write_records(
    cx: &FormatContext,
    outcome: &UploadOutcome,
    fields: &UploadFields,
    dist: &DistFilename,
) -> Result<(), FormatError> {
    let name = &fields.name;
    let mut requires_python = fields.requires_python.clone();
    let mut metadata_version = fields.metadata_version.clone();
    let mut metadata_bytes: Option<Vec<u8>> = None;
    if dist.kind() == DistKind::Wheel {
        let bytes = wheel_metadata(cx, outcome).await?;
        let meta = CoreMetadata::parse(&bytes).map_err(metadata_failure)?;
        if &meta.name != dist.project() || &meta.version != dist.version() {
            metrics::wheel_metadata("invalid");
            return Err(FormatError::bad_request(
                "the name or version in the wheel METADATA does not match the file name",
            ));
        }
        metrics::wheel_metadata("ok");
        // The file is the truth: its own `Requires-Python` wins over what the form said.
        requires_python = meta.requires_python.or(requires_python);
        metadata_version = Some(meta.metadata_version);
        metadata_bytes = Some(bytes);
    }

    // The file name was free (the upload is create-only), so anything at the record paths is
    // left over from a crash or a half deleted file. A stale yank marker would hide the new file.
    for stale in [
        yank_path(name, dist.as_str()),
        info_path(name, dist.as_str()),
        metadata_path(name, dist.as_str()),
    ] {
        delete_if_present(cx, &stale).await?;
    }
    if let Some(bytes) = metadata_bytes {
        put_record(
            cx,
            &metadata_path(name, dist.as_str()),
            bytes,
            "text/plain",
            name,
            dist,
        )
        .await?;
    }
    let info = FileInfo::new(requires_python, metadata_version).to_bytes();
    put_record(
        cx,
        &info_path(name, dist.as_str()),
        info,
        "application/json",
        name,
        dist,
    )
    .await
}

async fn wheel_metadata(
    cx: &FormatContext,
    outcome: &UploadOutcome,
) -> Result<Vec<u8>, FormatError> {
    let asset = &outcome.asset;
    let record = cx
        .ports()
        .blobs
        .get(asset.blob_id)
        .await?
        .ok_or_else(|| FormatError::internal("the blob row of a published file is missing"))?;
    let store = cx.store_for_blob(&record).await?;
    let source = BlobRange {
        store,
        key: BlobKey::new(record.key.clone()),
    };
    read_metadata(&source, asset.size_bytes)
        .await
        .map_err(|error| zip_failure(&error))
}

async fn put_record(
    cx: &FormatContext,
    path: &str,
    bytes: Vec<u8>,
    content_type: &str,
    name: &ProjectName,
    dist: &DistFilename,
) -> Result<(), FormatError> {
    let mut spec = UploadSpec::new(NormalizedPath::parse(path)?);
    spec.content_type = Some(content_type.to_owned());
    spec.content_length = Some(bytes.len() as u64);
    spec.component = Some(component_spec(name, dist));
    spec.overwrite = OverwriteRule::Never;
    cx.hosted()
        .upload(spec, Body::from(bytes))
        .await
        .map(|_| ())
}

/// Removes the file and its records after a failed finalization. Best effort: a failure is
/// logged loudly, because a file that stays has not passed the checks.
async fn take_back(cx: &FormatContext, name: &ProjectName, dist: &DistFilename) {
    for path in [
        yank_path(name, dist.as_str()),
        info_path(name, dist.as_str()),
        metadata_path(name, dist.as_str()),
        file_path(name, dist.as_str()),
    ] {
        if let Err(error) = delete_if_present(cx, &path).await {
            tracing::error!(%error, "could not take back a rejected upload; the file needs an administrator");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::DeclaredDigests;
    use crate::multipart::MultipartReader;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    fn multipart_with_file(data: &[u8]) -> Vec<u8> {
        let mut body =
            b"--B\r\nContent-Disposition: form-data; name=\"content\"; filename=\"f.whl\"\r\n\r\n"
                .to_vec();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n--B--\r\n");
        body
    }

    async fn run_stream(
        body: Vec<u8>,
        digests: DeclaredDigests,
    ) -> (Result<Vec<u8>, String>, Option<StreamProblem>) {
        let chunks: Vec<Result<Bytes, io::Error>> = body
            .chunks(17)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let mut reader = MultipartReader::new(stream::iter(chunks), "B", Duration::from_secs(5));
        reader.next_part().await.unwrap().unwrap();
        let problem: ProblemSlot = Arc::default();
        let body = file_body(reader, &digests, Arc::clone(&problem));
        let mut data = body.into_data_stream();
        let mut collected = Vec::new();
        while let Some(chunk) = data.next().await {
            match chunk {
                Ok(chunk) => collected.extend_from_slice(&chunk),
                Err(error) => return (Err(error.to_string()), take(&problem)),
            }
        }
        (Ok(collected), take(&problem))
    }

    #[test]
    fn the_file_stream_yields_exactly_the_file_and_verifies_digests() {
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let md5: [u8; 16] = Md5::digest(&data).into();
        let blake2: [u8; 32] = Blake2b256::digest(&data).into();
        let good = DeclaredDigests {
            sha256: None,
            md5: Some(md5),
            blake2_256: Some(blake2),
        };
        let (result, problem) = runtime().block_on(run_stream(multipart_with_file(&data), good));
        assert_eq!(result.unwrap(), data);
        assert_eq!(problem, None);

        let mut wrong_md5 = md5;
        wrong_md5[0] ^= 1;
        let bad = DeclaredDigests {
            sha256: None,
            md5: Some(wrong_md5),
            blake2_256: Some(blake2),
        };
        let (result, problem) = runtime().block_on(run_stream(multipart_with_file(&data), bad));
        assert!(result.is_err());
        assert_eq!(problem, Some(StreamProblem::DigestMismatch));

        let mut wrong_blake = blake2;
        wrong_blake[31] ^= 1;
        let bad = DeclaredDigests {
            sha256: None,
            md5: Some(md5),
            blake2_256: Some(wrong_blake),
        };
        let (_, problem) = runtime().block_on(run_stream(multipart_with_file(&data), bad));
        assert_eq!(problem, Some(StreamProblem::DigestMismatch));
    }

    #[test]
    fn must_not_end_the_file_stream_cleanly_when_the_body_was_cut() {
        let data = vec![9u8; 4000];
        let mut body = multipart_with_file(&data);
        body.truncate(body.len() - 40);
        let (result, problem) = runtime().block_on(run_stream(body, DeclaredDigests::default()));
        assert!(
            result.is_err(),
            "a truncated upload must reach the sink as an error"
        );
        assert_eq!(
            problem,
            Some(StreamProblem::Multipart(MultipartError::UnexpectedEof))
        );
    }

    #[test]
    fn problems_become_client_errors() {
        assert!(matches!(
            problem_error(&StreamProblem::DigestMismatch),
            FormatError::DigestMismatch
        ));
        assert!(matches!(
            problem_error(&StreamProblem::Multipart(MultipartError::Stalled)),
            FormatError::RequestTimeout
        ));
        assert!(matches!(
            problem_error(&StreamProblem::Multipart(MultipartError::UnexpectedEof)),
            FormatError::BadRequest(_)
        ));
        assert!(matches!(
            form_error(FormError::TooLarge),
            FormatError::PayloadTooLarge { .. }
        ));
        assert!(matches!(
            form_error(FormError::Multipart(MultipartError::Stalled)),
            FormatError::RequestTimeout
        ));
        assert!(matches!(
            form_error(FormError::NoContent),
            FormatError::BadRequest(_)
        ));
    }

    #[test]
    fn rejection_reasons_are_a_closed_set() {
        for (error, reason) in [
            (FormatError::conflict("x"), "exists"),
            (FormatError::DigestMismatch, "digest"),
            (FormatError::Forbidden, "forbidden"),
            (
                FormatError::bad_request("the file name does not match the name"),
                "mismatch",
            ),
            (
                FormatError::bad_request("the file name is not a wheel"),
                "filename",
            ),
            (FormatError::unprocessable("x"), "metadata"),
            (FormatError::bad_request("other"), "form"),
            (FormatError::internal("x"), "other"),
        ] {
            assert_eq!(rejection_reason(&error), reason);
        }
    }

    #[test]
    fn the_form_and_the_file_name_must_agree() {
        use crate::version::Version;
        let fields = |name: &str, version: &str, filetype| UploadFields {
            name: ProjectName::parse(name).unwrap(),
            version: Version::parse(version).unwrap(),
            filetype,
            requires_python: None,
            metadata_version: None,
            digests: DeclaredDigests::default(),
        };
        let wheel = DistFilename::parse("Foo_Bar-1.0.0-py3-none-any.whl").unwrap();
        assert!(check_agreement(&fields("foo-bar", "1.0", DistKind::Wheel), &wheel).is_ok());
        for bad in [
            fields("other", "1.0", DistKind::Wheel),
            fields("foo-bar", "1.1", DistKind::Wheel),
            fields("foo-bar", "1.0", DistKind::Sdist),
        ] {
            assert!(check_agreement(&bad, &wheel).is_err());
        }
    }
}
