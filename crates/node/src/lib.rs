//! shell-node: Node harness assembling all shell-chain components.
//!
//! Provides `NodeBuilder` for ergonomic construction and `Node` for
//! running the event loop with block production, mempool management,
//! and network message handling.

pub mod builder;
pub mod config;
pub mod error;
pub mod node;

pub use builder::NodeBuilder;
pub use config::NodeConfig;
pub use error::NodeError;
pub use node::Node;
