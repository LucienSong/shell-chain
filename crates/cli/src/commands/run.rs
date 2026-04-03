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
use shell_node::pruning::PruningConfig;
use shell_primitives::Address;
use shell_rpc::RpcConfig;
use shell_storage::{ChainStore, KvStore, MemoryDb};

use tracing::info;

/// Aggregated CLI arguments for the `run` subcommand.
pub struct RunArgs {
    pub datadir: PathBuf,
    pub rpc_addr: String,
    pub block_time: u64,
    pub keystore: Option<PathBuf>,
    pub chain_id: u64,
    pub db: String,
    pub ws: bool,
    pub ws_port: u16,
    pub p2p: bool,
    pub p2p_addr: String,
    pub bootnodes: Vec<String>,
    pub enable_mdns: bool,
    pub pruning: u64,
}

/// Start the node: load genesis, initialize state, and run the event loop.
pub async fn run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.db.as_str() {
        "memory" => {
            info!("Using in-memory storage (non-persistent)");
            let store = Arc::new(MemoryDb::new());
            run_with_store(store, args).await
        }
        "rocksdb" => {
            #[cfg(feature = "rocksdb")]
            {
                use shell_storage::RocksDbStore;
                let db_path = args.datadir.join("db");
                std::fs::create_dir_all(&db_path)?;
                info!("Opening RocksDB at {}", db_path.display());
                let stores = RocksDbStore::open_all(&db_path, None)?;
                // Use the `state` column family as a unified KvStore.
                // ChainStore and WorldState coexist via byte-prefix namespacing.
                let store = Arc::new(stores.state);
                run_with_store(store, args).await
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
    args: RunArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load or generate the signer.
    let signer: Arc<dyn Signer> = match args.keystore {
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
    let genesis_file = args.datadir.join("genesis.json");
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
            chain_id: args.chain_id,
            chain_name: "shell-chain-dev".into(),
            timestamp: 1_700_000_000,
            gas_limit: 30_000_000,
            extra_data: String::new(),
            consensus: ConsensusConfig::PoA {
                authorities: vec![authority],
                block_time_secs: args.block_time / 1000,
                epoch_length: 0,
            },
            alloc,
        };

        // Persist dev genesis for future restarts.
        std::fs::create_dir_all(&args.datadir)?;
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

    // Extract authorities and epoch_length from genesis.
    let (authorities, _block_time_secs, epoch_length) = match &genesis_config.consensus {
        ConsensusConfig::PoA {
            authorities,
            block_time_secs,
            epoch_length,
        } => (authorities.clone(), *block_time_secs, *epoch_length),
    };

    // Build node configuration.
    let listen_addr: SocketAddr = args.rpc_addr.parse()?;
    let ws_addr = if args.ws {
        Some(SocketAddr::from(([127, 0, 0, 1], args.ws_port)))
    } else {
        None
    };
    let node_config = NodeConfig {
        chain_id: genesis_config.chain_id,
        consensus: PoaConfig::new(authorities, args.block_time / 1000)
            .with_epoch_length(epoch_length),
        mempool: MempoolConfig {
            chain_id: genesis_config.chain_id,
            ..MempoolConfig::default()
        },
        rpc: RpcConfig {
            listen_addr,
            ws_addr,
            ..RpcConfig::default()
        },
        network: NetworkConfig::default(),
        proposer_address: Some(authority),
        block_time_ms: args.block_time,
        data_dir: args.datadir.to_string_lossy().into(),
        pruning: PruningConfig::new(args.pruning),
    };

    // Build the node (auto-detects existing state via NodeBuilder).
    let (node, _store) = shell_node::builder::NodeBuilder::new(node_config, store).build();

    // Set up the network backend.
    if args.p2p {
        #[cfg(feature = "libp2p")]
        {
            let p2p_listen: std::net::SocketAddr = args.p2p_addr.parse()?;
            let net_config = NetworkConfig {
                listen_addr: p2p_listen,
                boot_nodes: args.bootnodes,
                enable_mdns: args.enable_mdns,
                ..NetworkConfig::default()
            };
            let mut network = shell_network::Libp2pNetwork::new(&net_config).await?;

            eprintln!("🚀 Shell-chain node starting...");
            eprintln!("   Chain ID:    {}", genesis_config.chain_id);
            eprintln!("   RPC:         http://{listen_addr}");
            if let Some(ws) = ws_addr {
                eprintln!("   WS:          ws://{ws}");
            }
            eprintln!("   P2P:         {p2p_listen} (libp2p)");
            eprintln!("   Authority:   0x{}", hex::encode(authority.as_bytes()));
            eprintln!("   Block time:  {}ms", args.block_time);
            if args.pruning > 0 {
                eprintln!("   Pruning:     keep last {} state roots", args.pruning);
            } else {
                eprintln!("   Pruning:     archive (keep all)");
            }
            if resumed {
                eprintln!("   Mode:        resumed from persistent storage");
            }
            eprintln!();

            let node = Arc::new(node);
            let node_shutdown = node.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                eprintln!("\n⏹  Ctrl-C received, shutting down...");
                node_shutdown.shutdown();
            });

            node.run(signer, &mut network).await?;
        }
        #[cfg(not(feature = "libp2p"))]
        {
            return Err("libp2p support not compiled. Rebuild with: cargo build -p shell-cli --features libp2p".into());
        }
    } else {
        // In-process channel network (single-node mode).
        let bus = NetworkBus::new(64);
        let mut network = bus.join(&NetworkConfig::default());

        eprintln!("🚀 Shell-chain node starting...");
        eprintln!("   Chain ID:    {}", genesis_config.chain_id);
        eprintln!("   RPC:         http://{listen_addr}");
        if let Some(ws) = ws_addr {
            eprintln!("   WS:          ws://{ws}");
        }
        eprintln!("   Authority:   0x{}", hex::encode(authority.as_bytes()));
        eprintln!("   Block time:  {}ms", args.block_time);
        if args.pruning > 0 {
            eprintln!("   Pruning:     keep last {} state roots", args.pruning);
        } else {
            eprintln!("   Pruning:     archive (keep all)");
        }
        if resumed {
            eprintln!("   Mode:        resumed from persistent storage");
        }
        eprintln!();

        let node = Arc::new(node);
        let node_shutdown = node.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            eprintln!("\n⏹  Ctrl-C received, shutting down...");
            node_shutdown.shutdown();
        });

        node.run(signer, &mut network).await?;
    }

    eprintln!("✓ Node stopped gracefully");
    Ok(())
}
