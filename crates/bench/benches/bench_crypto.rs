/// Criterion benchmarks for shell-chain cryptographic operations.
///
/// Covers Dilithium3 sign/verify, blake3 hashing, and keccak256 hashing.
/// Run with: `cargo bench --package shell-bench --bench bench_crypto`
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use shell_crypto::{DilithiumSigner, DilithiumVerifier, Signer, Verifier};
use shell_primitives::{blake3_hash, keccak256};

const MESSAGE_SMALL: &[u8] = b"shell-chain benchmark message";
const MESSAGE_LARGE: &[u8] = &[0xab_u8; 4096];

fn bench_dilithium_sign(c: &mut Criterion) {
    let signer = DilithiumSigner::generate();

    let mut group = c.benchmark_group("dilithium3");
    group.throughput(Throughput::Bytes(MESSAGE_SMALL.len() as u64));

    group.bench_function("sign_small", |b| {
        b.iter(|| signer.sign(black_box(MESSAGE_SMALL)).unwrap())
    });

    group.throughput(Throughput::Bytes(MESSAGE_LARGE.len() as u64));
    group.bench_function("sign_large_4k", |b| {
        b.iter(|| signer.sign(black_box(MESSAGE_LARGE)).unwrap())
    });

    let sig = signer.sign(MESSAGE_SMALL).unwrap();
    let verifier = DilithiumVerifier;
    let pubkey = signer.public_key().to_vec();
    group.throughput(Throughput::Bytes(MESSAGE_SMALL.len() as u64));
    group.bench_function("verify_small", |b| {
        b.iter(|| {
            verifier
                .verify(
                    black_box(&pubkey),
                    black_box(MESSAGE_SMALL),
                    black_box(&sig),
                )
                .unwrap()
        })
    });

    group.finish();
}

fn bench_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash");

    for size in [32_usize, 256, 1024, 4096] {
        let data = vec![0x42_u8; size];
        group.throughput(Throughput::Bytes(size as u64));

        let label = format!("blake3_{size}b");
        group.bench_function(&label, |b| b.iter(|| blake3_hash(black_box(&data))));

        let label = format!("keccak256_{size}b");
        group.bench_function(&label, |b| b.iter(|| keccak256(black_box(&data))));
    }

    group.finish();
}

criterion_group!(benches, bench_dilithium_sign, bench_hash);
criterion_main!(benches);
