//! Tunables of the authorizer.

use std::time::Duration;

use crate::cache::MAX_DECISION_TTL;

/// Cache sizing of [`Rbac`](crate::Rbac).
///
/// Every cache is bounded in size and in age. The age limit is never above
/// [`MAX_DECISION_TTL`] (five minutes) whatever is configured here, because a
/// cached answer is a privilege decision and the longest a revoked right may
/// survive on a node that missed the invalidation message is that TTL.
#[derive(Debug, Clone)]
pub struct RbacConfig {
    /// Lifetime of cached effective privileges, selectors and built-in role ids.
    /// Default 60 s. Values above five minutes are lowered to five minutes.
    pub decision_ttl: Duration,
    /// Most cached privilege sets (one per distinct set of roles of a user).
    pub max_privilege_sets: u64,
    /// Most cached content selectors.
    pub max_selectors: u64,
    /// Most cached compiled programs.
    pub max_programs: u64,
    /// Initial position of the global anonymous switch (`auth.anonymous_enabled`). Default
    /// `false`: a freshly built authorizer refuses every anonymous caller until the composition
    /// root says otherwise. The switch can be flipped later through
    /// [`Rbac::anonymous_switch`](crate::Rbac::anonymous_switch).
    pub anonymous_enabled: bool,
}

impl Default for RbacConfig {
    fn default() -> Self {
        Self {
            decision_ttl: Duration::from_secs(60),
            max_privilege_sets: 10_000,
            max_selectors: 1_000,
            max_programs: 1_000,
            anonymous_enabled: false,
        }
    }
}

impl RbacConfig {
    /// The TTL actually applied: the configured value capped at five minutes.
    #[must_use]
    pub fn effective_ttl(&self) -> Duration {
        self.decision_ttl.min(MAX_DECISION_TTL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ttl_is_sixty_seconds() {
        assert_eq!(
            RbacConfig::default().effective_ttl(),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn must_not_enable_anonymous_access_by_default() {
        assert!(!RbacConfig::default().anonymous_enabled);
    }

    #[test]
    fn ttl_is_capped_at_five_minutes() {
        let config = RbacConfig {
            decision_ttl: Duration::from_secs(3600),
            ..RbacConfig::default()
        };
        assert_eq!(config.effective_ttl(), Duration::from_secs(300));
    }
}
