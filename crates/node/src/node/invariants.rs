use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoreInvariantSnapshot {
    pub(crate) head_number: u64,
    pub(crate) head_hash: ShellHash,
    pub(crate) finalized_number: u64,
    pub(crate) finalized_hash: ShellHash,
    pub(crate) chain_totals_head: Option<u64>,
    pub(crate) tx_pool_len: usize,
}

impl<S: KvStore + 'static> Node<S> {
    /// Check the core chain invariants that must hold before a node is considered
    /// safe to produce, import further blocks, or report healthy RPC readiness.
    pub(crate) fn check_core_invariants(&self) -> Result<CoreInvariantSnapshot, NodeError> {
        let head = self
            .chain_store
            .get_head_block()?
            .ok_or(NodeError::NoGenesis)?;
        let head_hash = head.hash();
        let head_number = head.number();

        let canonical_head_hash = self
            .chain_store
            .get_block_hash_by_number(head_number)?
            .ok_or_else(|| {
                NodeError::Startup(format!(
                    "core invariant violated: missing canonical mapping for head #{head_number}"
                ))
            })?;
        if canonical_head_hash != head_hash {
            return Err(NodeError::Startup(format!(
                "core invariant violated: head #{head_number} hash {head_hash} does not match canonical hash {canonical_head_hash}"
            )));
        }

        let (finalized_number, finalized_hash) = {
            let finality = self.finality.read();
            (
                finality.last_finalized_number(),
                *finality.last_finalized_hash(),
            )
        };
        if finalized_number > head_number {
            return Err(NodeError::Startup(format!(
                "core invariant violated: finalized #{finalized_number} is ahead of head #{head_number}"
            )));
        }

        if finalized_number > 0 {
            let canonical_finalized_hash = self
                .chain_store
                .get_block_hash_by_number(finalized_number)?
                .ok_or_else(|| {
                    NodeError::Startup(format!(
                        "core invariant violated: missing canonical mapping for finalized #{finalized_number}"
                    ))
                })?;
            if canonical_finalized_hash != finalized_hash {
                return Err(NodeError::Startup(format!(
                    "core invariant violated: finalized #{finalized_number} hash {finalized_hash} does not match canonical hash {canonical_finalized_hash}"
                )));
            }
        }

        let live_state_root = {
            let mut ws = self.world_state.write();
            ws.state_root()?
        };
        if live_state_root != head.header.state_root {
            return Err(NodeError::Startup(format!(
                "core invariant violated: live state root {live_state_root} does not match head state root {}",
                head.header.state_root
            )));
        }

        let chain_totals_head = self.chain_store.get_chain_totals_head()?;
        if chain_totals_head.is_some_and(|totals_head| totals_head > head_number) {
            return Err(NodeError::Startup(format!(
                "core invariant violated: chain totals head {} is ahead of canonical head {head_number}",
                chain_totals_head.unwrap_or_default()
            )));
        }

        Ok(CoreInvariantSnapshot {
            head_number,
            head_hash,
            finalized_number,
            finalized_hash,
            chain_totals_head,
            tx_pool_len: self.tx_pool.len(),
        })
    }
}
