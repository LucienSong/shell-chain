# shell-chain

<!-- [![Build Status](https://img.shields.io/github/actions/workflow/status/LucienSong/shell-chain/ci.yml?branch=main)](https://github.com/LucienSong/shell-chain/actions) -->
<!-- [![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE) -->
<!-- [![Version](https://img.shields.io/badge/version-0.6.0-green.svg)](CHANGELOG.md) -->

A **post-quantum secure, EVM-compatible blockchain** — quantum safety from day one, no migration needed.

## Overview

Shell-Chain follows [Vitalik Buterin's vision](https://ethresear.ch/t/how-to-hard-fork-to-save-most-users-funds-in-a-quantum-emergency/18901) for Ethereum's quantum upgrade, but skips the migration path entirely by building a new chain with PQ cryptography as the foundation.

### Key Features

- 🔐 **Post-Quantum Signatures** — CRYSTALS-Dilithium (NIST standard) as default, SPHINCS+ as conservative fallback
- ⚙️ **EVM Compatible** — Cancun-spec EVM; run Solidity contracts with familiar tooling (Hardhat, ethers.js, MetaMask)
- 🏗️ **Native Account Abstraction** — protocol-level smart accounts with built-in PQ validation, key rotation, and custom validator hooks
- 🧩 **PQ Precompiles** — on-chain Dilithium/SPHINCS+ verification, Kyber decapsulation, STARK proof verification
- 🔗 **PoA Consensus** — epoch-based Proof-of-Authority with dynamic validator management and finality tracking
- 🌐 **P2P Networking** — libp2p with GossipSub, Kademlia DHT, NAT traversal, and peer scoring
- 📡 **Full JSON-RPC** — Ethereum-compatible `eth_*`, `web3_*`, `net_*`, `debug_*`, plus Shell-specific APIs
- 🐳 **Production Ready** — Docker Compose orchestration, Prometheus/Grafana monitoring, TOML configuration
- 🛡️ **Security Hardened** — 50+ audit findings addressed across all subsystems

## Quick Start

See [docs/QUICKSTART.md](docs/QUICKSTART.md) for a complete guide to running a local node.

```bash
# Build
cargo build --release

# Initialize a new chain
./target/release/shell-node init --datadir ./data

# Run a node
./target/release/shell-node run --datadir ./data
```

For production deployments with Docker, see the [Operator Guide](docs/TESTNET_OPERATOR_GUIDE.md).

## Native Account Abstraction

Shell-Chain's long-term account model is **native AA**.
Externally, accounts are shown as `pq1...` Bech32m addresses; internally, they
remain 20-byte EVM addresses for compatibility.

Transaction validation follows three protocol-level paths:

1. **First use** — derive `tx.from` from `(version, algo_id, pubkey)` and verify the PQ signature
2. **Default existing account** — verify `pq_pubkey_hash` and the PQ signature
3. **Custom AA account** — call account-specific validator code through `validation_code_hash`

This design lets Shell-Chain support key rotation and custom validation logic
without introducing an ERC-4337 bundler or changing the account's address.

For the full design and current implementation status, see
[docs/ACCOUNT_ABSTRACTION_GUIDE.md](docs/ACCOUNT_ABSTRACTION_GUIDE.md).

## Architecture

```
┌─────────────────────────────────────────────┐
│                 shell-node                  │
│          (Node Builder / CLI)               │
├─────────┬──────────┬──────────┬─────────────┤
│   RPC   │ Mempool  │Consensus │  Network    │
├─────────┴──────────┴────┬─────┴─────────────┤
│                    shell-core               │
│       (Block, Transaction, Account)         │
├──────────┬──────────────┼───────────────────┤
│ shell-evm│ shell-crypto │  shell-storage    │
│  (revm)  │  (PQ Crypto) │   (RocksDB)      │
├──────────┴──────────────┴───────────────────┤
│              shell-primitives               │
│        (Hash, Address, U256, Bytes)         │
└─────────────────────────────────────────────┘
```

### Crate Map

| Crate | Description |
|-------|-------------|
| `shell-primitives` | Foundational types: Keccak-256, BLAKE3, H256, Address, U256, Bytes |
| `shell-crypto` | CRYSTALS-Dilithium & SPHINCS+ signing, multi-algorithm Signer/Verifier traits |
| `shell-core` | Block, Transaction (AA-native), Account, Receipt, EIP-1559 gas model |
| `shell-storage` | RocksDB backend, Merkle Patricia Trie, RLP serialization, state pruning |
| `shell-consensus` | Epoch-based PoA engine, finality tracker, fork choice rule, dynamic validator set |
| `shell-evm` | revm integration (Cancun spec), PQ precompiles, EIP-2930/4844, system contracts |
| `shell-mempool` | Transaction pool with PQ validation, fee-priority ordering, Replace-by-Fee |
| `shell-network` | libp2p P2P: GossipSub, Kademlia DHT, NAT traversal, peer scoring, tx gossip |
| `shell-rpc` | JSON-RPC (HTTP + WebSocket), CORS, rate limiting, filters, subscriptions, debug/trace APIs |
| `shell-node` | Async node harness, block production, chain sync, health endpoint, Prometheus metrics |
| `shell-cli` | CLI binary: `run`, `init`, `key`, `tx`, `account`, TOML config, structured logging |
| `shell-genesis` | Genesis block initialization from config |
| `shell-keystore` | PQ keystore with argon2id + XChaCha20-Poly1305 encryption |

### Project Structure

```
shell-chain/
├── Cargo.toml           # Workspace root
├── crates/
│   ├── cli/             # CLI binary and TOML config
│   ├── consensus/       # PoA consensus engine
│   ├── core/            # Block, Transaction, Account
│   ├── crypto/          # Post-quantum cryptography
│   ├── evm/             # EVM executor and precompiles
│   ├── genesis/         # Genesis configuration
│   ├── keystore/        # Encrypted key storage
│   ├── mempool/         # Transaction pool
│   ├── network/         # P2P networking
│   ├── node/            # Node harness
│   ├── primitives/      # Foundational types
│   ├── rpc/             # JSON-RPC server
│   └── storage/         # RocksDB storage
├── tests/e2e/           # End-to-end tests
├── docs/                # Documentation
├── CHANGELOG.md         # Release history
├── LICENSE              # MIT
└── README.md            # This file
```

## Post-Quantum Cryptography

| Algorithm | Type | Use Case | Security Level |
|-----------|------|----------|----------------|
| **CRYSTALS-Dilithium** (ML-DSA) | Lattice-based | Transaction signing (default) | NIST Level 3 |
| **SPHINCS+** (SLH-DSA) | Hash-based | High-security accounts (optional) | NIST Level 3 |
| **CRYSTALS-Kyber** (ML-KEM) | Lattice-based | P2P transport encryption | NIST Level 3 |
| **STARKs** | Hash-based proofs | Signature aggregation, light clients | Quantum-safe |

Addresses are derived as `blake3(version || algo_id || pq_public_key)[0..20]`
and encoded externally as Bech32m `pq1...`.

For details, see [docs/PQ_CRYPTO_GUIDE.md](docs/PQ_CRYPTO_GUIDE.md).

## Documentation

- [Quick Start Guide](docs/QUICKSTART.md) — run your first node in minutes
- [Operator Guide](docs/TESTNET_OPERATOR_GUIDE.md) — production deployment with Docker and monitoring
- [API Reference](docs/JSON_RPC_API.md) — complete JSON-RPC API documentation
- [PQ Crypto Guide](docs/PQ_CRYPTO_GUIDE.md) — post-quantum cryptography details
- [Native Account Abstraction Guide](docs/ACCOUNT_ABSTRACTION_GUIDE.md) — `pq1...` addresses, validation layers, and AA rollout
- [Changelog](CHANGELOG.md) — full release history

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

[MIT](LICENSE) © ShellDAO
