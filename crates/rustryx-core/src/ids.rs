//! Strongly typed identifiers. All ids are `UUIDv7` (time ordered), so a b-tree
//! index on them stays append-mostly.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a new time-ordered (`UUIDv7`) identifier.
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wraps an existing UUID (for example one read from the database).
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }
    };
}

id_type!(
    /// Identifier of a repository.
    RepositoryId
);
id_type!(
    /// Identifier of a blob store definition.
    BlobStoreId
);
id_type!(
    /// Identifier of a stored blob (one immutable content object).
    BlobId
);
id_type!(
    /// Identifier of a component (a logical package version).
    ComponentId
);
id_type!(
    /// Identifier of an asset (a file addressed by path inside a repository).
    AssetId
);
id_type!(
    /// Identifier of a user account.
    UserId
);
id_type!(
    /// Identifier of a role.
    RoleId
);
id_type!(
    /// Identifier of a privilege.
    PrivilegeId
);
id_type!(
    /// Identifier of a content selector.
    ContentSelectorId
);
id_type!(
    /// Identifier of an API token record (not the secret itself).
    ApiTokenId
);
id_type!(
    /// Identifier of an identity-provider group to role mapping.
    IdpGroupMappingId
);
id_type!(
    /// Identifier of a cleanup policy.
    CleanupPolicyId
);
id_type!(
    /// Identifier of a routing rule.
    RoutingRuleId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_unique_and_round_trip() {
        let a = RepositoryId::generate();
        let b = RepositoryId::generate();
        assert_ne!(a, b);
        let parsed: RepositoryId = a.to_string().parse().expect("round trip");
        assert_eq!(a, parsed);
    }

    #[test]
    fn invalid_uuid_is_rejected() {
        assert!("not-a-uuid".parse::<UserId>().is_err());
    }
}
