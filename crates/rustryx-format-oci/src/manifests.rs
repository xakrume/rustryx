//! Manifests of a hosted repository: push, read, delete.
//!
//! Push, in order:
//!
//! 1. the body is read into memory under the size cap (the only buffered document);
//! 2. the document is parsed and validated against the schema of its media type;
//! 3. its digest is computed and, when the request names a digest, compared with it;
//! 4. every blob and child manifest it references must exist in the image, with the size the
//!    descriptor claims (`MANIFEST_BLOB_UNKNOWN` otherwise). A `subject` is NOT required to
//!    exist: the specification asks registries to accept it;
//! 5. the manifest is stored under its digest, with the component of that digest;
//! 6. a tag, if the request named one, is linked to the same blob;
//! 7. a `subject` records a referrer link, and the answer says so in `OCI-Subject`.
//!
//! A manifest that is already stored is not written again, so pushing the same image twice
//! works on repositories whose write policy is `allow_once`.

use std::collections::BTreeMap;

use axum_core::body::Body;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_core::error::{MutationError, StoreError};
use rustryx_core::model::{Asset, NewAssetLink};
use rustryx_format_core::{
    ComponentSpec, DeleteIntent, ExpectedChecksum, FormatError, HashAlgo, UploadSpec, WriteIntent,
};
use serde_json::json;

use crate::error::{OciError, OciErrorCode};
use crate::headers::{DOCKER_CONTENT_DIGEST, OCI_SUBJECT, set_text, stamp_version, status_only};
use crate::ops::Op;
use crate::parse::digest::{Digest, DigestAlgorithm};
use crate::parse::manifest::{Descriptor, ParsedManifest};
use crate::parse::media_type::Accept;
use crate::parse::name::RepositoryName;
use crate::parse::reference::{Reference, Tag};
use crate::store;

/// Referenced objects looked up at once while a manifest is checked.
const CHECK_CONCURRENCY: usize = 8;

/// `GET` and `HEAD` of a manifest by tag or digest.
pub(crate) async fn read(op: &Op<'_>) -> Result<Response<Body>, OciError> {
    let asset = store::get_asset(op.cx, op.req.path.as_str())
        .await?
        .ok_or_else(OciError::manifest_unknown)?;
    negotiate(op, &asset.content_type)?;
    serve(op, &asset).await
}

/// Serves a stored manifest asset with its digest header.
pub(crate) async fn serve(op: &Op<'_>, asset: &Asset) -> Result<Response<Body>, OciError> {
    let mut response = op.cx.hosted().download(op.req).await.map_err(|error| {
        OciError::from(error).not_found_as(
            OciErrorCode::ManifestUnknown,
            "manifest unknown to registry",
        )
    })?;
    let digest = Digest::from_sha256_hex(&asset.checksums.sha256)
        .map_err(|_| OciError::internal("a stored checksum is not a sha256 digest"))?;
    set_text(
        response.headers_mut(),
        DOCKER_CONTENT_DIGEST,
        &digest.to_string(),
    );
    stamp_version(response.headers_mut());
    Ok(response)
}

/// Content negotiation: the stored media type must be acceptable to the client. A client
/// that sends no `Accept` takes whatever is stored.
pub(crate) fn negotiate(op: &Op<'_>, media_type: &str) -> Result<(), OciError> {
    let accept = Accept::parse(op.header_values(&header::ACCEPT));
    if accept.accepts(media_type) {
        return Ok(());
    }
    Err(OciError::manifest_unknown()
        .with_detail(json!({ "mediaType": media_type, "reason": "not acceptable to the client" })))
}

/// `PUT` of a manifest. See the module docs for the steps.
pub(crate) async fn put(
    op: &Op<'_>,
    name: &RepositoryName,
    reference: &Reference,
    body: Body,
) -> Result<Response<Body>, OciError> {
    let bytes = read_body(op, body).await?;
    let content_type = op.req.head.header(&header::CONTENT_TYPE);
    let manifest = ParsedManifest::parse(&bytes, content_type, op.config().max_descriptors)
        .map_err(|error| OciError::manifest_invalid(error.to_string()))?;
    let digest = match reference {
        Reference::Digest(declared) => {
            let computed = Digest::of(declared.algorithm(), &bytes);
            if computed != *declared {
                return Err(OciError::digest_invalid(
                    "the digest of the manifest does not match the digest in the URL",
                ));
            }
            computed
        }
        Reference::Tag(_) => Digest::of(DigestAlgorithm::Sha256, &bytes),
    };
    check_references(op, name, &manifest).await?;
    let stored = store_manifest(op, name, &manifest, &digest, bytes).await?;
    if let Reference::Tag(tag) = reference {
        link_tag(op, name, tag, &stored).await?;
    }
    if let Some(subject) = &manifest.subject {
        link_referrer(op, name, &subject.digest, &digest, &stored).await?;
    }
    let mut response = status_only(StatusCode::CREATED);
    let headers = response.headers_mut();
    set_text(
        headers,
        header::LOCATION,
        &op.url(&format!("{name}/manifests/{digest}")),
    );
    set_text(headers, DOCKER_CONTENT_DIGEST, &digest.to_string());
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    if let Some(subject) = &manifest.subject {
        set_text(headers, OCI_SUBJECT, &subject.digest.to_string());
    }
    Ok(response)
}

/// `DELETE` of a manifest: by tag it removes the tag only, by digest the manifest with all
/// its tags.
pub(crate) async fn delete(op: &Op<'_>, reference: &Reference) -> Result<Response<Body>, OciError> {
    let path = op.req.path.as_str();
    let asset = store::get_asset(op.cx, path)
        .await?
        .ok_or_else(OciError::manifest_unknown)?;
    match (reference, asset.component_id) {
        (Reference::Digest(_), Some(component)) => {
            let intent =
                DeleteIntent::new(op.cx.repository(), op.cx.principal(), &op.req.path, &asset);
            op.cx
                .ports()
                .policy
                .check_delete(&intent)
                .await
                .map_err(FormatError::from)?;
            // The component owns the manifest asset, every tag asset that points at it and
            // the referrer links it made: one transaction, with the immutability, hold and
            // read only guards of the port.
            match op
                .cx
                .ports()
                .components
                .delete_checked(component, &op.write_options())
                .await
            {
                Ok(()) => {}
                Err(MutationError::Store(StoreError::NotFound(_))) => {
                    return Err(OciError::manifest_unknown());
                }
                Err(error) => return Err(FormatError::from(error).into()),
            }
            crate::audit::record(
                op.cx,
                "oci.manifest.delete",
                format!("{}:{path}", op.cx.repository().name),
                json!({ "size_bytes": asset.size_bytes }),
            )
            .await;
        }
        _ => {
            // A tag, or a manifest asset without a component: just this path.
            match op.cx.hosted().delete(&op.req.path).await {
                Ok(_) => {}
                Err(FormatError::NotFound) => return Err(OciError::manifest_unknown()),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(status_only(StatusCode::ACCEPTED))
}

/// Reads the manifest body under the cap, with the same idle timeout as every upload.
async fn read_body(op: &Op<'_>, body: Body) -> Result<Bytes, OciError> {
    let cap = op.config().manifest_max_bytes;
    let too_large = || {
        OciError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            OciErrorCode::ManifestInvalid,
            format!("the manifest is larger than {cap} bytes"),
        )
    };
    if op
        .req
        .head
        .content_length()
        .is_some_and(|length| length > cap)
    {
        return Err(too_large());
    }
    let idle = op.cx.limits().body_idle_timeout;
    let mut data = body.into_data_stream();
    let mut buffer = BytesMut::new();
    loop {
        let next = tokio::time::timeout(idle, data.next()).await.map_err(|_| {
            OciError::new(
                StatusCode::REQUEST_TIMEOUT,
                OciErrorCode::ManifestInvalid,
                "the manifest was not received in time",
            )
        })?;
        let Some(chunk) = next else { break };
        let chunk =
            chunk.map_err(|_| OciError::manifest_invalid("the request body could not be read"))?;
        if (buffer.len() as u64).saturating_add(chunk.len() as u64) > cap {
            return Err(too_large());
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer.freeze())
}

/// Every blob and child manifest the document names must exist, with the stated size.
async fn check_references(
    op: &Op<'_>,
    name: &RepositoryName,
    manifest: &ParsedManifest,
) -> Result<(), OciError> {
    let mut wanted: BTreeMap<String, &Descriptor> = BTreeMap::new();
    for blob in manifest.required_blobs() {
        wanted.insert(store::blob_path(name, &blob.digest), blob);
    }
    for child in manifest.required_children() {
        wanted.insert(store::manifest_path(name, &child.digest.to_string()), child);
    }
    let entries: Vec<(String, &Descriptor)> = wanted.into_iter().collect();
    for chunk in entries.chunks(CHECK_CONCURRENCY) {
        let results = futures::future::join_all(
            chunk
                .iter()
                .map(|(path, descriptor)| check_one(op, path, descriptor)),
        )
        .await;
        for result in results {
            result?;
        }
    }
    Ok(())
}

/// One referenced object: it exists and has the size the descriptor says.
async fn check_one(op: &Op<'_>, path: &str, descriptor: &Descriptor) -> Result<(), OciError> {
    match store::get_asset(op.cx, path).await? {
        None => Err(OciError::manifest_blob_unknown(
            &descriptor.digest.to_string(),
        )),
        Some(asset) if asset.size_bytes != descriptor.size => Err(OciError::manifest_invalid(
            "the size of a referenced blob does not match its descriptor",
        )
        .with_detail(json!({ "digest": descriptor.digest.to_string() }))),
        Some(_) => Ok(()),
    }
}

/// Stores the manifest under its digest, unless it is already there.
async fn store_manifest(
    op: &Op<'_>,
    name: &RepositoryName,
    manifest: &ParsedManifest,
    digest: &Digest,
    bytes: Bytes,
) -> Result<Asset, OciError> {
    let path = store::manifest_path(name, &digest.to_string());
    if let Some(existing) = store::get_asset(op.cx, &path).await? {
        return Ok(existing);
    }
    let mut spec = UploadSpec::new(Op::path(&path)?);
    spec.content_type = Some(manifest.media_type().to_owned());
    spec.content_length = Some(bytes.len() as u64);
    spec.component = Some(ComponentSpec {
        group: None,
        name: name.as_str().to_owned(),
        version: Some(digest.to_string()),
    });
    if digest.algorithm() == DigestAlgorithm::Sha256 {
        spec.expected = Some(
            ExpectedChecksum::new(HashAlgo::Sha256, digest.hex())
                .map_err(|_| OciError::internal("a validated digest is not a valid checksum"))?,
        );
    }
    match op.cx.hosted().upload(spec, Body::from(bytes)).await {
        Ok(outcome) => Ok(outcome.asset),
        Err(error @ (FormatError::Conflict(_) | FormatError::Policy(_))) => {
            // Another request stored the same digest between the look-up and the write: the
            // content is the digest, so that copy is the one wanted.
            match store::get_asset(op.cx, &path).await? {
                Some(existing) => Ok(existing),
                None => Err(tag_or_manifest_conflict(error)),
            }
        }
        Err(FormatError::DigestMismatch) => Err(OciError::digest_invalid(
            "the digest of the manifest does not match the stored content",
        )),
        Err(error) => Err(error.into()),
    }
}

/// Points a tag at the stored manifest: one asset that shares the manifest's blob.
async fn link_tag(
    op: &Op<'_>,
    name: &RepositoryName,
    tag: &Tag,
    manifest: &Asset,
) -> Result<(), OciError> {
    let path = store::manifest_path(name, tag.as_str());
    let existing = store::get_asset(op.cx, &path).await?;
    if existing
        .as_ref()
        .is_some_and(|tag_asset| tag_asset.checksums.sha256 == manifest.checksums.sha256)
    {
        // Already pointing there: pushing the same image again changes nothing.
        return Ok(());
    }
    let normalized = Op::path(&path)?;
    let mut intent = WriteIntent::new(op.cx.repository(), op.cx.principal(), &normalized);
    intent.existing = existing.as_ref();
    op.cx
        .ports()
        .policy
        .check_write(&intent)
        .await
        .map_err(FormatError::from)?;
    let mut link = NewAssetLink::new(
        op.cx.repository().id,
        path.clone(),
        manifest.content_type.clone(),
        manifest.blob_id,
    );
    link.component_id = manifest.component_id;
    link.created_by = op.cx.principal().user_id;
    op.cx
        .ports()
        .assets
        .link(&link, &op.write_options())
        .await
        .map_err(|error| tag_or_manifest_conflict(FormatError::from(error)))?;
    crate::audit::record(
        op.cx,
        if existing.is_some() {
            "oci.tag.move"
        } else {
            "oci.tag.create"
        },
        format!("{}:{path}", op.cx.repository().name),
        json!({ "sha256": manifest.checksums.sha256 }),
    )
    .await;
    Ok(())
}

/// Records that `referrer` has `subject`: a link asset that shares the referrer's blob.
async fn link_referrer(
    op: &Op<'_>,
    name: &RepositoryName,
    subject: &Digest,
    referrer: &Digest,
    manifest: &Asset,
) -> Result<(), OciError> {
    let path = store::referrer_path(name, subject, referrer);
    if store::get_asset(op.cx, &path).await?.is_some() {
        return Ok(());
    }
    let mut link = NewAssetLink::new(
        op.cx.repository().id,
        path,
        manifest.content_type.clone(),
        manifest.blob_id,
    );
    link.component_id = manifest.component_id;
    link.created_by = op.cx.principal().user_id;
    op.cx
        .ports()
        .assets
        .link(&link, &op.write_options())
        .await
        .map_err(FormatError::from)?;
    Ok(())
}

/// A refused tag or manifest write, in the words of the registry.
fn tag_or_manifest_conflict(error: FormatError) -> OciError {
    match &error {
        FormatError::Conflict(_) => OciError::new(
            StatusCode::CONFLICT,
            OciErrorCode::Denied,
            "the tag or manifest already exists and cannot be overwritten",
        ),
        _ => OciError::from(error),
    }
}

#[cfg(test)]
mod tests;
