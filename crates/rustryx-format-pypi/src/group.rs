//! Groups: merging the index pages of the members, and the dependency confusion claim.
//!
//! The attack (dependency confusion): an internal package `acme-billing` lives in a hosted member.
//! Somebody publishes `acme-billing 99.0.0` on the public index that a proxy member mirrors.
//! pip prefers the highest version, so a group that merged both pages would install the
//! public package. Two protections, the first the most important one for `PyPI`:
//!
//! * **The claim** ([`PypiClaimer`]): when a hosted member has any component named like the
//!   request's project (the name is PEP 503 normalized, which is how components are stored, so
//!   `Acme_Billing` cannot dodge it), the group does not ask proxy members that follow it at all,
//!   neither for the page nor for a file. The decision is made by the group pipeline of
//!   `rustryx-format-core` against the component table, regardless of what the caller may
//!   read, and fails closed.
//! * **The merge** ([`PypiMerger`]): when a hosted member holds the claim, the documents of every
//!   non-hosted member are dropped from the merged page even if the member sits before the
//!   claiming one in the group order. Page merging therefore does not depend on member order.
//!   File requests do: list hosted members before proxy members.
//!
//! Duplicates are resolved by file name: the first member that lists a file wins, in group order.

use std::collections::HashSet;

use bytes::Bytes;
use rustryx_core::model::RepoType;
use rustryx_format_core::{
    ArtifactRequest, FormatError, GroupIndexMerger, GroupNameClaimer, MemberDocument, MergeInput,
    MergedDocument, NameClaim,
};

use crate::name::ProjectName;
use crate::negotiate::{Representation, negotiate};
use crate::page::{MAX_FILES_PER_PAGE, MAX_PROJECTS_PER_INDEX, ProjectPage, RootPage};
use crate::route::{Route, parse_route};
use crate::{simple_html, simple_json};

/// Claims the project a request addresses.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PypiClaimer;

impl GroupNameClaimer for PypiClaimer {
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim> {
        // `classify` put the normalized name into the coordinate; the root index and an upload
        // address no single project.
        request
            .coordinate
            .as_ref()
            .and_then(|coordinate| coordinate.name.clone())
            .map(NameClaim::package)
    }
}

/// Merges the pages of the members.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PypiMerger;

fn is_json(document: &MemberDocument) -> bool {
    document
        .content_type
        .as_deref()
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("json"))
}

fn member_page(document: &MemberDocument, name: &ProjectName) -> Option<ProjectPage> {
    let parsed = if is_json(document) {
        simple_json::parse_project(&document.body, name.as_str())
    } else {
        simple_html::parse_project(&document.body, None, name.as_str())
    };
    match parsed {
        Ok(page) => Some(page),
        Err(error) => {
            tracing::warn!(member = %document.member.name, %error, "a group member served an index that cannot be read; skipping it");
            None
        }
    }
}

fn member_root(document: &MemberDocument) -> Option<RootPage> {
    let parsed = if is_json(document) {
        simple_json::parse_root(&document.body)
    } else {
        simple_html::parse_root(&document.body)
    };
    match parsed {
        Ok(root) => Some(root),
        Err(error) => {
            tracing::warn!(member = %document.member.name, %error, "a group member served a root index that cannot be read; skipping it");
            None
        }
    }
}

fn merged(representation: Representation, body: Vec<u8>) -> MergedDocument {
    MergedDocument {
        content_type: representation.content_type().to_owned(),
        body: Bytes::from(body),
    }
}

fn merge_project(
    documents: &[MemberDocument],
    name: &ProjectName,
    representation: Representation,
) -> Result<MergedDocument, FormatError> {
    // A name that a hosted member holds belongs to the hosted members, wherever they sit.
    let claimed = documents.iter().any(|document| document.holds_claim);
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut readable = 0usize;
    for document in documents {
        if claimed && document.member.repo_type != RepoType::Hosted {
            continue;
        }
        let Some(page) = member_page(document, name) else {
            continue;
        };
        readable += 1;
        for file in page.files {
            if seen.insert(file.filename.clone()) {
                files.push(file);
            }
        }
        if files.len() > MAX_FILES_PER_PAGE {
            return Err(FormatError::IndexTooLarge {
                limit: MAX_FILES_PER_PAGE as u64,
            });
        }
    }
    if readable == 0 {
        return Err(FormatError::unprocessable(
            "no member of the group served a readable index",
        ));
    }
    let mut page = ProjectPage {
        name: name.to_string(),
        files,
    };
    page.sort_files();
    Ok(merged(
        representation,
        if representation.is_json() {
            simple_json::render_project(&page)
        } else {
            simple_html::render_project(&page)
        },
    ))
}

fn merge_root(
    documents: &[MemberDocument],
    representation: Representation,
) -> Result<MergedDocument, FormatError> {
    let mut names = Vec::new();
    let mut readable = 0usize;
    for document in documents {
        if let Some(root) = member_root(document) {
            readable += 1;
            names.extend(root.projects);
        }
        if names.len() > MAX_PROJECTS_PER_INDEX {
            return Err(FormatError::IndexTooLarge {
                limit: MAX_PROJECTS_PER_INDEX as u64,
            });
        }
    }
    if readable == 0 {
        return Err(FormatError::unprocessable(
            "no member of the group served a readable index",
        ));
    }
    let root = RootPage::from_names(names);
    Ok(merged(
        representation,
        if representation.is_json() {
            simple_json::render_root(&root)
        } else {
            simple_html::render_root(&root)
        },
    ))
}

impl GroupIndexMerger for PypiMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let representation = negotiate(input.request.head.header(&http::header::ACCEPT))
            .unwrap_or(Representation::TextHtml);
        match parse_route(input.request.method(), &input.request.path)? {
            Route::Root => merge_root(&input.documents, representation),
            Route::Project { name } => merge_project(&input.documents, &name, representation),
            _ => Err(FormatError::unprocessable("not an index document")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::FileEntry;
    use rustryx_core::ids::RepositoryId;
    use rustryx_core::names::RepoName;
    use rustryx_format_core::MemberRef;

    fn member(name: &str, repo_type: RepoType) -> MemberRef {
        MemberRef {
            id: RepositoryId::generate(),
            name: RepoName::parse(name).unwrap(),
            repo_type,
        }
    }

    fn document(name: &str, repo_type: RepoType, files: &[&str], claim: bool) -> MemberDocument {
        let page = ProjectPage {
            name: "foo".into(),
            files: files
                .iter()
                .map(|f| FileEntry::new(*f, format!("https://r.test/packages/foo/{f}")))
                .collect(),
        };
        MemberDocument {
            member: member(name, repo_type),
            content_type: Some("application/vnd.pypi.simple.v1+json".into()),
            body: Bytes::from(simple_json::render_project(&page)),
            holds_claim: claim,
        }
    }

    fn foo() -> ProjectName {
        ProjectName::parse("foo").unwrap()
    }

    fn names(merged: &MergedDocument) -> Vec<String> {
        simple_json::parse_project(&merged.body, "foo")
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.filename)
            .collect()
    }

    #[test]
    fn the_first_member_that_lists_a_file_wins_and_versions_are_united() {
        let docs = [
            document("hosted", RepoType::Hosted, &["foo-1.0.tar.gz"], false),
            document(
                "proxy",
                RepoType::Proxy,
                &["foo-1.0.tar.gz", "foo-2.0.tar.gz"],
                false,
            ),
        ];
        let result = merge_project(&docs, &foo(), Representation::Json).unwrap();
        assert_eq!(names(&result), ["foo-1.0.tar.gz", "foo-2.0.tar.gz"]);
        let page = simple_json::parse_project(&result.body, "foo").unwrap();
        assert!(page.files[0].url.contains("/packages/foo/"));
        assert_eq!(result.content_type, "application/vnd.pypi.simple.v1+json");
    }

    #[test]
    fn must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_holds() {
        // The group pipeline normally leaves later proxies out; this holds even when a proxy
        // document reaches the merger (a proxy listed before the hosted member).
        let docs = [
            document("public", RepoType::Proxy, &["foo-99.0.tar.gz"], false),
            document("internal", RepoType::Hosted, &["foo-1.0.tar.gz"], true),
        ];
        let result = merge_project(&docs, &foo(), Representation::Json).unwrap();
        assert_eq!(names(&result), ["foo-1.0.tar.gz"]);
    }

    #[test]
    fn an_unreadable_member_is_skipped_and_all_unreadable_is_unprocessable() {
        let mut broken = document("proxy", RepoType::Proxy, &[], false);
        broken.body = Bytes::from_static(b"not json");
        let good = document("hosted", RepoType::Hosted, &["foo-1.0.tar.gz"], false);
        let result = merge_project(&[broken.clone(), good], &foo(), Representation::Json).unwrap();
        assert_eq!(names(&result), ["foo-1.0.tar.gz"]);
        assert!(matches!(
            merge_project(&[broken], &foo(), Representation::Json),
            Err(FormatError::Unprocessable(_))
        ));
        assert!(matches!(
            merge_project(&[], &foo(), Representation::Json),
            Err(FormatError::Unprocessable(_))
        ));
    }

    #[test]
    fn html_members_and_html_output_work() {
        let page = ProjectPage {
            name: "foo".into(),
            files: vec![FileEntry::new(
                "foo-3.0.tar.gz",
                "https://r.test/packages/foo/foo-3.0.tar.gz",
            )],
        };
        let html = MemberDocument {
            member: member("legacy", RepoType::Proxy),
            content_type: Some("text/html".into()),
            body: Bytes::from(simple_html::render_project(&page)),
            holds_claim: false,
        };
        let json = document("hosted", RepoType::Hosted, &["foo-1.0.tar.gz"], false);
        let result = merge_project(&[json, html], &foo(), Representation::TextHtml).unwrap();
        assert!(result.content_type.starts_with("text/html"));
        let text = String::from_utf8(result.body.to_vec()).unwrap();
        assert!(text.contains("foo-1.0.tar.gz") && text.contains("foo-3.0.tar.gz"));
    }

    #[test]
    fn roots_are_united_sorted_and_deduplicated() {
        let root = |names: &[&str], json: bool| {
            let root = RootPage::from_names(names.iter().map(|n| (*n).to_owned()).collect());
            MemberDocument {
                member: member("m", RepoType::Hosted),
                content_type: Some(if json {
                    "application/json".into()
                } else {
                    "text/html".into()
                }),
                body: Bytes::from(if json {
                    simple_json::render_root(&root)
                } else {
                    simple_html::render_root(&root)
                }),
                holds_claim: false,
            }
        };
        let result = merge_root(
            &[root(&["b", "a"], true), root(&["c", "a"], false)],
            Representation::Json,
        )
        .unwrap();
        assert_eq!(
            simple_json::parse_root(&result.body).unwrap().projects,
            ["a", "b", "c"]
        );
        assert!(merge_root(&[], Representation::Json).is_err());
    }

    #[test]
    fn the_claimer_uses_the_normalized_name_of_the_coordinate() {
        use rustryx_core::identity::{Action, Principal};
        use rustryx_core::ports::auth::Coordinate;
        use rustryx_format_core::{NormalizedPath, RequestHead};
        let head = RequestHead::new(
            http::Method::GET,
            "simple/foo",
            Principal::anonymous(),
            url::Url::parse("https://r.test").unwrap(),
        );
        let plain = ArtifactRequest::new(
            &head,
            NormalizedPath::parse("simple/foo").unwrap(),
            Action::Read,
        );
        assert_eq!(PypiClaimer.claim_for(&plain), None);
        let named = plain.with_coordinate(Coordinate::new(None, Some("foo-bar".into()), None));
        assert_eq!(
            PypiClaimer.claim_for(&named),
            Some(NameClaim::package("foo-bar"))
        );
    }
}
