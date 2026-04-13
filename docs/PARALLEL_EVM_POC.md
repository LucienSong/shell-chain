# Parallel EVM PoC (M11)

This document records the **M11 parallel-EVM proof of concept** added to `shell-evm`.

## Scope

The PoC is intentionally **additive** and **feature-flagged**:

- it does **not** replace the current production executor path
- it builds **read/write sets**, **conflict graphs**, and **execution waves**
- it provides a generic rayon-backed scheduler for **conflict-free waves**
- it falls back to **serial execution** when the extracted rw-set is incomplete

## Implemented pieces

### 1. Read/write-set extraction

`shell-evm` now exposes:

- `TxAccessPath`
- `TxReadWriteSet`
- `ReadWriteSetExtractor`
- `HeuristicRwSetExtractor`

Covered transaction classes:

1. native value transfer
2. native system contracts (`ValidatorRegistry`, `AccountManager`)
3. ERC20 `transfer(address,uint256)`

Unsupported or dynamic flows are marked `complete = false` and conservatively widened.

### 2. Conflict graph

`TxConflictGraph` computes pairwise conflicts from extracted rw-sets.

Current conflict reasons:

- `ReadWrite`
- `WriteWrite`
- `Incomplete`

### 3. Parallel scheduler PoC

`ParallelScheduler` builds greedy conflict-free waves:

- independent transactions can share one wave
- conflicting transactions are split across waves
- incomplete rw-sets can trigger a full serial fallback

The scheduler also exposes a generic `execute()` helper that runs one wave at a time and uses rayon inside parallelizable waves.
That helper is intended for **side-effect-free per-transaction jobs** whose shared-state merge happens deterministically after the wave completes.

### 4. Feature flag

`shell-node::NodeConfig` now carries:

- `parallel_evm.enabled`
- `parallel_evm.max_workers`
- `parallel_evm.fallback_on_incomplete`

The default remains **disabled**.

### 5. Bench harness

Criterion bench added in:

- `crates/evm/benches/parallel_poc.rs`

It covers:

1. independent transfer planning
2. independent-wave execution
3. conflicting transfer fallback behavior

Run with:

```bash
cargo bench -p shell-evm --bench parallel_poc
```

## Current conclusion

The PoC is suitable for **further experimentation**, not for default execution yet.

Reasons:

1. rw-set extraction is still heuristic for general contract calls
2. ERC20 coverage is limited to the standard `transfer(address,uint256)` path
3. production integration should wait until benchmark results and wider opcode/storage coverage are expanded

## Recommended next step

Promote the scheduler from PoC to execution path only after:

1. broader storage-shape extraction
2. benchmark validation on realistic mixed workloads
3. determinism checks against the current serial executor
