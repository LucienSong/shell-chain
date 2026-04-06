//! `shell-node account` — account management subcommands.

use std::path::PathBuf;

use clap::Subcommand;
use shell_keystore::EncryptedKey;
use shell_primitives::Address;

#[derive(Subcommand)]
pub enum AccountCommand {
    /// List keystore addresses found in the data directory.
    List {
        /// Data directory to scan for keystore files.
        #[arg(long, default_value = "shell-data")]
        datadir: PathBuf,
    },

    /// Query the balance of an address.
    Balance {
        /// Address to query (0x-prefixed hex).
        address: String,

        /// JSON-RPC endpoint URL.
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,
    },

    /// Query the nonce (transaction count) of an address.
    Nonce {
        /// Address to query (0x-prefixed hex).
        address: String,

        /// JSON-RPC endpoint URL.
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,
    },
}

/// Execute an account subcommand.
pub fn execute(cmd: AccountCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        AccountCommand::List { datadir } => cmd_list(datadir),
        AccountCommand::Balance { address, rpc_url } => cmd_balance(address, rpc_url),
        AccountCommand::Nonce { address, rpc_url } => cmd_nonce(address, rpc_url),
    }
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

fn cmd_list(datadir: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if !datadir.exists() {
        return Err(format!("data directory not found: {}", datadir.display()).into());
    }

    let mut found = 0u32;

    for entry in std::fs::read_dir(&datadir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "json") {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if let Ok(ek) = serde_json::from_str::<EncryptedKey>(&contents) {
                    println!("0x{} ({})", ek.address, path.display());
                    found += 1;
                }
            }
        }
    }

    if found == 0 {
        eprintln!("No keystore files found in {}", datadir.display());
    } else {
        eprintln!("{found} keystore(s) found");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Balance
// ---------------------------------------------------------------------------

fn cmd_balance(address: String, rpc_url: String) -> Result<(), Box<dyn std::error::Error>> {
    let addr = parse_address(&address)?;

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [format!("{addr}"), "latest"],
        "id": 1
    });

    let result = rpc_post(&rpc_url, &body)?;
    if let Some(err) = result.get("error") {
        return Err(format!("RPC error: {err}").into());
    }
    let hex_str = result["result"]
        .as_str()
        .ok_or("unexpected eth_getBalance response")?;
    println!("{hex_str}");

    Ok(())
}

// ---------------------------------------------------------------------------
// Nonce
// ---------------------------------------------------------------------------

fn cmd_nonce(address: String, rpc_url: String) -> Result<(), Box<dyn std::error::Error>> {
    let addr = parse_address(&address)?;

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getTransactionCount",
        "params": [format!("{addr}"), "latest"],
        "id": 1
    });

    let result = rpc_post(&rpc_url, &body)?;
    if let Some(err) = result.get("error") {
        return Err(format!("RPC error: {err}").into());
    }
    let hex_str = result["result"]
        .as_str()
        .ok_or("unexpected eth_getTransactionCount response")?;
    println!("{hex_str}");

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers (shared with tx.rs via duplication — small surface, not worth a
// shared module for two one-liners)
// ---------------------------------------------------------------------------

fn parse_address(s: &str) -> Result<Address, Box<dyn std::error::Error>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 40 {
        return Err(format!(
            "invalid address length: expected 40 hex chars, got {}",
            s.len()
        )
        .into());
    }
    let bytes = hex::decode(s)?;
    Ok(Address::from_slice(&bytes))
}

fn rpc_post(
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let resp = ureq::post(url)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())?;
    let json: serde_json::Value = resp.into_json()?;
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_address() {
        let addr = parse_address("0x0000000000000000000000000000000000000042").unwrap();
        assert_eq!(addr.as_bytes()[19], 0x42);
    }

    #[test]
    fn parse_address_rejects_short() {
        assert!(parse_address("0x1234").is_err());
    }

    #[test]
    fn list_empty_dir() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("__test_empty_acct_dir__");
        let _ = std::fs::create_dir(&dir);
        let result = cmd_list(dir.clone());
        let _ = std::fs::remove_dir(&dir);
        assert!(result.is_ok());
    }
}
