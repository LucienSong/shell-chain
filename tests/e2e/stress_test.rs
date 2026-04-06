//! E2E Stress Test — verifies mempool throughput and correctness under load.
//!
//! Sends N transactions rapidly, mines them in one batch, then asserts every
//! receipt exists and the mempool drains to zero.

use std::time::Instant;

use shell_e2e::*;

use shell_core::SignedTransaction;
use shell_primitives::{Address, U256};
use shell_rpc::api::{EthApiServer, ShellApiServer};

/// Number of transactions to submit during the stress test.
const TX_COUNT: usize = 100;

// ---------------------------------------------------------------------------
// 1. Rapid submission — all N transactions accepted into the mempool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rapid_transaction_submission() {
    let env = setup();
    let genesis = make_genesis_block();
    store_block(&env, &genesis);

    let recipient = Address::from([0x02; 20]);
    let mut signed_txs: Vec<SignedTransaction> = Vec::with_capacity(TX_COUNT);

    let start = Instant::now();

    for i in 0..TX_COUNT {
        let sender = make_funded_account(&env);
        // Vary value to ensure unique unsigned tx hash per iteration
        let tx = make_transfer(TEST_CHAIN_ID, 0, recipient, U256::from(100 + i as u64));
        let signed = sign_tx(&sender.signer, sender.address, tx);
        let _hash = ShellApiServer::send_transaction(&env.handler, signed.clone())
            .await
            .unwrap();
        signed_txs.push(signed);

        // Progress check at 25 % intervals
        if (i + 1) % 25 == 0 {
            let pending = ShellApiServer::pending_count(&env.handler).await.unwrap();
            let pending_n = u64::from_str_radix(pending.trim_start_matches("0x"), 16).unwrap();
            assert_eq!(pending_n as usize, i + 1);
        }
    }

    let elapsed = start.elapsed();
    let tps = TX_COUNT as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stress] submitted {TX_COUNT} txs in {:.2?} — {tps:.0} tx/s",
        elapsed
    );

    // All transactions should be in the mempool
    assert_eq!(env.tx_pool.len(), TX_COUNT);

    // ---------------------------------------------------------------------------
    // 2. Batch inclusion — mine one block with all transactions
    // ---------------------------------------------------------------------------

    let block_hash = mine_block(&env, 1, genesis.hash(), signed_txs.clone());

    // Verify every receipt exists and is successful
    for signed in &signed_txs {
        let receipt = EthApiServer::get_transaction_receipt(&env.handler, signed.hash())
            .await
            .unwrap();
        assert!(
            receipt.is_some(),
            "receipt missing for tx {}",
            signed.hash()
        );
        assert_eq!(receipt.unwrap().status, "0x1");
    }

    // The block should be queryable
    let rpc_block = EthApiServer::get_block_by_hash(&env.handler, block_hash, false)
        .await
        .unwrap();
    assert!(rpc_block.is_some());

    // ---------------------------------------------------------------------------
    // 3. Mempool is empty after mining
    // ---------------------------------------------------------------------------

    assert_eq!(
        env.tx_pool.len(),
        0,
        "mempool must be empty after mining all txs"
    );
    let pending = ShellApiServer::pending_count(&env.handler).await.unwrap();
    assert_eq!(pending, "0x0");
}

// ---------------------------------------------------------------------------
// 4. Throughput with multiple blocks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn throughput_multi_block() {
    let env = setup();
    let genesis = make_genesis_block();
    store_block(&env, &genesis);

    let recipient = Address::from([0x03; 20]);
    let block_count = 5usize;
    let txs_per_block = 20usize;
    let total_txs = block_count * txs_per_block;
    let mut parent_hash = genesis.hash();

    let start = Instant::now();

    for blk in 0..block_count {
        let mut batch: Vec<SignedTransaction> = Vec::with_capacity(txs_per_block);
        for j in 0..txs_per_block {
            let sender = make_funded_account(&env);
            // Unique value per tx across all blocks
            let tx = make_transfer(
                TEST_CHAIN_ID,
                0,
                recipient,
                U256::from(50 + (blk * txs_per_block + j) as u64),
            );
            let signed = sign_tx(&sender.signer, sender.address, tx);
            ShellApiServer::send_transaction(&env.handler, signed.clone())
                .await
                .unwrap();
            batch.push(signed);
        }
        parent_hash = mine_block(&env, (blk + 1) as u64, parent_hash, batch);
    }

    let elapsed = start.elapsed();
    let tps = total_txs as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stress] {total_txs} txs across {block_count} blocks in {:.2?} — {tps:.0} tx/s",
        elapsed
    );

    // Block number should reflect all mined blocks
    let block_num = EthApiServer::block_number(&env.handler).await.unwrap();
    assert_eq!(block_num, format!("0x{:x}", block_count));

    // Mempool should be clean
    assert_eq!(env.tx_pool.len(), 0);
}

// ---------------------------------------------------------------------------
// 5. No panics on empty mempool mine
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mine_empty_block_no_panic() {
    let env = setup();
    let genesis = make_genesis_block();
    store_block(&env, &genesis);

    // Mining an empty block must not panic
    let _hash = mine_block(&env, 1, genesis.hash(), vec![]);

    let block_num = EthApiServer::block_number(&env.handler).await.unwrap();
    assert_eq!(block_num, "0x1");
}
