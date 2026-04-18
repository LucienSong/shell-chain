//! Shell-chain STARK block-compression benchmark.
//!
//! Measures the full prove→verify pipeline across multiple batch sizes
//! (simulating blocks with different transaction counts) for a configurable
//! duration (default 6 h).
//!
//! # Output
//!
//! * Rolling console summary every `--report-interval` seconds.
//! * One CSV row per proof attempt written to `--out` (default
//!   `stark-bench-<timestamp>.csv`).
//! * Final summary table at end of run.
//!
//! # Metrics tracked per batch size
//!
//! | Metric | Description |
//! |--------|-------------|
//! | `prove_ms` | Wall-clock prove latency (ms) |
//! | `verify_us` | Wall-clock verify latency (µs) |
//! | `proof_bytes` | Serialized proof size (bytes) |
//! | `raw_bytes` | Uncompressed input size: entries × 64 bytes |
//! | `compression_ratio` | `raw_bytes / proof_bytes` |
//! | `ok` | 1 = success, 0 = failure |
//!
//! # Usage
//!
//! ```
//! # 6 h soak, write CSV to /tmp/out.csv
//! cargo run -p shell-stark-bench --release -- --duration 21600 --out /tmp/out.csv
//!
//! # Quick 5-minute smoke run
//! cargo run -p shell-stark-bench --release -- --duration 300
//! ```

use std::{
    fs::File,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::Utc;
use clap::Parser;
use csv::WriterBuilder;
use hdrhistogram::Histogram;
use rand::{Rng, SeedableRng};
use sysinfo::{ProcessExt, System, SystemExt};
use tracing::{info, warn};

use shell_stark_prover::{
    prove_sig_batch, verify_sig_batch, HealthStatus, ProverHealth, ProverHealthConfig,
    SigBatchEntry,
};

// ─── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "shell-stark-bench",
    about = "STARK block-compression soak benchmark"
)]
struct Cli {
    /// Total benchmark duration in seconds (default 21600 = 6 h)
    #[arg(long, default_value_t = 21_600)]
    duration: u64,

    /// Batch sizes to cycle through (comma-separated)
    #[arg(long, default_value = "1,4,8,16,32,64,128,256")]
    batch_sizes: String,

    /// Console summary interval in seconds
    #[arg(long, default_value_t = 60)]
    report_interval: u64,

    /// CSV output file path (default: stark-bench-<timestamp>.csv)
    #[arg(long)]
    out: Option<PathBuf>,

    /// Maximum failure rate before health-check abort (0.0–1.0)
    #[arg(long, default_value_t = 0.5)]
    max_failure_rate: f64,

    /// Seed for deterministic SigBatchEntry generation (0 = random)
    #[arg(long, default_value_t = 0)]
    seed: u64,
}

// ─── CSV record ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct Record {
    timestamp_utc: String,
    elapsed_secs: u64,
    batch_size: usize,
    prove_ms: f64,
    verify_us: f64,
    proof_bytes: usize,
    raw_bytes: usize,
    compression_ratio: f64,
    ok: u8,
    error_msg: String,
}

// ─── Per-batch statistics ─────────────────────────────────────────────────────

struct BatchStats {
    batch_size: usize,
    prove_hist: Histogram<u64>,  // microseconds
    verify_hist: Histogram<u64>, // nanoseconds
    proof_size_hist: Histogram<u64>,
    compression_hist: Histogram<u64>, // × 100 (fixed-point)
    ok: u64,
    fail: u64,
}

impl BatchStats {
    fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            prove_hist: Histogram::new(4).unwrap(),
            verify_hist: Histogram::new(4).unwrap(),
            proof_size_hist: Histogram::new(4).unwrap(),
            compression_hist: Histogram::new(4).unwrap(),
            ok: 0,
            fail: 0,
        }
    }

    fn record_success(&mut self, prove_us: u64, verify_ns: u64, proof_bytes: usize) {
        let raw_bytes = self.batch_size * 64;
        let ratio_x100 = (raw_bytes * 100).checked_div(proof_bytes).unwrap_or(0);
        let _ = self.prove_hist.record(prove_us.max(1));
        let _ = self.verify_hist.record(verify_ns.max(1));
        let _ = self.proof_size_hist.record(proof_bytes as u64);
        let _ = self.compression_hist.record(ratio_x100 as u64);
        self.ok += 1;
    }

    fn record_failure(&mut self) {
        self.fail += 1;
    }

    fn print_summary(&self) {
        let total = self.ok + self.fail;
        let success_rate = if total > 0 {
            100.0 * self.ok as f64 / total as f64
        } else {
            0.0
        };
        if self.prove_hist.is_empty() {
            info!(
                "  batch={:>4}tx  proofs={:>6}  failures={:>4}  success={:.1}%  [no data yet]",
                self.batch_size, self.ok, self.fail, success_rate
            );
            return;
        }
        let prove_p50_ms = self.prove_hist.value_at_quantile(0.50) as f64 / 1_000.0;
        let prove_p99_ms = self.prove_hist.value_at_quantile(0.99) as f64 / 1_000.0;
        let verify_p50_us = self.verify_hist.value_at_quantile(0.50) as f64 / 1_000.0;
        let proof_median = self.proof_size_hist.value_at_quantile(0.50);
        let ratio_median = self.compression_hist.value_at_quantile(0.50) as f64 / 100.0;
        info!(
            "  batch={:>4}tx  proofs={:>6}  fail={:>4}  ok={:.1}%  \
             prove p50={:.1}ms p99={:.1}ms  verify p50={:.1}µs  \
             proof_bytes p50={}  ratio p50={:.1}×",
            self.batch_size,
            self.ok,
            self.fail,
            success_rate,
            prove_p50_ms,
            prove_p99_ms,
            verify_p50_us,
            proof_median,
            ratio_median,
        );
    }
}

// ─── Entry generation ─────────────────────────────────────────────────────────

fn random_entries(rng: &mut impl Rng, n: usize) -> Vec<SigBatchEntry> {
    (0..n)
        .map(|_| {
            let mut msg_hash = [0u8; 32];
            let mut pk_hash = [0u8; 32];
            rng.fill(&mut msg_hash);
            rng.fill(&mut pk_hash);
            SigBatchEntry { msg_hash, pk_hash }
        })
        .collect()
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let cli = Cli::parse();

    let batch_sizes: Vec<usize> = cli
        .batch_sizes
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if batch_sizes.is_empty() {
        anyhow::bail!("--batch-sizes must contain at least one value");
    }

    let duration = Duration::from_secs(cli.duration);
    let report_interval = Duration::from_secs(cli.report_interval);

    let out_path = cli.out.unwrap_or_else(|| {
        PathBuf::from(format!(
            "stark-bench-{}.csv",
            Utc::now().format("%Y%m%dT%H%M%S")
        ))
    });

    info!("╔══════════════════════════════════════════════════════════╗");
    info!("║  Shell-chain STARK Block-Compression Soak Benchmark      ║");
    info!("╚══════════════════════════════════════════════════════════╝");
    info!(
        "Duration    : {}h {:02}m {:02}s ({} s)",
        cli.duration / 3600,
        (cli.duration % 3600) / 60,
        cli.duration % 60,
        cli.duration
    );
    info!("Batch sizes : {:?}", batch_sizes);
    info!("Report every: {}s", cli.report_interval);
    info!("CSV output  : {}", out_path.display());

    let csv_file = File::create(&out_path)?;
    let mut csv_writer = WriterBuilder::new().has_headers(true).from_writer(csv_file);

    let mut rng = if cli.seed == 0 {
        rand::rngs::StdRng::from_entropy()
    } else {
        rand::rngs::StdRng::seed_from_u64(cli.seed)
    };

    let mut stats: Vec<BatchStats> = batch_sizes.iter().map(|&s| BatchStats::new(s)).collect();

    let mut health = ProverHealth::new(ProverHealthConfig {
        warn_backlog_depth: 5,
        overload_backlog_depth: 20,
        failure_window: 20,
        max_failure_rate: cli.max_failure_rate,
        stale_after: Duration::from_secs(300),
    });

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut batch_idx = 0usize;
    let mut total_proofs = 0u64;
    let mut total_failures = 0u64;
    let mut sys = System::new_all();

    // Track peak memory
    let pid = sysinfo::get_current_pid().ok();

    info!("Starting benchmark loop …");

    loop {
        let elapsed = start.elapsed();
        if elapsed >= duration {
            break;
        }

        // Health check — abort if prover is in Failing state
        let hs = health.status(0);
        if hs == HealthStatus::Failing {
            warn!("ProverHealth = Failing — failure rate too high, aborting benchmark");
            break;
        }

        // Cycle batch sizes
        let batch_size = batch_sizes[batch_idx % batch_sizes.len()];
        batch_idx += 1;

        let entries = random_entries(&mut rng, batch_size);
        let raw_bytes = batch_size * 64;

        // ── PROVE ──────────────────────────────────────────────────────────────
        let t_prove = Instant::now();
        let prove_result = prove_sig_batch(&entries);
        let prove_us = t_prove.elapsed().as_micros() as u64;

        let (ok, proof_bytes, verify_us, error_msg) = match prove_result {
            Ok(proof) => {
                // ── VERIFY ────────────────────────────────────────────────────
                let serialized = serde_json::to_vec(&proof).unwrap_or_default();
                let proof_bytes = serialized.len();

                let t_verify = Instant::now();
                let verify_result = verify_sig_batch(&proof);
                let verify_ns = t_verify.elapsed().as_nanos() as u64;

                match verify_result {
                    Ok(()) => (true, proof_bytes, verify_ns, String::new()),
                    Err(e) => (false, proof_bytes, verify_ns, format!("verify: {e}")),
                }
            }
            Err(e) => (false, 0, 0, format!("prove: {e}")),
        };

        // Record to stats
        let stat_idx = (batch_idx - 1) % batch_sizes.len();
        let stat = &mut stats[stat_idx];

        if ok {
            stat.record_success(prove_us, verify_us, proof_bytes);
            health.record_success();
            total_proofs += 1;
        } else {
            stat.record_failure();
            health.record_failure();
            total_failures += 1;
        }

        // ── CSV row ────────────────────────────────────────────────────────────
        let compression_ratio = if proof_bytes > 0 {
            raw_bytes as f64 / proof_bytes as f64
        } else {
            0.0
        };
        csv_writer.serialize(Record {
            timestamp_utc: Utc::now().to_rfc3339(),
            elapsed_secs: elapsed.as_secs(),
            batch_size,
            prove_ms: prove_us as f64 / 1_000.0,
            verify_us: verify_us as f64 / 1_000.0,
            proof_bytes,
            raw_bytes,
            compression_ratio,
            ok: ok as u8,
            error_msg,
        })?;

        // Flush CSV periodically (every 100 records)
        if total_proofs.is_multiple_of(100) {
            csv_writer.flush()?;
        }

        // ── Rolling summary ────────────────────────────────────────────────────
        if last_report.elapsed() >= report_interval {
            last_report = Instant::now();
            let h_elapsed = elapsed.as_secs() / 3600;
            let m_elapsed = (elapsed.as_secs() % 3600) / 60;
            let s_elapsed = elapsed.as_secs() % 60;
            let remaining = duration.saturating_sub(elapsed);
            let h_rem = remaining.as_secs() / 3600;
            let m_rem = (remaining.as_secs() % 3600) / 60;

            // Memory
            sys.refresh_processes();
            let mem_kb = pid
                .and_then(|p| sys.process(p))
                .map(|p| p.memory())
                .unwrap_or(0);

            info!(
                "─── Report @ {}h{:02}m{:02}s  ({}h{:02}m remaining)  health={} ───",
                h_elapsed, m_elapsed, s_elapsed, h_rem, m_rem, hs
            );
            info!(
                "Total proofs: {}  failures: {}  mem: {:.1} MB",
                total_proofs,
                total_failures,
                mem_kb as f64 / 1024.0
            );
            for s in &stats {
                s.print_summary();
            }
        }
    }

    // ── Final flush + summary ──────────────────────────────────────────────────
    csv_writer.flush()?;

    let total_elapsed = start.elapsed();
    info!("══════════════════════════════════════════════════════════");
    info!(
        "FINAL SUMMARY  ({:.1}h elapsed)",
        total_elapsed.as_secs_f64() / 3600.0
    );
    info!("══════════════════════════════════════════════════════════");
    info!("Total proofs   : {}", total_proofs);
    info!(
        "Total failures : {} ({:.2}%)",
        total_failures,
        if total_proofs + total_failures > 0 {
            100.0 * total_failures as f64 / (total_proofs + total_failures) as f64
        } else {
            0.0
        }
    );
    info!("ProverHealth   : {}", health.status(0));
    info!("CSV written to : {}", out_path.display());
    info!("──────────────────────────────────────────────────────────");
    for s in &stats {
        s.print_summary();
    }
    info!("══════════════════════════════════════════════════════════");

    Ok(())
}
