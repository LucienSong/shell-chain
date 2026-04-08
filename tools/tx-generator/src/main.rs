//! Shell-Chain testnet transaction generator.
//!
//! Generates random post-quantum-signed transactions and submits them via
//! JSON-RPC to stress-test the RPC layer, mempool, and signature verification
//! pipeline.

use std::time::{Duration, Instant};

use clap::Parser;
use rand::Rng;
use serde::{Deserialize, Serialize};
use shell_core::{SignedTransaction, Transaction};
use shell_crypto::{DilithiumSigner, Signer};
use shell_primitives::{Address, Bytes, U256};

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "shell-tx-generator", about = "Testnet stress-testing transaction generator")]
struct Cli {
    /// JSON-RPC endpoint URL.
    #[arg(long, default_value = "http://localhost:8545")]
    rpc_url: String,

    /// Number of test accounts to generate.
    #[arg(long, default_value_t = 5)]
    num_accounts: usize,

    /// How long to run, in seconds.
    #[arg(long, default_value_t = 60)]
    duration: u64,

    /// Minimum delay between transactions (ms).
    #[arg(long, default_value_t = 500)]
    min_interval: u64,

    /// Maximum delay between transactions (ms).
    #[arg(long, default_value_t = 3000)]
    max_interval: u64,

    /// Chain ID.
    #[arg(long, default_value_t = 1337)]
    chain_id: u64,
}

// ── Account ──────────────────────────────────────────────────────────

struct TestAccount {
    signer: DilithiumSigner,
    address: Address,
    pubkey: Vec<u8>,
    nonce: u64,
    pubkey_registered: bool,
    tx_sent: u64,
    tx_ok: u64,
    tx_fail: u64,
}

impl TestAccount {
    fn generate() -> Self {
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let address = Address::from_public_key(&pubkey);
        Self {
            signer,
            address,
            pubkey,
            nonce: 0,
            pubkey_registered: false,
            tx_sent: 0,
            tx_ok: 0,
            tx_fail: 0,
        }
    }
}

// ── Transaction types ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxKind {
    SimpleTransfer,
    ContractCreation,
    ContractCall,
    ZeroValue,
    HighGas,
}

impl TxKind {
    const ALL: [TxKind; 5] = [
        TxKind::SimpleTransfer,
        TxKind::ContractCreation,
        TxKind::ContractCall,
        TxKind::ZeroValue,
        TxKind::HighGas,
    ];

    fn pick(rng: &mut impl Rng) -> Self {
        Self::ALL[rng.gen_range(0..Self::ALL.len())]
    }

    fn label(self) -> &'static str {
        match self {
            TxKind::SimpleTransfer => "transfer",
            TxKind::ContractCreation => "create",
            TxKind::ContractCall => "call",
            TxKind::ZeroValue => "zero-val",
            TxKind::HighGas => "high-gas",
        }
    }
}

// ── Statistics ───────────────────────────────────────────────────────

#[derive(Default)]
struct Stats {
    total: u64,
    ok: u64,
    fail: u64,
    latency_sum_ms: u64,
    by_kind: [u64; 5],
}

impl Stats {
    fn record(&mut self, kind: TxKind, success: bool, latency: Duration) {
        self.total += 1;
        if success {
            self.ok += 1;
        } else {
            self.fail += 1;
        }
        self.latency_sum_ms += latency.as_millis() as u64;
        self.by_kind[kind as usize] += 1;
    }
}

// ── JSON-RPC helpers ─────────────────────────────────────────────────

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    method: &'a str,
    params: serde_json::Value,
    id: u64,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<serde_json::Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize, Debug)]
struct RpcError {
    code: i64,
    message: String,
}

async fn rpc_send_tx(
    client: &reqwest::Client,
    url: &str,
    signed_tx: &SignedTransaction,
    req_id: u64,
) -> Result<String, String> {
    let tx_json = serde_json::to_value(signed_tx).map_err(|e| format!("serialize: {e}"))?;
    let req = RpcRequest {
        jsonrpc: "2.0",
        method: "shell_sendTransaction",
        params: serde_json::json!([tx_json]),
        id: req_id,
    };
    let resp = client
        .post(url)
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?;

    let body: RpcResponse = resp.json().await.map_err(|e| format!("decode: {e}"))?;
    if let Some(err) = body.error {
        Err(format!("[{}] {}", err.code, err.message))
    } else if let Some(result) = body.result {
        Ok(result
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| result.to_string()))
    } else {
        Err("empty response".into())
    }
}

async fn rpc_set_balance(
    client: &reqwest::Client,
    url: &str,
    address: &Address,
    balance_hex: &str,
    req_id: u64,
) -> Result<bool, String> {
    let req = RpcRequest {
        jsonrpc: "2.0",
        method: "shell_setBalance",
        params: serde_json::json!([address, balance_hex]),
        id: req_id,
    };
    let resp = client
        .post(url)
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?;

    let body: RpcResponse = resp.json().await.map_err(|e| format!("decode: {e}"))?;
    if let Some(err) = body.error {
        Err(format!("[{}] {}", err.code, err.message))
    } else if body
        .result
        .as_ref()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        Ok(true)
    } else {
        Err(format!(
            "unexpected result: {:?}",
            body.result.unwrap_or(serde_json::Value::Null)
        ))
    }
}

// ── Transaction builder ──────────────────────────────────────────────

fn build_tx(
    kind: TxKind,
    chain_id: u64,
    nonce: u64,
    recipient: Address,
    rng: &mut impl Rng,
) -> Transaction {
    match kind {
        TxKind::SimpleTransfer => Transaction {
            chain_id,
            nonce,
            to: Some(recipient),
            value: U256::from(rng.gen_range(1_000u64..1_000_000)),
            data: Bytes::new(),
            gas_limit: 21_000,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 100_000_000,
            access_list: None,
            tx_type: 2,
            max_fee_per_blob_gas: None,
            blob_versioned_hashes: None,
        },
        TxKind::ContractCreation => {
            // Minimal bytecode: PUSH1 1, PUSH1 0, MSTORE, PUSH1 32, PUSH1 0, RETURN
            let bytecode = hex::decode("600160005260206000f3").unwrap();
            Transaction {
                chain_id,
                nonce,
                to: None,
                value: U256::ZERO,
                data: Bytes::copy_from_slice(&bytecode),
                gas_limit: 100_000,
                max_fee_per_gas: 1_000_000_000,
                max_priority_fee_per_gas: 100_000_000,
                access_list: None,
                tx_type: 2,
                max_fee_per_blob_gas: None,
                blob_versioned_hashes: None,
            }
        }
        TxKind::ContractCall => {
            // Random 4-byte function selector + 32 bytes of random data
            let mut data = vec![0u8; 36];
            rng.fill(&mut data[..]);
            Transaction {
                chain_id,
                nonce,
                to: Some(recipient),
                value: U256::ZERO,
                data: Bytes::copy_from_slice(&data),
                gas_limit: 50_000,
                max_fee_per_gas: 1_000_000_000,
                max_priority_fee_per_gas: 100_000_000,
                access_list: None,
                tx_type: 2,
                max_fee_per_blob_gas: None,
                blob_versioned_hashes: None,
            }
        }
        TxKind::ZeroValue => Transaction {
            chain_id,
            nonce,
            to: Some(recipient),
            value: U256::ZERO,
            data: Bytes::copy_from_slice(&[0xde, 0xad]),
            gas_limit: 21_000,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 100_000_000,
            access_list: None,
            tx_type: 2,
            max_fee_per_blob_gas: None,
            blob_versioned_hashes: None,
        },
        TxKind::HighGas => Transaction {
            chain_id,
            nonce,
            to: Some(recipient),
            value: U256::from(rng.gen_range(1u64..1_000)),
            data: Bytes::new(),
            gas_limit: 10_000_000,
            max_fee_per_gas: 5_000_000_000,
            max_priority_fee_per_gas: 500_000_000,
            access_list: None,
            tx_type: 2,
            max_fee_per_blob_gas: None,
            blob_versioned_hashes: None,
        },
    }
}

fn sign_tx(
    signer: &DilithiumSigner,
    from: Address,
    tx: Transaction,
    pubkey: Option<Vec<u8>>,
) -> SignedTransaction {
    let sig = signer.sign(tx.hash().0.as_slice()).expect("signing failed");
    match pubkey {
        Some(pk) => SignedTransaction::with_pubkey(from, tx, sig, pk),
        None => SignedTransaction::new(from, tx, sig),
    }
}

// ── ANSI colours ─────────────────────────────────────────────────────

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const _YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

// ── main ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    println!(
        "\n{BOLD}{CYAN}═══ Shell-Chain Transaction Generator ═══{RESET}\n"
    );
    println!("  RPC endpoint : {}", cli.rpc_url);
    println!("  Accounts     : {}", cli.num_accounts);
    println!("  Duration     : {}s", cli.duration);
    println!("  Interval     : {}–{}ms", cli.min_interval, cli.max_interval);
    println!("  Chain ID     : {}", cli.chain_id);
    println!();

    // ── 1. Generate accounts ────────────────────────────────────────
    println!("{BOLD}▸ Generating {} Dilithium3 keypairs …{RESET}", cli.num_accounts);
    let mut accounts: Vec<TestAccount> = (0..cli.num_accounts).map(|_| TestAccount::generate()).collect();
    for (i, acct) in accounts.iter().enumerate() {
        println!(
            "  {CYAN}[{}]{RESET} {}  (pubkey {}…)",
            i,
            acct.address,
            hex::encode(&acct.pubkey[..8])
        );
    }
    println!();

    if accounts.len() < 2 {
        eprintln!("{RED}Need at least 2 accounts for transfers.{RESET}");
        std::process::exit(1);
    }

    // ── 1b. Fund accounts via shell_setBalance ──────────────────────
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client");

    println!("{BOLD}▸ Funding accounts via shell_setBalance …{RESET}");
    let fund_amount = "0x3635c9adc5dea00000"; // 1000 ETH each
    let mut fund_ok = 0usize;
    for (i, acct) in accounts.iter().enumerate() {
        match rpc_set_balance(&client, &cli.rpc_url, &acct.address, fund_amount, i as u64 + 1000).await {
            Ok(_) => {
                println!("  {GREEN}✓{RESET} [{i}] {} funded with 1000 ETH", acct.address);
                fund_ok += 1;
            }
            Err(e) => {
                println!("  {RED}✗{RESET} [{i}] {} fund failed: {e}", acct.address);
            }
        }
    }
    if fund_ok == 0 {
        eprintln!("\n{RED}WARNING: No accounts funded — txs will likely fail with insufficient balance.{RESET}");
        eprintln!("  Make sure shell_setBalance is available (requires updated node).\n");
    }
    println!();

    // ── 2. Run loop ─────────────────────────────────────────────────
    let mut stats = Stats::default();
    let mut rng = rand::thread_rng();
    let mut req_id: u64 = 1;
    let deadline = Instant::now() + Duration::from_secs(cli.duration);

    println!(
        "{BOLD}▸ Sending transactions for {}s …{RESET}\n",
        cli.duration
    );

    while Instant::now() < deadline {
        // Pick random sender / recipient (distinct)
        let sender_idx = rng.gen_range(0..accounts.len());
        let mut recip_idx = rng.gen_range(0..accounts.len());
        while recip_idx == sender_idx {
            recip_idx = rng.gen_range(0..accounts.len());
        }
        let recipient = accounts[recip_idx].address;

        let kind = TxKind::pick(&mut rng);
        let nonce = accounts[sender_idx].nonce;

        let tx = build_tx(kind, cli.chain_id, nonce, recipient, &mut rng);

        // Always include pubkey — node only registers it when tx is included in a block,
        // so subsequent txs before block inclusion would fail without it.
        let pubkey = Some(accounts[sender_idx].pubkey.clone());

        let signed = sign_tx(
            &accounts[sender_idx].signer,
            accounts[sender_idx].address,
            tx,
            pubkey,
        );

        let t0 = Instant::now();
        let result = rpc_send_tx(&client, &cli.rpc_url, &signed, req_id).await;
        let latency = t0.elapsed();

        accounts[sender_idx].tx_sent += 1;
        req_id += 1;

        match &result {
            Ok(hash) => {
                accounts[sender_idx].tx_ok += 1;
                accounts[sender_idx].nonce += 1;
                accounts[sender_idx].pubkey_registered = true;
                stats.record(kind, true, latency);
                println!(
                    "  {GREEN}✓{RESET} #{:<4} {:<8} sender={} hash={} ({:.0}ms)",
                    stats.total,
                    kind.label(),
                    &accounts[sender_idx].address.to_string()[..10],
                    hash,
                    latency.as_secs_f64() * 1000.0,
                );
            }
            Err(e) => {
                accounts[sender_idx].tx_fail += 1;
                stats.record(kind, false, latency);
                println!(
                    "  {RED}✗{RESET} #{:<4} {:<8} sender={} err={} ({:.0}ms)",
                    stats.total,
                    kind.label(),
                    &accounts[sender_idx].address.to_string()[..10],
                    e,
                    latency.as_secs_f64() * 1000.0,
                );
            }
        }

        // Random sleep between txs
        let delay_ms = rng.gen_range(cli.min_interval..=cli.max_interval);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    // ── 3. Report ───────────────────────────────────────────────────
    println!(
        "\n{BOLD}{CYAN}═══ Summary ═══{RESET}\n"
    );
    println!("  Total sent   : {}", stats.total);
    println!(
        "  Succeeded    : {GREEN}{}{RESET}",
        stats.ok
    );
    println!(
        "  Failed       : {RED}{}{RESET}",
        stats.fail
    );
    if stats.total > 0 {
        println!(
            "  Success rate : {:.1}%",
            stats.ok as f64 / stats.total as f64 * 100.0
        );
        println!(
            "  Avg latency  : {:.1}ms",
            stats.latency_sum_ms as f64 / stats.total as f64
        );
    }

    println!("\n  {BOLD}By type:{RESET}");
    for kind in TxKind::ALL {
        let count = stats.by_kind[kind as usize];
        if count > 0 {
            println!("    {:<12} : {}", kind.label(), count);
        }
    }

    println!("\n  {BOLD}Per account:{RESET}");
    for (i, acct) in accounts.iter().enumerate() {
        if acct.tx_sent > 0 {
            println!(
                "    {CYAN}[{}]{RESET} {} — sent:{} ok:{GREEN}{}{RESET} fail:{RED}{}{RESET}",
                i,
                &acct.address.to_string()[..10],
                acct.tx_sent,
                acct.tx_ok,
                acct.tx_fail,
            );
        }
    }
    println!();
}
