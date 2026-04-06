//! Benchmarks for shell-crypto: Dilithium3, SPHINCS+, SHA3-256.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use sha3::{Digest, Sha3_256};
use shell_crypto::{
    BatchVerifier, DilithiumSigner, DilithiumVerifier, MultiVerifier, PQSignature, Signer,
    SphincsSigner, SphincsVerifier, Verifier, VerifyItem,
};

const MSG: &[u8] = b"shell-chain benchmark payload 32B";

// ── Dilithium3 ───────────────────────────────────────────────

fn bench_dilithium_keygen(c: &mut Criterion) {
    c.bench_function("dilithium3/keygen", |b| {
        b.iter(|| black_box(DilithiumSigner::generate()));
    });
}

fn bench_dilithium_sign(c: &mut Criterion) {
    let signer = DilithiumSigner::generate();
    c.bench_function("dilithium3/sign", |b| {
        b.iter(|| signer.sign(black_box(MSG)).unwrap());
    });
}

fn bench_dilithium_verify(c: &mut Criterion) {
    let signer = DilithiumSigner::generate();
    let sig = signer.sign(MSG).unwrap();
    let verifier = DilithiumVerifier;
    c.bench_function("dilithium3/verify", |b| {
        b.iter(|| {
            verifier
                .verify(
                    black_box(signer.public_key()),
                    black_box(MSG),
                    black_box(&sig),
                )
                .unwrap()
        });
    });
}

fn bench_dilithium_batch_verify(c: &mut Criterion) {
    let verifier = MultiVerifier;
    let mut group = c.benchmark_group("dilithium3/batch_verify");

    for count in [10, 50, 100] {
        // Pre-generate signers, signatures
        let signers: Vec<DilithiumSigner> =
            (0..count).map(|_| DilithiumSigner::generate()).collect();
        let sigs: Vec<PQSignature> = signers.iter().map(|s| s.sign(MSG).unwrap()).collect();
        let pubkeys: Vec<Vec<u8>> = signers.iter().map(|s| s.public_key().to_vec()).collect();

        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                let items: Vec<VerifyItem<'_>> = pubkeys
                    .iter()
                    .zip(sigs.iter())
                    .map(|(pk, sig)| VerifyItem {
                        pubkey: pk.as_slice(),
                        message: MSG,
                        signature: sig,
                    })
                    .collect();
                verifier.verify_batch(black_box(&items)).unwrap()
            });
        });
    }
    group.finish();
}

// ── SPHINCS+ ─────────────────────────────────────────────────

fn bench_sphincs_sign(c: &mut Criterion) {
    let signer = SphincsSigner::generate();
    c.bench_function("sphincs+/sign", |b| {
        b.iter(|| signer.sign(black_box(MSG)).unwrap());
    });
}

fn bench_sphincs_verify(c: &mut Criterion) {
    let signer = SphincsSigner::generate();
    let sig = signer.sign(MSG).unwrap();
    let verifier = SphincsVerifier;
    c.bench_function("sphincs+/verify", |b| {
        b.iter(|| {
            verifier
                .verify(
                    black_box(signer.public_key()),
                    black_box(MSG),
                    black_box(&sig),
                )
                .unwrap()
        });
    });
}

// ── SHA3-256 ─────────────────────────────────────────────────

fn bench_sha3_256(c: &mut Criterion) {
    let data_32 = vec![0xABu8; 32];
    let data_1kb = vec![0xABu8; 1024];
    let data_1mb = vec![0xABu8; 1024 * 1024];

    let mut group = c.benchmark_group("sha3-256");
    group.bench_function("32B", |b| {
        b.iter(|| {
            let mut h = Sha3_256::new();
            h.update(black_box(&data_32));
            black_box(h.finalize())
        });
    });
    group.bench_function("1KB", |b| {
        b.iter(|| {
            let mut h = Sha3_256::new();
            h.update(black_box(&data_1kb));
            black_box(h.finalize())
        });
    });
    group.bench_function("1MB", |b| {
        b.iter(|| {
            let mut h = Sha3_256::new();
            h.update(black_box(&data_1mb));
            black_box(h.finalize())
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_dilithium_keygen,
    bench_dilithium_sign,
    bench_dilithium_verify,
    bench_dilithium_batch_verify,
    bench_sphincs_sign,
    bench_sphincs_verify,
    bench_sha3_256,
);
criterion_main!(benches);
