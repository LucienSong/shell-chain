//! JSON-RPC server builder and configuration.

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::server::{Server, ServerHandle};
use tracing::{info, warn};

use shell_core::SignedTransaction;
use shell_crypto::Signer;
use shell_mempool::TxPool;
use shell_primitives::Address;
use shell_storage::{ChainStore, KvStore, WorldState};

use crate::api::{EthApiServer, ShellApiServer, Web3ApiServer, NetApiServer};
use crate::handler::RpcHandler;
use crate::subscriptions::{BlockEvent, EthPubSubServer};
use crate::tls;

/// Configuration for the JSON-RPC server.
#[derive(Debug, Clone)]
pub struct RpcConfig {
    /// Address to bind the HTTP (+WS) server (default: 127.0.0.1:8545).
    pub listen_addr: SocketAddr,
    /// Maximum number of concurrent connections (default: 100).
    pub max_connections: u32,
    /// Optional dedicated WebSocket address. When `Some`, a WS-only server is
    /// started on this address and the HTTP server becomes HTTP-only.
    /// When `None`, the main server at `listen_addr` handles both HTTP and WS.
    pub ws_addr: Option<SocketAddr>,
    /// Path to a PEM-encoded TLS certificate file for WSS/HTTPS transport.
    /// Both `tls_cert_path` and `tls_key_path` must be set to enable TLS.
    pub tls_cert_path: Option<String>,
    /// Path to a PEM-encoded TLS private key file for WSS/HTTPS transport.
    /// Both `tls_cert_path` and `tls_key_path` must be set to enable TLS.
    pub tls_key_path: Option<String>,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8545)),
            max_connections: 100,
            ws_addr: Some(SocketAddr::from(([127, 0, 0, 1], 8546))),
            tls_cert_path: None,
            tls_key_path: None,
        }
    }
}

/// Handles returned by [`start_rpc_server`] for graceful shutdown.
pub struct RpcServerHandle {
    /// Bound HTTP (or HTTP+WS) address.
    pub http_addr: SocketAddr,
    /// Handle to stop the HTTP server.
    pub http_handle: ServerHandle,
    /// Bound WebSocket address, if a dedicated WS server was started.
    pub ws_addr: Option<SocketAddr>,
    /// Handle to stop the WS server (present when `ws_addr` is `Some`).
    pub ws_handle: Option<ServerHandle>,
}

/// Build and start the JSON-RPC server(s).
///
/// When `config.ws_addr` is `Some`, two servers are started:
///   - HTTP-only on `config.listen_addr`
///   - WS-only on `config.ws_addr`
///
/// When `config.ws_addr` is `None`, a single server on `config.listen_addr`
/// handles both HTTP and WebSocket (the jsonrpsee default).
///
/// Returns an [`RpcServerHandle`] for graceful shutdown.
pub async fn start_rpc_server<S: KvStore + 'static>(
    config: RpcConfig,
    chain_store: Arc<ChainStore<S>>,
    world_state: Arc<parking_lot::RwLock<WorldState<S>>>,
    tx_pool: Arc<TxPool>,
    chain_id: u64,
    tx_broadcast: Option<tokio::sync::mpsc::UnboundedSender<SignedTransaction>>,
    block_events: tokio::sync::broadcast::Sender<BlockEvent>,
    proposer_signer: Option<Arc<dyn Signer>>,
    proposer_address: Option<Address>,
) -> Result<RpcServerHandle, Box<dyn std::error::Error + Send + Sync>> {
    // Validate TLS configuration if provided.
    match tls::load_tls_config(
        config.tls_cert_path.as_deref(),
        config.tls_key_path.as_deref(),
    ) {
        Ok(Some(_tls_cfg)) => {
            // TLS cert/key validated successfully. jsonrpsee's ServerBuilder does
            // not natively accept a rustls config, so full WSS transport requires
            // a TLS-terminating reverse proxy (e.g. nginx, caddy) or a custom
            // hyper-rustls acceptor in front of the RPC server.
            //
            // TODO: integrate tokio-rustls TlsAcceptor with a custom hyper
            // service to serve WSS directly.
            info!(
                "TLS certificate and key validated successfully \
                 (cert={}, key={}). \
                 NOTE: WSS transport is not yet wired — use a TLS-terminating \
                 proxy for production WSS until native support is added.",
                config.tls_cert_path.as_deref().unwrap_or(""),
                config.tls_key_path.as_deref().unwrap_or(""),
            );
        }
        Ok(None) => {
            info!("RPC server starting without TLS (plain HTTP/WS)");
        }
        Err(e) => {
            warn!("TLS configuration error: {e}. Starting without TLS.");
        }
    }

    let mut handler = RpcHandler::new(
        chain_store,
        world_state,
        tx_pool,
        chain_id,
        tx_broadcast,
        block_events,
    );
    if let (Some(signer), Some(addr)) = (proposer_signer, proposer_address) {
        handler = handler.with_proposer(signer, addr);
    }

    let mut module = jsonrpsee::server::RpcModule::new(());
    module.merge(EthApiServer::into_rpc(handler.clone()))?;
    module.merge(ShellApiServer::into_rpc(handler.clone()))?;
    module.merge(Web3ApiServer::into_rpc(handler.clone()))?;
    module.merge(NetApiServer::into_rpc(handler.clone()))?;
    module.merge(EthPubSubServer::into_rpc(handler))?;

    if let Some(ws_listen) = config.ws_addr {
        // Separate ports: HTTP-only + WS-only.
        let http_server = Server::builder()
            .max_connections(config.max_connections)
            .http_only()
            .build(config.listen_addr)
            .await?;
        let http_addr = http_server.local_addr()?;
        let http_handle = http_server.start(module.clone());

        let ws_server = Server::builder()
            .max_connections(config.max_connections)
            .ws_only()
            .build(ws_listen)
            .await?;
        let ws_addr = ws_server.local_addr()?;
        let ws_handle = ws_server.start(module);

        Ok(RpcServerHandle {
            http_addr,
            http_handle,
            ws_addr: Some(ws_addr),
            ws_handle: Some(ws_handle),
        })
    } else {
        // Single port: both HTTP and WS on listen_addr (jsonrpsee default).
        let server = Server::builder()
            .max_connections(config.max_connections)
            .build(config.listen_addr)
            .await?;
        let http_addr = server.local_addr()?;
        let http_handle = server.start(module);

        Ok(RpcServerHandle {
            http_addr,
            http_handle,
            ws_addr: None,
            ws_handle: None,
        })
    }
}
