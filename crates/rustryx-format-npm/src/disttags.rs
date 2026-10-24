//! Dist-tags: the named pointers (`latest`, `next`, ...) from a tag to a version.

use std::collections::BTreeMap;

use semver::Version;
use serde_json::{Map, Value};

use crate::version::{highest, parse_version, validate_tag};

/// The tag every package has once it has a version.
pub const LATEST: &str = "latest";

/// Most tags one package may carry. A packument is a document the registry has to read whole; an
/// unbounded tag map would let one publisher grow it without limit.
pub const MAX_TAGS: usize = 256;

/// Why a dist-tags object was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DistTagsError {
    /// `dist-tags` is present but is not a JSON object.
    #[error("dist-tags must be an object")]
    NotAnObject,
    /// A tag name that cannot be a tag.
    #[error("invalid dist-tag name")]
    InvalidTag,
    /// A tag value that is not a strict `SemVer` string.
    #[error("dist-tag value is not a valid version")]
    InvalidVersion,
    /// More than [`MAX_TAGS`] tags.
    #[error("too many dist-tags")]
    TooMany,
}

/// A validated tag to version map. Ordered by tag name, so serialization is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DistTags(BTreeMap<String, Version>);

impl DistTags {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a `dist-tags` JSON value strictly: any invalid entry refuses the whole object.
    ///
    /// # Errors
    /// The first invalid entry.
    pub fn from_value_strict(value: &Value) -> Result<Self, DistTagsError> {
        let object = value.as_object().ok_or(DistTagsError::NotAnObject)?;
        if object.len() > MAX_TAGS {
            return Err(DistTagsError::TooMany);
        }
        let mut tags = BTreeMap::new();
        for (tag, version) in object {
            validate_tag(tag).map_err(|_| DistTagsError::InvalidTag)?;
            let text = version.as_str().ok_or(DistTagsError::InvalidVersion)?;
            let version = parse_version(text).map_err(|_| DistTagsError::InvalidVersion)?;
            tags.insert(tag.clone(), version);
        }
        Ok(Self(tags))
    }

    /// Reads a `dist-tags` JSON value leniently, for documents written by somebody else (an
    /// upstream registry, a member of a group): entries that are not valid are skipped, because
    /// one odd tag on a public package must not make the whole package unreadable.
    #[must_use]
    pub fn from_value_lenient(value: &Value) -> Self {
        let mut tags = BTreeMap::new();
        if let Some(object) = value.as_object() {
            for (tag, version) in object.iter().take(MAX_TAGS) {
                let parsed = version.as_str().and_then(|text| parse_version(text).ok());
                if let Some(version) = parsed
                    && !tag.is_empty()
                    && tag.len() <= crate::version::MAX_TAG_LEN
                {
                    tags.insert(tag.clone(), version);
                }
            }
        }
        Self(tags)
    }

    /// The JSON object form.
    #[must_use]
    pub fn to_value(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(tag, version)| (tag.clone(), Value::String(version.to_string())))
                .collect::<Map<String, Value>>(),
        )
    }

    /// The version a tag points at.
    #[must_use]
    pub fn get(&self, tag: &str) -> Option<&Version> {
        self.0.get(tag)
    }

    /// Points `tag` at `version`.
    ///
    /// # Errors
    /// [`DistTagsError::InvalidTag`] for a bad name, [`DistTagsError::TooMany`] when the tag is
    /// new and the map is full.
    pub fn set(&mut self, tag: &str, version: Version) -> Result<(), DistTagsError> {
        validate_tag(tag).map_err(|_| DistTagsError::InvalidTag)?;
        if !self.0.contains_key(tag) && self.0.len() >= MAX_TAGS {
            return Err(DistTagsError::TooMany);
        }
        self.0.insert(tag.to_owned(), version);
        Ok(())
    }

    /// Removes a tag; true when it existed.
    pub fn remove(&mut self, tag: &str) -> bool {
        self.0.remove(tag).is_some()
    }

    /// Removes every tag that points at `version` and returns their names.
    pub fn remove_pointing_at(&mut self, version: &Version) -> Vec<String> {
        let gone: Vec<String> = self
            .0
            .iter()
            .filter(|(_, v)| *v == version)
            .map(|(tag, _)| tag.clone())
            .collect();
        for tag in &gone {
            self.0.remove(tag);
        }
        gone
    }

    /// Iterates over `(tag, version)` in tag order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Version)> {
        self.0.iter().map(|(tag, version)| (tag.as_str(), version))
    }

    /// Number of tags.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when there is no tag.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Makes sure `latest` exists and points at a version that exists: after a removal or on a
    /// first publish it falls back to the highest release among `versions`.
    pub fn repair_latest<'a, I>(&mut self, versions: I)
    where
        I: IntoIterator<Item = &'a Version> + Clone,
    {
        let current_ok = self
            .0
            .get(LATEST)
            .is_some_and(|v| versions.clone().into_iter().any(|candidate| candidate == v));
        if current_ok {
            return;
        }
        match highest(versions) {
            Some(best) => {
                self.0.insert(LATEST.to_owned(), best.clone());
            }
            None => {
                self.0.remove(LATEST);
            }
        }
    }
}

/// Merges the dist-tags of the members of a group, given in group order with a flag for the members
/// that hold the name claim: a tag a claiming member has comes from it (the first such member),
/// any other tag takes the highest version among the members that have it.
///
/// The rule keeps a public registry from moving `latest` of a package the organization owns, and
/// still lets a package that only the public registry has keep its own tags.
#[must_use]
pub fn merge_dist_tags(sets: &[(&DistTags, bool)]) -> DistTags {
    let mut names: Vec<&str> = sets
        .iter()
        .flat_map(|(tags, _)| tags.iter().map(|(tag, _)| tag))
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut merged = DistTags::new();
    for tag in names {
        let from_claim = sets
            .iter()
            .filter(|(_, holds)| *holds)
            .find_map(|(tags, _)| tags.get(tag));
        let chosen =
            from_claim.or_else(|| highest(sets.iter().filter_map(|(tags, _)| tags.get(tag))));
        if let Some(version) = chosen {
            // A refused tag (a full map, a name that was only tolerated in a lenient read) is
            // left out of the merge rather than failing the whole document.
            let _ = merged.set(tag, version.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn strict_parse_refuses_any_bad_entry() {
        assert!(
            DistTags::from_value_strict(&json!({"latest": "1.0.0", "next": "2.0.0-rc.1"})).is_ok()
        );
        for bad in [
            json!([]),
            json!("latest"),
            json!({"latest": 1}),
            json!({"latest": "1.x"}),
            json!({"1.0.0": "1.0.0"}),
            json!({"": "1.0.0"}),
        ] {
            assert!(DistTags::from_value_strict(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn lenient_parse_skips_bad_entries_and_never_fails() {
        let tags = DistTags::from_value_lenient(&json!({
            "latest": "1.0.0", "bad": "not-a-version", "num": 3, "": "1.0.0"
        }));
        assert_eq!(tags.len(), 1);
        assert_eq!(tags.get("latest"), Some(&v("1.0.0")));
        assert!(DistTags::from_value_lenient(&json!("x")).is_empty());
    }

    #[test]
    fn json_round_trip_is_stable() {
        let mut tags = DistTags::new();
        tags.set("next", v("2.0.0-rc.1")).unwrap();
        tags.set("latest", v("1.0.0")).unwrap();
        let back = DistTags::from_value_strict(&tags.to_value()).unwrap();
        assert_eq!(back, tags);
        assert_eq!(
            tags.to_value().to_string(),
            r#"{"latest":"1.0.0","next":"2.0.0-rc.1"}"#
        );
    }

    #[test]
    fn set_validates_and_bounds() {
        let mut tags = DistTags::new();
        assert_eq!(
            tags.set("1.0.0", v("1.0.0")),
            Err(DistTagsError::InvalidTag)
        );
        for i in 0..MAX_TAGS {
            tags.set(&format!("t{i}"), v("1.0.0")).unwrap();
        }
        assert_eq!(
            tags.set("one-more", v("1.0.0")),
            Err(DistTagsError::TooMany)
        );
        // Moving an existing tag is not growth.
        tags.set("t0", v("2.0.0")).unwrap();
    }

    #[test]
    fn removal_helpers() {
        let mut tags = DistTags::new();
        tags.set("latest", v("1.0.0")).unwrap();
        tags.set("stable", v("1.0.0")).unwrap();
        tags.set("next", v("2.0.0")).unwrap();
        let mut gone = tags.remove_pointing_at(&v("1.0.0"));
        gone.sort();
        assert_eq!(gone, ["latest", "stable"]);
        assert!(tags.remove("next"));
        assert!(!tags.remove("next"));
        assert!(tags.is_empty());
    }

    #[test]
    fn repair_latest_falls_back_to_the_highest_release() {
        let versions = [v("1.0.0"), v("1.2.0"), v("2.0.0-rc.1")];
        let mut tags = DistTags::new();
        tags.repair_latest(&versions);
        assert_eq!(tags.get(LATEST), Some(&v("1.2.0")));
        tags.set(LATEST, v("1.0.0")).unwrap();
        tags.repair_latest(&versions);
        assert_eq!(
            tags.get(LATEST),
            Some(&v("1.0.0")),
            "a valid latest is kept"
        );
        tags.set(LATEST, v("9.9.9")).unwrap();
        tags.repair_latest(&versions);
        assert_eq!(tags.get(LATEST), Some(&v("1.2.0")));
        tags.repair_latest(&[] as &[Version]);
        assert!(tags.get(LATEST).is_none());
        assert_eq!(tags.iter().count(), 0);
    }
}
