/// Criterion benchmarks for PQ block data reduction (Tier 1 + Tier 2).
///
/// Measures:
/// - A1: RocksDB Zstd write throughput vs no-compression baseline
/// - A2: PubkeyMode RLP encoding size (Embedded vs Reference)
/// - Combined: projected bytes-on-disk per block at varying pubkey dedup rates
///
/// Run with: `cargo bench --package shell-bench --bench bench_compression`
use alloy_rlp::Encodable;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use shell_core::{SignedTransaction, Transaction, DILITHIUM3_PUBKEY_LEN};
use shell_crypto::{DilithiumSigner, Signer};
use shell_primitives::{Address, Bytes, U256};
use shell_storage::{CfCompressionStrategy, KvStore, RocksDbConfig, RocksDbStore, RocksDbStores};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Dilithium3 signature size in bytes.
const DILITHIUM3_SIG_LEN: usize = 3309;
/// PubkeyMode::Reference RLP encoding: empty byte string (1 byte: 0x80).
const PUBKEY_REFERENCE_RLP_LEN: usize = 1;
/// Approximate transaction metadata size (chain_id, nonce, gas, value, etc).
const TX_METADATA_LEN: usize = 140;
/// Baseline tx size with embedded pubkey (first tx from new address).
pub const TX_SIZE_EMBEDDED: usize = TX_METADATA_LEN + DILITHIUM3_SIG_LEN + DILITHIUM3_PUBKEY_LEN;
/// Reference tx size (subsequent txs from registered address).
pub const TX_SIZE_REFERENCE: usize =
    TX_METADATA_LEN + DILITHIUM3_SIG_LEN + PUBKEY_REFERENCE_RLP_LEN;

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn make_transfer_tx(nonce: u64) -> Transaction {
    Transaction {
        chain_id: 1337,
        nonce,
        to: Some(Address::from([0xAA; 20])),
        value: U256::from(1u64),
        data: Bytes::new(),
        gas_limit: 21_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        access_list: None,
        tx_type: 2,
        max_fee_per_blob_gas: None,
        blob_versioned_hashes: None,
    }
}

/// Build a batch of signed transactions mixing Embedded and Reference modes.
/// `dedup_rate` is the fraction of txs that use Reference mode (0.0 = all Embedded).
fn build_tx_batch(
    signer: &DilithiumSigner,
    from: Address,
    count: usize,
    dedup_rate: f64,
) -> Vec<SignedTransaction> {
    let pubkey = signer.public_key().to_vec();
    (0..count)
        .map(|i| {
            let tx = make_transfer_tx(i as u64);
            let sig = signer.sign(tx.hash().0.as_slice()).unwrap();
            // First tx always Embedded; subsequent txs at dedup_rate
            if i == 0 || (i as f64 / count as f64) < (1.0 - dedup_rate) {
                SignedTransaction::with_pubkey(from, tx, sig, pubkey.clone())
            } else {
                SignedTransaction::new(from, tx, sig)
            }
        })
        .collect()
}

/// Return the total RLP-encoded byte size of a transaction batch.
fn batch_rlp_size(txs: &[SignedTransaction]) -> usize {
    txs.iter().map(|t| t.length()).sum()
}

// ─── A2: PubkeyMode wire size benchmarks ─────────────────────────────────────

fn bench_pubkeymode_encoding(c: &mut Criterion) {
    let signer = DilithiumSigner::generate();
    let from = Address::from_public_key(signer.public_key(), signer.sig_type().as_u8());
    let pubkey = signer.public_key().to_vec();

    let tx = make_transfer_tx(0);
    let sig = signer.sign(tx.hash().0.as_slice()).unwrap();

    let embedded = SignedTransaction::with_pubkey(from, tx.clone(), sig.clone(), pubkey);
    let reference = SignedTransaction::new(from, tx, sig);

    let mut group = c.benchmark_group("pubkeymode_rlp");

    let embedded_len = embedded.length();
    let reference_len = reference.length();
    group.throughput(Throughput::Bytes(embedded_len as u64));
    group.bench_function("encode_embedded", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(embedded_len);
            black_box(&embedded).encode(&mut buf);
            buf
        })
    });

    group.throughput(Throughput::Bytes(reference_len as u64));
    group.bench_function("encode_reference", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(reference_len);
            black_box(&reference).encode(&mut buf);
            buf
        })
    });

    group.finish();
}

// ─── A2: Batch dedup rate impact ─────────────────────────────────────────────

fn bench_batch_dedup_rates(c: &mut Criterion) {
    let signer = DilithiumSigner::generate();
    let from = Address::from_public_key(signer.public_key(), signer.sig_type().as_u8());

    let mut group = c.benchmark_group("batch_dedup");

    for dedup_rate in [0.0f64, 0.50, 0.90, 0.95, 0.99] {
        let txs = build_tx_batch(&signer, from, 100, dedup_rate);
        let total_bytes = batch_rlp_size(&txs) as u64;
        group.throughput(Throughput::Bytes(total_bytes));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("dedup_{:.0}pct", dedup_rate * 100.0)),
            &txs,
            |b, txs| {
                b.iter(|| {
                    let mut total = 0usize;
                    for t in black_box(txs) {
                        total += t.length();
                    }
                    total
                })
            },
        );
    }

    group.finish();
}

// ─── A1: RocksDB write throughput with/without Zstd ──────────────────────────

fn bench_rocksdb_write_throughput(c: &mut Criterion) {
    // Pre-generate a block payload: 100 txs × ~5.4KB each = ~540KB
    let payload: Vec<u8> = {
        let sig = vec![0xD3u8; DILITHIUM3_SIG_LEN];
        let pk = vec![0xABu8; DILITHIUM3_PUBKEY_LEN];
        let meta = vec![0x11u8; TX_METADATA_LEN];
        // Concatenate into a realistic block payload
        let tx_data: Vec<u8> = [meta.as_slice(), sig.as_slice(), pk.as_slice()].concat();
        tx_data.repeat(100)
    };

    let mut group = c.benchmark_group("rocksdb_write");
    group.throughput(Throughput::Bytes(payload.len() as u64));

    // Baseline: no compression
    let dir_none = tempfile::tempdir().unwrap();
    let cfg_none = RocksDbConfig {
        bulk_compression: CfCompressionStrategy::None,
        ..Default::default()
    };
    let stores_none: RocksDbStores =
        RocksDbStore::open_all(dir_none.path(), Some(cfg_none)).unwrap();

    group.bench_function("write_no_compression", |b| {
        let mut seq = 0u64;
        b.iter(|| {
            let key = seq.to_be_bytes();
            stores_none.chain.put(&key, black_box(&payload)).unwrap();
            seq += 1;
        })
    });

    // Zstd cold (L2+ compression)
    let dir_zstd = tempfile::tempdir().unwrap();
    let cfg_zstd = RocksDbConfig {
        bulk_compression: CfCompressionStrategy::ZstdCold,
        ..Default::default()
    };
    let stores_zstd: RocksDbStores =
        RocksDbStore::open_all(dir_zstd.path(), Some(cfg_zstd)).unwrap();

    group.bench_function("write_zstd_cold", |b| {
        let mut seq = 0u64;
        b.iter(|| {
            let key = seq.to_be_bytes();
            stores_zstd.chain.put(&key, black_box(&payload)).unwrap();
            seq += 1;
        })
    });

    group.finish();
}

// ─── A1: RocksDB read throughput with/without Zstd ───────────────────────────

fn bench_rocksdb_read_throughput(c: &mut Criterion) {
    let payload: Vec<u8> = {
        let sig = vec![0xD3u8; DILITHIUM3_SIG_LEN];
        let pk = vec![0xABu8; DILITHIUM3_PUBKEY_LEN];
        let meta = vec![0x11u8; TX_METADATA_LEN];
        [meta.as_slice(), sig.as_slice(), pk.as_slice()].concat()
    };

    let mut group = c.benchmark_group("rocksdb_read");
    group.throughput(Throughput::Bytes(payload.len() as u64));

    // Pre-populate 100 entries in each store
    let dir_none = tempfile::tempdir().unwrap();
    let stores_none: RocksDbStores = RocksDbStore::open_all(
        dir_none.path(),
        Some(RocksDbConfig {
            bulk_compression: CfCompressionStrategy::None,
            ..Default::default()
        }),
    )
    .unwrap();
    for i in 0u64..100 {
        stores_none.chain.put(&i.to_be_bytes(), &payload).unwrap();
    }

    let dir_zstd = tempfile::tempdir().unwrap();
    let stores_zstd: RocksDbStores = RocksDbStore::open_all(
        dir_zstd.path(),
        Some(RocksDbConfig {
            bulk_compression: CfCompressionStrategy::ZstdCold,
            ..Default::default()
        }),
    )
    .unwrap();
    for i in 0u64..100 {
        stores_zstd.chain.put(&i.to_be_bytes(), &payload).unwrap();
    }

    group.bench_function("read_no_compression", |b| {
        let mut seq = 0u64;
        b.iter(|| {
            let key = (seq % 100).to_be_bytes();
            let _ = black_box(stores_none.chain.get(&key).unwrap());
            seq += 1;
        })
    });

    group.bench_function("read_zstd_cold", |b| {
        let mut seq = 0u64;
        b.iter(|| {
            let key = (seq % 100).to_be_bytes();
            let _ = black_box(stores_zstd.chain.get(&key).unwrap());
            seq += 1;
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_pubkeymode_encoding,
    bench_batch_dedup_rates,
    bench_rocksdb_write_throughput,
    bench_rocksdb_read_throughput,
);
criterion_main!(benches);
