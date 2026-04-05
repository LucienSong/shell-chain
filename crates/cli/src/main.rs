//! Shell-chain node CLI.
//!
//! Binary entry point for the post-quantum blockchain node.
//! Subcommands:
//! - `run`           — start the node (block production + RPC + network)
//! - `init`          — initialize genesis and data directory
//! - `key generate`  — create a new encrypted keystore file
//! - `tx send|deploy|call` — transaction operations
//! - `account list|balance|nonce` — account management
//! - `export-state`  — export chain state to a snapshot file
//! - `import-state`  — import chain state from a snapshot file
//! - `removedb`      — remove the chain database
//! - `version`       — print version information

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;
mod config;

use config::ShellConfig;

#[derive(Parser)]
#[command(
    name = "shell-node",
    about = "Shell-chain post-quantum blockchain node",
    version = env!("CARGO_PKG_VERSION"),
)]
struct Cli {
    /// Data directory for chain storage and keystore.
    #[arg(long, default_value = "shell-data", global = true)]
    datadir: PathBuf,

    /// Log output format: "text" (human-readable) or "json" (structured).
    #[arg(long, default_value = "text", global = true)]
    log_format: String,

    /// Log level filter (RUST_LOG style, e.g. "debug", "shell_node=trace").
    #[arg(long, global = true)]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the node.
    Run {
        /// Path to TOML configuration file.
        #[arg(long)]
        config: Option<PathBuf>,

        /// JSON-RPC listen address.
        #[arg(long, default_value = "127.0.0.1:8545")]
        rpc_addr: String,

        /// Block production interval in milliseconds.
        #[arg(long, default_value = "2000")]
        block_time: u64,

        /// Path to the encrypted keystore file.
        #[arg(long)]
        keystore: Option<PathBuf>,

        /// Chain ID.
        #[arg(long, default_value = "1337")]
        chain_id: u64,

        /// Storage backend: "memory" or "rocksdb".
        #[arg(long, default_value = "memory")]
        db: String,

        /// Enable dedicated WebSocket RPC server on a separate port.
        #[arg(long)]
        ws: bool,

        /// WebSocket RPC listen port (used with --ws).
        #[arg(long, default_value = "8546")]
        ws_port: u16,

        /// Enable libp2p P2P networking (requires --features libp2p).
        #[arg(long)]
        p2p: bool,

        /// P2P listen address (ip:port for libp2p TCP).
        #[arg(long, default_value = "0.0.0.0:30303")]
        p2p_addr: String,

        /// Bootstrap peer multiaddrs (repeatable).
        #[arg(long)]
        bootnode: Vec<String>,

        /// Comma-separated bootstrap peer multiaddrs.
        #[arg(long, value_delimiter = ',')]
        bootnodes: Vec<String>,

        /// Enable mDNS local peer discovery (disable in production/cloud).
        #[arg(long)]
        enable_mdns: bool,

        /// Number of recent state roots to retain (0 = archive mode, keeps all).
        #[arg(long, default_value = "0")]
        pruning: u64,

        /// Checkpoint sync: download snapshot from URL on first start.
        #[arg(long)]
        checkpoint_url: Option<String>,

        /// CORS allowed origins (comma-separated, '*' for all).
        #[arg(long)]
        rpc_cors: Option<String>,

        /// RPC rate limit per second per connection.
        #[arg(long)]
        rpc_rate_limit: Option<u32>,

        /// API namespaces to enable (comma-separated: eth,net,web3,shell,debug,trace).
        #[arg(long)]
        rpc_api: Option<String>,

        /// Metrics HTTP server listen address (ip:port).
        #[arg(long, default_value = "127.0.0.1:9090")]
        metrics_addr: String,
    },

    /// Initialize genesis block and data directory.
    Init {
        /// Path to genesis.json configuration file.
        #[arg(long)]
        genesis: Option<PathBuf>,

        /// Chain ID.
        #[arg(long, default_value = "1337")]
        chain_id: u64,
    },

    /// Key management subcommands.
    Key {
        #[command(subcommand)]
        action: KeyCommands,
    },

    /// Export chain state to a snapshot file.
    ExportState {
        /// Block number to export state at (default: latest).
        #[arg(long)]
        block: Option<u64>,

        /// Output file path.
        #[arg(long, default_value = "snapshot.jsonl")]
        output: PathBuf,
    },

    /// Import chain state from a snapshot file.
    ImportState {
        /// Path to the snapshot file.
        #[arg(long)]
        snapshot: PathBuf,
    },

    /// Remove the chain database directory.
    Removedb {
        /// Remove without confirmation prompt.
        #[arg(long)]
        force: bool,
    },

    /// Print version information.
    Version,

    /// Send, deploy, or call transactions.
    Tx {
        #[command(subcommand)]
        command: commands::tx::TxCommand,
    },

    /// Account management (list keystores, query balance/nonce).
    Account {
        #[command(subcommand)]
        command: commands::account::AccountCommand,
    },
}

#[derive(Subcommand)]
enum KeyCommands {
    /// Generate a new Dilithium3 keypair and save as encrypted keystore.
    Generate {
        /// Output path for the keystore file.
        #[arg(long, default_value = "keystore.json")]
        output: PathBuf,
    },

    /// Display the address of a keystore file.
    Inspect {
        /// Path to the keystore file.
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Build env filter: --log-level flag > RUST_LOG env var > "info" default.
    let filter = match &cli.log_level {
        Some(level) => EnvFilter::new(level),
        None => EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info")),
    };

    // Initialize tracing subscriber with the chosen format.
    match cli.log_format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_target(true)
                .with_file(true)
                .with_line_number(true)
                .with_env_filter(filter)
                .init();
        }
        _ => {
            tracing_subscriber::fmt()
                .with_target(true)
                .with_env_filter(filter)
                .init();
        }
    }

    let result = match cli.command {
        Commands::Run {
            config: config_path,
            rpc_addr,
            block_time,
            keystore,
            chain_id,
            db,
            ws,
            ws_port,
            p2p,
            p2p_addr,
            bootnode,
            bootnodes,
            enable_mdns,
            pruning,
            checkpoint_url,
            rpc_cors,
            rpc_rate_limit,
            rpc_api,
            metrics_addr,
        } => {
            // Load config file if specified (CLI args override file values).
            let file_config = match &config_path {
                Some(path) => match config::load_config(path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                },
                None => ShellConfig::default(),
            };

            // Merge: CLI explicit values take priority over config file.
            let datadir = if cli.datadir != PathBuf::from("shell-data") {
                cli.datadir
            } else {
                file_config
                    .node
                    .datadir
                    .map(PathBuf::from)
                    .unwrap_or(cli.datadir)
            };

            let effective_rpc_addr = if rpc_addr != "127.0.0.1:8545" {
                rpc_addr
            } else {
                file_config
                    .rpc
                    .listen_addr
                    .unwrap_or(rpc_addr)
            };

            let effective_block_time = if block_time != 2000 {
                block_time
            } else {
                file_config.node.block_time.unwrap_or(block_time)
            };

            let effective_keystore = keystore.or_else(|| {
                file_config.node.keystore.map(PathBuf::from)
            });

            let effective_chain_id = if chain_id != 1337 {
                chain_id
            } else {
                file_config.node.chain_id.unwrap_or(chain_id)
            };

            let effective_db = if db != "memory" {
                db
            } else {
                file_config.node.db.unwrap_or(db)
            };

            let effective_ws = ws || file_config.rpc.ws_enabled.unwrap_or(false);

            let effective_ws_port = if ws_port != 8546 {
                ws_port
            } else {
                file_config.rpc.ws_port.unwrap_or(ws_port)
            };

            let effective_p2p = p2p || file_config.p2p.enabled.unwrap_or(false);

            let effective_p2p_addr = if p2p_addr != "0.0.0.0:30303" {
                p2p_addr
            } else {
                file_config.p2p.listen_addr.unwrap_or(p2p_addr)
            };

            let effective_enable_mdns =
                enable_mdns || file_config.p2p.enable_mdns.unwrap_or(false);

            let effective_pruning = if pruning != 0 {
                pruning
            } else {
                file_config.node.pruning.unwrap_or(pruning)
            };

            let effective_rpc_cors = rpc_cors.or_else(|| {
                file_config
                    .rpc
                    .cors_origins
                    .map(|v| v.join(","))
            });

            let effective_rpc_rate_limit =
                rpc_rate_limit.or(file_config.rpc.rate_limit);

            let effective_rpc_api = rpc_api.or_else(|| {
                file_config
                    .rpc
                    .api_modules
                    .map(|v| v.join(","))
            });

            // Merge --bootnode (repeatable) and --bootnodes (comma-separated).
            let mut all_bootnodes = bootnode;
            all_bootnodes.extend(bootnodes);
            if all_bootnodes.is_empty() {
                if let Some(cfg_bootnodes) = file_config.p2p.bootnodes {
                    all_bootnodes = cfg_bootnodes;
                }
            }

            let effective_metrics_addr = if metrics_addr != "127.0.0.1:9090" {
                metrics_addr
            } else {
                file_config
                    .metrics
                    .listen_addr
                    .unwrap_or(metrics_addr)
            };

            commands::run(commands::run::RunArgs {
                datadir,
                rpc_addr: effective_rpc_addr,
                block_time: effective_block_time,
                keystore: effective_keystore,
                chain_id: effective_chain_id,
                db: effective_db,
                ws: effective_ws,
                ws_port: effective_ws_port,
                p2p: effective_p2p,
                p2p_addr: effective_p2p_addr,
                bootnodes: all_bootnodes,
                enable_mdns: effective_enable_mdns,
                pruning: effective_pruning,
                checkpoint_url,
                rpc_cors: effective_rpc_cors,
                rpc_rate_limit: effective_rpc_rate_limit,
                rpc_api: effective_rpc_api,
                metrics_addr: effective_metrics_addr,
            })
            .await
        }
        Commands::Init { genesis, chain_id } => {
            commands::init(cli.datadir, genesis, chain_id)
        }
        Commands::Key { action } => match action {
            KeyCommands::Generate { output } => commands::key_generate(output),
            KeyCommands::Inspect { path } => commands::key_inspect(path),
        },
        Commands::ExportState { block, output } => {
            commands::export_state(cli.datadir, output, block)
        }
        Commands::ImportState { snapshot } => {
            commands::import_state(cli.datadir, snapshot)
        }
        Commands::Removedb { force } => {
            commands::removedb(cli.datadir, force)
        }
        Commands::Version => commands::version(),
        Commands::Tx { command } => commands::tx::execute(command),
        Commands::Account { command } => commands::account::execute(command),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
