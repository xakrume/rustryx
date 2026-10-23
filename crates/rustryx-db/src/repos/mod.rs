//! Implementations of the store ports of `rustryx-core`.
//!
//! Every method is one statement or one transaction (the port contract: cancel safe), binds
//! all values as parameters, and maps errors through [`crate::error`].

pub mod asset;
pub mod blob;
pub mod blob_store;
pub mod component;
pub mod idp_mapping;
pub mod login_attempt;
pub mod privilege;
pub mod repository;
pub mod role;
pub mod routing_rule;
pub mod selector;
pub mod token;
pub mod user;

mod asset_write;
mod common;
mod rows;
mod write;

pub(crate) use common::{escape_like, notify};

pub use asset::PgAssetRepo;
pub use blob::PgBlobRepo;
pub use blob_store::PgBlobStoreRepo;
pub use component::PgComponentRepo;
pub use idp_mapping::PgIdpMappingRepo;
pub use login_attempt::PgLoginAttemptRepo;
pub use privilege::PgPrivilegeRepo;
pub use repository::PgRepositoryRepo;
pub use role::PgRoleRepo;
pub use routing_rule::PgRoutingRuleRepo;
pub use selector::PgSelectorRepo;
pub use token::PgTokenRepo;
pub use user::PgUserRepo;
