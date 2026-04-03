//! shell-rpc: JSON-RPC server for the shell-chain node.
//!
//! Provides Ethereum-compatible `eth_*` endpoints and shell-chain
//! extension `shell_*` endpoints for post-quantum features.

pub mod api;
pub mod filter;
pub mod handler;
pub mod server;
pub mod subscriptions;
pub mod tls;
pub mod types;

pub use handler::RpcHandler;
pub use server::{start_rpc_server, RpcConfig, RpcServerHandle};
pub use subscriptions::BlockEvent;
pub use tls::TlsConfig;
