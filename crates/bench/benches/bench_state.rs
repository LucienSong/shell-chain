/// Criterion benchmarks for shell-chain world-state / trie operations.
///
/// Covers account read/write, balance update, storage SSTORE/SLOAD, and
/// state-root computation — with and without the LRU account cache.
/// Run with: `cargo bench --package shell-bench --bench bench_state`
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use shell_primitives::{Address, ShellHash, U256};
use shell_storage::{MemoryDb, WorldState};

fn sample_address(idx: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[19] = idx;
    Address::from(bytes)
}

fn bench_account_read_cached(c: &mut Criterion) {
    let db = Arc::new(MemoryDb::new());
    let mut ws = WorldState::new(db.clone());

    // Pre-populate 256 accounts.
    for i in 0u8..=255 {
        let addr = sample_address(i);
        ws.set_balance(&addr, U256::from(i as u64 * 1_000_000_u64))
            .unwrap();
    }
    // Warm the LRU cache.
    for i in 0u8..=255 {
        let _ = ws.get_account(&sample_address(i));
    }

    let mut group = c.benchmark_group("world_state");
    group.throughput(Throughput::Elements(1));

    group.bench_function("get_account_cache_hit", |b| {
        let addr = sample_address(42);
        b.iter(|| ws.get_account(black_box(&addr)).unwrap())
    });

    group.bench_function("set_balance", |b| {
        let addr = sample_address(0);
        b.iter(|| {
            ws.set_balance(black_box(&addr), black_box(U256::from(999_u64)))
                .unwrap()
        })
    });

    group.finish();
}

fn bench_state_root(c: &mut Criterion) {
    let mut group = c.benchmark_group("world_state");

    for account_count in [10_u32, 100, 1000] {
        let db = Arc::new(MemoryDb::new());
        let mut ws = WorldState::new(db);
        for i in 0..account_count {
            let mut bytes = [0u8; 20];
            bytes[16..20].copy_from_slice(&i.to_be_bytes());
            let addr = Address::from(bytes);
            ws.set_balance(&addr, U256::from(i as u64)).unwrap();
        }

        group.throughput(Throughput::Elements(account_count as u64));
        group.bench_with_input(
            BenchmarkId::new("state_root", account_count),
            &account_count,
            |b, _| b.iter(|| ws.state_root().unwrap()),
        );
    }

    group.finish();
}

fn bench_storage_rw(c: &mut Criterion) {
    let db = Arc::new(MemoryDb::new());
    let mut ws = WorldState::new(db);
    let addr = sample_address(1);
    let slot = ShellHash::from([0xde_u8; 32]);
    let value = ShellHash::from([0xad_u8; 32]);

    let mut group = c.benchmark_group("world_state");
    group.throughput(Throughput::Elements(1));

    group.bench_function("sstore", |b| {
        b.iter(|| {
            ws.set_storage(black_box(&addr), black_box(&slot), black_box(&value))
                .unwrap()
        })
    });

    group.bench_function("sload", |b| {
        b.iter(|| ws.get_storage(black_box(&addr), black_box(&slot)).unwrap())
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_account_read_cached,
    bench_state_root,
    bench_storage_rw
);
criterion_main!(benches);
