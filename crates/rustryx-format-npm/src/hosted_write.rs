//! Publishing to a hosted repository: `PUT /<package>`.
//!
//! The body is a JSON document whose `_attachments` entry carries the tarball as base64. It is
//! never held whole: [`crate::scan::Scanner`] splits it into a bounded metadata buffer and a
//! decoded byte stream, and the byte stream goes straight into the hosted upload pipeline
//! ([`crate::attach::AttachmentStream`]). The order of events:
//!
//! 1. read the body until the payload starts (the head), refusing a document that is not a
//!    publish of this package, or that carries a version that exists;
//! 2. stream the tarball into the blob store as a create-only asset of the version's component;
//!    the pipeline hashes it, the stream verifies the SHA-512 the client declared, and the asset
//!    becomes visible only if the whole document was valid;
//! 3. add the version to the package document (compare and set on the next generation);
//! 4. if step 3 fails, remove the tarball again, so the client can retry.
//!
//! A `PUT` without attachment is not a publish: it is how `npm deprecate` writes back the
//! document ([`NpmHandler::deprecate_by_document`]), and with the `-rev` form how npm unpublishes
//! a version ([`NpmHandler::unpublish_by_document`]).

use std::sync::Arc;
use std::time::Duration;

use axum_core::body::{Body, BodyDataStream};
use futures::StreamExt;
use http::{Response, StatusCode};
use rustryx_format_core::{
    ArtifactRequest, ComponentSpec, FormatContext, FormatError, NormalizedPath, OverwriteRule,
    UploadSpec,
};
use semver::Version;

use crate::attach::{AttachmentStream, OutcomeSlot, StreamFailure};
use crate::disttags::LATEST;
use crate::docstore::{Change, DocStore};
use crate::handler::NpmHandler;
use crate::metrics;
use crate::name::PackageName;
use crate::packument::{Packument, rev_of};
use crate::publish::{PublishRequest, normalized_manifest};
use crate::respond;
use crate::scan::{ScanError, ScanLimits, Scanner};

/// Maps a scanner refusal to the answer the client gets.
pub(crate) fn scan_error(error: ScanError) -> FormatError {
    match error {
        ScanError::MetaTooLarge { limit } => FormatError::PayloadTooLarge {
            limit: limit as u64,
        },
        other => FormatError::bad_request(other.to_string()),
    }
}

fn published_over(version: &Version) -> FormatError {
    FormatError::conflict(format!(
        "You cannot publish over the previously published versions: {version}."
    ))
}

/// Reads the body until the payload of the attachment starts, or to its end when there is none.
async fn read_head(
    source: &mut BodyDataStream,
    scanner: &mut Scanner,
    idle: Duration,
) -> Result<(), FormatError> {
    loop {
        let next = tokio::time::timeout(idle, source.next())
            .await
            .map_err(|_| FormatError::RequestTimeout)?;
        match next {
            None => return scanner.finish().map_err(scan_error),
            Some(Err(_)) => return Err(FormatError::bad_request("request body could not be read")),
            Some(Ok(chunk)) => {
                scanner.feed(&chunk).map_err(scan_error)?;
                if scanner.data_started() {
                    return Ok(());
                }
            }
        }
    }
}

impl NpmHandler {
    /// `PUT /<package>` and `PUT /<package>/-rev/<rev>` on a hosted repository.
    pub(crate) async fn hosted_put(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        package: &PackageName,
        body: Body,
        revision: bool,
    ) -> Result<Response<Body>, FormatError> {
        self.refuse_oversized(cx, req)?;
        let mut scanner = Scanner::new(ScanLimits {
            max_meta_bytes: self.config.max_publish_metadata_bytes,
            ..ScanLimits::default()
        });
        let mut source = body.into_data_stream();
        read_head(&mut source, &mut scanner, cx.limits().body_idle_timeout).await?;
        if scanner.data_started() {
            if revision {
                return Err(FormatError::bad_request(
                    "a document with attachments cannot be sent to a -rev route",
                ));
            }
            let result = self.publish(cx, package, source, scanner).await;
            match &result {
                Ok(_) => metrics::publish_result("ok"),
                Err(error) => metrics::publish_result(error.code()),
            }
            return result;
        }
        let meta = scanner.meta().to_vec();
        if revision {
            self.unpublish_by_document(cx, package, meta).await
        } else {
            self.deprecate_by_document(cx, package, meta).await
        }
    }

    /// Refuses a declared length that cannot be a publish within the limits, before any byte is
    /// read: base64 inflates the tarball by a third, plus the metadata part.
    fn refuse_oversized(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
    ) -> Result<(), FormatError> {
        let Some(length) = req.head.content_length() else {
            return Ok(());
        };
        let blob = cx.limits().max_blob_bytes;
        let allowed = blob
            .saturating_mul(4)
            .div_ceil(3)
            .saturating_add(self.config.max_publish_metadata_bytes as u64)
            .saturating_add(64);
        if length > allowed {
            return Err(FormatError::PayloadTooLarge { limit: blob });
        }
        Ok(())
    }

    async fn publish(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        source: BodyDataStream,
        scanner: Scanner,
    ) -> Result<Response<Body>, FormatError> {
        // A new package is created under the strict naming rules; reading and proxying keep
        // accepting legacy names.
        PackageName::parse_new(&package.full()).map_err(|_| {
            FormatError::bad_request(
                "package names must be lowercase and use only URL-safe characters",
            )
        })?;
        let attachment = scanner
            .attachment_name()
            .map(str::to_owned)
            .ok_or_else(|| FormatError::internal("the payload started without an attachment"))?;
        let wait = cx.limits().blocking_permit_wait;
        let head = scanner.head_document();
        let request = {
            let (package, attachment) = (package.clone(), attachment.clone());
            self.blocking
                .run(wait, move || {
                    PublishRequest::parse(&head, &package, &attachment)
                })
                .await??
        };
        let store = DocStore::new(cx, package);
        if let Some((document, _)) = store
            .current(&self.blocking, self.config.max_packument_bytes)
            .await?
            && document.has_release(&request.version)
        {
            return Err(published_over(&request.version));
        }

        let slot = OutcomeSlot::new();
        let stream = AttachmentStream::new(source, scanner, request.claims.sha512, slot.clone());
        let outcome = cx
            .hosted()
            .upload(
                Self::tarball_spec(package, &request)?,
                Body::from_stream(stream),
            )
            .await
            .map_err(|error| translate_upload_error(error, &slot, &request.version))?;
        let Some(completed) = slot.take_completed() else {
            self.rollback(cx, package, &request.version).await;
            return Err(FormatError::internal(
                "the attachment was stored but its stream did not complete",
            ));
        };
        metrics::publish_bytes(completed.size);

        if let Err(error) = self
            .verify_published(
                cx,
                package,
                &attachment,
                &request,
                &outcome.asset.checksums.sha256,
                &completed.meta,
            )
            .await
        {
            self.rollback(cx, package, &request.version).await;
            return Err(error);
        }
        let manifest = normalized_manifest(
            &request,
            &outcome.asset.checksums.sha1,
            &completed.sha512,
            &cx.principal().username,
        );
        let updated = self.add_version(cx, package, &request, manifest).await;
        match updated {
            Ok(updated) => Ok(respond::ok_with_id(
                StatusCode::CREATED,
                &package.full(),
                &rev_of(updated.document.versions().len(), &updated.digest),
            )),
            Err(error) => {
                self.rollback(cx, package, &request.version).await;
                Err(error)
            }
        }
    }

    fn tarball_spec(
        package: &PackageName,
        request: &PublishRequest,
    ) -> Result<UploadSpec, FormatError> {
        let canonical = package.canonical();
        let mut spec = UploadSpec::new(NormalizedPath::parse(
            &package.tarball_path(&request.version),
        )?);
        spec.content_type = Some("application/octet-stream".to_owned());
        // The decoded length is not known before the payload has been read; the byte limit of
        // the pipeline is enforced on the bytes that arrive.
        spec.content_length = None;
        spec.component = Some(ComponentSpec {
            group: canonical.scope().map(str::to_owned),
            name: canonical.name().to_owned(),
            version: Some(request.version.to_string()),
        });
        spec.expected = request.claims.pipeline_expectation();
        // A version is immutable whatever the write policy of the repository says.
        spec.overwrite = OverwriteRule::Never;
        Ok(spec)
    }

    /// What can only be checked once the whole document has been read: the digests the pipeline
    /// could not verify, and that the complete document says the same as its head.
    async fn verify_published(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        attachment: &str,
        request: &PublishRequest,
        sha256_hex: &str,
        final_meta: &[u8],
    ) -> Result<(), FormatError> {
        if let Some(claimed) = request.claims.sha256
            && !hex::encode(claimed).eq_ignore_ascii_case(sha256_hex)
        {
            return Err(FormatError::DigestMismatch);
        }
        let (package, attachment, meta) =
            (package.clone(), attachment.to_owned(), final_meta.to_vec());
        let reparsed = self
            .blocking
            .run(cx.limits().blocking_permit_wait, move || {
                PublishRequest::parse(&meta, &package, &attachment)
            })
            .await?;
        match reparsed {
            Ok(complete) if complete == *request => Ok(()),
            _ => Err(FormatError::bad_request(
                "the publish document is not valid JSON or changed after the attachment",
            )),
        }
    }

    async fn add_version(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        request: &PublishRequest,
        manifest: serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::docstore::Updated, FormatError> {
        let (package_owned, version, tags, now) = (
            package.clone(),
            request.version.clone(),
            request.tags.clone(),
            cx.now(),
        );
        let change = Arc::new(move |current: Option<Packument>, applied: bool| {
            let mut document = current.unwrap_or_else(|| Packument::new(&package_owned));
            if document.has_release(&version) {
                // The tarball is create-only, so a manifest with this tarball's digest in the
                // document can only be the one this call wrote: a writer built on it before it
                // was superseded. That is a success, not a republish.
                let ours = applied
                    && document
                        .versions()
                        .get(&version)
                        .and_then(|m| m.get("dist"))
                        .and_then(|d| d.get("integrity"))
                        == manifest.get("dist").and_then(|d| d.get("integrity"));
                return if ours {
                    Ok(Change::Keep(document))
                } else {
                    Err(published_over(&version))
                };
            }
            let mut tags = tags.clone();
            if tags.is_empty() && document.dist_tags().get(LATEST).is_none() {
                tags.push(LATEST.to_owned());
            }
            document
                .add_version(version.clone(), manifest.clone(), &tags, now)
                .map_err(|e| FormatError::bad_request(e.to_string()))?;
            Ok(Change::Replace(document))
        });
        DocStore::new(cx, package)
            .update(&self.blocking, self.config.max_packument_bytes, change)
            .await
    }

    /// Removes the tarball of a publish that could not be completed, so that the client can
    /// retry the same version. A failure here is logged: the version is then stuck until an
    /// administrator deletes the component.
    async fn rollback(&self, cx: &FormatContext, package: &PackageName, version: &Version) {
        if let Err(error) = self.remove_version_content(cx, package, version).await {
            tracing::error!(%error, package = %package, %version, "could not remove the tarball of a failed publish");
        }
    }
}

/// Turns what the upload pipeline reported into the answer of a publish.
///
/// The pipeline reports every error of the attachment stream as one generic bad request; the real
/// reason is in the slot. A conflict on the tarball path is the registry's well known refusal of a
/// republish.
fn translate_upload_error(
    error: FormatError,
    slot: &OutcomeSlot,
    version: &Version,
) -> FormatError {
    match slot.take_failure() {
        Some(StreamFailure::Scan(scan)) => scan_error(scan),
        Some(StreamFailure::Integrity) => FormatError::DigestMismatch,
        Some(StreamFailure::Read) => FormatError::bad_request("request body could not be read"),
        None => match error {
            FormatError::Conflict(_) => published_over(version),
            other => other,
        },
    }
}
