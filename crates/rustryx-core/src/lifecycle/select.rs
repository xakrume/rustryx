//! Which components a retention policy deletes.
//!
//! A rule says what to keep. A component a rule covers and no rule keeps is deleted. A
//! component no rule covers stays. Unless the policy allows it, the newest version of a
//! package is kept even when every rule would drop it.

use std::collections::{HashMap, HashSet};

use time::OffsetDateTime;

use super::{RetentionPolicy, RetentionTemplate};
use crate::error::DomainError;
use crate::ids::ComponentId;
use crate::lifecycle::NameSelector;
use crate::ports::Candidate;

/// Component ids the policy would delete, in the order of `candidates`.
///
/// # Errors
/// Returns [`DomainError::Invalid`] when a selector does not compile.
pub fn deletions_for(
    policy: &RetentionPolicy,
    candidates: &[Candidate],
    now: OffsetDateTime,
) -> Result<Vec<ComponentId>, DomainError> {
    let mut covered: HashMap<ComponentId, Vec<usize>> = HashMap::new();
    for (index, rule) in policy.rules.iter().enumerate() {
        for candidate in candidates {
            if covers(
                &rule.repository,
                &rule.item,
                rule.include_untagged,
                candidate,
            )? {
                covered
                    .entry(candidate.component_id)
                    .or_default()
                    .push(index);
            }
        }
    }
    let mut kept = HashSet::new();
    for (index, rule) in policy.rules.iter().enumerate() {
        let mut group: Vec<&Candidate> = candidates
            .iter()
            .filter(|candidate| {
                covered
                    .get(&candidate.component_id)
                    .is_some_and(|rules| rules.contains(&index))
            })
            .collect();
        keep(&mut group, &rule.template, now, &mut kept);
    }
    if !policy.allow_delete_last_version {
        keep_newest_of_each_package(candidates, &mut kept);
    }
    Ok(candidates
        .iter()
        .filter(|candidate| covered.contains_key(&candidate.component_id))
        .filter(|candidate| !kept.contains(&candidate.component_id))
        .map(|candidate| candidate.component_id)
        .collect())
}

fn covers(
    repository: &NameSelector,
    item: &NameSelector,
    include_untagged: bool,
    candidate: &Candidate,
) -> Result<bool, DomainError> {
    if !matches_selector(repository, &candidate.repository)? {
        return Ok(false);
    }
    if candidate.tags.is_empty() && candidate.version.is_none() {
        return Ok(include_untagged);
    }
    if !candidate.tags.is_empty() {
        return candidate
            .tags
            .iter()
            .try_fold(false, |any, tag| Ok(any || matches_selector(item, tag)?));
    }
    matches_selector(item, candidate.version.as_deref().unwrap_or(""))
}

fn matches_selector(selector: &NameSelector, name: &str) -> Result<bool, DomainError> {
    selector
        .selects(|pattern| regex::Regex::new(pattern).is_ok_and(|compiled| compiled.is_match(name)))
}

fn keep(
    group: &mut Vec<&Candidate>,
    template: &RetentionTemplate,
    now: OffsetDateTime,
    kept: &mut HashSet<ComponentId>,
) {
    match *template {
        RetentionTemplate::KeepLastPushed { count }
        | RetentionTemplate::KeepLatestVersions { count } => {
            let by_push = matches!(template, RetentionTemplate::KeepLastPushed { .. });
            group.sort_by(|left, right| {
                let key = if by_push {
                    left.pushed_at.cmp(&right.pushed_at)
                } else {
                    left.published_at.cmp(&right.published_at)
                };
                key.then(left.component_id.cmp(&right.component_id))
                    .reverse()
            });
            for candidate in group.iter().take(count as usize) {
                kept.insert(candidate.component_id);
            }
        }
        RetentionTemplate::KeepLastPulled { count } => {
            group.retain(|candidate| candidate.pulled_at.is_some());
            group.sort_by(|left, right| {
                left.pulled_at
                    .cmp(&right.pulled_at)
                    .then(left.component_id.cmp(&right.component_id))
                    .reverse()
            });
            for candidate in group.iter().take(count as usize) {
                kept.insert(candidate.component_id);
            }
        }
        RetentionTemplate::KeepPushedWithinDays { days } => {
            let cutoff = now - time::Duration::days(i64::from(days));
            for candidate in group
                .iter()
                .filter(|candidate| candidate.pushed_at >= cutoff)
            {
                kept.insert(candidate.component_id);
            }
        }
        RetentionTemplate::KeepPulledWithinDays { days } => {
            let cutoff = now - time::Duration::days(i64::from(days));
            for candidate in group
                .iter()
                .filter(|candidate| candidate.pulled_at.is_some_and(|pulled| pulled >= cutoff))
            {
                kept.insert(candidate.component_id);
            }
        }
    }
}

fn keep_newest_of_each_package(candidates: &[Candidate], kept: &mut HashSet<ComponentId>) {
    let mut packages: HashMap<(uuid::Uuid, String, String), Vec<&Candidate>> = HashMap::new();
    for candidate in candidates {
        let key = (
            candidate.repository_id.as_uuid(),
            candidate.group_id.clone().unwrap_or_default(),
            candidate.name.clone(),
        );
        packages.entry(key).or_default().push(candidate);
    }
    for group in packages.values() {
        let any_kept = group
            .iter()
            .any(|candidate| kept.contains(&candidate.component_id));
        if any_kept {
            continue;
        }
        if let Some(newest) = group.iter().max_by(|left, right| {
            left.published_at
                .cmp(&right.published_at)
                .then(left.component_id.cmp(&right.component_id))
        }) {
            kept.insert(newest.component_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{RepositoryId, RetentionPolicyId};
    use crate::lifecycle::{NameSelector, PolicyScope, RetentionRule};

    fn candidate(name: &str, version: &str, days_ago: i64) -> Candidate {
        let at = OffsetDateTime::UNIX_EPOCH + time::Duration::days(100 - days_ago);
        let mut row = Candidate::new(
            crate::ids::ComponentId::generate(),
            RepositoryId::generate(),
            "libs",
            name,
            at,
        );
        row.version = Some(version.to_owned());
        row.pushed_at = at;
        row
    }

    fn policy(count: u32, allow_last: bool) -> RetentionPolicy {
        let mut policy = RetentionPolicy::new(
            RetentionPolicyId::generate(),
            PolicyScope::Repository(RepositoryId::generate()),
            "keep",
            vec![RetentionRule {
                template: RetentionTemplate::KeepLatestVersions { count },
                repository: NameSelector::everything(),
                item: NameSelector::everything(),
                include_untagged: true,
            }],
            OffsetDateTime::UNIX_EPOCH,
        );
        policy.allow_delete_last_version = allow_last;
        policy
    }

    #[test]
    fn the_newest_versions_stay_and_the_last_one_is_protected() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(100);
        let rows = vec![
            candidate("left", "1", 3),
            candidate("left", "2", 2),
            candidate("left", "3", 1),
        ];
        let same_repo = rows[0].repository_id;
        let mut rows = rows;
        for row in &mut rows {
            row.repository_id = same_repo;
        }
        let dropped = deletions_for(&policy(2, false), &rows, now).unwrap();
        assert_eq!(dropped, vec![rows[0].component_id]);

        let only = vec![candidate("left", "1", 1)];
        let dropped = deletions_for(&policy(0, false), &only, now).unwrap();
        assert_eq!(dropped, Vec::<ComponentId>::new());
        let dropped = deletions_for(&policy(0, true), &only, now).unwrap();
        assert_eq!(dropped, vec![only[0].component_id]);
    }
}
