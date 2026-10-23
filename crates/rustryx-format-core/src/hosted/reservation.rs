//! In-process reservation of a path while an immutable upload is in flight.
//!
//! `AssetRepo::publish` is an upsert, so two concurrent uploads of the same path in
//! an `allow_once` repository would both pass the "does it exist" check and the
//! second would silently overwrite the first. The reservation closes that race
//! inside one node: the second upload is refused with a conflict up front.
//!
//! Limit, stated honestly: across nodes the race stays open until the metadata port
//! gets a create-only publish. A violation that slips through is counted
//! (`hosted_allow_once_violation_total`) and logged.

use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use rustryx_core::ids::RepositoryId;

type Key = (RepositoryId, String);

/// Set of paths with an upload in flight.
#[derive(Clone, Default)]
pub struct UploadReservations {
    map: Arc<DashMap<Key, ()>>,
}

impl UploadReservations {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves `(repository, path)`. `None` when someone else holds it.
    #[must_use]
    pub fn try_reserve(&self, repository: RepositoryId, path: &str) -> Option<UploadReservation> {
        let key = (repository, path.to_owned());
        match self.map.entry(key.clone()) {
            Entry::Occupied(_) => None,
            Entry::Vacant(slot) => {
                slot.insert(());
                Some(UploadReservation {
                    map: Arc::clone(&self.map),
                    key,
                })
            }
        }
    }

    /// Number of paths currently reserved.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when nothing is reserved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A held reservation. Released on drop, whatever the reason.
pub struct UploadReservation {
    map: Arc<DashMap<Key, ()>>,
    key: Key,
}

impl Drop for UploadReservation {
    fn drop(&mut self) {
        self.map.remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_reservation_of_the_same_path_is_refused_until_release() {
        let set = UploadReservations::new();
        let repo = RepositoryId::generate();
        let first = set.try_reserve(repo, "a/b").expect("free path");
        assert!(set.try_reserve(repo, "a/b").is_none());
        assert!(
            set.try_reserve(repo, "a/c").is_some(),
            "other paths are independent"
        );
        assert!(
            set.try_reserve(RepositoryId::generate(), "a/b").is_some(),
            "other repositories too"
        );
        assert_eq!(
            set.len(),
            1,
            "the two temporary reservations above were dropped"
        );
        drop(first);
        assert!(set.is_empty());
        assert!(set.try_reserve(repo, "a/b").is_some());
    }
}
