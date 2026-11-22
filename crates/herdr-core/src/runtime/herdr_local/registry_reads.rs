use super::Core;
use crate::rpc::RpcError;
use std::time::Duration;

impl Core {
    /// For a mutation's own barrier, capture this with the state lock held after saving.
    pub(crate) fn submitted_registry_version(&self) -> u64 {
        self.registry_writer.submitted()
    }

    pub(crate) async fn flush_saved_through(&self, version: u64) -> Result<(), RpcError> {
        tokio::time::timeout(Duration::from_secs(10), self.registry_writer.flush_through(version))
            .await
            .map_err(|_| {
                RpcError::new(
                    "unavailable",
                    "registry persistence timed out; the operation may already have completed",
                    "check the terminal and disk before retrying",
                )
            })?
            .map_err(|e| RpcError::internal(format!("cannot save terminals: {e}")))
    }
}
