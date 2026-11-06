//! Implementations of the store ports of `rustryx-core`.
//!
//! Every method is one statement or one transaction (the port contract: cancel safe), binds
//! all values as parameters, and maps errors through [`crate::error`].

pub mod allowlist;
pub mod asset;
pub mod blob;
pub mod blob_group;
pub mod blob_store;
pub mod cleanup;
pub mod component;
pub mod downloads;
pub mod gc;
pub mod idp_mapping;
pub mod immutability;
pub mod label;
pub mod lifecycle;
pub mod lifecycle_hold;
pub mod login_attempt;
pub mod notification;
pub mod privilege;
pub mod project_member;
pub mod promotion;
pub mod quota;
pub mod registry;
pub mod replication;
pub mod repository;
pub mod retention;
pub mod robot;
pub mod role;
pub mod routing_rule;
pub mod scan_policy;
pub mod scanner;
pub mod selector;
pub mod token;
pub mod trust;
pub mod user;
pub mod webhook;
pub mod webhook_delivery;

mod asset_write;
mod common;
mod rows;
mod write;

pub(crate) use common::{escape_like, notify};

pub use allowlist::PgCveAllowlistRepo;
pub use asset::PgAssetRepo;
pub use blob::PgBlobRepo;
pub use blob_group::PgBlobStoreMemberRepo;
pub use blob_store::PgBlobStoreRepo;
pub use cleanup::PgCleanupPolicyRepo;
pub use component::PgComponentRepo;
pub use downloads::PgDownloadStatsRepo;
pub use gc::PgGcRunRepo;
pub use idp_mapping::PgIdpMappingRepo;
pub use immutability::PgImmutabilityRepo;
pub use label::PgLabelRepo;
pub use lifecycle::{PgLifecycleRepo, PgLifecycleRunRepo};
pub use lifecycle_hold::PgLifecycleHoldRepo;
pub use login_attempt::PgLoginAttemptRepo;
pub use notification::PgNotificationRepo;
pub use privilege::PgPrivilegeRepo;
pub use project_member::{PgProjectAccessRepo, PgProjectMemberRepo};
pub use promotion::PgPromotionRepo;
pub use quota::PgQuotaRepo;
pub use registry::PgRegistryRepo;
pub use replication::PgReplicationRepo;
pub use repository::PgRepositoryRepo;
pub use retention::PgRetentionPolicyRepo;
pub use robot::PgRobotTokenRepo;
pub use role::PgRoleRepo;
pub use routing_rule::PgRoutingRuleRepo;
pub use scan_policy::PgScanPolicyRepo;
pub use scanner::PgScannerRepo;
pub use selector::PgSelectorRepo;
pub use token::PgTokenRepo;
pub use trust::PgTrustRepo;
pub use user::PgUserRepo;
pub use webhook::PgWebhookRepo;
pub use webhook_delivery::PgWebhookDeliveryRepo;
