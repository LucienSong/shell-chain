//! Shell-chain node CLI.
//!
//! Binary entry point for the post-quantum blockchain node.
//! Subcommands:
//! - `run`  — start the node (block production + RPC + network)
//! - `init` — initialize genesis and data directory
//! - `key generate` — create a new encrypted keystore file

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;

#[derive(Parser)]
#[command(name = "shell-node", about = "Shell-chain post-quantum blockchain node")]
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
        } => {
            // Merge --bootnode (repeatable) and --bootnodes (comma-separated).
            let mut all_bootnodes = bootnode;
            all_bootnodes.extend(bootnodes);

            commands::run(commands::run::RunArgs {
                datadir: cli.datadir,
                rpc_addr,
                block_time,
                keystore,
                chain_id,
                db,
                ws,
                ws_port,
                p2p,
                p2p_addr,
                bootnodes: all_bootnodes,
                enable_mdns,
                pruning,
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
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
