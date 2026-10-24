//! Group repositories: reads, derived checksum files of merged metadata, the merger and the
//! dependency confusion claim.

use std::sync::Arc;

use axum_core::body::Body;
use http::{HeaderMap, Method, Response};
use rustryx_format_core::stream::read_bounded;
use rustryx_format_core::{
    ArtifactRequest, FormatContext, FormatError, GroupIndexMerger, GroupNameClaimer, MergeInput,
    MergedDocument, NameClaim, NormalizedPath, RequestHead,
};

use crate::checksum::digest_of_bytes;
use crate::handler::MavenHandler;
use crate::layout::{ChecksumAlgo, MavenPath, MetadataLevel};
use crate::metadata::{merge_documents, parse_metadata, write_metadata};
use crate::metrics;
use crate::response::{checksum_document, respond};

/// Headers of a merged answer that must survive when a checksum is derived from it: the marker
/// of an incomplete merge and the instruction not to cache it.
const CARRIED: [http::HeaderName; 2] = [
    http::HeaderName::from_static("x-rustryx-group-skipped"),
    http::header::CACHE_CONTROL,
];

impl MavenHandler {
    /// A read on a group repository.
    pub(crate) async fn group_get(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        parsed: Option<MavenPath>,
    ) -> Result<Response<Body>, FormatError> {
        // A path outside the layout has no group and artifact, so no claim could cover it: a
        // proxy member would answer for a name a hosted member owns. A group serves layout paths only.
        let Some(parsed) = parsed else {
            return Err(FormatError::NotFound);
        };
        match parsed {
            MavenPath::Metadata {
                level,
                checksum: Some(algo),
            } => self.merged_checksum(cx, req, &level, algo).await,
            _ => cx.group().resolve(req).await,
        }
    }

    /// The checksum of a merged document is the digest of the merged bytes, never a member's
    /// `.sha1`: a member's file describes the member's document, and a client verifying the
    /// merged metadata against it would fail on every group with more than one answering member.
    async fn merged_checksum(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        level: &MetadataLevel,
        algo: ChecksumAlgo,
    ) -> Result<Response<Body>, FormatError> {
        let document = document_request(req, level)?;
        let merged = cx.group().resolve(&document).await?;
        let (parts, body) = merged.into_parts();
        let bytes = read_bounded(body, cx.limits().group_merge_cap_bytes)
            .await
            .map_err(|error| FormatError::internal(format!("reading merged metadata: {error}")))?;
        let mut extra = HeaderMap::new();
        for name in &CARRIED {
            if let Some(value) = parts.headers.get(name) {
                extra.insert(name.clone(), value.clone());
            }
        }
        respond(
            &req.head,
            checksum_document(digest_of_bytes(algo, &bytes), None).extra(extra),
        )
    }
}

/// The request for the metadata document a checksum request describes: a `GET` without the
/// client's validators (they describe the checksum text, not the document).
fn document_request(
    req: &ArtifactRequest,
    level: &MetadataLevel,
) -> Result<ArtifactRequest, FormatError> {
    let mut head: RequestHead = (*req.head).clone();
    head.method = Method::GET;
    for name in [
        http::header::IF_NONE_MATCH,
        http::header::IF_MODIFIED_SINCE,
        http::header::IF_MATCH,
        http::header::IF_UNMODIFIED_SINCE,
        http::header::IF_RANGE,
        http::header::RANGE,
    ] {
        head.headers.remove(name);
    }
    let path = NormalizedPath::parse(&level.document_path())?;
    Ok(ArtifactRequest {
        head: Arc::new(head),
        path,
        ..req.clone()
    })
}

/// Merges the `maven-metadata.xml` documents of the members of a group.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MavenMerger;

impl GroupIndexMerger for MavenMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let Ok(MavenPath::Metadata { level, .. }) = MavenPath::parse(input.request.path.as_str())
        else {
            return Err(FormatError::unprocessable(
                "only maven-metadata.xml documents are merged",
            ));
        };
        let mut parsed = Vec::with_capacity(input.documents.len());
        for document in &input.documents {
            // A member that serves garbage (an HTML error page behind a proxy, a DOCTYPE) is
            // skipped, not fatal: it must not take the group down (contract of the merger).
            match parse_metadata(&document.body) {
                Ok(metadata) => parsed.push(metadata),
                Err(_) => metrics::metadata_op("merge", "member_skipped"),
            }
        }
        let Some(merged) = merge_documents(&level, &parsed) else {
            metrics::metadata_op("merge", "unprocessable");
            return Err(FormatError::unprocessable(
                "no member document could be merged",
            ));
        };
        metrics::metadata_op("merge", "ok");
        Ok(MergedDocument {
            content_type: "application/xml".to_owned(),
            body: write_metadata(&merged).into(),
        })
    }
}

/// Dependency confusion protection: the package a request addresses is `groupId` plus
/// `artifactId`. Once a hosted member of the group has any version of it, proxy members that come
/// later in the group are not asked about it, neither for the version list nor for a file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MavenClaimer;

impl GroupNameClaimer for MavenClaimer {
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim> {
        let parsed = MavenPath::parse(request.path.as_str()).ok()?;
        let (group, artifact) = parsed.group_and_artifact()?;
        Some(NameClaim::scoped(group, artifact))
    }
}
