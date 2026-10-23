//! Metrics of the authorizer, emitted through the `metrics` facade.
//!
//! The exporter lives in `rustryx-observability`; this crate only increments. Every label is a
//! closed set (SRE-09): no principal, repository, path, role or selector name ever becomes a
//! label value, so the series count is fixed by the code, not by the data.

use metrics::counter;
use rustryx_core::error::AuthError;
use rustryx_core::ports::InvalidationEvent;
use rustryx_core::ports::auth::Decision;

/// Counter `rbac_decisions_total{kind,result}`: one per authorization question answered.
/// `kind` is `content` (`check` and every member of `filter_visible_members`), `app`
/// (`check_app`), `project` (`check_project`) or `admin` (`is_admin`). `result` is `allow`,
/// `deny`, or `error` when the store failed and the caller had to refuse (503 class).
pub const RBAC_DECISIONS_TOTAL: &str = "rbac_decisions_total";

/// Counter `rbac_cache_events_total{cache,event}`. `cache` is `privileges`, `builtin_roles`,
/// `selectors` or `programs`. `event` is `hit` (served without this call touching the store,
/// including a call that shared another call's load), `miss` (this call loaded from the store)
/// or `error` (the load failed and nothing was cached). Store reads are `miss + error`.
pub const RBAC_CACHE_EVENTS_TOTAL: &str = "rbac_cache_events_total";

/// Counter `rbac_invalidations_total{event}`: invalidation messages applied on this node.
/// `event` is `rbac`, `user`, `reset`, `projects`, `config`, `unknown` (a variant this build
/// does not know, treated as `reset`) or `stream_end` (the bus closed, caches fall back to
/// their TTL).
pub const RBAC_INVALIDATIONS_TOTAL: &str = "rbac_invalidations_total";

/// What kind of question a decision answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecisionKind {
    Content,
    App,
    Project,
    Admin,
}

impl DecisionKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::App => "app",
            Self::Project => "project",
            Self::Admin => "admin",
        }
    }
}

/// What a cache call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheEvent {
    Hit,
    Miss,
    Error,
}

impl CacheEvent {
    const fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Error => "error",
        }
    }
}

/// Counts one decision and hands the outcome back unchanged, so a call site stays one
/// expression: `metrics::decision(kind, self.decide(..).await)`.
pub(crate) fn decision(
    kind: DecisionKind,
    outcome: Result<Decision, AuthError>,
) -> Result<Decision, AuthError> {
    let result = match &outcome {
        Ok(Decision::Allow) => "allow",
        Ok(Decision::Deny) => "deny",
        Err(_) => "error",
    };
    counter!(RBAC_DECISIONS_TOTAL, "kind" => kind.label(), "result" => result).increment(1);
    outcome
}

pub(crate) fn cache_event(cache: &'static str, event: CacheEvent) {
    counter!(RBAC_CACHE_EVENTS_TOTAL, "cache" => cache, "event" => event.label()).increment(1);
}

pub(crate) fn invalidation(event: &InvalidationEvent) {
    let label = match event {
        InvalidationEvent::Rbac => "rbac",
        InvalidationEvent::User(_) => "user",
        InvalidationEvent::Reset => "reset",
        InvalidationEvent::Projects => "projects",
        InvalidationEvent::Config(_) => "config",
        _ => "unknown",
    };
    counter!(RBAC_INVALIDATIONS_TOTAL, "event" => label).increment(1);
}

pub(crate) fn stream_ended() {
    counter!(RBAC_INVALIDATIONS_TOTAL, "event" => "stream_end").increment(1);
}
