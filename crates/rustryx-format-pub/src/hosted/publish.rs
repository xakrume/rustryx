//! Publish steps 1 and 2: the upload URL and the multipart upload.
//!
//! ```text
//! GET  api/packages/versions/new            -> {"url": ".../upload/<id>", "fields": {}}
//! POST api/packages/versions/upload/<id>    -> 204, Location: .../finish/<id>
//! GET  api/packages/versions/finish/<id>    -> {"success": {...}}        (see finalize.rs)
//! ```
//!
//! Step 2 is where the archive arrives, and the rules of the engine hold on it: the body is
//! authorized before it is read, it is streamed (never collected), and the bytes go through
//! format-core's hosted pipeline into the blob store, which hashes them and refuses more than
//! the limits allow. What the pipeline stores is the content of the multipart `file` part only,
//! cut out of the body while it streams. It is stored at the path of the request itself,
//! `api/packages/versions/upload/<id>`, so the path that was authorized is the path that is
//! written. That asset is a staging copy: it is turned into the published version by step 3, or
//! deleted. Nothing about an archive is trusted until step 3 has read it.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use axum_core::body::Body;
use futures::StreamExt;
use http::{HeaderValue, Response, StatusCode, header};
use multer::{Constraints, Multipart, SizeLimit};
use rustryx_core::model::WritePolicy;
use rustryx_format_core::{
    ArtifactRequest, FormatContext, FormatError, NormalizedPath, OverwriteRule, RequestHead,
    UploadSpec,
};
use serde_json::json;

use crate::config::PubConfig;
use crate::name::PackageName;
use crate::response;
use crate::route::{Declared, Route, STAGING_PREFIX, UploadId};
use crate::version::PubVersion;

/// Room for the multipart framing around the file part, so that a request that carries a file
/// of exactly the limit is not refused for its boundaries.
const MULTIPART_OVERHEAD: u64 = 64 * 1024;
/// Uploads listed per page when staged uploads are reaped.
const REAP_PAGE: u32 = 200;
/// Name of the form field that carries the archive.
const FILE_FIELD: &str = "file";

/// The coordinate a client declared on `versions/new`, if it did. Both parameters or neither.
pub(crate) fn declared_from_head(head: &RequestHead) -> Result<Option<Declared>, FormatError> {
    match (head.query_param("package"), head.query_param("version")) {
        (None, None) => Ok(None),
        (Some(name), Some(version)) => Ok(Some(Declared {
            name: PackageName::parse(&name)?,
            version: PubVersion::parse(&version)?,
        })),
        _ => Err(FormatError::bad_request(
            "declare both package and version, or neither",
        )),
    }
}

/// Step 1: `GET api/packages/versions/new`.
pub(crate) fn begin(
    cx: &FormatContext,
    cfg: &PubConfig,
    req: &ArtifactRequest,
) -> Result<Response<Body>, FormatError> {
    if cx.repository().write_policy == WritePolicy::Deny {
        // The upload would refuse it anyway; the publisher learns before it sends the archive.
        return Err(FormatError::Forbidden);
    }
    let declared = declared_from_head(&req.head)?;
    let id = UploadId::new(cx.ports().ids.next_uuid());
    let url = format!(
        "{}/{}",
        req.head.repository_url(&cx.origin_repository().name),
        Route::Upload { id, declared }.to_path()
    );
    // Housekeeping rides on the start of a publication: tracked by the engine, not awaited.
    drop(cx.tasks().spawn(reap(cx.clone(), cfg.clone())));
    let body = serde_json::to_vec(&json!({ "url": url, "fields": {} }))
        .map_err(|error| FormatError::internal(format!("upload answer: {error}")))?;
    Ok(response::json(StatusCode::OK, body))
}

/// Step 2: `POST api/packages/versions/upload/<id>`.
pub(crate) async fn receive(
    cx: &FormatContext,
    cfg: &PubConfig,
    req: &ArtifactRequest,
    body: Body,
) -> Result<Response<Body>, FormatError> {
    let Route::Upload { id, declared } = Route::parse(&req.path)? else {
        return Err(FormatError::internal("receive called for another route"));
    };
    // An upload id is written once. Refusing a repeat here, before the body is touched, costs
    // nothing; the create-only publish of the pipeline is what decides a race between nodes.
    if cx
        .ports()
        .assets
        .get_by_path(cx.repository().id, req.path.as_str())
        .await?
        .is_some()
    {
        return Err(FormatError::conflict(
            "this upload has already been received",
        ));
    }
    let max = cfg.max_upload_bytes;
    if req
        .head
        .content_length()
        .is_some_and(|declared| declared > max.saturating_add(MULTIPART_OVERHEAD))
    {
        // Refused before a byte is read.
        return Err(FormatError::PayloadTooLarge { limit: max });
    }
    let boundary = req
        .head
        .header(&header::CONTENT_TYPE)
        .and_then(|value| multer::parse_boundary(value).ok())
        .ok_or_else(|| FormatError::bad_request("expected a multipart/form-data body"))?;
    let constraints = Constraints::new()
        .allowed_fields(vec![FILE_FIELD])
        .size_limit(
            SizeLimit::new()
                .whole_stream(max.saturating_add(MULTIPART_OVERHEAD))
                .for_field(FILE_FIELD, max),
        );
    let mut multipart = Multipart::with_constraints(body.into_data_stream(), boundary, constraints);
    let field = match multipart.next_field().await {
        Ok(Some(field)) => field,
        Ok(None) => return Err(FormatError::bad_request("the upload has no file part")),
        Err(error) => return Err(from_multer(&error, max)),
    };

    let fault = Arc::new(Mutex::new(None));
    let recorder = Arc::clone(&fault);
    // The file part, as a body for the hosted pipeline. A failure of the multipart parser is
    // recorded on the side, because the pipeline reports any broken body in the same words.
    let file = field.map(move |item| {
        item.map_err(|error| {
            *recorder.lock().unwrap_or_else(PoisonError::into_inner) = Some(Fault::of(&error));
            io::Error::other("multipart stream failed")
        })
    });
    let mut spec = UploadSpec::new(req.path.clone());
    spec.content_type = Some("application/gzip".to_owned());
    // An upload id is written once: a second POST to it is refused before its body is read.
    spec.overwrite = OverwriteRule::Never;
    if let Err(error) = cx.hosted().upload(spec, Body::from_stream(file)).await {
        return Err(refine(error, &fault, max));
    }

    // The part ended; the closing boundary must follow, with nothing else after it.
    match multipart.next_field().await {
        Ok(None) => {}
        Ok(Some(_)) => {
            discard_staged(cx, &req.path).await;
            return Err(FormatError::bad_request("only one file part is accepted"));
        }
        Err(error) => {
            discard_staged(cx, &req.path).await;
            return Err(from_multer(&error, max));
        }
    }

    let finish = format!(
        "{}/{}",
        req.head.repository_url(&cx.origin_repository().name),
        Route::Finish { id, declared }.to_path()
    );
    let location = HeaderValue::from_str(&finish)
        .map_err(|_| FormatError::internal("the finalize url is not a valid header value"))?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// What went wrong inside the multipart stream, as far as the answer is concerned.
#[derive(Debug, Clone, Copy)]
enum Fault {
    TooLarge,
    Malformed,
}

impl Fault {
    fn of(error: &multer::Error) -> Self {
        match error {
            multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
                Self::TooLarge
            }
            _ => Self::Malformed,
        }
    }
}

/// Replaces the generic "request body could not be read" of the pipeline with the cause the
/// multipart parser recorded.
fn refine(error: FormatError, fault: &Mutex<Option<Fault>>, limit: u64) -> FormatError {
    if !matches!(error, FormatError::BadRequest(_)) {
        return error;
    }
    match *fault.lock().unwrap_or_else(PoisonError::into_inner) {
        Some(Fault::TooLarge) => FormatError::PayloadTooLarge { limit },
        Some(Fault::Malformed) => FormatError::bad_request("the multipart body is malformed"),
        None => error,
    }
}

fn from_multer(error: &multer::Error, limit: u64) -> FormatError {
    match error {
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            FormatError::PayloadTooLarge { limit }
        }
        multer::Error::UnknownField { .. } => FormatError::bad_request("unexpected form field"),
        _ => FormatError::bad_request("the multipart body is malformed"),
    }
}

/// Removes a staged upload that will not be finalized. A failure is logged and not returned:
/// the caller already has a better error to report, and the reaper collects what is left.
pub(crate) async fn discard_staged(cx: &FormatContext, path: &NormalizedPath) {
    if let Err(error) = cx.hosted().delete(path).await {
        tracing::warn!(%error, path = %path, "a staged pub upload could not be removed");
    }
}

/// Removes staged uploads older than the configured age. Runs as a tracked task when a
/// publication starts, bounded to one batch per run.
async fn reap(cx: FormatContext, cfg: PubConfig) {
    let cutoff = cx.now() - cfg.staging_ttl;
    let mut reaped = 0usize;
    let mut after = None;
    loop {
        let page = match cx
            .ports()
            .assets
            .list(
                cx.repository().id,
                STAGING_PREFIX,
                &rustryx_core::ports::PageRequest {
                    limit: REAP_PAGE,
                    after: after.take(),
                },
            )
            .await
        {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(%error, "listing staged pub uploads failed");
                break;
            }
        };
        for asset in page.items.iter().filter(|asset| asset.created_at < cutoff) {
            if reaped >= cfg.reap_batch {
                break;
            }
            let Ok(path) = NormalizedPath::parse(&asset.path) else {
                continue;
            };
            match cx.hosted().delete(&path).await {
                Ok(_) => reaped += 1,
                Err(error) => tracing::warn!(%error, "a stale pub upload could not be removed"),
            }
        }
        match page.next {
            Some(token) if reaped < cfg.reap_batch => after = Some(token),
            _ => break,
        }
    }
    if reaped > 0 {
        crate::metrics::staged_reaped(reaped);
    }
}
