//! Manually driven clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustryx_core::ports::Clock;
use time::OffsetDateTime;
use time::macros::datetime;

use crate::support::{add_std, lock};

/// Instant a [`FixedClock::default`] starts at.
const DEFAULT_START: OffsetDateTime = datetime!(2026-01-01 00:00:00 UTC);

/// A [`Clock`] that only moves when a test says so.
///
/// Clones share one instant, so the clock handed to a service and the one the test
/// advances are the same.
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: Arc<Mutex<OffsetDateTime>>,
}

impl FixedClock {
    /// Creates a clock standing at `start`.
    #[must_use]
    pub fn new(start: OffsetDateTime) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Moves the clock forward. Saturates at the largest representable instant.
    pub fn advance(&self, by: Duration) {
        let mut now = lock(&self.now);
        *now = add_std(*now, by);
    }

    /// Jumps to an arbitrary instant, including one in the past.
    pub fn set(&self, at: OffsetDateTime) {
        *lock(&self.now) = at;
    }
}

impl Default for FixedClock {
    /// Starts at 2026-01-01T00:00:00Z so that tests need no wall-clock reads.
    fn default() -> Self {
        Self::new(DEFAULT_START)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        *lock(&self.now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_where_told_and_only_moves_on_request() {
        let clock = FixedClock::new(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH);
    }

    #[test]
    fn advance_and_set_move_every_clone() {
        let clock = FixedClock::default();
        let other = clock.clone();
        clock.advance(Duration::from_secs(90));
        assert_eq!(other.now(), DEFAULT_START + time::Duration::seconds(90));
        other.set(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH);
    }

    #[test]
    fn advance_saturates_instead_of_panicking() {
        let clock = FixedClock::default();
        clock.advance(Duration::MAX);
        assert!(clock.now() > DEFAULT_START);
    }
}
