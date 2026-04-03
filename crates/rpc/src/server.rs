//! JSON-RPC server builder and configuration.

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::server::{Server, ServerHandle};

use shell_core::SignedTransaction;
use shell_mempool::TxPool;
use shell_storage::{ChainStore, KvStore, WorldState};

use crate::api::{EthApiServer, ShellApiServer};
use crate::handler::RpcHandler;

/// Configuration for the JSON-RPC server.
#[derive(Debug, Clone)]
pub struct RpcConfig {
    /// Address to bind the server to (default: 127.0.0.1:8545).
    pub listen_addr: SocketAddr,
    /// Maximum number of concurrent connections (default: 100).
    pub max_connections: u32,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8545)),
            max_connections: 100,
        }
    }
}

/// Build and start the JSON-RPC server.
///
/// Returns a `ServerHandle` that can be used to stop the server gracefully.
pub async fn start_rpc_server<S: KvStore + 'static>(
    config: RpcConfig,
    chain_store: Arc<ChainStore<S>>,
    world_state: Arc<parking_lot::RwLock<WorldState<S>>>,
    tx_pool: Arc<TxPool>,
    chain_id: u64,
    tx_broadcast: Option<tokio::sync::mpsc::UnboundedSender<SignedTransaction>>,
) -> Result<(SocketAddr, ServerHandle), Box<dyn std::error::Error + Send + Sync>> {
    let server = Server::builder()
        .max_connections(config.max_connections)
        .build(config.listen_addr)
        .await?;

    let handler = RpcHandler::new(chain_store, world_state, tx_pool, chain_id, tx_broadcast);

    let mut module = jsonrpsee::server::RpcModule::new(());
    module.merge(EthApiServer::into_rpc(handler.clone()))?;
    module.merge(ShellApiServer::into_rpc(handler))?;

    let addr = server.local_addr()?;
    let handle = server.start(module);

    Ok((addr, handle))
}
