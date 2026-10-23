//! Merging of index documents of the members of a group.

use bytes::Bytes;
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::{RepoType, Repository};
use rustryx_core::names::RepoName;

use crate::error::FormatError;
use crate::request::ArtifactRequest;

/// Who a member document came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRef {
    /// Member repository id.
    pub id: RepositoryId,
    /// Member repository name.
    pub name: RepoName,
    /// Hosted, proxy or (nested) group.
    pub repo_type: RepoType,
}

/// One member's answer for the requested index document.
#[derive(Debug, Clone)]
pub struct MemberDocument {
    /// The member that produced it.
    pub member: MemberRef,
    /// Its `Content-Type`, if it sent one.
    pub content_type: Option<String>,
    /// The document bytes. Already in client-ready form: URLs inside point to the
    /// repository the client addressed (the group), because members build them from
    /// [`crate::FormatContext::origin_repository`].
    pub body: Bytes,
    /// True when this member is a hosted member that holds the package-name claim.
    pub holds_claim: bool,
}

/// Input of [`GroupIndexMerger::merge`].
#[derive(Debug)]
pub struct MergeInput<'a> {
    /// The classified request (path, coordinate).
    pub request: &'a ArtifactRequest,
    /// The group being read.
    pub group: &'a Repository,
    /// Documents of the members that answered, **in group order**. Members that
    /// answered 404, were not authorized, were skipped because a hosted member
    /// claimed the name, or failed, are not in the list.
    pub documents: Vec<MemberDocument>,
    /// The name claim of this request, if the format has a claimer and the request
    /// addresses a package.
    pub claim: Option<crate::group::NameClaim>,
}

/// The merged document.
#[derive(Debug, Clone)]
pub struct MergedDocument {
    /// `Content-Type` of the result.
    pub content_type: String,
    /// The merged bytes.
    pub body: Bytes,
}

/// Merges the member documents of one index request into one.
///
/// ## Contract
///
/// * Runs on the blocking pool (permit taken before spawning), never on a runtime
///   worker: it may parse and serialize up to the memory cap of 64 MiB.
/// * The documents are in group order. **The ordering rules are the format's**:
///   which duplicate version wins (the first member that has it, by the group
///   order), how flags such as "retracted" combine, which member's package-level
///   fields win (the claim holder, else the first).
/// * It must be a pure function of its input, with no I/O.
/// * It must not panic on malformed documents: return [`FormatError::Unprocessable`]
///   or skip the member's document when the format can tell it apart safely.
///   A member that served garbage must not be able to take the group down.
pub trait GroupIndexMerger: Send + Sync {
    /// Merges the documents.
    ///
    /// # Errors
    /// A [`FormatError`] when the documents cannot be merged.
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError>;
}
