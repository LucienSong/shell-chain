/// Configuration for the transaction mempool.
#[derive(Debug, Clone)]
pub struct MempoolConfig {
    /// Maximum number of transactions in the pool.
    pub max_pool_size: usize,
    /// Maximum number of pending transactions per sender address.
    pub max_per_sender: usize,
    /// Expected chain ID — reject transactions targeting other chains.
    pub chain_id: u64,
    /// Minimum gas price (max_fee_per_gas) to accept into the pool.
    pub min_gas_price: u64,
    /// Minimum fee bump percentage for Replace-by-Fee (RBF).
    /// A new tx must offer at least `(100 + bump_pct)%` of the old tx's
    /// `max_priority_fee_per_gas` to replace it at the same nonce.
    pub replacement_fee_bump_pct: u64,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            max_pool_size: 4096,
            max_per_sender: 64,
            chain_id: 1,
            min_gas_price: 0,
            replacement_fee_bump_pct: 10,
        }
    }
}
