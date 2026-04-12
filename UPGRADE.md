# Upgrade Guide — v0.9 → v0.13.0 (M10: Mainnet Readiness)

This guide covers breaking changes and migration steps when upgrading a
`shell-chain` node from any v0.9.x release to v0.13.0.

---

## Table of Contents

1. [Overview](#overview)
2. [New CLI Flags](#new-cli-flags)
3. [Configuration File Changes](#configuration-file-changes)
4. [wPoA Migration](#wpoa-migration)
5. [RPC Changes](#rpc-changes)
6. [Metrics Changes](#metrics-changes)
7. [Data Directory](#data-directory)
8. [Docker / Docker Compose](#docker--docker-compose)
9. [SDK Changes](#sdk-changes)
10. [Rollback](#rollback)

---

## Overview

v0.13.0 is a significant mainnet-readiness release. The highlights are:

| Area | Change |
|------|--------|
| Consensus | PoA → wPoA (Weighted PoA) |
| Security | TLS support, per-IP rate limiting, API key auth |
| Performance | LRU account cache, mempool priority tuning |
| Observability | Structured JSON logging, extended Prometheus metrics, Admin RPC |
| SDK | `shell-sdk` TypeScript SDK with PQ signer |

---

## New CLI Flags

### `shell-node run`

| Flag | Default | Description |
|------|---------|-------------|
| `--rpc-tls-cert <path>` | — | TLS certificate file (PEM) |
| `--rpc-tls-key <path>` | — | TLS private key file (PEM) |
| `--rpc-rate-limit <n>` | 100 | Max requests/second (server-wide) |
| `--rpc-api-key <key>` | — | Bearer token required for all methods |
| `--log-format <json\|text>` | text | Structured logging format |
| `--state-cache-size-mb <n>` | 64 | LRU account cache size |
| `--mempool-max-size <n>` | 4096 | Max transactions in mempool |
| `--mempool-price-bump <pct>` | 10 | Minimum fee bump % for tx replacement |

### `shell-node validator`

```
shell-node validator register --stake <amount>
shell-node validator status [--address <addr>]
shell-node validator exit
```

### `shell-node backup`

```
shell-node backup create <path>
shell-node backup restore <path>
shell-node backup schedule --interval 6h --keep 7
```

### `shell-node wallet`

```
shell-node wallet create
shell-node wallet balance <addr>
shell-node wallet send <to> <amount>
shell-node wallet export
```

---

## Configuration File Changes

If you use a `config.toml` file, add the following new fields under `[node]`:

```toml
[node]
state_cache_size_mb = 64   # LRU account cache (new in v0.13.0)

[mempool]
max_pool_size = 4096        # previously hardcoded to 4096
replacement_fee_bump_pct = 10  # previously hardcoded to 10%

[consensus]
engine = "wpoa"             # upgraded from "poa"
```

---

## wPoA Migration

### Genesis changes

If you are starting a fresh network, add `[validators]` to your genesis config:

```toml
[[genesis.validators]]
address = "pq1..."
weight  = 100
stake   = "1000000000000000000"  # 1 token in wei
```

### Existing PoA networks

Existing PoA networks with a single validator are automatically migrated:
the existing validator is registered with `weight = 1` and `stake = 0`.

To update validator weights after genesis:
```
shell-node validator register --stake <amount>
```

### Slashing configuration

Add to `config.toml` to override defaults:

```toml
[consensus.slashing]
slash_fraction_double_sign = 10    # percent (default: 10%)
slash_fraction_offline     = 1     # percent (default: 1%)
offline_window_blocks      = 50    # blocks (default: 50)
```

---

## RPC Changes

### New methods

| Method | Description |
|--------|-------------|
| `shell_getValidatorSet` | Returns current active validator set with weights |
| `shell_getValidatorInfo(addr)` | Returns single validator state + stake |
| `shell_submitSlashEvidence(evidence)` | Submit double-sign proof |
| `admin_nodeInfo` | Node info (requires `--admin-api`) |
| `admin_peers` | Peer list (requires `--admin-api`) |
| `admin_addPeer(enode)` | Add peer dynamically (requires `--admin-api`) |
| `admin_removePeer(enode)` | Remove peer (requires `--admin-api`) |

### Rate limiting

If `--rpc-rate-limit` is set, clients exceeding the limit receive:
```json
{"jsonrpc":"2.0","error":{"code":-32005,"message":"rate limited"},"id":1}
```

Configure your client to back off on `-32005` errors with exponential retry.

### TLS

For production deployments, we recommend terminating TLS at a reverse proxy
(Caddy or Nginx) rather than enabling the built-in TLS. Example Caddyfile:

```caddy
rpc.example.com {
    reverse_proxy localhost:8545
}
```

To use the built-in TLS directly (e.g., for operator tools):
```bash
shell-node run --rpc-tls-cert /etc/ssl/node.crt --rpc-tls-key /etc/ssl/node.key
```

---

## Metrics Changes

New Prometheus metrics added in v0.13.0:

| Metric | Type | Description |
|--------|------|-------------|
| `shell_aa_tx_total` | Counter | AA transactions (by `validation_type` label) |
| `shell_key_rotation_total` | Counter | PQ key rotations |
| `shell_validator_weight{address}` | Gauge | Current validator weight |
| `shell_consensus_slot_miss` | Counter | Empty slots (missed proposer) |
| `shell_evm_gas_used_total` | Counter | Cumulative gas used |
| `shell_snapshot_size_bytes` | Gauge | Latest backup snapshot size |

Update your Grafana dashboards by importing the updated JSON from `docker/grafana/`.

---

## Data Directory

The data directory layout is unchanged. No migration steps are required.

RocksDB column families added: `validator_registry`, `slash_records`.
These are created automatically on first start.

---

## Docker / Docker Compose

The default image is `ghcr.io/lucienSong/shell-chain:v0.13.0`.

Multi-arch images are available for `linux/amd64` and `linux/arm64`:

```yaml
services:
  node1:
    image: ghcr.io/lucienSong/shell-chain:v0.13.0
    platform: linux/amd64   # or linux/arm64
```

---

## SDK Changes

The `shell-sdk` npm package is published separately from the node binary.

```bash
npm install @shellchain/sdk@0.13.0
```

### Breaking changes in `shell-sdk`

- `PQAddress.encode()` now returns `pq1...` bech32m by default (was hex in pre-release)
- `ShellProvider` constructor now requires `{ transport }` option object

### Migration example

```typescript
// Before (pre-M10)
const provider = new ShellProvider("http://localhost:8545");

// After (v0.13.0)
import { ShellProvider, httpTransport } from "@shellchain/sdk";
const provider = new ShellProvider({ transport: httpTransport("http://localhost:8545") });
```

---

## Rollback

To roll back to a v0.9.x node:

1. Stop the v0.13.0 node
2. The RocksDB data is forward-compatible; v0.9.x can read blocks written by v0.13.0
3. **Exception**: if wPoA was activated (any validator registration occurred), v0.9.x
   cannot read the `validator_registry` column family — a fresh sync from genesis is required
4. Restart the v0.9.x binary with the old config

For network-wide rollbacks, coordinate a governance vote to freeze the wPoA
activation epoch before downgrading.
