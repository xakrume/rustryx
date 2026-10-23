//! The node registry and the managed trust store of outbound certificates.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::TrustedCertificateId;
use rustryx_core::node::NodeInfo;
use rustryx_core::ports::{CertificateDependents, NodeRegistryRepo, TrustedCertificateRepo};
use rustryx_core::settings::TrustedCertificate;

use super::state::State;
use super::{MemStore, conflict, not_found};

/// Longest node id, in characters.
const MAX_NODE_ID_CHARS: usize = 200;

#[async_trait]
impl NodeRegistryRepo for MemStore {
    async fn heartbeat(&self, node: &NodeInfo) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // The CHECK constraints of `node_heartbeats`.
        if node.node_id.is_empty()
            || node.node_id.chars().count() > MAX_NODE_ID_CHARS
            || node.feature_level < 1
        {
            return Err(conflict(
                "a node needs an id of 1 to 200 characters and a level",
            ));
        }
        let mut row = node.clone();
        // Time comes from the store, not from the node: a node with a skewed clock must not
        // look alive for longer, or dead for shorter, than it is.
        row.seen_at = now;
        state.nodes.insert(row.node_id.clone(), row);
        Ok(())
    }

    async fn list_live(&self, max_age: Duration) -> Result<Vec<NodeInfo>, StoreError> {
        let now = self.now();
        let oldest = now - time::Duration::try_from(max_age).unwrap_or(time::Duration::MAX);
        let state = self.tables()?;
        let mut live: Vec<NodeInfo> = state
            .nodes
            .values()
            .filter(|node| node.seen_at >= oldest)
            .cloned()
            .collect();
        live.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(live)
    }
}

#[async_trait]
impl TrustedCertificateRepo for MemStore {
    async fn create(&self, certificate: &TrustedCertificate) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        certificate
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let taken = state.trusted_certificates.values().any(|c| {
            c.id == certificate.id
                || c.name == certificate.name
                || c.fingerprint_sha256 == certificate.fingerprint_sha256
        });
        if taken {
            return Err(conflict(format!(
                "certificate {} already exists",
                certificate.name
            )));
        }
        state
            .trusted_certificates
            .insert(certificate.id, certificate.clone());
        Ok(())
    }

    async fn get(
        &self,
        id: TrustedCertificateId,
    ) -> Result<Option<TrustedCertificate>, StoreError> {
        Ok(self.tables()?.trusted_certificates.get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<TrustedCertificate>, StoreError> {
        let mut all: Vec<TrustedCertificate> = self
            .tables()?
            .trusted_certificates
            .values()
            .cloned()
            .collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn delete(&self, id: TrustedCertificateId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.trusted_certificates.contains_key(&id) {
            return Err(not_found("trusted certificate", id));
        }
        let dependents = state.certificate_dependents(id);
        if !dependents.repositories.is_empty()
            || !dependents.registries.is_empty()
            || !dependents.scanners.is_empty()
        {
            return Err(conflict("the certificate is still in use"));
        }
        state.trusted_certificates.remove(&id);
        Ok(())
    }

    async fn dependents(
        &self,
        id: TrustedCertificateId,
    ) -> Result<CertificateDependents, StoreError> {
        Ok(self.tables()?.certificate_dependents(id))
    }
}

impl State {
    fn certificate_dependents(&self, id: TrustedCertificateId) -> CertificateDependents {
        let mut repositories: Vec<_> = self
            .repositories
            .values()
            .filter(|r| {
                r.proxy
                    .as_ref()
                    .is_some_and(|p| p.ca_certificate_ids.contains(&id))
            })
            .map(|r| r.id)
            .collect();
        repositories.sort();
        let mut registries: Vec<_> = self
            .registries
            .values()
            .filter(|r| r.ca_certificate_ids.contains(&id))
            .map(|r| r.id)
            .collect();
        registries.sort();
        let mut scanners: Vec<_> = self
            .scanners
            .values()
            .filter(|s| s.ca_certificate_ids.contains(&id))
            .map(|s| s.id)
            .collect();
        scanners.sort();
        CertificateDependents {
            repositories,
            registries,
            scanners,
        }
    }
}
