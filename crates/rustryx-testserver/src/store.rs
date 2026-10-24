//! The blob store registry of the harness.

use std::sync::Arc;

use async_trait::async_trait;
use rustryx_core::error::BlobError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::ports::BlobStore;
use rustryx_format_core::BlobStoreRegistry;

/// Hands one store to every repository and every blob row.
///
/// One store is enough because the harness never models blob store groups: a repository
/// points at the fixture store id and every id resolves to the same object. A test that needs
/// a different store (a digest-only store for a 256 MiB body, a failing store) passes it to
/// `TestServerBuilder::blob_store`.
pub(crate) struct SingleStoreRegistry(pub(crate) Arc<dyn BlobStore>);

#[async_trait]
impl BlobStoreRegistry for SingleStoreRegistry {
    async fn store_by_id(&self, _id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError> {
        Ok(Arc::clone(&self.0))
    }
}
