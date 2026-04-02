#!/bin/bash
# Shell-chain 3-node E2E test
# Usage: ./tests/e2e/run-e2e.sh
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_DIR"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}✓ $1${NC}"; }
fail() { echo -e "${RED}✗ $1${NC}"; FAILURES=$((FAILURES + 1)); }
info() { echo -e "${YELLOW}→ $1${NC}"; }

FAILURES=0

rpc() {
    local port=$1
    local method=$2
    local params=${3:-[]}
    curl -sf "http://127.0.0.1:${port}" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
        2>/dev/null | jq -r '.result // .error'
}

cleanup() {
    info "Tearing down containers..."
    docker compose down -v --remove-orphans 2>/dev/null || true
}
trap cleanup EXIT

# ─── Build & Start ────────────────────────────────────────────
echo ""
echo "╔══════════════════════════════════════════╗"
echo "║   Shell-chain 3-Node E2E Test Suite      ║"
echo "╚══════════════════════════════════════════╝"
echo ""

info "Building Docker image..."
docker compose build --quiet

info "Starting 3-node testnet..."
docker compose up -d

# Wait for node1 to be healthy (producing blocks).
info "Waiting for node1 to produce blocks..."
for i in $(seq 1 60); do
    BLOCK=$(rpc 8545 eth_blockNumber 2>/dev/null || echo "0x0")
    if [ "$BLOCK" != "0x0" ] && [ "$BLOCK" != "" ] && [ "$BLOCK" != "null" ]; then
        break
    fi
    sleep 2
done

# ─── Test 1: Node1 is producing blocks ───────────────────────
BLOCK_HEX=$(rpc 8545 eth_blockNumber)
BLOCK_NUM=$((16#${BLOCK_HEX#0x}))
if [ "$BLOCK_NUM" -gt 0 ]; then
    pass "Node1 producing blocks (current: #${BLOCK_NUM})"
else
    fail "Node1 not producing blocks (block: ${BLOCK_HEX})"
fi

# ─── Test 2: Chain ID matches across all nodes ───────────────
CHAIN1=$(rpc 8545 eth_chainId)
CHAIN2=$(rpc 8546 eth_chainId)
CHAIN3=$(rpc 8547 eth_chainId)

if [ "$CHAIN1" = "$CHAIN2" ] && [ "$CHAIN2" = "$CHAIN3" ] && [ "$CHAIN1" = "0x539" ]; then
    pass "Chain ID consistent across all nodes (${CHAIN1})"
else
    fail "Chain ID mismatch: node1=${CHAIN1} node2=${CHAIN2} node3=${CHAIN3}"
fi

# ─── Test 3: Node2 and Node3 are syncing blocks ─────────────
info "Polling for block sync..."

N2_DEC=0
for i in $(seq 1 30); do
    N2=$(rpc 8546 eth_blockNumber 2>/dev/null || echo "0x0")
    N2_DEC=$((16#${N2#0x}))
    if [ "$N2_DEC" -gt 0 ]; then break; fi
    sleep 2
done

N3_DEC=0
for i in $(seq 1 30); do
    N3=$(rpc 8547 eth_blockNumber 2>/dev/null || echo "0x0")
    N3_DEC=$((16#${N3#0x}))
    if [ "$N3_DEC" -gt 0 ]; then break; fi
    sleep 2
done

N1=$(rpc 8545 eth_blockNumber)
N1_DEC=$((16#${N1#0x}))

if [ "$N2_DEC" -gt 0 ]; then
    pass "Node2 synced (block #${N2_DEC})"
else
    fail "Node2 not syncing (block: ${N2})"
fi

if [ "$N3_DEC" -gt 0 ]; then
    pass "Node3 synced (block #${N3_DEC})"
else
    fail "Node3 not syncing (block: ${N3})"
fi

# ─── Test 4: RPC eth_getBlockByNumber on all nodes ──────────
BLOCK1=$(rpc 8545 eth_getBlockByNumber '["0x1", false]')
BLOCK2=$(rpc 8546 eth_getBlockByNumber '["0x1", false]')
BLOCK3=$(rpc 8547 eth_getBlockByNumber '["0x1", false]')

HASH1=$(echo "$BLOCK1" | jq -r '.hash // empty' 2>/dev/null)
HASH2=$(echo "$BLOCK2" | jq -r '.hash // empty' 2>/dev/null)
HASH3=$(echo "$BLOCK3" | jq -r '.hash // empty' 2>/dev/null)

if [ -n "$HASH1" ] && [ "$HASH1" = "$HASH2" ] && [ "$HASH2" = "$HASH3" ]; then
    pass "Block #1 hash consistent across all nodes"
else
    fail "Block #1 hash mismatch: ${HASH1} / ${HASH2} / ${HASH3}"
fi

# ─── Test 5: eth_gasPrice works ──────────────────────────────
GAS=$(rpc 8545 eth_gasPrice)
if [ -n "$GAS" ] && [ "$GAS" != "null" ]; then
    pass "eth_gasPrice returns ${GAS}"
else
    fail "eth_gasPrice failed"
fi

# ─── Test 6: eth_getBalance works ────────────────────────────
# Genesis allocates to the authority address; get it from block 1
PROPOSER=$(echo "$BLOCK1" | jq -r '.miner // .proposer // empty' 2>/dev/null)
if [ -n "$PROPOSER" ]; then
    BAL=$(rpc 8545 eth_getBalance "[\"${PROPOSER}\"]")
    if [ -n "$BAL" ] && [ "$BAL" != "0x0" ] && [ "$BAL" != "null" ]; then
        pass "eth_getBalance for proposer returns ${BAL}"
    else
        fail "eth_getBalance returned zero or error: ${BAL}"
    fi
else
    info "Skipping balance test (could not determine proposer)"
fi

# ─── Results ─────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════"
if [ "$FAILURES" -eq 0 ]; then
    echo -e "${GREEN}All tests passed!${NC}"
    exit 0
else
    echo -e "${RED}${FAILURES} test(s) failed${NC}"
    exit 1
fi
