//! `shell-node run` — start the node.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use shell_consensus::PoaConfig;
use shell_crypto::{DilithiumSigner, Signer};
use shell_genesis::{AllocEntry, ConsensusConfig, GenesisConfig, initialize_genesis};
use shell_keystore::{decrypt, EncryptedKey};
use shell_mempool::MempoolConfig;
use shell_network::{NetworkBus, NetworkConfig};
use shell_node::config::NodeConfig;
use shell_primitives::Address;
use shell_rpc::RpcConfig;
use shell_storage::{ChainStore, KvStore, MemoryDb};

use tracing::info;

/// Start the node: load genesis, initialize state, and run the event loop.
pub async fn run(
    datadir: PathBuf,
    rpc_addr: String,
    block_time: u64,
    keystore_path: Option<PathBuf>,
    chain_id: u64,
    db_backend: String,
) -> Result<(), Box<dyn std::error::Error>> {
    match db_backend.as_str() {
        "memory" => {
            info!("Using in-memory storage (non-persistent)");
            let store = Arc::new(MemoryDb::new());
            run_with_store(store, datadir, rpc_addr, block_time, keystore_path, chain_id).await
        }
        "rocksdb" => {
            #[cfg(feature = "rocksdb")]
            {
                use shell_storage::RocksDbStore;
                let db_path = datadir.join("db");
                std::fs::create_dir_all(&db_path)?;
                info!("Opening RocksDB at {}", db_path.display());
                let stores = RocksDbStore::open_all(&db_path, None)?;
                // Use the `state` column family as a unified KvStore.
                // ChainStore and WorldState coexist via byte-prefix namespacing.
                let store = Arc::new(stores.state);
                run_with_store(store, datadir, rpc_addr, block_time, keystore_path, chain_id).await
            }
            #[cfg(not(feature = "rocksdb"))]
            {
                Err("RocksDB support not compiled. Rebuild with: cargo build -p shell-cli --features rocksdb".into())
            }
        }
        other => Err(format!("Unknown storage backend: '{other}'. Use 'memory' or 'rocksdb'.").into()),
    }
}

/// Core node startup logic, generic over storage backend.
async fn run_with_store<S: KvStore + 'static>(
    store: Arc<S>,
    datadir: PathBuf,
    rpc_addr: String,
    block_time: u64,
    keystore_path: Option<PathBuf>,
    chain_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load or generate the signer.
    let signer: Arc<dyn Signer> = match keystore_path {
        Some(path) => {
            info!("Loading keystore from {}", path.display());
            let json = std::fs::read_to_string(&path)?;
            let encrypted: EncryptedKey = serde_json::from_str(&json)?;

            eprint!("Enter keystore password: ");
            let password = rpassword::read_password()?;

            let signer = decrypt(&encrypted, password.as_bytes())?;
            info!("Keystore unlocked: 0x{}", encrypted.address);
            Arc::new(signer)
        }
        None => {
            info!("No keystore provided, generating ephemeral key (dev mode)");
            Arc::new(DilithiumSigner::generate())
        }
    };

    let authority = Address::from_public_key(signer.public_key());
    info!("Node authority: 0x{}", hex::encode(authority.as_bytes()));

    // Check if chain is already initialized (persistent storage resume).
    let chain_store = ChainStore::new(store.clone());
    let resumed = if let Ok(Some(head)) = chain_store.get_head_block() {
        info!(
            "Resuming from block #{} (state_root: {:?})",
            head.number(),
            head.header.state_root
        );
        true
    } else {
        false
    };

    // Load genesis config.
    let genesis_file = datadir.join("genesis.json");
    let genesis_config = if genesis_file.exists() {
        info!("Loading genesis from {}", genesis_file.display());
        GenesisConfig::from_file(&genesis_file)?
    } else {
        info!("No genesis.json found, using dev genesis");
        use shell_primitives::U256;

        let mut alloc = std::collections::HashMap::new();
        alloc.insert(authority, AllocEntry {
            balance: U256::from(1_000_000_000_000_000_000u128),
            nonce: 0,
            code: None,
            storage: None,
        });

        let config = GenesisConfig {
            chain_id,
            chain_name: "shell-chain-dev".into(),
            timestamp: 1_700_000_000,
            gas_limit: 30_000_000,
            extra_data: String::new(),
            consensus: ConsensusConfig::PoA {
                authorities: vec![authority],
                block_time_secs: block_time / 1000,
            },
            alloc,
        };

        // Persist dev genesis for future restarts.
        std::fs::create_dir_all(&datadir)?;
        let json = serde_json::to_string_pretty(&config)?;
        std::fs::write(&genesis_file, &json)?;
        info!("Dev genesis written to {}", genesis_file.display());

        config
    };

    // Initialize genesis only if chain has no head block.
    if !resumed {
        let genesis_block = initialize_genesis(&genesis_config, store.clone())?;
        info!(
            "Genesis block #{} (state_root: {:?})",
            genesis_block.number(),
            genesis_block.header.state_root
        );
    }

    // Extract authorities from genesis.
    let (authorities, _block_time_secs) = match &genesis_config.consensus {
        ConsensusConfig::PoA {
            authorities,
            block_time_secs,
        } => (authorities.clone(), *block_time_secs),
    };

    // Build node configuration.
    let listen_addr: SocketAddr = rpc_addr.parse()?;
    let node_config = NodeConfig {
        chain_id: genesis_config.chain_id,
        consensus: PoaConfig::new(authorities, block_time / 1000),
        mempool: MempoolConfig {
            chain_id: genesis_config.chain_id,
            ..MempoolConfig::default()
        },
        rpc: RpcConfig {
            listen_addr,
            ..RpcConfig::default()
        },
        network: NetworkConfig::default(),
        proposer_address: Some(authority),
        block_time_ms: block_time,
        data_dir: datadir.to_string_lossy().into(),
    };

    // Build the node (auto-detects existing state via NodeBuilder).
    let (node, _store) = shell_node::builder::NodeBuilder::new(node_config, store).build();

    // Set up in-process network (single-node, no libp2p yet).
    let bus = NetworkBus::new(64);
    let mut network = bus.join(&NetworkConfig::default());

    eprintln!("🚀 Shell-chain node starting...");
    eprintln!("   Chain ID:    {}", genesis_config.chain_id);
    eprintln!("   RPC:         http://{listen_addr}");
    eprintln!("   Authority:   0x{}", hex::encode(authority.as_bytes()));
    eprintln!("   Block time:  {block_time}ms");
    if resumed {
        eprintln!("   Mode:        resumed from persistent storage");
    }
    eprintln!();

    // Install Ctrl-C handler.
    let node = Arc::new(node);
    let node_shutdown = node.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("\n⏹  Ctrl-C received, shutting down...");
        node_shutdown.shutdown();
    });

    // Run the event loop (blocks until shutdown).
    node.run(signer, &mut network).await?;

    eprintln!("✓ Node stopped gracefully");
    Ok(())
}
