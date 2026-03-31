# shell-chain

A **post-quantum secure blockchain** built natively with quantum-resistant cryptography. No migration needed — quantum safety from day one.

## Overview

Shell-Chain follows [Vitalik Buterin's vision](https://ethresear.ch/t/how-to-hard-fork-to-save-most-users-funds-in-a-quantum-emergency/18901) for Ethereum's quantum upgrade, but skips the migration path entirely by building a new chain with PQ cryptography as the foundation.

### Key Features

- 🔐 **Post-Quantum Signatures** — CRYSTALS-Dilithium (NIST standard) as default, SPHINCS+ as conservative fallback
- 🏗️ **Native Account Abstraction** — every account can upgrade its signature scheme without a hard fork
- ⚙️ **EVM Compatible** — run existing Solidity contracts; use familiar tooling (Hardhat, ethers.js, MetaMask)
- 🧩 **PQ Precompiles** — on-chain Dilithium/SPHINCS+ verification, Kyber decapsulation, STARK proof verification
- 🔗 **Pluggable Consensus** — PoA for devnet/testnet, upgradable to BFT

### Architecture

```
┌─────────────────────────────────────────────┐
│                 shell-node                  │
│          (Node Builder / Harness)           │
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

| Crate | Description | Status |
|-------|-------------|--------|
| `shell-primitives` | Foundational types, Keccak-256, BLAKE3 | ✅ Done |
| `shell-crypto` | Dilithium/SPHINCS+ signing, Signer/Verifier traits | ✅ Done |
| `shell-core` | Block, Transaction (AA-native), Account, Receipt | ✅ Done |
| `shell-storage` | RocksDB + Merkle Patricia Trie | 🔜 Planned |
| `shell-consensus` | Pluggable consensus engine (PoA first) | 🔜 Planned |
| `shell-evm` | revm integration + PQ precompiles | 🔜 Planned |
| `shell-mempool` | Transaction pool | 🔜 Planned |
| `shell-network` | libp2p P2P networking | 🔜 Planned |
| `shell-rpc` | JSON-RPC server (eth_* compatible) | 🔜 Planned |
| `shell-node` | Node binary + CLI | 🔜 Planned |

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) 1.75+ (2021 edition)
- C compiler (for pqcrypto native bindings)

### Build

```bash
cargo build
```

### Test

```bash
cargo test
```

### Project Structure

```
shell-chain/
├── Cargo.toml           # Workspace root
├── crates/
│   ├── primitives/      # shell-primitives
│   ├── crypto/          # shell-crypto
│   ├── core/            # shell-core
│   └── ...              # More crates coming
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

Addresses are derived as `keccak256(pq_public_key)[12..]` — same 20-byte format as Ethereum, but from PQ public keys.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

[MIT](LICENSE) © ShellDAO

