# Changelog

All notable changes to this project will be documented in this file.

## [0.13.0] — 2026-04-15 — M10: Mainnet Readiness

### Added

**Batch 1 — Production Security**
- RPC TLS termination via `tokio-rustls` (`--rpc-tls-cert` / `--rpc-tls-key`)
- Server-wide request rate limiting middleware (`--rpc-rate-limit`)
- API key authentication for all methods (`--rpc-api-key`)
- P2P message signature verification for GossipSub block/tx broadcasts

**Batch 2 — Developer Ecosystem**
- `shell-sdk` TypeScript/JavaScript SDK (viem-based): PQ address encode/decode, AA transaction builders, `ShellProvider`, HTTP/WS transports
- `sdk-signer`: `ShellSigner` class, Dilithium3 WASM binding, keystore JSON support
- `ShellERC20` and `ShellERC721` reference contracts with PQ-signature `permit`/`mint`
- `shell-node wallet` CLI commands: `create`, `balance`, `send`, `export`

**Batch 3 — wPoA Consensus**
- Weighted Proof-of-Authority (`WPoaEngine`) with stake-weighted round-robin proposer selection
- `ValidatorSet`: genesis population, weighted proposer, lifecycle state machine (Pending → Active → Exiting → Exited)
- `SlashingEngine`: double-sign detection, offline detection, configurable slash fractions
- Validator lifecycle CLI: `shell-node validator register / status / exit`
- RPC methods: `shell_getValidatorSet`, `shell_getValidatorInfo`

**Batch 4 — Operations & Observability**
- Structured JSON logging with `--log-format json|text` and sensitive-field filtering
- Extended Prometheus metrics: `shell_aa_tx_total`, `shell_key_rotation_total`, `shell_validator_weight`, `shell_consensus_slot_miss`, `shell_evm_gas_used_total`, `shell_snapshot_size_bytes`
- Admin RPC namespace (`admin_nodeInfo`, `admin_peers`, `admin_addPeer`, `admin_removePeer`, `admin_datadir`); requires `--admin-api` flag (loopback-only by default)
- Hot backup / restore CLI: `shell-node backup create|restore|schedule`

**Batch 5 — Performance**
- Criterion benchmark suite (`crates/bench/`): `bench_crypto`, `bench_state`, `bench_consensus`
- LRU account cache on `WorldState` (default 64 MiB, `--state-cache-size-mb`): write-through, None-caching, configurable capacity
- Mempool tuning CLI flags: `--mempool-max-size` (default 4096), `--mempool-price-bump` (default 10%)

**Batch 6 — QA & Release**
- `tests/e2e/wpoa_regression.rs`: 14 regression tests covering wPoA, slashing, WorldState LRU cache, mempool priority
- `tests/e2e/throughput_test.rs`: in-process 500 TPS baseline tests
- `fuzz/` directory with three `cargo-fuzz` targets: `fuzz_rlp`, `fuzz_rpc`, `fuzz_p2p_msg`
- `deny.toml` for `cargo deny` security policy (advisory, license, and ban checks)
- `run-load-test.sh`: extended with `TX_COUNT` / `DURATION` env vars for 500 TPS / 1h production soak (`TX_COUNT=1800000 DURATION=3600`)
- `run-security-audit.sh`: M10 checks — admin RPC exposure, slash evidence replay guard, TLS downgrade prevention
- `UPGRADE.md`: migration guide from v0.9 → v0.13.0
- `scripts/release.sh`: automated git tagging and release procedure

### Changed
- Workspace version bumped from `0.6.0` to `0.13.0`
- Dockerfile updated with `--platform` multi-arch comment and `arm64` build note
- `NodeConfig`: added `state_cache_size_mb` (default 64), `mempool_max_size`, `mempool_price_bump` fields

### Fixed
- F-379: `WorldState::snapshot()` LRU capacity rounding drift documented
- F-380: WorldState LRU cache now covered by regression tests
- F-381: `PoaEngine::proposer_for_block` private field — bench uses inline modulo
- F-382: Prometheus cache metrics deferred to M11

## [Unreleased]

### Added
- Native Account Abstraction guide covering `pq1...` addresses, validation layers, custom validator flow, and rollout boundaries

### Changed
- README, quickstart, RPC API, PQ crypto, smart contract, and operator docs aligned to the native AA / `pq1...` address model
- JSON-RPC documentation updated to describe `eth_gasPrice` as a dynamic base-fee value instead of a fixed 1 gwei example

## [0.6.0] — 2026-04-06 — Public Testnet Launch Readiness

### Added
- CORS middleware and configurable API namespace whitelist for RPC server
- Rate limiting on RPC endpoints to prevent abuse
- RLP dual-format transaction encoding for Ethereum tooling compatibility
- Full-featured CLI with TOML config parsing, `tx send/deploy/call`, and account management commands
- Production Docker Compose with multi-node orchestration and monitoring stack
- Prometheus + Grafana example monitoring configurations
- Operator guide, API reference, quickstart guide, and post-quantum crypto documentation
- Comprehensive end-to-end smoke tests, stress tests, and sync tests
- CHANGELOG.md with full milestone history
- README.md with project overview and architecture

### Fixed
- 9 cross-cutting audit findings from M5 consolidated audit (F-301 through F-309)
- RPC audit findings F-310, F-311, F-315 (input validation, error handling)
- Infrastructure and CLI audit findings from B2/B3 review
- Documentation audit findings F-335, F-336, F-337, F-338, F-340

### Changed
- Version bump to 0.6.0 across all workspace crates
- Updated `web3_clientVersion` and `shell_getNodeInfo` version strings

## [0.5.0] — 2026-04-05 — EVM Compatibility & Security Hardening

### Added
- Upgrade from Shanghai to Cancun EVM specification
- EIP-2930 access list support in transactions, EVM execution, and RPC
- EIP-4844 basic blob transaction type and gas pricing
- `debug_traceTransaction` and trace API for transaction debugging
- Missing standard Ethereum JSON-RPC methods (full eth_* coverage)
- Comprehensive EVM compatibility verification tests
- State pruning with configurable retention policy
- Batch parallel signature verification with Rayon
- RLP serialization replacing JSON in chain store for performance
- Comprehensive benchmark suite with Criterion

### Fixed
- Crypto security hardening: 6 audit findings in PQ signature handling
- Consensus security hardening: 7 audit findings in PoA engine
- RPC & filter security hardening: 11 audit findings in JSON-RPC layer
- Storage & CLI security hardening: 8 audit findings
- Network security hardening: 3 audit findings in P2P layer
- Validator set bounds check in `produce_block` (F-202)
- Block import transaction validation and mempool size check (F-181, F-182)
- Access list RLP encoding and size caps (F-171, F-172)
- Unsafe zeroize removed, replaced with `Zeroizing` wrapper (F-150, F-151)

## [0.4.0] — 2026-04-04 — Consensus Finality & Validator Management

### Added
- Finality tracker with epoch-based finalization
- Fork choice rule with longest-chain and finality awareness
- Snapshot manager for fast state sync
- Attestation broadcast over P2P network
- Reorg engine with world state rollback
- Dynamic validator set with world state registry
- `shell_addValidator` / `removeValidator` admin RPCs
- `shell_proposeAddValidator` / `proposeRemoveValidator` governance RPCs
- Governance query RPCs (validatorStatus, governanceInfo, estimateGas)
- ValidatorRegistry native system contract in EVM
- Checkpoint sync (`--checkpoint-url`) in CLI and node
- `eth_subscribe` / `eth_unsubscribe` for newHeads, logs, newPendingTransactions, and syncing
- `eth_newFilter`, `eth_newBlockFilter`, `eth_getFilterChanges`, `eth_getFilterLogs`
- Finality-related RPC support
- 31 comprehensive finality, fork choice, and PoA engine tests
- 18 sync and node integration tests

### Fixed
- Equivocation rejection and world state restore on reorg (F-076, F-083)
- Full transaction support in block responses, sync timeout, unified block tags (F-133, F-134, F-099)
- Filter cap and TTL cleanup scheduling (F-116, F-117)
- Audit findings F-072 through F-075 in RPC layer
- P1 robustness audit findings F-136, F-155, F-157
- P1 audit findings F-078, F-080, F-118, F-119, F-121

## [0.3.0] — 2026-04-03 — Execution Pipeline

### Added
- EIP-1559 dynamic base fee calculation with effective gas pricing
- `eth_gasPrice` RPC and `eth_feeHistory` support
- WebSocket transport alongside HTTP for RPC
- `eth_getLogs` with bloom filter fast-path
- Epoch-based PoA consensus with dynamic validator support
- Receipt retrieval by block number and transaction hash
- Transaction gossip — broadcast transactions to connected peers
- Logs bloom filter computation from receipt logs
- State root tracking with pruning readiness
- Prometheus metrics endpoint
- Kademlia DHT for global peer discovery
- GossipSub peer scoring for block and transaction topics
- Bootstrap node configuration in genesis and CLI
- NAT traversal with relay, DCUtR, and autonat
- TLS configuration support for WSS transport
- `shell_getNodeInfo`, `shell_getNetworkStats`, `shell_getChainStats` dashboard RPCs
- Standard `web3_*`, `net_*`, `eth_*` RPC methods for tooling compatibility
- `--log-format json` for structured logging
- Enhanced `/health` endpoint with detailed node status

### Fixed
- Effective gas price in transaction responses (F-055)
- Log connection errors for NAT traversal debugging (F-054)
- Audit findings F-047 through F-053
- 8 review findings from board #15 (F-039 through F-046)

## [0.2.0] — 2026-04-02 — Storage & Consensus

### Added
- RocksDB storage backend with 4 column families
- Merkle Patricia Trie for state root computation
- WorldState manager for account state
- PoA consensus engine with pluggable trait
- Genesis block initialization from config
- Code storage and PQ public key registry in ChainStore
- EVM executor with revm integration
- PQ precompiles — disabled `ecrecover`, added Dilithium3 verify
- Transaction validation pipeline with hybrid pubkey registration
- JSON-RPC server with `eth_*` and `shell_*` endpoints
- `eth_call`, `eth_estimateGas`, `eth_getCode`, `eth_getStorageAt`
- `sendRawTransaction` implementation
- Mempool with PQ signature validation and fee-priority ordering
- Replace-by-Fee support in mempool
- P2P network abstraction with channel-based transport
- libp2p networking with GossipSub and mDNS
- Node harness with block production, async event loop, and RPC
- Block seal verification for imported blocks
- Chain sync protocol (BlockRequest / BlockResponse)
- Docker 3-node testnet E2E test harness
- PQ keystore with argon2id + XChaCha20-Poly1305 encryption
- CLI binary with `run`, `init`, `key` subcommands
- `export-state`, `import-state`, `removedb`, `version` CLI subcommands
- RocksDB persistent storage backend with chain resumption

### Fixed
- Deterministic MessageId with BLAKE3, toggleable mDNS (F-031, F-032)
- Cap BlockRequest count to 128 to prevent OOM DoS (F-033)
- Typed GapDetected error prevents sync amplification DoS (F-037)
- Password echo suppression with rpassword (F-030)
- Balance check in mempool (F-020), v/r/s compat fields (F-022)
- Checked arithmetic in gas cost calculation (F-015)
- Configurable RocksDB tuning parameters (F-018)
- DNS transport + mDNS dial + active sync for Docker E2E
- Non-root container user, polling-based sync wait (F-035, F-036)

## [0.1.0] — 2026-04-01 — Foundation

### Added
- `shell-primitives` crate: Keccak-256, BLAKE3, H256, Address, U256, Bytes types
- `shell-crypto` crate: CRYSTALS-Dilithium signing and verification
- SPHINCS+ signer with multi-algorithm verification support
- `shell-core` crate: Block, Transaction (AA-native), Account, Receipt types
- Native account abstraction with pluggable signature schemes
- Post-quantum address derivation (`keccak256(pq_pubkey)[12..]`)
- MIT license and open-source scaffolding
- Workspace structure with modular crate architecture

### Fixed
- 9 immediate review findings (F-002 through F-015)
- M1 review findings for primitives, crypto, and core crates
- 4 review findings (F-005, F-007, F-009, F-010)
- 3 review findings (F-011, F-012, F-013) with F-014 documentation
