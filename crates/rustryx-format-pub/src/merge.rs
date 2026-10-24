//! Group behaviour: merging member listings and claiming package names.
//!
//! The rules are the ones of the format requirements (section 1.6.1), and each is held by a
//! specific mechanism:
//!
//! * **Union by exact version.** Entries are keyed by the parsed `SemVer` (build metadata
//!   included), so `1.0.0+1` and `1.0.0+2` are two versions and `1.0.0` offered by two members
//!   is one.
//! * **The first member wins a duplicate.** Documents arrive in group order and
//!   [`PackageDoc::new`] keeps the first entry of a version. The archive a client then
//!   downloads comes from the first member that has it (format-core's group pipeline), which is
//!   the member whose entry won, so the `archive_sha256` in the merged entry describes the bytes
//!   served. If that member is unavailable at download time the next one answers and the client's
//!   checksum comparison refuses the mismatch, which fails closed.
//! * **A hosted member that has the package pins the name.** That is a claim
//!   ([`PubClaimer`]): format-core's group pipeline does not ask a proxy member that comes after
//!   a hosted member that has the name, for the listing, the version document and the archive
//!   alike. The merger never sees such a member's document, so it cannot add versions of a pinned
//!   name. The claim is evaluated against components, which is why a hosted publish creates one.
//! * **`retracted` stays with the version** it came with, because the winning entry is copied
//!   whole.
//! * **`isDiscontinued` and `replacedBy`** come from the member that holds the claim, else from
//!   the first member that has the package.
//! * **`latest` is recomputed** from the merged versions.
//!
//! The merger is a pure function over documents that format-core read under its memory cap and
//! runs on the blocking pool. A member document it cannot read fails the merge (422): the
//! documents are produced by this format's own handlers, so an unreadable one is a bug or a
//! corrupted store, and silently leaving a member out would make the answer incomplete without
//! telling the client.

use bytes::Bytes;
use rustryx_format_core::{
    ArtifactRequest, FormatError, GroupIndexMerger, GroupNameClaimer, MergeInput, MergedDocument,
    NameClaim,
};

use crate::document::{ArchiveUrls, PackageDoc};
use crate::error::MEDIA_TYPE;
use crate::metrics;
use crate::route::Route;

/// Merges the listings of the members of a group.
#[derive(Debug, Clone)]
pub struct PubMerger {
    max_versions: usize,
}

impl PubMerger {
    /// A merger that refuses a member listing with more than `max_versions` versions.
    #[must_use]
    pub fn new(max_versions: usize) -> Self {
        Self { max_versions }
    }
}

impl GroupIndexMerger for PubMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        match Route::parse(&input.request.path)? {
            Route::List(name) => self.merge_lists(&name, &input),
            Route::Version(..) => {
                // One version: the first member that has it, as for an archive.
                let first = input
                    .documents
                    .first()
                    .ok_or_else(|| FormatError::internal("merge without documents"))?;
                Ok(MergedDocument {
                    content_type: MEDIA_TYPE.to_owned(),
                    body: Bytes::clone(&first.body),
                })
            }
            _ => Err(FormatError::internal(
                "the pub merger was asked to merge a document that is not an index",
            )),
        }
    }
}

impl PubMerger {
    fn merge_lists(
        &self,
        name: &crate::name::PackageName,
        input: &MergeInput<'_>,
    ) -> Result<MergedDocument, FormatError> {
        let mut documents = Vec::with_capacity(input.documents.len());
        for member in &input.documents {
            match PackageDoc::parse(&member.body, name, &ArchiveUrls::Keep, self.max_versions) {
                Ok(parsed) => documents.push((parsed.doc, member.holds_claim)),
                Err(error) => {
                    metrics::merge_skipped();
                    tracing::warn!(%error, member = %member.member.name, "a member listing could not be read");
                    return Err(FormatError::unprocessable(
                        "a member repository returned a listing that could not be read",
                    ));
                }
            }
        }
        // Package level fields belong to the member that holds the claim, else to the first.
        let source = documents
            .iter()
            .find(|(_, holds_claim)| *holds_claim)
            .or_else(|| documents.first());
        let (discontinued, replaced_by) = source.map_or((false, None), |(doc, _)| {
            (doc.discontinued, doc.replaced_by.clone())
        });
        let entries = documents
            .iter()
            .flat_map(|(doc, _)| doc.versions().iter().cloned());
        let merged = PackageDoc::new(name.clone(), discontinued, replaced_by, entries);
        Ok(MergedDocument {
            content_type: MEDIA_TYPE.to_owned(),
            body: Bytes::from(merged.to_bytes()),
        })
    }
}

/// Names the package a request addresses, for dependency confusion protection.
#[derive(Debug, Clone, Copy, Default)]
pub struct PubClaimer;

impl GroupNameClaimer for PubClaimer {
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim> {
        match Route::parse(&request.path).ok()? {
            Route::List(name)
            | Route::Version(name, _)
            | Route::Archive(name, _)
            | Route::Advisories(name) => Some(NameClaim::package(name.to_string())),
            Route::NewUpload | Route::Upload { .. } | Route::Finish { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
