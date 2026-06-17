#!/usr/bin/env bash
set -euo pipefail

: "${SHELL_NETWORK:=testnet}"
: "${SHELL_CHAIN_ID:=10}"
: "${SHELL_DATADIR:=/mnt/shell-data/data/node2}"
: "${SHELL_KEYSTORE:=/opt/shell/keys/validator.json}"
: "${SHELL_PASSWORD_FILE:=/opt/shell/secrets/v1-pw}"
: "${SHELL_RPC_ADDR:=0.0.0.0:8545}"
: "${SHELL_WS_PORT:=8546}"
: "${SHELL_METRICS_ADDR:=127.0.0.1:9090}"
: "${SHELL_RPC_API:=eth,net,web3,shell}"
: "${SHELL_RPC_CORS:=https://testnet-rpc.shell.org,https://explorer.shell.org,http://localhost,http://127.0.0.1}"
: "${SHELL_RPC_RATE_LIMIT:=50}"
: "${SHELL_BLOCK_TIME_MS:=2000}"
: "${SHELL_MAX_IDLE_INTERVAL_SECS:=600}"
: "${SHELL_STATE_CACHE_SIZE_MB:=32}"
: "${SHELL_STORAGE_PROFILE:=full}"
: "${SHELL_NODE_ROLE:=validator}"
: "${SHELL_CONSENSUS_ENGINE:=wpoa}"
: "${SHELL_ENABLE_STARK_AGGREGATION:=false}"
: "${SHELL_BOOTNODES:=}"

args=(
  run
  --db rocksdb
  --datadir "$SHELL_DATADIR"
  --keystore "$SHELL_KEYSTORE"
  --password-file "$SHELL_PASSWORD_FILE"
  --rpc-addr "$SHELL_RPC_ADDR"
  --ws
  --ws-port "$SHELL_WS_PORT"
  --rpc-api "$SHELL_RPC_API"
  --rpc-rate-limit "$SHELL_RPC_RATE_LIMIT"
  --rpc-cors "$SHELL_RPC_CORS"
  --metrics-addr "$SHELL_METRICS_ADDR"
  --network "$SHELL_NETWORK"
  --chain-id "$SHELL_CHAIN_ID"
  --block-time "$SHELL_BLOCK_TIME_MS"
  --max-idle-interval "$SHELL_MAX_IDLE_INTERVAL_SECS"
  --state-cache-size-mb "$SHELL_STATE_CACHE_SIZE_MB"
  --storage-profile "$SHELL_STORAGE_PROFILE"
  --node-role "$SHELL_NODE_ROLE"
  --consensus-engine "$SHELL_CONSENSUS_ENGINE"
)

if [[ "$SHELL_ENABLE_STARK_AGGREGATION" == "true" ]]; then
  args+=(--enable-stark-aggregation)
fi

if [[ -n "$SHELL_BOOTNODES" ]]; then
  args+=(--bootnodes "$SHELL_BOOTNODES")
fi

exec ionice -c3 nice -n 10 /usr/local/bin/shell-node "${args[@]}"
