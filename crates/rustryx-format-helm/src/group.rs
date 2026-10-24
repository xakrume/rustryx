//! Group index merge and the chart-name claim.
//!
//! A hosted member that appears before a proxy member owns every chart name it publishes.
//! Later proxy entries of those names are dropped, so a higher public version cannot win.
//! Hosted members have to be listed first for that to apply: a proxy that comes first has
//! already contributed its versions.

use std::collections::{BTreeMap, BTreeSet};

use rustryx_core::model::RepoType;
use rustryx_format_core::{
    FormatError, GroupIndexMerger, GroupNameClaimer, MergeInput, MergedDocument, NameClaim,
};

use crate::index::{parse_index, to_bytes};
use crate::model::{ChartVersion, IndexFile};
use crate::names::split_chart_file;
use crate::response::INDEX_TYPE;

/// Merges member `index.yaml` documents in group order.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HelmMerger;

/// Claims the chart name of `charts/<name>-<version>.tgz`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HelmClaimer;

impl GroupIndexMerger for HelmMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let mut claimed = BTreeSet::new();
        let mut entries: BTreeMap<String, Vec<ChartVersion>> = BTreeMap::new();
        let mut usable = 0usize;
        for document in &input.documents {
            let Ok(index) = parse_index::<()>(&document.body, || ()) else {
                tracing::info!(
                    member = %document.member.name,
                    "a group member index.yaml was skipped"
                );
                continue;
            };
            usable += 1;
            absorb(&mut entries, &mut claimed, index, document.member.repo_type);
        }
        if usable == 0 {
            return Err(FormatError::unprocessable(
                "index.yaml from the group members is not valid",
            ));
        }
        sort_entries(&mut entries);
        let body = to_bytes(&IndexFile {
            entries,
            ..IndexFile::default()
        })?;
        Ok(MergedDocument {
            content_type: INDEX_TYPE.to_owned(),
            body,
        })
    }
}

fn absorb(
    entries: &mut BTreeMap<String, Vec<ChartVersion>>,
    claimed: &mut BTreeSet<String>,
    index: IndexFile,
    repo_type: RepoType,
) {
    let proxy = repo_type == RepoType::Proxy;
    for (name, versions) in index.entries {
        if proxy && claimed.contains(&name) {
            continue;
        }
        if repo_type == RepoType::Hosted {
            claimed.insert(name.clone());
        }
        let slot = entries.entry(name).or_default();
        for version in versions {
            if slot.iter().any(|have| have.version == version.version) {
                continue;
            }
            slot.push(version);
        }
    }
}

fn sort_entries(entries: &mut BTreeMap<String, Vec<ChartVersion>>) {
    for versions in entries.values_mut() {
        versions.sort_by(|left, right| cmp_desc(&left.version, &right.version));
    }
}

fn cmp_desc(left: &str, right: &str) -> std::cmp::Ordering {
    use crate::names::parse_version;
    match (parse_version(left).ok(), parse_version(right).ok()) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => right.cmp(left),
    }
}

impl GroupNameClaimer for HelmClaimer {
    fn claim_for(&self, request: &rustryx_format_core::ArtifactRequest) -> Option<NameClaim> {
        let file = request.path.as_str().strip_prefix("charts/")?;
        if file.contains('/') {
            return None;
        }
        let parsed = split_chart_file(file)?;
        if parsed.provenance {
            return None;
        }
        Some(NameClaim::package(parsed.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Method;
    use rustryx_core::identity::{Action, Principal};
    use rustryx_core::ids::RepositoryId;
    use rustryx_core::names::RepoName;
    use rustryx_format_core::{ArtifactRequest, MemberDocument, MemberRef, NormalizedPath};
    use url::Url;

    use crate::model::ChartVersion;
    use crate::yaml::{YamlLimits, parse, render};

    fn version(name: &str, version: &str) -> ChartVersion {
        ChartVersion {
            name: name.to_owned(),
            version: version.to_owned(),
            urls: vec![format!("https://group.example/charts/{name}-{version}.tgz")],
            ..ChartVersion::default()
        }
    }

    fn document(
        name: &str,
        repo_type: RepoType,
        entries: BTreeMap<String, Vec<ChartVersion>>,
    ) -> MemberDocument {
        let index = IndexFile {
            entries,
            ..IndexFile::default()
        };
        MemberDocument {
            member: MemberRef {
                id: RepositoryId::generate(),
                name: RepoName::parse(name).unwrap(),
                repo_type,
            },
            content_type: Some(INDEX_TYPE.to_owned()),
            body: bytes::Bytes::from(render(&index).unwrap()),
            holds_claim: false,
        }
    }

    fn request() -> ArtifactRequest {
        let head = rustryx_format_core::RequestHead::new(
            Method::GET,
            "index.yaml",
            Principal::anonymous(),
            Url::parse("https://rustryx.test").unwrap(),
        );
        ArtifactRequest::new(
            &head,
            NormalizedPath::parse("index.yaml").unwrap(),
            Action::Read,
        )
        .index()
    }

    #[test]
    fn a_hosted_member_before_a_proxy_owns_the_chart_name() {
        let hosted = document(
            "internal",
            RepoType::Hosted,
            BTreeMap::from([("web".into(), vec![version("web", "1.0.0")])]),
        );
        let proxy = document(
            "public",
            RepoType::Proxy,
            BTreeMap::from([
                ("web".into(), vec![version("web", "9.9.9")]),
                ("other".into(), vec![version("other", "1.2.3")]),
            ]),
        );
        let merged = HelmMerger
            .merge(MergeInput {
                request: &request(),
                group: &rustryx_testkit::fixtures::repository(
                    "all",
                    rustryx_core::model::RepoFormat::Helm,
                    RepoType::Group,
                ),
                documents: vec![hosted, proxy],
                claim: None,
            })
            .unwrap();
        let index: IndexFile = parse(&merged.body, YamlLimits::index(1 << 20)).unwrap();
        let web: Vec<_> = index.entries["web"]
            .iter()
            .map(|entry| entry.version.as_str())
            .collect();
        assert_eq!(web, ["1.0.0"]);
        assert_eq!(index.entries["other"][0].version, "1.2.3");
        assert!(index.entries["other"][0].urls[0].contains("other-1.2.3.tgz"));
    }

    #[test]
    fn the_first_copy_of_a_version_wins() {
        let first = document(
            "a",
            RepoType::Hosted,
            BTreeMap::from([("web".into(), vec![version("web", "1.0.0")])]),
        );
        let mut second_version = version("web", "1.0.0");
        second_version.description = Some("from second".into());
        let second = document(
            "b",
            RepoType::Hosted,
            BTreeMap::from([("web".into(), vec![second_version])]),
        );
        let merged = HelmMerger
            .merge(MergeInput {
                request: &request(),
                group: &rustryx_testkit::fixtures::repository(
                    "all",
                    rustryx_core::model::RepoFormat::Helm,
                    RepoType::Group,
                ),
                documents: vec![first, second],
                claim: None,
            })
            .unwrap();
        let index: IndexFile = parse(&merged.body, YamlLimits::index(1 << 20)).unwrap();
        assert_eq!(index.entries["web"].len(), 1);
        assert_eq!(index.entries["web"][0].description, None);
    }

    #[test]
    fn a_chart_request_claims_its_name_and_the_index_claims_nothing() {
        let head = rustryx_format_core::RequestHead::new(
            Method::GET,
            "charts/web-1.0.0.tgz",
            Principal::anonymous(),
            Url::parse("https://rustryx.test").unwrap(),
        );
        let chart = ArtifactRequest::new(
            &head,
            NormalizedPath::parse("charts/web-1.0.0.tgz").unwrap(),
            Action::Read,
        );
        let claim = HelmClaimer.claim_for(&chart).unwrap();
        assert_eq!(claim.name.as_deref(), Some("web"));
        assert!(claim.group.is_none());
        assert!(HelmClaimer.claim_for(&request()).is_none());
    }
}
