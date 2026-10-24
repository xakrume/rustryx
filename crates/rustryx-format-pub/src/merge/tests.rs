//! Tests of the group merge and the name claim.

use http::Method;
use rustryx_core::identity::{Action, Principal};
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::names::RepoName;
use rustryx_format_core::{MemberDocument, MemberRef, NormalizedPath, RequestHead};
use rustryx_testkit::fixtures;
use serde_json::{Value, json};
use url::Url;

use super::*;
use crate::document::VersionEntry;
use crate::name::PackageName;
use crate::version::PubVersion;

struct Fixture {
    group: Repository,
}

impl Fixture {
    fn new() -> Self {
        Self {
            group: fixtures::repository("pub-group", RepoFormat::Pub, RepoType::Group),
        }
    }

    fn request(&self, path: &str) -> ArtifactRequest {
        let head = RequestHead::new(
            Method::GET,
            path,
            Principal::anonymous(),
            Url::parse("https://rustryx.test").unwrap(),
        );
        ArtifactRequest::new(&head, NormalizedPath::parse(path).unwrap(), Action::Read).index()
    }

    fn merge(
        &self,
        path: &str,
        documents: Vec<MemberDocument>,
    ) -> Result<MergedDocument, FormatError> {
        let request = self.request(path);
        PubMerger::new(100).merge(MergeInput {
            request: &request,
            group: &self.group,
            documents,
            claim: None,
        })
    }
}

fn entry(version: &str, url: &str, retracted: bool) -> VersionEntry {
    VersionEntry {
        version: PubVersion::parse(version).unwrap(),
        retracted,
        archive_url: url.to_owned(),
        archive_sha256: Some("c".repeat(64)),
        pubspec: json!({ "name": "demo", "version": version })
            .as_object()
            .unwrap()
            .clone(),
        published: None,
    }
}

fn member_doc(
    member: &str,
    kind: RepoType,
    holds_claim: bool,
    discontinued: Option<&str>,
    entries: Vec<VersionEntry>,
) -> MemberDocument {
    let doc = PackageDoc::new(
        PackageName::parse("demo").unwrap(),
        discontinued.is_some(),
        discontinued.and_then(|replacement| PackageName::parse(replacement).ok()),
        entries,
    );
    MemberDocument {
        member: MemberRef {
            id: RepositoryId::generate(),
            name: RepoName::parse(member).unwrap(),
            repo_type: kind,
        },
        content_type: Some(MEDIA_TYPE.to_owned()),
        body: Bytes::from(doc.to_bytes()),
        holds_claim,
    }
}

fn merged_json(result: Result<MergedDocument, FormatError>) -> Value {
    let merged = result.unwrap();
    assert_eq!(merged.content_type, MEDIA_TYPE);
    serde_json::from_slice(&merged.body).unwrap()
}

fn versions_of(doc: &Value) -> Vec<(String, String)> {
    doc["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["version"].as_str().unwrap().to_owned(),
                v["archive_url"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn the_union_is_keyed_by_exact_version_and_sorted() {
    let fixture = Fixture::new();
    let doc = merged_json(fixture.merge(
        "api/packages/demo",
        vec![
            member_doc(
                "hosted",
                RepoType::Hosted,
                true,
                None,
                vec![
                    entry("1.0.0", "https://g/h/1.0.0", false),
                    entry("1.2.0", "https://g/h/1.2.0", false),
                ],
            ),
            member_doc(
                "proxy",
                RepoType::Proxy,
                false,
                None,
                vec![
                    entry("0.9.0", "https://g/p/0.9.0", false),
                    entry("1.0.0+build", "https://g/p/1.0.0+build", false),
                    entry("2.0.0", "https://g/p/2.0.0", false),
                ],
            ),
        ],
    ));
    let versions: Vec<String> = versions_of(&doc).into_iter().map(|(v, _)| v).collect();
    // `1.0.0` and `1.0.0+build` are two versions, as they are for Dart.
    assert_eq!(
        versions,
        ["0.9.0", "1.0.0", "1.0.0+build", "1.2.0", "2.0.0"]
    );
    assert_eq!(doc["latest"]["version"], "2.0.0");
    assert_eq!(doc["name"], "demo");
}

#[test]
fn the_first_member_in_group_order_wins_a_duplicate_version() {
    let fixture = Fixture::new();
    let doc = merged_json(fixture.merge(
        "api/packages/demo",
        vec![
            member_doc(
                "a",
                RepoType::Hosted,
                false,
                None,
                vec![entry("1.0.0", "https://g/a", false)],
            ),
            member_doc(
                "b",
                RepoType::Proxy,
                false,
                None,
                vec![entry("1.0.0", "https://g/b", true)],
            ),
        ],
    ));
    assert_eq!(
        versions_of(&doc),
        [("1.0.0".to_owned(), "https://g/a".to_owned())]
    );
    // The retracted flag came with the winning entry, not with the other one.
    assert_eq!(doc["versions"][0]["retracted"], false);
}

#[test]
fn retracted_stays_with_its_version() {
    let fixture = Fixture::new();
    let doc = merged_json(fixture.merge(
        "api/packages/demo",
        vec![member_doc(
            "a",
            RepoType::Hosted,
            false,
            None,
            vec![
                entry("1.0.0", "https://g/1", false),
                entry("2.0.0", "https://g/2", true),
            ],
        )],
    ));
    assert_eq!(doc["versions"][0]["retracted"], false);
    assert_eq!(doc["versions"][1]["retracted"], true);
    // The latest is the highest version that is not retracted.
    assert_eq!(doc["latest"]["version"], "1.0.0");
}

#[test]
fn discontinuation_comes_from_the_claim_holder_else_from_the_first_member() {
    let fixture = Fixture::new();
    let one = |version: &str| vec![entry(version, &format!("https://g/{version}"), false)];
    // The holder of the claim is second in the list and says discontinued: it decides.
    let doc = merged_json(fixture.merge(
        "api/packages/demo",
        vec![
            member_doc("first", RepoType::Hosted, false, None, one("1.0.0")),
            member_doc(
                "holder",
                RepoType::Hosted,
                true,
                Some("successor"),
                one("2.0.0"),
            ),
        ],
    ));
    assert_eq!(doc["isDiscontinued"], true);
    assert_eq!(doc["replacedBy"], "successor");
    // No holder: the first member decides.
    let doc = merged_json(fixture.merge(
        "api/packages/demo",
        vec![
            member_doc(
                "first",
                RepoType::Hosted,
                false,
                Some("other"),
                one("1.0.0"),
            ),
            member_doc("second", RepoType::Proxy, false, None, one("2.0.0")),
        ],
    ));
    assert_eq!(doc["isDiscontinued"], true);
    assert_eq!(doc["replacedBy"], "other");
    // And a holder that is not discontinued overrides a first member that is.
    let doc = merged_json(fixture.merge(
        "api/packages/demo",
        vec![
            member_doc("first", RepoType::Proxy, false, Some("other"), one("1.0.0")),
            member_doc("holder", RepoType::Hosted, true, None, one("2.0.0")),
        ],
    ));
    assert!(doc.get("isDiscontinued").is_none());
}

#[test]
fn the_single_version_document_is_the_first_members() {
    let fixture = Fixture::new();
    let first = member_doc(
        "a",
        RepoType::Hosted,
        false,
        None,
        vec![entry("1.0.0", "https://g/a", false)],
    );
    let single =
        serde_json::to_vec(&entry("1.0.0", "https://g/a", false).to_single_json()).unwrap();
    let document = MemberDocument {
        body: Bytes::from(single.clone()),
        ..first
    };
    let other = MemberDocument {
        body: Bytes::from_static(b"{\"version\":\"1.0.0\",\"archive_url\":\"https://g/b\"}"),
        ..member_doc("b", RepoType::Proxy, false, None, vec![])
    };
    let merged = fixture
        .merge("api/packages/demo/versions/1.0.0", vec![document, other])
        .unwrap();
    assert_eq!(merged.body.as_ref(), single.as_slice());
    assert_eq!(merged.content_type, MEDIA_TYPE);
}

#[test]
fn must_not_merge_a_document_that_cannot_be_read() {
    let fixture = Fixture::new();
    let garbage = MemberDocument {
        body: Bytes::from_static(b"<html>502 bad gateway from 10.1.2.3</html>"),
        ..member_doc("proxy", RepoType::Proxy, false, None, vec![])
    };
    let good = member_doc(
        "hosted",
        RepoType::Hosted,
        true,
        None,
        vec![entry("1.0.0", "https://g/1", false)],
    );
    let error = fixture
        .merge("api/packages/demo", vec![good, garbage])
        .unwrap_err();
    assert!(matches!(&error, FormatError::Unprocessable(m) if !m.contains("10.1.2.3")));
    // A listing for another package is just as unusable.
    let mut other = member_doc(
        "other",
        RepoType::Proxy,
        false,
        None,
        vec![entry("1.0.0", "https://g/x", false)],
    );
    other.body =
        Bytes::from(serde_json::to_vec(&json!({ "name": "elsewhere", "versions": [] })).unwrap());
    assert!(matches!(
        fixture.merge("api/packages/demo", vec![other]),
        Err(FormatError::Unprocessable(_))
    ));
}

#[test]
fn only_index_routes_are_merged() {
    let fixture = Fixture::new();
    let doc = member_doc(
        "a",
        RepoType::Hosted,
        false,
        None,
        vec![entry("1.0.0", "https://g/a", false)],
    );
    assert!(matches!(
        fixture.merge("packages/demo/versions/1.0.0.tar.gz", vec![doc]),
        Err(FormatError::Internal(_))
    ));
    // The engine never merges nothing; if it did, the answer would be an empty listing.
    assert!(fixture.merge("api/packages/demo", vec![]).is_ok());
    assert!(matches!(
        fixture.merge("api/packages/versions/new", vec![]),
        Err(FormatError::Internal(_))
    ));
    assert!(matches!(
        fixture.merge("api/packages/demo/versions/1.0.0", vec![]),
        Err(FormatError::Internal(_))
    ));
}

#[test]
fn the_claim_names_the_package_of_every_read_route() {
    let fixture = Fixture::new();
    for path in [
        "api/packages/demo",
        "api/packages/demo/versions/1.0.0",
        "api/packages/demo/advisories",
        "packages/demo/versions/1.0.0.tar.gz",
    ] {
        let claim = PubClaimer.claim_for(&fixture.request(path)).unwrap();
        assert_eq!(claim, NameClaim::package("demo"), "{path}");
    }
    for path in [
        "api/packages/versions/new",
        "api/packages/versions/upload/0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10",
        "api/packages/versions/finish/0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10/demo/1.0.0",
        "unknown/path",
    ] {
        assert_eq!(PubClaimer.claim_for(&fixture.request(path)), None, "{path}");
    }
}
