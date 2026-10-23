//! The global anonymous switch.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The global switch for anonymous access (`auth.anonymous_enabled`).
///
/// Anonymous access needs this switch **and** the repository flag
/// (`ResourceRef::allow_anonymous`); both are checked inside `RbacChecker::check`, so a caller
/// that forgets one still gets a refusal (CONTRIBUTING rule 4).
///
/// It is a shared handle and not a copy of the configuration value, so the settings layer can
/// flip it at run time without rebuilding the authorizer. Clones share the flag. The value is
/// read on every anonymous request and never cached: turning it off refuses the very next
/// anonymous request on this node.
#[derive(Debug, Clone, Default)]
pub struct AnonymousSwitch {
    on: Arc<AtomicBool>,
}

impl AnonymousSwitch {
    /// A switch in the given position. `AnonymousSwitch::default()` is off.
    #[must_use]
    pub fn new(on: bool) -> Self {
        Self {
            on: Arc::new(AtomicBool::new(on)),
        }
    }

    /// True while anonymous access is allowed at all.
    #[must_use]
    pub fn is_on(&self) -> bool {
        // Acquire pairs with the Release in `set`: a request that starts after `set(false)`
        // returned cannot still read the old value.
        self.on.load(Ordering::Acquire)
    }

    /// Turns anonymous access on or off for every clone of this handle.
    pub fn set(&self, on: bool) {
        self.on.store(on, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off_and_clones_share_the_flag() {
        let switch = AnonymousSwitch::default();
        assert!(!switch.is_on());
        let clone = switch.clone();
        clone.set(true);
        assert!(switch.is_on());
        switch.set(false);
        assert!(!clone.is_on());
    }

    #[test]
    fn new_starts_in_the_given_position() {
        assert!(AnonymousSwitch::new(true).is_on());
        assert!(!AnonymousSwitch::new(false).is_on());
    }

    #[test]
    fn independent_switches_do_not_interact() {
        let a = AnonymousSwitch::new(true);
        let b = AnonymousSwitch::new(true);
        a.set(false);
        assert!(b.is_on());
    }
}
