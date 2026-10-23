//! One bundle of every port implementation over one pool.

use std::sync::Arc;

use rustryx_core::Secret;
use sqlx::PgPool;

use crate::audit::PgAuditSink;
use crate::audit_read::PgAuditRead;
use crate::invalidation::PgInvalidationBus;
use crate::lease::PgJobLeases;
use crate::repos::{
    PgAssetRepo, PgBlobRepo, PgBlobStoreRepo, PgComponentRepo, PgIdpMappingRepo,
    PgLoginAttemptRepo, PgPrivilegeRepo, PgRepositoryRepo, PgRoleRepo, PgRoutingRuleRepo,
    PgSelectorRepo, PgTokenRepo, PgUserRepo,
};
use crate::revoked::PgRevokedTokens;
use crate::sealer::SecretSealer;
use crate::system_config::PgSystemConfig;

/// Every store port of `rustryx-core` implemented on `PostgreSQL`, sharing one pool.
///
/// The fields are cheap handles (a pool clone each). The composition root wraps the ones it
/// needs in `Arc<dyn Port>`, for example `Arc::new(store.repositories.clone())`.
#[derive(Debug, Clone)]
pub struct PgStore {
    pool: PgPool,
    /// [`rustryx_core::ports::RepositoryRepo`].
    pub repositories: PgRepositoryRepo,
    /// [`rustryx_core::ports::BlobStoreRepo`].
    pub blob_stores: PgBlobStoreRepo,
    /// [`rustryx_core::ports::BlobRepo`].
    pub blobs: PgBlobRepo,
    /// [`rustryx_core::ports::ComponentRepo`].
    pub components: PgComponentRepo,
    /// [`rustryx_core::ports::AssetRepo`].
    pub assets: PgAssetRepo,
    /// [`rustryx_core::ports::UserRepo`].
    pub users: PgUserRepo,
    /// [`rustryx_core::ports::RoleRepo`].
    pub roles: PgRoleRepo,
    /// [`rustryx_core::ports::PrivilegeRepo`].
    pub privileges: PgPrivilegeRepo,
    /// [`rustryx_core::ports::SelectorRepo`].
    pub selectors: PgSelectorRepo,
    /// [`rustryx_core::ports::TokenRepo`].
    pub tokens: PgTokenRepo,
    /// [`rustryx_core::ports::IdpMappingRepo`].
    pub idp_mappings: PgIdpMappingRepo,
    /// [`rustryx_core::ports::LoginAttemptRepo`].
    pub login_attempts: PgLoginAttemptRepo,
    /// [`rustryx_core::ports::AuditSink`].
    pub audit: PgAuditSink,
    /// [`rustryx_core::ports::AuditReadRepo`].
    pub audit_read: PgAuditRead,
    /// [`rustryx_core::ports::JobLeaseRepo`]: durable job queues.
    pub job_leases: PgJobLeases,
    /// [`rustryx_core::ports::SystemConfigRepo`]: runtime key/value configuration.
    pub system_config: PgSystemConfig,
    /// [`rustryx_core::ports::RevokedTokenRepo`].
    pub revoked_tokens: PgRevokedTokens,
    /// [`rustryx_core::ports::RoutingRuleRepo`].
    pub routing_rules: PgRoutingRuleRepo,
}

impl PgStore {
    /// Builds the bundle. `sealer` protects upstream credentials of proxy repositories and the
    /// sealed fields of the settings.
    #[must_use]
    pub fn new(pool: PgPool, sealer: Arc<dyn SecretSealer>) -> Self {
        Self {
            repositories: PgRepositoryRepo::new(pool.clone(), Arc::clone(&sealer)),
            blob_stores: PgBlobStoreRepo::new(pool.clone()),
            blobs: PgBlobRepo::new(pool.clone()),
            components: PgComponentRepo::new(pool.clone()),
            assets: PgAssetRepo::new(pool.clone()),
            users: PgUserRepo::new(pool.clone()),
            roles: PgRoleRepo::new(pool.clone()),
            privileges: PgPrivilegeRepo::new(pool.clone()),
            selectors: PgSelectorRepo::new(pool.clone()),
            tokens: PgTokenRepo::new(pool.clone()),
            idp_mappings: PgIdpMappingRepo::new(pool.clone()),
            login_attempts: PgLoginAttemptRepo::new(pool.clone()),
            audit: PgAuditSink::new(pool.clone()),
            audit_read: PgAuditRead::new(pool.clone()),
            job_leases: PgJobLeases::new(pool.clone()),
            system_config: PgSystemConfig::new(pool.clone(), sealer),
            revoked_tokens: PgRevokedTokens::new(pool.clone()),
            routing_rules: PgRoutingRuleRepo::new(pool.clone()),
            pool,
        }
    }

    /// The invalidation bus over this store's pool. `listen_url` must be a direct URL (see
    /// [`PgInvalidationBus`]).
    #[must_use]
    pub fn invalidation_bus(&self, listen_url: Secret) -> PgInvalidationBus {
        PgInvalidationBus::new(self.pool.clone(), listen_url)
    }

    /// The shared pool, for `ping`, `schema_status` and the maintenance functions.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}
