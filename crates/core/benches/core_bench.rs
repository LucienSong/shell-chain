//! Benchmarks for shell-core: Transaction RLP, hash, fee calculations.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use alloy_rlp::{Decodable, Encodable};
use shell_core::{
    calc_blob_gas_price, calc_excess_blob_gas, calculate_base_fee, SignedTransaction, Transaction,
};
use shell_crypto::{PQSignature, SignatureType};
use shell_primitives::{Address, Bytes, U256};

fn sample_tx() -> Transaction {
    Transaction {
        chain_id: 1337,
        nonce: 42,
        to: Some(Address::from([0x42; 20])),
        value: U256::from(1_000_000u64),
        data: Bytes::from(vec![0xAB; 64]),
        gas_limit: 21_000,
        max_fee_per_gas: 2_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        access_list: None,
        tx_type: 2,
        max_fee_per_blob_gas: None,
        blob_versioned_hashes: None,
    }
}

fn sample_signed_tx() -> SignedTransaction {
    let tx = sample_tx();
    let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xAB; 3293]);
    SignedTransaction::new(Address::from([0x01; 20]), tx, sig)
}

// ── Transaction RLP ──────────────────────────────────────────

fn bench_tx_rlp(c: &mut Criterion) {
    let tx = sample_tx();
    let mut buf = Vec::new();
    tx.encode(&mut buf);
    let encoded = buf.clone();

    let mut group = c.benchmark_group("transaction/rlp");
    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut out = Vec::with_capacity(256);
            black_box(&tx).encode(&mut out);
            black_box(out);
        });
    });
    group.bench_function("decode", |b| {
        b.iter(|| {
            let t = Transaction::decode(&mut black_box(encoded.as_slice())).unwrap();
            black_box(t);
        });
    });
    group.finish();
}

fn bench_signed_tx_rlp(c: &mut Criterion) {
    let signed = sample_signed_tx();
    let mut buf = Vec::new();
    signed.encode(&mut buf);
    let encoded = buf.clone();

    let mut group = c.benchmark_group("signed_tx/rlp");
    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut out = Vec::with_capacity(4096);
            black_box(&signed).encode(&mut out);
            black_box(out);
        });
    });
    group.bench_function("decode", |b| {
        b.iter(|| {
            let t = SignedTransaction::decode(&mut black_box(encoded.as_slice())).unwrap();
            black_box(t);
        });
    });
    group.finish();
}

// ── Transaction hash ─────────────────────────────────────────

fn bench_tx_hash(c: &mut Criterion) {
    let tx = sample_tx();
    c.bench_function("transaction/hash", |b| {
        b.iter(|| black_box(black_box(&tx).hash()));
    });
}

// ── Base fee calculation ─────────────────────────────────────

fn bench_base_fee(c: &mut Criterion) {
    c.bench_function("fee/base_fee_calc", |b| {
        b.iter(|| {
            // Simulate a block at 80% utilization
            black_box(calculate_base_fee(
                black_box(24_000_000),
                black_box(30_000_000),
                black_box(1_000_000_000),
            ))
        });
    });
}

// ── Blob gas price ───────────────────────────────────────────

fn bench_blob_gas_price(c: &mut Criterion) {
    let mut group = c.benchmark_group("fee/blob_gas");

    group.bench_function("calc_price", |b| {
        b.iter(|| black_box(calc_blob_gas_price(black_box(393_216))));
    });

    group.bench_function("calc_excess", |b| {
        b.iter(|| black_box(calc_excess_blob_gas(black_box(393_216), black_box(131_072))));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_tx_rlp,
    bench_signed_tx_rlp,
    bench_tx_hash,
    bench_base_fee,
    bench_blob_gas_price,
);
criterion_main!(benches);
