//! Group merge and name claims. The engine applies the claim; these tests cover the
//! decisions this crate makes from the documents it is given.

use bytes::Bytes;
use http::Method;
use rustryx_core::identity::Principal;
use rustryx_core::model::{RepoFormat, RepoType};
use rustryx_format_core::{
    ArtifactRequest, FormatError, GroupIndexMerger, GroupNameClaimer, MemberDocument, MemberRef,
    MergeInput, MergedDocument, NameClaim, NormalizedPath, PathRules, RequestHead,
};
use rustryx_testkit::fixtures;
use url::Url;

use super::{NpmClaims, NpmMerger};
use crate::packument::FULL_MEDIA_TYPE;
use crate::route::Route;

fn artifact(path: &str) -> ArtifactRequest {
    let rules = PathRules {
        allow_encoded_slash: true,
        allow_empty: true,
        ..PathRules::default()
    };
    let head = RequestHead::new(
        Method::GET,
        path,
        Principal::anonymous(),
        Url::parse("https://repo.example").unwrap(),
    );
    let normalized = NormalizedPath::parse_with(path, &rules).unwrap();
    let route = Route::parse(&Method::GET, &normalized).unwrap();
    let mut request = ArtifactRequest::new(&head, normalized, route.action(&Method::GET));
    if let Some(coordinate) = route.coordinate() {
        request = request.with_coordinate(coordinate);
    }
    if route.is_index(&Method::GET) {
        request = request.index();
    }
    request
}

fn member(
    name: &str,
    repo_type: RepoType,
    body: &'static [u8],
    holds_claim: bool,
) -> MemberDocument {
    let repository = fixtures::repository(name, RepoFormat::Npm, repo_type);
    MemberDocument {
        member: MemberRef {
            id: repository.id,
            name: repository.name,
            repo_type: repository.repo_type,
        },
        content_type: None,
        body: Bytes::from_static(body),
        holds_claim,
    }
}

fn merge(
    request: &ArtifactRequest,
    documents: Vec<MemberDocument>,
) -> Result<MergedDocument, FormatError> {
    let group = fixtures::repository("npm-group", RepoFormat::Npm, RepoType::Group);
    NpmMerger.merge(MergeInput {
        request,
        group: &group,
        documents,
        claim: Some(NameClaim::package("left-pad")),
    })
}

const GOOD: &[u8] = br#"{"name":"left-pad","dist-tags":{"latest":"1.0.0"},"versions":{"1.0.0":{"name":"left-pad","version":"1.0.0"}}}"#;

#[test]
fn must_not_let_a_garbage_member_take_the_group_down() {
    let request = artifact("left-pad");
    let merged = merge(
        &request,
        vec![
            member("html-proxy", RepoType::Proxy, b"<html>9.9.9</html>", false),
            member("array-proxy", RepoType::Proxy, b"[]", false),
            member("hosted", RepoType::Hosted, GOOD, true),
        ],
    )
    .unwrap();
    assert_eq!(merged.content_type, FULL_MEDIA_TYPE);
    let value: serde_json::Value = serde_json::from_slice(&merged.body).unwrap();
    assert_eq!(value["name"], "left-pad");
    assert_eq!(value["versions"]["1.0.0"]["version"], "1.0.0");
    assert_eq!(value["dist-tags"]["latest"], "1.0.0");
    assert!(value["versions"].get("9.9.9").is_none());

    let err = merge(
        &request,
        vec![
            member("html-proxy", RepoType::Proxy, b"<html>", false),
            member("array-proxy", RepoType::Proxy, b"[]", false),
            member("cut-off", RepoType::Proxy, b"{", false),
        ],
    )
    .unwrap_err();
    assert!(matches!(err, FormatError::Unprocessable(_)));
}

#[test]
fn must_not_merge_a_request_that_is_not_a_document() {
    let request = artifact("left-pad/-/left-pad-1.0.0.tgz");
    let err = merge(
        &request,
        vec![member("hosted", RepoType::Hosted, GOOD, true)],
    )
    .unwrap_err();
    assert!(matches!(err, FormatError::Unprocessable(_)));
}

#[test]
fn must_not_claim_a_lookalike_spelling_or_a_route_without_a_package() {
    let claims = NpmClaims;
    let scoped = claims.claim_for(&artifact("@Acme/Billing")).unwrap();
    assert_eq!(scoped, NameClaim::scoped("acme", "billing"));
    assert_ne!(scoped, NameClaim::scoped("Acme", "Billing"));
    let tarball = claims
        .claim_for(&artifact("@Acme/Billing/-/Billing-1.0.0.tgz"))
        .unwrap();
    assert_eq!(tarball, NameClaim::scoped("acme", "billing"));
    let plain = claims.claim_for(&artifact("JSONStream")).unwrap();
    assert_eq!(plain, NameClaim::package("jsonstream"));
    assert_ne!(plain, NameClaim::package("JSONStream"));
    assert!(claims.claim_for(&artifact("-/v1/search")).is_none());
    assert!(claims.claim_for(&artifact("-/ping")).is_none());
}
