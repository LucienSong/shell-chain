//! `shell-node run` — start the node.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use shell_consensus::PoaConfig;
use shell_crypto::{DilithiumSigner, Signer};
use shell_genesis::{initialize_genesis, AllocEntry, ConsensusConfig, GenesisConfig};
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
    pub checkpoint_url: Option<String>,
    pub rpc_cors: Option<String>,
    pub rpc_rate_limit: Option<u32>,
    pub rpc_api: Option<String>,
    pub metrics_addr: String,
}

/// Maximum genesis file size: 10 MB (F-082).
const MAX_GENESIS_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Start the node: load genesis, initialize state, and run the event loop.
pub async fn run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    // F-096: Canonicalize and validate data directory.
    let datadir = if args.datadir.exists() {
        args.datadir.canonicalize()?
    } else {
        std::fs::create_dir_all(&args.datadir)?;
        args.datadir.canonicalize()?
    };

    let args = RunArgs { datadir, ..args };

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
        other => {
            Err(format!("Unknown storage backend: '{other}'. Use 'memory' or 'rocksdb'.").into())
        }
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
            // F-096: Validate keystore path.
            if !path.exists() {
                return Err(format!("keystore file not found: {}", path.display()).into());
            }
            let path = path.canonicalize().map_err(|e| {
                format!(
                    "failed to canonicalize keystore path '{}': {e}",
                    path.display()
                )
            })?;
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
        // F-082: Validate genesis file before loading.
        let file_size = std::fs::metadata(&genesis_file)?.len();
        if file_size > MAX_GENESIS_FILE_SIZE {
            return Err(format!(
                "genesis file too large: {} bytes (max {} bytes)",
                file_size, MAX_GENESIS_FILE_SIZE
            )
            .into());
        }
        info!("Loading genesis from {}", genesis_file.display());
        GenesisConfig::from_file(&genesis_file)?
    } else {
        info!("No genesis.json found, using dev genesis");
        use shell_primitives::U256;

        let mut alloc = std::collections::HashMap::new();
        alloc.insert(
            authority,
            AllocEntry {
                balance: U256::from(1_000_000_000_000_000_000u128),
                nonce: 0,
                code: None,
                storage: None,
            },
        );

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
            boot_nodes: vec![],
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

    // Checkpoint sync: download and import snapshot if --checkpoint-url is set
    // and the chain has no blocks beyond genesis.
    if let Some(ref url) = args.checkpoint_url {
        if shell_node::checkpoint::should_checkpoint_sync(&chain_store) {
            info!("Chain is empty, starting checkpoint sync");
            let block_num = shell_node::checkpoint::checkpoint_sync(
                url,
                &chain_store,
                &args.datadir,
                args.chain_id,
            )
            .await
            .map_err(|e| -> Box<dyn std::error::Error> {
                format!("checkpoint sync failed: {e}").into()
            })?;
            info!("Checkpoint sync complete at block #{block_num}");
        } else {
            info!("Chain already has blocks, skipping checkpoint sync");
        }
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
            cors_allowed_origins: args
                .rpc_cors
                .as_ref()
                .map(|s| s.split(',').map(|o| o.trim().to_string()).collect()),
            rate_limit_per_sec: args.rpc_rate_limit.or(Some(50)),
            api_namespaces: args
                .rpc_api
                .as_ref()
                .map(|s| s.split(',').map(|n| n.trim().to_string()).collect())
                .unwrap_or_else(|| vec!["eth".into(), "net".into(), "web3".into(), "shell".into()]),
            max_request_body_size: 5 * 1024 * 1024,
            ..RpcConfig::default()
        },
        network: NetworkConfig::default(),
        proposer_address: Some(authority),
        block_time_ms: args.block_time,
        data_dir: args.datadir.to_string_lossy().into(),
        pruning: PruningConfig::new(args.pruning),
        metrics: shell_node::config::MetricsConfig {
            enabled: true,
            listen_addr: args.metrics_addr.parse()?,
        },
    };

    // Build the node (auto-detects existing state via NodeBuilder).
    let (node, _store) = shell_node::builder::NodeBuilder::new(node_config, store).build();

    // Set up the network backend.
    if args.p2p {
        #[cfg(feature = "libp2p")]
        {
            let p2p_listen: std::net::SocketAddr = args.p2p_addr.parse()?;
            // Merge CLI boot nodes with genesis boot nodes (CLI takes priority via ordering).
            let mut boot_nodes = args.bootnodes;
            for addr in &genesis_config.boot_nodes {
                if !boot_nodes.contains(addr) {
                    boot_nodes.push(addr.clone());
                }
            }
            let net_config = NetworkConfig {
                listen_addr: p2p_listen,
                boot_nodes,
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
            eprintln!("   Metrics:     http://{}", args.metrics_addr);
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
        eprintln!("   Metrics:     http://{}", args.metrics_addr);
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
