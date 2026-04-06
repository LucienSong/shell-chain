//! shell-node: Node harness assembling all shell-chain components.
//!
//! Provides `NodeBuilder` for ergonomic construction and `Node` for
//! running the event loop with block production, mempool management,
//! and network message handling.

pub mod builder;
pub mod checkpoint;
pub mod config;
pub mod error;
pub mod metrics;
pub mod node;
pub mod pruning;
pub mod reorg;

pub use builder::NodeBuilder;
pub use config::{MetricsConfig, NodeConfig};
pub use error::NodeError;
pub use metrics::Metrics;
pub use node::Node;
pub use pruning::{PruningConfig, StateRootTracker};
pub use reorg::{ReorgEngine, ReorgResult};
