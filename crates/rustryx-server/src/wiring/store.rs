//! The metadata store as the trait objects the rest of the wiring hands around.
//!
//! `PgStore` is one struct with a concrete repository per port. Everything downstream takes
//! `Arc<dyn Port>`, never the concrete type (the rule of `rustryx_core::ports`), so this is the
//! single place that names the PostgreSQL types.

use std::sync::Arc;

use rustryx_core::ports::{
    AssetRepo, AuditSink, BlobRepo, BlobStoreRepo, Clock, ComponentRepo, IdpMappingRepo,
    LoginAttemptRepo, RepositoryRepo, RevokedTokenRepo, RoleRepo, RoutingRuleRepo, SelectorRepo,
    SystemClock, TokenRepo, UserRepo,
};
use rustryx_db::PgStore;

/// The ports the composition root uses, all over one pool.
#[derive(Clone)]
pub struct Repos {
    /// Accounts.
    pub users: Arc<dyn UserRepo>,
    /// Roles.
    pub roles: Arc<dyn RoleRepo>,
    /// Selectors (content filters of privileges).
    pub selectors: Arc<dyn SelectorRepo>,
    /// API tokens.
    pub tokens: Arc<dyn TokenRepo>,
    /// Group to role mappings of the identity providers.
    pub idp_mappings: Arc<dyn IdpMappingRepo>,
    /// Failed login counters.
    pub login_attempts: Arc<dyn LoginAttemptRepo>,
    /// Revoked session token ids.
    pub revoked_tokens: Arc<dyn RevokedTokenRepo>,
    /// The audit trail.
    pub audit: Arc<dyn AuditSink>,
    /// Repositories.
    pub repositories: Arc<dyn RepositoryRepo>,
    /// Blob store definitions.
    pub blob_stores: Arc<dyn BlobStoreRepo>,
    /// Blob metadata.
    pub blobs: Arc<dyn BlobRepo>,
    /// Components.
    pub components: Arc<dyn ComponentRepo>,
    /// Assets.
    pub assets: Arc<dyn AssetRepo>,
    /// Routing rules of proxy and group repositories.
    pub routing_rules: Arc<dyn RoutingRuleRepo>,
    /// The wall clock every component shares.
    pub clock: Arc<dyn Clock>,
}

impl Repos {
    /// The ports of `store`.
    #[must_use]
    pub fn from_store(store: &PgStore) -> Self {
        Self {
            users: Arc::new(store.users.clone()),
            roles: Arc::new(store.roles.clone()),
            selectors: Arc::new(store.selectors.clone()),
            tokens: Arc::new(store.tokens.clone()),
            idp_mappings: Arc::new(store.idp_mappings.clone()),
            login_attempts: Arc::new(store.login_attempts.clone()),
            revoked_tokens: Arc::new(store.revoked_tokens.clone()),
            audit: Arc::new(store.audit.clone()),
            repositories: Arc::new(store.repositories.clone()),
            blob_stores: Arc::new(store.blob_stores.clone()),
            blobs: Arc::new(store.blobs.clone()),
            components: Arc::new(store.components.clone()),
            assets: Arc::new(store.assets.clone()),
            routing_rules: Arc::new(store.routing_rules.clone()),
            clock: Arc::new(SystemClock),
        }
    }
}

#[cfg(test)]
impl Repos {
    /// The ports of an in-memory store, for tests that run the real wiring without PostgreSQL.
    #[must_use]
    pub fn from_mem(store: &rustryx_testkit::MemStore) -> Self {
        Self {
            users: Arc::new(store.clone()),
            roles: Arc::new(store.clone()),
            selectors: Arc::new(store.clone()),
            tokens: Arc::new(store.clone()),
            idp_mappings: Arc::new(store.clone()),
            login_attempts: Arc::new(store.clone()),
            revoked_tokens: Arc::new(store.clone()),
            audit: Arc::new(store.audit_sink()),
            repositories: Arc::new(store.clone()),
            blob_stores: Arc::new(store.clone()),
            blobs: Arc::new(store.clone()),
            components: Arc::new(store.clone()),
            assets: Arc::new(store.clone()),
            routing_rules: Arc::new(store.clone()),
            clock: Arc::new(SystemClock),
        }
    }
}
