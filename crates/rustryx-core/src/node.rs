//! Node registry types and the feature level gate that keeps a rolling upgrade safe.
//!
//! The schema compatibility check tells a binary whether the schema suits it. It does not cover
//! the opposite direction: an N-1 binary that keeps serving while data of a kind it cannot read
//! starts to appear. A feature that creates such data waits until every live node is at the
//! level that understands it ([`gate_open`]); the gate opens by itself when the last old pod is
//! gone, so no operator step can be forgotten.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Feature level of the running binary. Phase 1 was level 1; wave 2 ships as level 2. A later
/// release that adds a gated feature raises it by one.
pub const CURRENT_FEATURE_LEVEL: u32 = 2;

/// A feature whose data an older node cannot read, or whose enforcement an older node would
/// skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeatureId {
    /// Projects, project members and project roles.
    Projects,
    /// Robot tokens.
    RobotTokens,
    /// Application area privileges.
    ApplicationPrivileges,
    /// Azure and group blob stores.
    BlobGroupsAndAzure,
    /// The pull gate in `enforce` mode.
    PullGateEnforce,
    /// Quarantine of new proxy components.
    Quarantine,
    /// Quota counters as the authority.
    QuotaCounters,
    /// Read only mode (the middleware part; the SQL guard applies to every binary).
    ReadOnlyMode,
}

impl FeatureId {
    /// Every feature, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Projects,
        Self::RobotTokens,
        Self::ApplicationPrivileges,
        Self::BlobGroupsAndAzure,
        Self::PullGateEnforce,
        Self::Quarantine,
        Self::QuotaCounters,
        Self::ReadOnlyMode,
    ];

    /// The level at which every node understands the feature. All wave 2 features are 2.
    #[must_use]
    pub const fn required_level(self) -> u32 {
        2
    }
}

/// One row of the node registry: a running instance and what it has applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodeInfo {
    /// Node id from configuration or the hostname.
    pub node_id: String,
    /// Build version.
    pub version: String,
    /// Feature level of the binary.
    pub feature_level: u32,
    /// Process start.
    #[serde(with = "crate::rfc3339")]
    pub started_at: OffsetDateTime,
    /// Last heartbeat.
    #[serde(with = "crate::rfc3339")]
    pub seen_at: OffsetDateTime,
    /// Highest `system_config` revision this node has applied (read only mode, settings).
    pub settings_revision: i64,
    /// Write requests in flight, for the read only quiesce check.
    pub inflight_writes: u32,
}

impl NodeInfo {
    /// A node that has applied no settings yet and has no write in flight.
    #[must_use]
    pub fn new(
        node_id: impl Into<String>,
        version: impl Into<String>,
        feature_level: u32,
        started_at: OffsetDateTime,
        seen_at: OffsetDateTime,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            version: version.into(),
            feature_level,
            started_at,
            seen_at,
            settings_revision: 0,
            inflight_writes: 0,
        }
    }
}

/// Open when every live node is at or above the level of the feature. An empty node list is
/// closed (fail closed): not seeing any node, this one included, means the registry cannot be
/// trusted.
#[must_use]
pub fn gate_open(feature: FeatureId, live_nodes: &[NodeInfo]) -> bool {
    !live_nodes.is_empty()
        && live_nodes
            .iter()
            .all(|node| node.feature_level >= feature.required_level())
}

/// The nodes that keep a gate closed, for the `cluster_upgrade_in_progress` answer.
#[must_use]
pub fn nodes_below_level(feature: FeatureId, live_nodes: &[NodeInfo]) -> Vec<&NodeInfo> {
    live_nodes
        .iter()
        .filter(|node| node.feature_level < feature.required_level())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn node(id: &str, level: u32) -> NodeInfo {
        NodeInfo::new(id, "0.2.0", level, at(), at())
    }

    /// No wildcard arm: a new feature has to be added here and to `ALL`.
    fn feature_position(feature: FeatureId) -> usize {
        match feature {
            FeatureId::Projects => 0,
            FeatureId::RobotTokens => 1,
            FeatureId::ApplicationPrivileges => 2,
            FeatureId::BlobGroupsAndAzure => 3,
            FeatureId::PullGateEnforce => 4,
            FeatureId::Quarantine => 5,
            FeatureId::QuotaCounters => 6,
            FeatureId::ReadOnlyMode => 7,
        }
    }

    #[test]
    fn all_lists_every_feature_exactly_once_in_order() {
        assert_eq!(FeatureId::ALL.len(), 8);
        for (index, feature) in FeatureId::ALL.iter().enumerate() {
            assert_eq!(feature_position(*feature), index, "{feature:?}");
        }
    }

    #[test]
    fn current_level_satisfies_every_feature() {
        for feature in FeatureId::ALL {
            assert!(
                CURRENT_FEATURE_LEVEL >= feature.required_level(),
                "{feature:?}"
            );
            assert_eq!(feature.required_level(), 2);
        }
    }

    #[test]
    fn must_not_open_a_gate_while_an_older_node_is_live() {
        let nodes = [node("a", 2), node("b", 1), node("c", 2)];
        for feature in FeatureId::ALL {
            assert!(!gate_open(*feature, &nodes), "{feature:?}");
        }
        let below = nodes_below_level(FeatureId::Projects, &nodes);
        assert_eq!(below.len(), 1);
        assert_eq!(below[0].node_id, "b");
    }

    #[test]
    fn a_gate_opens_when_every_node_is_at_the_level_or_above() {
        let nodes = [node("a", 2), node("b", 3)];
        assert!(gate_open(FeatureId::QuotaCounters, &nodes));
        assert!(nodes_below_level(FeatureId::QuotaCounters, &nodes).is_empty());
    }

    #[test]
    fn must_not_open_a_gate_for_an_empty_registry() {
        assert!(!gate_open(FeatureId::Projects, &[]));
    }

    #[test]
    fn constructor_defaults_and_wire_form() {
        let info = node("n1", 2);
        assert_eq!((info.settings_revision, info.inflight_writes), (0, 0));
        let json = round_trip_json(&info);
        assert_eq!(json["seen_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["feature_level"], 2);
    }

    #[test]
    fn feature_ids_use_snake_case() {
        assert_wire(&FeatureId::BlobGroupsAndAzure, "\"blob_groups_and_azure\"");
        assert_wire(&FeatureId::PullGateEnforce, "\"pull_gate_enforce\"");
        assert_wire(&FeatureId::ReadOnlyMode, "\"read_only_mode\"");
    }
}
