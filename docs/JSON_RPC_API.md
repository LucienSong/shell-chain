# Shell-Chain JSON-RPC API Reference

Complete reference for the shell-chain JSON-RPC API. All methods follow the [JSON-RPC 2.0](https://www.jsonrpc.org/specification) specification.

> **See also:** [Quickstart Guide](QUICKSTART.md) · [Testnet Operator Guide](TESTNET_OPERATOR_GUIDE.md)

---

## Table of Contents

- [Connection](#connection)
- [CORS Configuration](#cors-configuration)
- [API Namespace Whitelist](#api-namespace-whitelist)
- [Rate Limiting](#rate-limiting)
- [WebSocket Support](#websocket-support)
- [eth\_ Namespace](#eth_-namespace)
- [net\_ Namespace](#net_-namespace)
- [web3\_ Namespace](#web3_-namespace)
- [shell\_ Namespace](#shell_-namespace)
- [debug\_ Namespace](#debug_-namespace)
- [trace\_ Namespace](#trace_-namespace)

---

## Connection

**HTTP endpoint** (default):
```
http://127.0.0.1:8545
```

**WebSocket endpoint** (when `--ws` is enabled):
```
ws://127.0.0.1:8546
```

All requests use POST with `Content-Type: application/json`.

## CORS Configuration

Control cross-origin access with `--rpc-cors`:

```bash
# Allow all origins
shell-node run --rpc-cors "*"

# Allow specific origins (comma-separated)
shell-node run --rpc-cors "http://localhost:3000,https://app.example.com"
```

Or in TOML config:
```toml
[rpc]
cors_origins = ["http://localhost:3000", "https://app.example.com"]
```

## API Namespace Whitelist

Control which API namespaces are exposed with `--rpc-api`:

```bash
# Validators (minimal surface)
shell-node run --rpc-api eth,net,web3,shell

# RPC nodes (full API)
shell-node run --rpc-api eth,net,web3,shell,debug,trace
```

Available namespaces: `eth`, `net`, `web3`, `shell`, `debug`, `trace`

If `--rpc-api` is not specified, all namespaces registered by the node are available.

## Rate Limiting

Per-connection rate limiting is available via `--rpc-rate-limit`:

```bash
shell-node run --rpc-rate-limit 100  # 100 requests/sec per connection
```

Or in TOML config:
```toml
[rpc]
rate_limit = 100
```

When the limit is exceeded, the server returns an HTTP 429 response.

## WebSocket Support

Enable the WebSocket server with `--ws`:

```bash
shell-node run --ws --ws-port 8546
```

The WebSocket endpoint supports all the same JSON-RPC methods as HTTP, plus filter-based subscriptions via `eth_newFilter`, `eth_newBlockFilter`, and `eth_getFilterChanges`.

---

## eth_ Namespace

Standard Ethereum-compatible JSON-RPC methods.

### eth_blockNumber

Returns the current block height.

**Parameters:** None

**Returns:** `String` — Hex-encoded block number.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x1a"}
```

---

### eth_chainId

Returns the chain ID.

**Parameters:** None

**Returns:** `String` — Hex-encoded chain ID.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x539"}
```

---

### eth_syncing

Returns sync status. Shell-chain has no sync protocol — always returns `false`.

**Parameters:** None

**Returns:** `Boolean` — Always `false`.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_syncing","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":false}
```

---

### eth_mining

Returns whether the node is producing blocks.

**Parameters:** None

**Returns:** `Boolean` — `true` if the node has a validator keystore loaded.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_mining","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":true}
```

---

### eth_hashrate

Returns mining hashrate. Always `"0x0"` (PoA consensus, no mining).

**Parameters:** None

**Returns:** `String` — Always `"0x0"`.

---

### eth_accounts

Returns managed accounts. Always empty — the node does not hold user private keys.

**Parameters:** None

**Returns:** `Array` — Always `[]`.

---

### eth_protocolVersion

Returns the protocol version.

**Parameters:** None

**Returns:** `String` — `"0x45"` (Cancun-compatible).

---

### eth_gasPrice

Returns the current base fee.

**Parameters:** None

**Returns:** `String` — Hex-encoded gas price in wei.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_gasPrice","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x3b9aca00"}
```

---

### eth_maxPriorityFeePerGas

Returns the suggested priority fee. Always `"0x0"` on this PoA chain.

**Parameters:** None

**Returns:** `String` — `"0x0"`.

---

### eth_feeHistory

Returns historical base fee and gas usage data.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Block count (hex) — max 1024 |
| 2 | `String` | Yes | Newest block (block tag or hex number) |
| 3 | `Array<Number>` | No | Reward percentiles |

**Returns:** Object with `oldestBlock`, `baseFeePerGas`, `gasUsedRatio`, `reward`.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_feeHistory","params":["0x5","latest",[]],"id":1}'
```

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "oldestBlock": "0x15",
    "baseFeePerGas": ["0x3b9aca00","0x3b9aca00","0x3b9aca00","0x3b9aca00","0x3b9aca00","0x3b9aca00"],
    "gasUsedRatio": [0.0, 0.0, 0.0, 0.0, 0.0],
    "reward": []
  }
}
```

---

### eth_getBalance

Returns the balance of an address.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address (0x-prefixed) |
| 2 | `String` | No | Block tag (`"latest"`, `"earliest"`, `"pending"`, `"safe"`, `"finalized"`, or hex number) |

**Returns:** `String` — Hex-encoded balance in wei.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18","latest"],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x3635c9adc5dea00000"}
```

---

### eth_getTransactionCount

Returns the nonce (transaction count) for an address.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address (0x-prefixed) |
| 2 | `String` | No | Block tag |

**Returns:** `String` — Hex-encoded nonce.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getTransactionCount","params":["0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18","latest"],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x0"}
```

---

### eth_getBlockByNumber

Returns a block by number.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Block tag or hex number |
| 2 | `Boolean` | Yes | `true` for full transaction objects, `false` for hashes only |

Supported block tags: `"latest"`, `"earliest"`, `"pending"`, `"safe"`, `"finalized"`.

When `"pending"` is requested, a pseudo-block is constructed from the current mempool.

**Returns:** `Object|null` — Block object or `null` if not found.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["latest",false],"id":1}'
```

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "number": "0x1a",
    "hash": "0xabc...",
    "parentHash": "0xdef...",
    "timestamp": "0x65a5f200",
    "gasLimit": "0x1c9c380",
    "gasUsed": "0x0",
    "transactions": []
  }
}
```

---

### eth_getBlockByHash

Returns a block by hash.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Block hash (0x-prefixed, 32 bytes) |
| 2 | `Boolean` | Yes | `true` for full tx objects, `false` for hashes |

**Returns:** `Object|null` — Block object or `null`.

---

### eth_getTransactionByHash

Returns a transaction by hash. Checks the mempool first, then on-chain storage.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Transaction hash (0x-prefixed, 32 bytes) |

**Returns:** `Object|null` — Transaction object or `null`.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getTransactionByHash","params":["0xabc123..."],"id":1}'
```

---

### eth_getTransactionReceipt

Returns the receipt of a mined transaction.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Transaction hash |

**Returns:** `Object|null` — Receipt object (includes `status`, `gasUsed`, `logs`, `blockNumber`, etc.) or `null` if not yet mined.

---

### eth_sendRawTransaction

Submits a signed transaction. Accepts RLP-encoded or JSON-encoded transaction bytes.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Hex-encoded signed transaction data |

**Returns:** `String` — Transaction hash.

**Validation:** The transaction's `max_fee_per_gas` must be ≥ the current base fee.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["0x...signed_tx_bytes..."],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0xabc123..."}
```

---

### eth_call

Executes a read-only call against the EVM (no state changes).

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `Object` | Yes | Call request: `{from?, to, data?, value?, gas?}` |
| 2 | `String` | No | Block tag |

**Returns:** `String` — Hex-encoded return data.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_call","params":[{"to":"0xContractAddr","data":"0x..."},"latest"],"id":1}'
```

---

### eth_estimateGas

Estimates gas for a transaction. Returns `gas_used × 1.2` with a minimum of 21,000.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `Object` | Yes | Call request: `{from?, to?, data?, value?}` |

**Returns:** `String` — Hex-encoded gas estimate.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_estimateGas","params":[{"to":"0x...","value":"0xde0b6b3a7640000"}],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x5208"}
```

---

### eth_createAccessList

Creates an EIP-2930 access list for a transaction.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `Object` | Yes | Call request |
| 2 | `String` | No | Block tag |

**Returns:** Object with `accessList` and `gasUsed`.

---

### eth_getCode

Returns the bytecode at an address.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address |
| 2 | `String` | No | Block tag |

**Returns:** `String` — Hex-encoded bytecode, or `"0x"` for EOAs.

---

### eth_getStorageAt

Returns a storage slot value.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address |
| 2 | `String` | Yes | Storage position (hex-encoded, 32-byte key) |
| 3 | `String` | No | Block tag |

**Returns:** `String` — Zero-padded 32-byte hex value.

---

### eth_getLogs

Returns logs matching a filter.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `Object` | Yes | Filter: `{address?, topics?, fromBlock?, toBlock?}` |

**Returns:** `Array` — Log objects with `address`, `topics`, `data`, `blockNumber`, `blockHash`, `transactionHash`, `transactionIndex`, `logIndex`, `removed`.

Uses bloom filters for fast block-level filtering. The block range is capped at `MAX_BLOCK_RANGE` to prevent DoS.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getLogs","params":[{"fromBlock":"0x0","toBlock":"latest"}],"id":1}'
```

---

### eth_newFilter

Creates a poll-based log filter.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `Object` | Yes | Filter: `{address?, topics?, fromBlock?, toBlock?}` |

**Returns:** `String` — Filter ID.

---

### eth_newBlockFilter

Creates a poll-based block filter.

**Parameters:** None

**Returns:** `String` — Filter ID.

---

### eth_getFilterChanges

Returns changes since last poll for a filter.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Filter ID |

**Returns:** `Array` — Logs (for log filters) or block hashes (for block filters).

---

### eth_getFilterLogs

Re-queries all logs matching a filter's criteria.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Filter ID |

**Returns:** `Array` — Log objects.

---

### eth_uninstallFilter

Removes a filter.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Filter ID |

**Returns:** `Boolean` — `true` if the filter was removed.

---

### eth_blobBaseFee

Returns the current blob gas price (EIP-4844).

**Parameters:** None

**Returns:** `String` — Hex-encoded blob gas price, calculated from `excess_blob_gas`.

---

### eth_sign *(not supported)*

Returns error code `-32601`. The node does not hold private keys — sign transactions client-side with the CLI or SDK.

### eth_signTransaction *(not supported)*

Returns error code `-32601`. Same reason as `eth_sign`.

### eth_getCompilers *(deprecated)*

Returns `[]`. This method is deprecated.

---

## net_ Namespace

### net_version

Returns the network/chain ID as a decimal string.

**Parameters:** None

**Returns:** `String` — Chain ID (e.g., `"1337"`).

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"net_version","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"1337"}
```

---

### net_listening

Returns whether the node is accepting connections. Always `true`.

**Parameters:** None

**Returns:** `Boolean` — `true`.

---

### net_peerCount

Returns the number of connected peers.

**Parameters:** None

**Returns:** `String` — Hex-encoded peer count (e.g., `"0x3"`).

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"net_peerCount","params":[],"id":1}'
```

---

## web3_ Namespace

### web3_clientVersion

Returns the client identifier string.

**Parameters:** None

**Returns:** `String` — `"shell-chain/0.5.0"`.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"web3_clientVersion","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"shell-chain/0.5.0"}
```

---

### web3_sha3

Returns the Keccak-256 hash of the given data.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Hex-encoded data (max 32 KB) |

**Returns:** `String` — Hex-encoded Keccak-256 hash.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"web3_sha3","params":["0x68656c6c6f"],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8"}
```

---

## shell_ Namespace

Shell-chain custom extensions for post-quantum features, validator governance, and node information.

### shell_getPqPubkey

Returns the post-quantum public key associated with an address.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address (0x-prefixed) |

**Returns:** `String|null` — Hex-encoded PQ public key, or `null` if not found.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"shell_getPqPubkey","params":["0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18"],"id":1}'
```

---

### shell_pendingCount

Returns the number of pending transactions in the mempool.

**Parameters:** None

**Returns:** `String` — Hex-encoded count.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"shell_pendingCount","params":[],"id":1}'
```

```json
{"jsonrpc":"2.0","id":1,"result":"0x0"}
```

---

### shell_sendTransaction

Submits a pre-signed shell-chain transaction.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `Object` | Yes | Signed transaction object |

**Returns:** `String` — Transaction hash.

---

### shell_getValidators

Returns the current validator set.

**Parameters:** None

**Returns:** `Array<String>` — List of validator addresses.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"shell_getValidators","params":[],"id":1}'
```

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": [
    "0x0000000000000000000000000000000000000001",
    "0x0000000000000000000000000000000000000002"
  ]
}
```

---

### shell_getValidatorStatus

Returns whether an address is a validator.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address |

**Returns:** Object:
```json
{
  "address": "0x...",
  "isValidator": true
}
```

---

### shell_proposeAddValidator

Submits a governance transaction to add a validator (requires the node to have a proposer keystore).

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address to add |

**Returns:** `String` — Transaction hash.

---

### shell_proposeRemoveValidator

Submits a governance transaction to remove a validator.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address to remove |

**Returns:** `String` — Transaction hash.

---

### shell_encodeAddValidator

Generates calldata for the validator management system contract (does not submit a transaction).

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address to add |

**Returns:** `String` — Hex-encoded calldata.

---

### shell_encodeRemoveValidator

Generates calldata for validator removal.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Address to remove |

**Returns:** `String` — Hex-encoded calldata.

---

### shell_estimateGovernanceGas

Estimates gas for a governance operation.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Operation: `"addValidator"`, `"removeValidator"`, `"getValidators"`, or `"isValidator"` |

**Returns:** `String` — Hex-encoded gas estimate.

---

### shell_getGovernanceInfo

Returns governance configuration.

**Parameters:** None

**Returns:**
```json
{
  "validatorCount": 3,
  "validators": ["0x...", "0x...", "0x..."],
  "systemContractAddress": "0x...",
  "proposalGasLimit": 100000
}
```

---

### shell_getNodeInfo

Returns node status information.

**Parameters:** None

**Returns:**
```json
{
  "version": "ShellChain/v0.1.0/rust",
  "chainId": 1337,
  "blockHeight": 42,
  "peerCount": 0,
  "txPoolSize": 5,
  "isMining": true,
  "uptime": 3600,
  "baseFee": "0x3b9aca00"
}
```

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"shell_getNodeInfo","params":[],"id":1}'
```

---

### shell_getNetworkStats

Returns P2P network statistics.

**Parameters:** None

**Returns:**
```json
{
  "peerCount": 0,
  "protocolVersion": "shell/1.0.0",
  "listeningAddress": "/ip4/0.0.0.0/tcp/30303",
  "protocols": ["gossipsub", "kademlia", "mdns"]
}
```

---

### shell_getChainStats

Returns aggregate chain statistics (scans the last 1,000 blocks).

**Parameters:** None

**Returns:**
```json
{
  "blockHeight": 1500,
  "totalTransactions": 3200,
  "avgBlockTime": 2.01,
  "gasUsedTotal": "0x...",
  "latestBaseFee": "0x3b9aca00"
}
```

---

### shell_getFinalityInfo

Returns finality status.

**Parameters:** None

**Returns:**
```json
{
  "lastFinalizedBlock": "0x18",
  "currentHead": "0x1a",
  "pendingAttestations": 2
}
```

---

### shell_addValidator / shell_removeValidator *(disabled)*

These methods return error `-32601`. Direct validator set mutation is disabled to prevent split-brain issues. Use `shell_proposeAddValidator` and `shell_proposeRemoveValidator` instead, which go through the governance transaction flow.

---

## debug_ Namespace

> **Note:** The `debug` namespace must be explicitly enabled via `--rpc-api eth,net,web3,shell,debug`. It is not enabled by default on validator nodes.

### debug_traceTransaction

Replays a transaction and returns an execution trace.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Transaction hash |
| 2 | `Object` | No | Trace options |

**Returns:**
```json
{
  "frame": {
    "type": "CALL",
    "from": "0x...",
    "to": "0x...",
    "gas": 21000,
    "gasUsed": 21000,
    "input": "0x",
    "output": "0x",
    "value": "0xde0b6b3a7640000",
    "error": null
  },
  "failed": false
}
```

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"debug_traceTransaction","params":["0xabc123..."],"id":1}'
```

---

### debug_traceBlockByNumber

Traces all transactions in a block.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Block number (hex) or block tag |
| 2 | `Object` | No | Trace options |

**Returns:** `Array` — Array of trace result objects (one per transaction).

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"debug_traceBlockByNumber","params":["0x1a"],"id":1}'
```

---

## trace_ Namespace

OpenEthereum-compatible trace format.

> **Note:** Like `debug`, the `trace` namespace must be explicitly enabled via `--rpc-api`.

### trace_block

Returns traces for all transactions in a block.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Block number (hex) or block tag |

**Returns:** `Array` — OpenEthereum-format trace objects.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"trace_block","params":["latest"],"id":1}'
```

---

### trace_oeTransaction

Returns the trace for a specific transaction in OpenEthereum format.

**Parameters:**
| # | Type | Required | Description |
|---|------|----------|-------------|
| 1 | `String` | Yes | Transaction hash |

**Returns:** `Array` — Single-element array with the transaction trace.

```bash
curl -s http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"trace_oeTransaction","params":["0xabc123..."],"id":1}'
```

---

## Method Summary

| Namespace | Method | Params | Description |
|-----------|--------|--------|-------------|
| eth_ | `eth_blockNumber` | — | Current block height |
| eth_ | `eth_chainId` | — | Chain ID |
| eth_ | `eth_syncing` | — | Sync status (always false) |
| eth_ | `eth_mining` | — | Is validator active |
| eth_ | `eth_hashrate` | — | Always 0x0 |
| eth_ | `eth_accounts` | — | Always [] |
| eth_ | `eth_protocolVersion` | — | Protocol version |
| eth_ | `eth_gasPrice` | — | Current base fee |
| eth_ | `eth_maxPriorityFeePerGas` | — | Always 0x0 |
| eth_ | `eth_feeHistory` | count, block, percentiles | Fee history |
| eth_ | `eth_getBalance` | addr, block? | Account balance |
| eth_ | `eth_getTransactionCount` | addr, block? | Account nonce |
| eth_ | `eth_getBlockByNumber` | num, full? | Block by number |
| eth_ | `eth_getBlockByHash` | hash, full? | Block by hash |
| eth_ | `eth_getTransactionByHash` | hash | Transaction details |
| eth_ | `eth_getTransactionReceipt` | hash | Transaction receipt |
| eth_ | `eth_sendRawTransaction` | data | Submit signed tx |
| eth_ | `eth_call` | tx, block? | Read-only EVM call |
| eth_ | `eth_estimateGas` | tx | Gas estimation |
| eth_ | `eth_createAccessList` | tx, block? | Access list |
| eth_ | `eth_getCode` | addr, block? | Contract bytecode |
| eth_ | `eth_getStorageAt` | addr, pos, block? | Storage slot |
| eth_ | `eth_getLogs` | filter | Event logs |
| eth_ | `eth_newFilter` | filter | Create log filter |
| eth_ | `eth_newBlockFilter` | — | Create block filter |
| eth_ | `eth_getFilterChanges` | id | Poll filter |
| eth_ | `eth_getFilterLogs` | id | Re-query filter |
| eth_ | `eth_uninstallFilter` | id | Remove filter |
| eth_ | `eth_blobBaseFee` | — | Blob gas price |
| net_ | `net_version` | — | Chain ID (string) |
| net_ | `net_listening` | — | Always true |
| net_ | `net_peerCount` | — | Peer count |
| web3_ | `web3_clientVersion` | — | Client version |
| web3_ | `web3_sha3` | data | Keccak-256 hash |
| shell_ | `shell_getPqPubkey` | addr | PQ public key |
| shell_ | `shell_pendingCount` | — | Mempool size |
| shell_ | `shell_sendTransaction` | tx | Submit PQ tx |
| shell_ | `shell_getValidators` | — | Validator list |
| shell_ | `shell_getValidatorStatus` | addr | Validator check |
| shell_ | `shell_proposeAddValidator` | addr | Governance: add |
| shell_ | `shell_proposeRemoveValidator` | addr | Governance: remove |
| shell_ | `shell_encodeAddValidator` | addr | Encode calldata |
| shell_ | `shell_encodeRemoveValidator` | addr | Encode calldata |
| shell_ | `shell_estimateGovernanceGas` | op | Gov gas estimate |
| shell_ | `shell_getGovernanceInfo` | — | Gov config |
| shell_ | `shell_getNodeInfo` | — | Node status |
| shell_ | `shell_getNetworkStats` | — | Network stats |
| shell_ | `shell_getChainStats` | — | Chain statistics |
| shell_ | `shell_getFinalityInfo` | — | Finality status |
| debug_ | `debug_traceTransaction` | hash, opts? | Tx trace |
| debug_ | `debug_traceBlockByNumber` | num, opts? | Block trace |
| trace_ | `trace_block` | num | OE block traces |
| trace_ | `trace_oeTransaction` | hash | OE tx trace |

---

*Last updated: 2025*
