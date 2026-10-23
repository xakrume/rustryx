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
id_type!(
    /// Identifier of a project.
    ProjectId
);
id_type!(
    /// Identifier of a project member row.
    ProjectMemberId
);
id_type!(
    /// Identifier of a label.
    LabelId
);
id_type!(
    /// Identifier of a robot token record.
    RobotTokenId
);
id_type!(
    /// Identifier of an immutability rule.
    ImmutabilityRuleId
);
id_type!(
    /// Identifier of a retention policy.
    RetentionPolicyId
);
id_type!(
    /// Identifier of a lifecycle run (cleanup, retention, label cleanup).
    LifecycleRunId
);
id_type!(
    /// Identifier of a lifecycle hold.
    LifecycleHoldId
);
id_type!(
    /// Identifier of a garbage collection run.
    GcRunId
);
id_type!(
    /// Identifier of a task definition.
    TaskDefinitionId
);
id_type!(
    /// Identifier of one run of a task.
    TaskRunId
);
id_type!(
    /// Identifier of an event (stable across delivery attempts).
    EventId
);
id_type!(
    /// Identifier of a scanner registration.
    ScannerId
);
id_type!(
    /// Identifier of a scan report.
    ScanReportId
);
id_type!(
    /// Identifier of a CVE allowlist entry.
    CveAllowlistEntryId
);
id_type!(
    /// Identifier of a scan or gate policy row.
    SecurityPolicyId
);
id_type!(
    /// Identifier of a trust policy.
    TrustPolicyId
);
id_type!(
    /// Identifier of a piece of trust material (key, root, store).
    TrustMaterialId
);
id_type!(
    /// Identifier of a scanner data set version.
    ScannerDataSetId
);
id_type!(
    /// Identifier of an export (CSV and similar).
    ExportId
);
id_type!(
    /// Identifier of a registry endpoint.
    RegistryId
);
id_type!(
    /// Identifier of a replication rule.
    ReplicationRuleId
);
id_type!(
    /// Identifier of a replication execution.
    ReplicationExecutionId
);
id_type!(
    /// Identifier of a replication task.
    ReplicationTaskId
);
id_type!(
    /// Identifier of a webhook.
    WebhookId
);
id_type!(
    /// Identifier of a webhook delivery.
    WebhookDeliveryId
);
id_type!(
    /// Identifier of a notification subscription.
    SubscriptionId
);
id_type!(
    /// Identifier of an e-mail delivery.
    EmailDeliveryId
);
id_type!(
    /// Identifier of a promotion request.
    PromotionId
);
id_type!(
    /// Identifier of a trusted certificate.
    TrustedCertificateId
);
id_type!(
    /// Identifier of a blob store verification run.
    VerifyRunId
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

    /// Every wave 2 id type generates, prints, parses and serializes as a bare UUID string.
    macro_rules! assert_id_behaviour {
        ($($name:ident),+ $(,)?) => {$(
            let a = $name::generate();
            let b = $name::generate();
            assert_ne!(a, b, stringify!($name));
            assert_eq!(a.to_string().parse::<$name>().expect("parse"), a, stringify!($name));
            assert_eq!($name::from_uuid(a.as_uuid()), a, stringify!($name));
            assert_eq!($name::from(a.as_uuid()), a, stringify!($name));
            let json = serde_json::to_string(&a).expect("serialize");
            assert_eq!(json, format!("\"{a}\""), "transparent serde of {}", stringify!($name));
            assert_eq!(serde_json::from_str::<$name>(&json).expect("deserialize"), a);
            assert!("not-a-uuid".parse::<$name>().is_err(), stringify!($name));
        )+};
    }

    #[test]
    fn wave2_ids_round_trip_as_plain_uuids() {
        assert_id_behaviour!(
            ProjectId,
            ProjectMemberId,
            LabelId,
            RobotTokenId,
            ImmutabilityRuleId,
            RetentionPolicyId,
            LifecycleRunId,
            LifecycleHoldId,
            GcRunId,
            TaskDefinitionId,
            TaskRunId,
            EventId,
            ScannerId,
            ScanReportId,
            CveAllowlistEntryId,
            SecurityPolicyId,
            TrustPolicyId,
            TrustMaterialId,
            ScannerDataSetId,
            ExportId,
            RegistryId,
            ReplicationRuleId,
            ReplicationExecutionId,
            ReplicationTaskId,
            WebhookId,
            WebhookDeliveryId,
            SubscriptionId,
            EmailDeliveryId,
            PromotionId,
            TrustedCertificateId,
            VerifyRunId,
        );
    }

    #[test]
    fn ids_are_time_ordered() {
        let first = EventId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = EventId::generate();
        assert!(first < second, "UUIDv7 sorts by creation time");
    }
}
