//! `shell-node init` — initialize genesis and data directory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use shell_crypto::DilithiumSigner;
use shell_crypto::Signer;
use shell_genesis::{AllocEntry, ConsensusConfig, GenesisConfig, initialize_genesis};
use shell_primitives::{Address, U256};
use shell_storage::MemoryDb;

use tracing::info;

/// Initialize a data directory with genesis block.
///
/// If no genesis.json is provided, creates a dev genesis with a single
/// pre-funded authority account.
pub fn init(
    datadir: PathBuf,
    genesis_path: Option<PathBuf>,
    chain_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&datadir)?;

    let genesis_config = match genesis_path {
        Some(path) => {
            info!("Loading genesis from {}", path.display());
            GenesisConfig::from_file(&path)?
        }
        None => {
            info!("No genesis.json provided, generating dev genesis");
            let signer = DilithiumSigner::generate();
            let authority = Address::from_public_key(signer.public_key());

            let mut alloc = HashMap::new();
            alloc.insert(authority, AllocEntry {
                balance: U256::from(1_000_000_000_000_000_000u128), // 1e18
                nonce: 0,
                code: None,
                storage: None,
            });

            GenesisConfig {
                chain_id,
                chain_name: "shell-chain-dev".into(),
                timestamp: 1_700_000_000,
                gas_limit: 30_000_000,
                extra_data: String::new(),
                consensus: ConsensusConfig::PoA {
                    authorities: vec![authority],
                    block_time_secs: 2,
                    epoch_length: 0,
                },
                alloc,
            }
        }
    };

    // Use MemoryDb to compute genesis state (actual storage on `run`).
    let store = Arc::new(MemoryDb::new());
    let genesis_block = initialize_genesis(&genesis_config, store)?;

    let genesis_json = serde_json::to_string_pretty(&genesis_config)?;
    let genesis_file = datadir.join("genesis.json");
    std::fs::write(&genesis_file, &genesis_json)?;

    info!(
        "Genesis block #{} written (state_root: {:?})",
        genesis_block.number(),
        genesis_block.header.state_root
    );

    eprintln!("✓ Genesis initialized at {}", datadir.display());
    eprintln!("  Block hash: {:?}", genesis_block.hash());
    eprintln!("  State root: {:?}", genesis_block.header.state_root);
    eprintln!("  Alloc accounts: {}", genesis_config.alloc.len());

    Ok(())
}
