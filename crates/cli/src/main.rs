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
    // Initialize tracing (respects RUST_LOG env var).
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Run {
            rpc_addr,
            block_time,
            keystore,
            chain_id,
        } => {
            commands::run(cli.datadir, rpc_addr, block_time, keystore, chain_id).await
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
