# Shell Chain Benchmarks

## Overview

This document reports benchmark results for the block data reduction initiative (A1 + A2),
covering compression effectiveness and pubkey deduplication savings.

All benchmarks run on the Criterion framework (`cargo bench -p bench`).

---

## A1: RocksDB Zstd Compression

### Methodology

- 10,000 random key-value writes to a temp RocksDB instance (64-byte key, 4 KB value)
- Measured: write throughput (MiB/s), read latency (ns), and read throughput (GiB/s)
- Two configurations: `NoCompression` vs `ZstdCold` (level 3, applied to L0+)
- Run: `cargo bench -p bench --bench bench_compression rocksdb`

### Results

| Metric | NoCompression | ZstdCold |
|--------|--------------|---------|
| Write latency (mean) | 10.997 ms | 13.155 ms |
| Write throughput | 46.840 MiB/s | 39.154 MiB/s |
| Read latency (mean) | 479.94 ns | 471.01 ns |
| Read throughput | 10.481 GiB/s | 10.679 GiB/s |

### Analysis

- **Write overhead**: ~16% slower writes (10.997 ms → 13.155 ms)
  - Within acceptable range: writes are I/O-bound; CPU is not the bottleneck
  - Block production rate (1 block/2 s) is not affected
- **Read benefit**: ~2% faster reads (decompressed data is smaller, fits better in OS page cache)
- **Disk savings estimate**: 8–15% on `chain` + `receipts` CFs
  - Dilithium3 signatures (3,309 B) and pubkeys (1,952 B) are near-random → Zstd <5% compression
  - Transaction metadata (nonce, to, value, gas) and block headers are repetitive → ~30–40% compression
  - PQ bytes dominate volume (97% of tx data), so overall savings are modest

### Conclusion

A1 delivers moderate disk savings (~8–15%) with acceptable overhead. Primary value is as a
baseline for future improvements — once witness separation (B-tier) reduces PQ bytes from
the chain CF, Zstd will become significantly more effective.

---

## A2: PubkeyMode — Pubkey-by-Reference Deduplication

### Methodology

- Encoded `SignedTransaction` RLP in two modes:
  - **Embedded**: full 1,952-byte Dilithium3 pubkey inline (first tx from a sender)
  - **Reference**: empty pubkey field (0x80 = 1 byte); node resolves from `pk/` store
- Measured: RLP encoding speed (ns) and throughput (GiB/s)
- Batch deduplication measured at 0 / 50 / 90 / 95 / 99% repeat-sender rates
- Run: `cargo bench -p bench --bench bench_compression pubkeymode`

### Results: Encoding Speed

| Mode | Latency (mean) | Throughput |
|------|---------------|-----------|
| Embedded | 155.58 ns | 31.949 GiB/s |
| Reference | 137.50 ns | 22.914 GiB/s |

> Reference mode is ~12% faster to encode (smaller payload). Throughput appears lower
> because throughput is computed against bytes encoded — Reference encodes fewer bytes total.

### Results: Per-Transaction Wire Size

| Mode | RLP size | Delta |
|------|---------|-------|
| Embedded (`PubkeyMode::Embedded`) | ~5,431 B | baseline |
| Reference (`PubkeyMode::Reference`) | ~3,477 B | **-1,954 B (-36%)** |

### Results: Batch Impact (500 tx/block)

| Dedup Rate | Embedded txs | Reference txs | Block saving | Block size reduction |
|-----------|-------------|--------------|-------------|---------------------|
| 0% | 500 | 0 | 0 B | 0% |
| 50% | 250 | 250 | ~488 KB | ~18% |
| 90% | 50 | 450 | ~878 KB | ~32% |
| 95% | 25 | 475 | ~927 KB | ~34% |
| 99% | 5 | 495 | ~966 KB | ~36% |

Base block size (0% dedup, 500 tx): ~2.7 MB  
At 95% dedup: ~1.77 MB/block = **~34% reduction from A2 alone**

### Analysis

- Real-world dedup rate: Most active chains see 80–95%+ repeat senders per block
- 95% dedup is the target operating point
- Savings are deterministic and proportional to sender-repeat rate

---

## Combined A1 + A2 Impact

| Scenario | Per-block | Per-hour | Per-day |
|---------|----------|---------|--------|
| Baseline (no optimization) | 2.70 MB | 4.70 GB | 113 GB |
| A1 only (Zstd, ~12% disk) | 2.38 MB | 4.13 GB | ~99 GB |
| A2 only (95% dedup) | 1.77 MB | 3.07 GB | ~74 GB |
| A1 + A2 combined | ~1.56 MB | ~2.71 GB | ~65 GB |

**Combined reduction: ~42% vs baseline** at 95% dedup rate. The 50% design target is
achievable at ≥99% dedup rate or when account deduplication extends to intra-epoch reuse.

> Note: Zstd compression ratio improves further after B-tier witness separation removes
> raw PQ bytes from the `chain` CF. Post-B-tier estimate: 55–65% combined reduction.

---

## Benchmark Commands

```bash
# Run all compression benchmarks
cargo bench -p bench --bench bench_compression

# Run specific group
cargo bench -p bench --bench bench_compression -- pubkeymode_rlp
cargo bench -p bench --bench bench_compression -- batch_dedup
cargo bench -p bench --bench bench_compression -- rocksdb_write
cargo bench -p bench --bench bench_compression -- rocksdb_read

# Open HTML report
open target/criterion/rocksdb_write/write_zstd_cold/report/index.html
```

---

## Environment

| Item | Value |
|------|-------|
| CPU | Apple Silicon M-series |
| Compression | RocksDB ZstdCold level 3 |
| Bench framework | Criterion 0.5 |
| Signature scheme | Dilithium3 (CRYSTALS-Dilithium, NIST PQC standard) |
| Pubkey size | 1,952 bytes (`DILITHIUM3_PUBKEY_LEN`) |
| Signature size | 3,309 bytes |
