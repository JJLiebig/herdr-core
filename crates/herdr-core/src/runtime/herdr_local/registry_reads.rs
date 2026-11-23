use super::Core;
use crate::rpc::RpcError;

impl Core {
    /// For a mutation's own barrier, capture this with the state lock held after saving.
    pub(crate) fn submitted_registry_version(&self) -> u64 {
        self.registry_writer.submitted()
    }

    pub(crate) async fn flush_observed_through(&self, version: u64) -> Result<(), RpcError> {
        self.wait_for_save(version).await.map(drop)
    }
}
