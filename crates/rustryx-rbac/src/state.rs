//! Cached lookups against the metadata store.
//!
//! Everything the authorizer reads from PostgreSQL goes through here, so there
//! is one place that states the freshness rules:
//!
//! - **Version in the key.** Each cache key that depends on stored roles,
//!   privileges or selectors carries `version`, a counter bumped by every
//!   invalidation message that can change them (`Rbac`, `Reset`, and a message
//!   this build does not know). A load reads the counter *before* it queries the
//!   store, so a load that was already running when the message arrived stores
//!   its (possibly stale) result under the old version, where nothing will ever
//!   look. Without this, a slow load could re-insert old data right after
//!   `invalidate_all`, and it would be served until the TTL.
//! - **Exact keys.** The privilege cache is keyed by the sorted role ids
//!   themselves, not by a hash of them. A hash collision would hand one user the
//!   privileges of another.
//! - **No stale fallback.** A store error is returned to the caller and nothing
//!   is cached; the previous answer is not served "just this once".

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rustryx_core::error::{AuthError, StoreError};
use rustryx_core::identity::{ContentSelector, Privilege, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS};
use rustryx_core::ids::{ContentSelectorId, RoleId, UserId};
use rustryx_core::ports::{InvalidationEvent, RoleRepo, SelectorRepo};

use crate::anonymous::AnonymousSwitch;
use crate::cache::{MAX_DECISION_TTL, TtlCache};
use crate::config::RbacConfig;
use crate::metrics;
use crate::selector::SelectorEngine;

/// Roles the authorizer treats specially, resolved by name in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Builtin {
    /// `nx-admin`.
    Admin,
    /// `nx-anonymous`.
    Anonymous,
}

impl Builtin {
    fn role_name(self) -> &'static str {
        match self {
            Self::Admin => ROLE_NX_ADMIN,
            Self::Anonymous => ROLE_NX_ANONYMOUS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PrivilegeKey {
    version: u64,
    /// Kept in the key only so that a `User` invalidation can drop the entries
    /// of one user; the roles decide the privileges, never the user id.
    user: Option<UserId>,
    roles: Arc<[RoleId]>,
}

/// A selector row with its compiled program. `program` is `None` when the
/// expression does not compile: the selector then matches nothing.
pub(crate) struct CompiledSelector<P> {
    pub(crate) program: Option<Arc<P>>,
}

/// Compiled program cached by `(selector id, updated_at)`. The source text is
/// kept to double check the key: if a store bug changed the expression without
/// moving `updated_at`, the stale program is detected and not used.
struct CompiledEntry<P> {
    expression: String,
    program: Option<Arc<P>>,
}

type ProgramKey = (ContentSelectorId, i128);

/// A cached selector lookup: `None` when the selector does not exist.
type SelectorEntry<P> = Option<Arc<CompiledSelector<P>>>;

pub(crate) struct State<E: SelectorEngine> {
    roles: Arc<dyn RoleRepo>,
    selectors: Arc<dyn SelectorRepo>,
    pub(crate) engine: E,
    /// The global anonymous switch. Read on every anonymous request and never cached, so
    /// flipping it takes effect on the next request without waiting for a TTL.
    pub(crate) anonymous: AnonymousSwitch,
    version: AtomicU64,
    privileges: TtlCache<PrivilegeKey, Arc<[Privilege]>>,
    builtin_roles: TtlCache<(u64, Builtin), Option<RoleId>>,
    selector_rows: TtlCache<(u64, ContentSelectorId), SelectorEntry<E::Program>>,
    programs: TtlCache<ProgramKey, Arc<CompiledEntry<E::Program>>>,
}

fn backend(context: &'static str, source: Arc<StoreError>) -> AuthError {
    AuthError::Backend {
        context,
        source: Box::new(source),
    }
}

impl<E: SelectorEngine> State<E> {
    pub(crate) fn new(
        roles: Arc<dyn RoleRepo>,
        selectors: Arc<dyn SelectorRepo>,
        engine: E,
        config: &RbacConfig,
    ) -> Self {
        let ttl = config.effective_ttl();
        Self {
            roles,
            selectors,
            engine,
            anonymous: AnonymousSwitch::new(config.anonymous_enabled),
            version: AtomicU64::new(0),
            privileges: TtlCache::new("privileges", config.max_privilege_sets, ttl, true),
            builtin_roles: TtlCache::new("builtin_roles", 16, ttl, false),
            selector_rows: TtlCache::new("selectors", config.max_selectors, ttl, false),
            // A compiled program is a pure function of its key, so it may live
            // as long as any cached decision.
            programs: TtlCache::new("programs", config.max_programs, MAX_DECISION_TTL, false),
        }
    }

    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Id of the built-in role of that name, `None` when the store has no such
    /// role. The id is what identifies the role: a name carried by a principal
    /// is never trusted.
    pub(crate) async fn builtin_role(&self, which: Builtin) -> Result<Option<RoleId>, AuthError> {
        let key = (self.version(), which);
        self.builtin_roles
            .get_or_load(key, || async {
                self.roles
                    .get_by_name(which.role_name())
                    .await
                    .map(|role| role.map(|r| r.id))
            })
            .await
            .map_err(|e| backend("rbac built-in role lookup", e))
    }

    /// Effective privileges of a set of roles. `role_ids` must be sorted and
    /// deduplicated so that equal sets share one entry.
    pub(crate) async fn privileges(
        &self,
        user: Option<UserId>,
        role_ids: Vec<RoleId>,
    ) -> Result<Arc<[Privilege]>, AuthError> {
        if role_ids.is_empty() {
            return Ok(Arc::from(Vec::new()));
        }
        let key = PrivilegeKey {
            version: self.version(),
            user,
            roles: Arc::from(role_ids),
        };
        let roles = key.roles.clone();
        self.privileges
            .get_or_load(key, || async {
                self.roles.effective_privileges(&roles).await.map(Arc::from)
            })
            .await
            .map_err(|e| backend("rbac effective privileges", e))
    }

    /// The selector with that id and its compiled program, `None` when the
    /// selector no longer exists.
    pub(crate) async fn selector(
        &self,
        id: ContentSelectorId,
    ) -> Result<Option<Arc<CompiledSelector<E::Program>>>, AuthError> {
        let key = (self.version(), id);
        self.selector_rows
            .get_or_load(key, || async {
                let Some(row) = self.selectors.get(id).await? else {
                    return Ok(None);
                };
                let program = self.program_for(&row).await;
                Ok(Some(Arc::new(CompiledSelector { program })))
            })
            .await
            .map_err(|e| backend("rbac content selector lookup", e))
    }

    async fn program_for(&self, row: &ContentSelector) -> Option<Arc<E::Program>> {
        let key = (row.id, row.updated_at.unix_timestamp_nanos());
        let entry = self
            .programs
            .get_or_compute(key, || async { Arc::new(self.compile(row)) })
            .await;
        if entry.expression == row.expression {
            return entry.program.clone();
        }
        tracing::warn!(
            selector_id = %row.id,
            "selector expression changed without a new updated_at, compiling it again"
        );
        self.compile(row).program
    }

    fn compile(&self, row: &ContentSelector) -> CompiledEntry<E::Program> {
        let program = match self.engine.compile(&row.expression) {
            Ok(program) => Some(Arc::new(program)),
            Err(error) => {
                // Once per compile, not per request: the result is cached.
                tracing::warn!(
                    selector_id = %row.id,
                    selector = %row.name,
                    %error,
                    "content selector does not compile, it matches nothing"
                );
                None
            }
        };
        CompiledEntry {
            expression: row.expression.clone(),
            program,
        }
    }

    /// Applies a cache invalidation message from the bus.
    pub(crate) fn apply(&self, event: &InvalidationEvent) {
        metrics::invalidation(event);
        match event {
            InvalidationEvent::Rbac => self.drop_stored_data(),
            InvalidationEvent::User(id) => {
                let user = UserId::from_uuid(*id);
                self.privileges
                    .invalidate_where(move |key| key.user == Some(user));
            }
            // Membership, group mapping and visibility of projects (`Projects`), and a settings
            // revision or security policy (`Config`). Nothing this crate caches depends on them
            // yet: the repository carries its project in `ResourceRef`, no decision here reads a
            // setting (the anonymous switch has its own handle), and the caches of project roles
            // (`direct`, `groups`, wave 2 stream B) do not exist. `Projects` drops them here when
            // they arrive. The version does not move: no key below holds project data, and
            // bumping it would only throw away privileges that are still right.
            InvalidationEvent::Projects | InvalidationEvent::Config(_) => {}
            // `Reset`, and a variant added to core after this build. Not knowing what the
            // latter changed, drop everything: the rule an N-1 node applies to a payload it
            // cannot decode.
            _ => self.reset(),
        }
    }

    fn reset(&self) {
        self.drop_stored_data();
        self.programs.invalidate_all();
    }

    /// Bumps the version first, so loads that start from now on use fresh keys,
    /// then clears what is cached under older ones.
    fn drop_stored_data(&self) {
        self.version.fetch_add(1, Ordering::AcqRel);
        self.privileges.invalidate_all();
        self.builtin_roles.invalidate_all();
        self.selector_rows.invalidate_all();
    }
}
