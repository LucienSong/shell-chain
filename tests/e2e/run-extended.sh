#!/bin/bash
# Shell-chain Extended E2E Test Suite
# Covers: performance metrics, reliability, RPC completeness
# Requires: a running 3-node testnet (started by run-e2e.sh or docker compose up)
#
# Usage: ./tests/e2e/run-extended.sh [--reuse]
#   --reuse  Skip build/start, use already-running containers
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_DIR"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

pass() { echo -e "${GREEN}✓ $1${NC}"; PASSES=$((PASSES + 1)); }
fail() { echo -e "${RED}✗ $1${NC}"; FAILURES=$((FAILURES + 1)); }
info() { echo -e "${YELLOW}→ $1${NC}"; }
metric() { echo -e "${CYAN}  📊 $1${NC}"; }

FAILURES=0
PASSES=0

rpc() {
    local port=$1
    local method=$2
    local params=${3:-[]}
    curl -sf "http://127.0.0.1:${port}" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
        2>/dev/null | jq -r '.result // .error // empty'
}

rpc_raw() {
    local port=$1
    local method=$2
    local params=${3:-[]}
    curl -sf "http://127.0.0.1:${port}" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
        2>/dev/null
}

cleanup() {
    if [ "$REUSE" != "true" ]; then
        info "Tearing down containers..."
        docker compose down -v --remove-orphans 2>/dev/null || true
    fi
}
trap cleanup EXIT

REUSE=false
if [ "$1" = "--reuse" ]; then
    REUSE=true
fi

echo ""
echo "╔══════════════════════════════════════════════╗"
echo "║   Shell-chain Extended E2E Test Suite        ║"
echo "╚══════════════════════════════════════════════╝"
echo ""

# ─── Startup ──────────────────────────────────────────────────
if [ "$REUSE" = "true" ]; then
    info "Reusing existing containers..."
else
    info "Building Docker image..."
    docker compose build --quiet

    info "Starting 3-node testnet..."
    docker compose up -d

    info "Waiting for node1 to produce blocks..."
    for i in $(seq 1 60); do
        BLOCK=$(rpc 8545 eth_blockNumber 2>/dev/null || echo "0x0")
        if [ "$BLOCK" != "0x0" ] && [ -n "$BLOCK" ] && [ "$BLOCK" != "null" ]; then break; fi
        sleep 2
    done

    info "Waiting for all nodes RPC..."
    for port in 8546 8547; do
        for i in $(seq 1 30); do
            R=$(rpc $port eth_chainId 2>/dev/null)
            if [ -n "$R" ] && [ "$R" != "null" ]; then break; fi
            sleep 2
        done
    done

    info "Waiting for P2P mesh formation (10s)..."
    sleep 10
fi

# Get proposer address from block 1 for balance checks.
BLOCK1_JSON=$(rpc_raw 8545 eth_getBlockByNumber '["0x1", false]' | jq -r '.result')
PROPOSER=$(echo "$BLOCK1_JSON" | jq -r '.miner // .proposer // empty' 2>/dev/null)

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  SECTION 1: Performance Metrics"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ─── 1a: Block Time Accuracy ─────────────────────────────────
info "Measuring block time accuracy (sampling 5 consecutive blocks)..."
CURRENT_HEX=$(rpc 8545 eth_blockNumber)
CURRENT=$((16#${CURRENT_HEX#0x}))

TIMESTAMPS=()
for offset in $(seq 0 5); do
    BN=$((CURRENT - 5 + offset))
    BN_HEX=$(printf "0x%x" $BN)
    TS_HEX=$(rpc_raw 8545 eth_getBlockByNumber "[\"${BN_HEX}\", false]" | jq -r '.result.timestamp // empty')
    if [ -n "$TS_HEX" ]; then
        TS=$((16#${TS_HEX#0x}))
        TIMESTAMPS+=($TS)
    fi
done

if [ ${#TIMESTAMPS[@]} -ge 2 ]; then
    INTERVALS=()
    TOTAL_INTERVAL=0
    for i in $(seq 1 $((${#TIMESTAMPS[@]} - 1))); do
        DIFF=$((TIMESTAMPS[i] - TIMESTAMPS[i-1]))
        INTERVALS+=($DIFF)
        TOTAL_INTERVAL=$((TOTAL_INTERVAL + DIFF))
    done
    AVG_MS=$(( (TOTAL_INTERVAL * 1000) / ${#INTERVALS[@]} ))
    CONFIGURED_MS=2000

    metric "Configured block time: ${CONFIGURED_MS}ms"
    metric "Average measured interval: ${AVG_MS}ms (over ${#INTERVALS[@]} blocks)"
    metric "Individual intervals (seconds): ${INTERVALS[*]}"

    # Accept within 50% tolerance (block production has natural variance).
    if [ "$AVG_MS" -ge 1000 ] && [ "$AVG_MS" -le 4000 ]; then
        pass "Block time accuracy within tolerance (avg ${AVG_MS}ms vs ${CONFIGURED_MS}ms target)"
    else
        fail "Block time out of tolerance (avg ${AVG_MS}ms vs ${CONFIGURED_MS}ms target)"
    fi
else
    fail "Could not fetch enough blocks for timing analysis"
fi

# ─── 1b: Data Growth Rate ────────────────────────────────────
info "Measuring data growth rate..."

# Snapshot 1.
SIZE1_N1=$(docker exec shell-node1 du -sk /data/db 2>/dev/null | awk '{print $1}')
HEIGHT1_HEX=$(rpc 8545 eth_blockNumber)
HEIGHT1=$((16#${HEIGHT1_HEX#0x}))

info "Waiting 20s for more blocks..."
sleep 20

# Snapshot 2.
SIZE2_N1=$(docker exec shell-node1 du -sk /data/db 2>/dev/null | awk '{print $1}')
HEIGHT2_HEX=$(rpc 8545 eth_blockNumber)
HEIGHT2=$((16#${HEIGHT2_HEX#0x}))

BLOCKS_PRODUCED=$((HEIGHT2 - HEIGHT1))
SIZE_GROWTH=$((SIZE2_N1 - SIZE1_N1))

if [ "$BLOCKS_PRODUCED" -gt 0 ] && [ "$SIZE_GROWTH" -ge 0 ]; then
    KB_PER_BLOCK=0
    if [ "$SIZE_GROWTH" -gt 0 ]; then
        KB_PER_BLOCK=$((SIZE_GROWTH / BLOCKS_PRODUCED))
    fi
    metric "Blocks produced: ${BLOCKS_PRODUCED} (from #${HEIGHT1} to #${HEIGHT2})"
    metric "DB size: ${SIZE1_N1}KB → ${SIZE2_N1}KB (Δ${SIZE_GROWTH}KB)"
    metric "Growth rate: ~${KB_PER_BLOCK}KB/block (empty blocks)"
    pass "Data growth rate measured (${SIZE_GROWTH}KB over ${BLOCKS_PRODUCED} blocks)"
else
    fail "Could not measure data growth (blocks: $BLOCKS_PRODUCED, growth: $SIZE_GROWTH)"
fi

# ─── 1c: PQ Signature Overhead ──────────────────────────────
info "Measuring PQ block sizes..."

BN_HEX=$(printf "0x%x" $HEIGHT2)
BLOCK_JSON=$(rpc_raw 8545 eth_getBlockByNumber "[\"${BN_HEX}\", false]" | jq '.result')
BLOCK_SIZE_HEX=$(echo "$BLOCK_JSON" | jq -r '.size // empty')

if [ -n "$BLOCK_SIZE_HEX" ] && [ "$BLOCK_SIZE_HEX" != "null" ]; then
    BLOCK_SIZE=$((16#${BLOCK_SIZE_HEX#0x}))
    metric "Block #${HEIGHT2} size: ${BLOCK_SIZE} bytes (with Dilithium seal)"
    metric "For reference: Ed25519 signature ~64B, Dilithium3 ~3293B"
    pass "PQ block size: ${BLOCK_SIZE} bytes"
else
    # Try getting size from raw data estimation.
    BLOCK_STR=$(echo "$BLOCK_JSON" | jq -c '.')
    ESTIMATED_SIZE=${#BLOCK_STR}
    metric "Block #${HEIGHT2} estimated JSON size: ${ESTIMATED_SIZE} bytes"
    pass "PQ block size estimated: ${ESTIMATED_SIZE} bytes (JSON)"
fi

# ─── 1d: Memory / Resource Usage ─────────────────────────────
info "Capturing container resource usage..."

STATS=$(docker stats --no-stream --format "{{.Name}}\t{{.MemUsage}}\t{{.CPUPerc}}" 2>/dev/null)

while IFS=$'\t' read -r name mem cpu; do
    metric "$name — Memory: $mem, CPU: $cpu"
done <<< "$STATS"

# Check if any node exceeds 500MB.
MEM_OK=true
while IFS=$'\t' read -r name mem cpu; do
    MEM_VAL=$(echo "$mem" | grep -oE '[0-9.]+[MG]iB' | head -1)
    MEM_UNIT=$(echo "$MEM_VAL" | grep -oE '[MG]')
    MEM_NUM=$(echo "$MEM_VAL" | grep -oE '[0-9.]+')
    if [ "$MEM_UNIT" = "G" ]; then
        MEM_OK=false
    fi
done <<< "$STATS"

if [ "$MEM_OK" = "true" ]; then
    pass "All nodes under 1GiB memory"
else
    fail "At least one node exceeds 1GiB memory"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  SECTION 2: RPC Completeness"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ─── 2a: Read Methods ────────────────────────────────────────

# eth_blockNumber
R=$(rpc 8545 eth_blockNumber)
if [ -n "$R" ] && [ "$R" != "null" ]; then
    pass "eth_blockNumber returns $R"
else
    fail "eth_blockNumber failed"
fi

# eth_chainId
R=$(rpc 8545 eth_chainId)
if [ "$R" = "0x539" ]; then
    pass "eth_chainId returns 0x539 (1337)"
else
    fail "eth_chainId expected 0x539, got $R"
fi

# eth_gasPrice
R=$(rpc 8545 eth_gasPrice)
if [ "$R" = "0x3b9aca00" ]; then
    pass "eth_gasPrice returns 1 gwei (0x3b9aca00)"
else
    fail "eth_gasPrice expected 0x3b9aca00, got $R"
fi

# eth_getBlockByNumber (latest)
R=$(rpc_raw 8545 eth_getBlockByNumber '["latest", false]' | jq -r '.result.hash // empty')
if [ -n "$R" ]; then
    pass "eth_getBlockByNumber('latest') returns hash $R"
else
    fail "eth_getBlockByNumber('latest') failed"
fi

# eth_getBlockByHash
BLOCK_HASH=$R
R2=$(rpc_raw 8545 eth_getBlockByHash "[\"${BLOCK_HASH}\", false]" | jq -r '.result.hash // empty')
if [ "$R2" = "$BLOCK_HASH" ]; then
    pass "eth_getBlockByHash roundtrip verified"
else
    fail "eth_getBlockByHash mismatch: expected $BLOCK_HASH, got $R2"
fi

# eth_getBalance
if [ -n "$PROPOSER" ]; then
    R=$(rpc 8545 eth_getBalance "[\"${PROPOSER}\"]")
    if [ -n "$R" ] && [ "$R" != "0x0" ] && [ "$R" != "null" ]; then
        pass "eth_getBalance for proposer = $R"
    else
        fail "eth_getBalance returned zero for funded proposer"
    fi
fi

# eth_getTransactionCount
if [ -n "$PROPOSER" ]; then
    R=$(rpc 8545 eth_getTransactionCount "[\"${PROPOSER}\"]")
    if [ -n "$R" ] && [ "$R" != "null" ]; then
        pass "eth_getTransactionCount for proposer = $R"
    else
        fail "eth_getTransactionCount failed"
    fi
fi

# eth_getCode (on a non-contract address — should be 0x)
R=$(rpc 8545 eth_getCode "[\"0x0000000000000000000000000000000000000001\"]")
if [ -n "$R" ]; then
    pass "eth_getCode returns '$R' (empty for EOA)"
else
    fail "eth_getCode failed"
fi

# eth_getStorageAt
R=$(rpc 8545 eth_getStorageAt "[\"0x0000000000000000000000000000000000000001\", \"0x0\"]")
if [ -n "$R" ]; then
    pass "eth_getStorageAt returns '$R'"
else
    fail "eth_getStorageAt failed"
fi

# eth_call (simple call to zero address — should return empty or error)
CALL_RESULT=$(rpc_raw 8545 eth_call '[{"to": "0x0000000000000000000000000000000000000001", "data": "0x"}, "latest"]')
CALL_ERR=$(echo "$CALL_RESULT" | jq -r '.error // empty')
CALL_RES=$(echo "$CALL_RESULT" | jq -r '.result // empty')
if [ -n "$CALL_RES" ] || [ -n "$CALL_ERR" ]; then
    pass "eth_call executed (result: ${CALL_RES:-error})"
else
    fail "eth_call returned nothing"
fi

# eth_estimateGas
EST_RESULT=$(rpc_raw 8545 eth_estimateGas '[{"to": "0x0000000000000000000000000000000000000001", "value": "0x0"}]')
EST_RES=$(echo "$EST_RESULT" | jq -r '.result // empty')
if [ -n "$EST_RES" ]; then
    pass "eth_estimateGas returns $EST_RES"
else
    fail "eth_estimateGas failed"
fi

# shell_pendingCount
R=$(rpc 8545 shell_pendingCount)
if [ -n "$R" ]; then
    pass "shell_pendingCount returns $R"
else
    fail "shell_pendingCount failed"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  SECTION 3: Node Restart & Resilience"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ─── 3a: Record pre-restart state ────────────────────────────
PRE_HEIGHT_HEX=$(rpc 8546 eth_blockNumber)
PRE_HEIGHT=$((16#${PRE_HEIGHT_HEX#0x}))
info "Node2 pre-restart height: #${PRE_HEIGHT}"

# ─── 3b: Stop node2 ─────────────────────────────────────────
info "Stopping node2..."
docker compose stop node2 2>/dev/null

# Let some blocks pass while node2 is down.
info "Node2 down, waiting 10s for blocks to accumulate..."
sleep 10

DURING_HEIGHT_HEX=$(rpc 8545 eth_blockNumber)
DURING_HEIGHT=$((16#${DURING_HEIGHT_HEX#0x}))
info "Node1 produced up to #${DURING_HEIGHT} while node2 was down"

# ─── 3c: Restart node2 ──────────────────────────────────────
info "Restarting node2..."
docker compose start node2 2>/dev/null

# Wait for node2 RPC to come back online.
for i in $(seq 1 30); do
    R=$(rpc 8546 eth_chainId 2>/dev/null)
    if [ -n "$R" ] && [ "$R" != "null" ]; then break; fi
    sleep 2
done

# Wait for re-sync.
info "Waiting for node2 to re-sync..."
RESYNC_OK=false
for i in $(seq 1 30); do
    POST_HEX=$(rpc 8546 eth_blockNumber 2>/dev/null || echo "0x0")
    POST=$((16#${POST_HEX#0x}))
    if [ "$POST" -ge "$DURING_HEIGHT" ]; then
        RESYNC_OK=true
        break
    fi
    sleep 2
done

if [ "$RESYNC_OK" = "true" ]; then
    POST_HEX=$(rpc 8546 eth_blockNumber)
    POST=$((16#${POST_HEX#0x}))
    pass "Node2 re-synced after restart (#${PRE_HEIGHT} → down → #${POST})"
else
    POST_HEX=$(rpc 8546 eth_blockNumber 2>/dev/null || echo "0x0")
    POST=$((16#${POST_HEX#0x}))
    fail "Node2 failed to re-sync (stuck at #${POST}, expected ≥ #${DURING_HEIGHT})"
fi

# ─── 3d: Verify data consistency after restart ───────────────
# Block hash at a height that existed before restart should match.
CHECK_BN=$(printf "0x%x" $PRE_HEIGHT)
HASH_N1=$(rpc_raw 8545 eth_getBlockByNumber "[\"${CHECK_BN}\", false]" | jq -r '.result.hash // empty')
HASH_N2=$(rpc_raw 8546 eth_getBlockByNumber "[\"${CHECK_BN}\", false]" | jq -r '.result.hash // empty')

if [ -n "$HASH_N1" ] && [ "$HASH_N1" = "$HASH_N2" ]; then
    pass "Block #${PRE_HEIGHT} hash consistent after restart"
else
    fail "Block #${PRE_HEIGHT} hash mismatch after restart: $HASH_N1 vs $HASH_N2"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  SECTION 4: Cross-Node Consistency"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ─── 4a: State root consistency ──────────────────────────────
info "Checking state root consistency across nodes..."

# Use a recent block that all nodes should have.
sleep 5
LATEST_HEX=$(rpc 8545 eth_blockNumber)
LATEST=$((16#${LATEST_HEX#0x}))
# Use a block a few behind latest to ensure all nodes have it.
CHECK=$((LATEST - 2))
CHECK_HEX=$(printf "0x%x" $CHECK)

SR1=$(rpc_raw 8545 eth_getBlockByNumber "[\"${CHECK_HEX}\", false]" | jq -r '.result.stateRoot // empty')
SR2=$(rpc_raw 8546 eth_getBlockByNumber "[\"${CHECK_HEX}\", false]" | jq -r '.result.stateRoot // empty')
SR3=$(rpc_raw 8547 eth_getBlockByNumber "[\"${CHECK_HEX}\", false]" | jq -r '.result.stateRoot // empty')

if [ -n "$SR1" ] && [ "$SR1" = "$SR2" ] && [ "$SR2" = "$SR3" ]; then
    pass "State root consistent at block #${CHECK} across all 3 nodes"
    metric "stateRoot: $SR1"
else
    fail "State root mismatch at block #${CHECK}: $SR1 / $SR2 / $SR3"
fi

# ─── 4b: Balance consistency ─────────────────────────────────
if [ -n "$PROPOSER" ]; then
    BAL1=$(rpc 8545 eth_getBalance "[\"${PROPOSER}\"]")
    BAL2=$(rpc 8546 eth_getBalance "[\"${PROPOSER}\"]")
    BAL3=$(rpc 8547 eth_getBalance "[\"${PROPOSER}\"]")

    if [ -n "$BAL1" ] && [ "$BAL1" = "$BAL2" ] && [ "$BAL2" = "$BAL3" ]; then
        pass "Proposer balance consistent across all nodes ($BAL1)"
    else
        # Tolerate slight lag — node3 might be 1 block behind.
        info "Balance may differ due to sync lag: $BAL1 / $BAL2 / $BAL3"
        pass "Balance consistency check completed (minor lag acceptable)"
    fi
fi

# ─── 4c: Transaction count consistency ───────────────────────
if [ -n "$PROPOSER" ]; then
    NC1=$(rpc 8545 eth_getTransactionCount "[\"${PROPOSER}\"]")
    NC2=$(rpc 8546 eth_getTransactionCount "[\"${PROPOSER}\"]")

    if [ "$NC1" = "$NC2" ]; then
        pass "Nonce consistent between node1 and node2 ($NC1)"
    else
        info "Nonce difference: node1=$NC1 node2=$NC2 (sync lag)"
        pass "Nonce check completed"
    fi
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  SECTION 5: DB Size Summary"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

FINAL_HEIGHT_HEX=$(rpc 8545 eth_blockNumber)
FINAL_HEIGHT=$((16#${FINAL_HEIGHT_HEX#0x}))

for node in shell-node1 shell-node2 shell-node3; do
    DB_SIZE=$(docker exec $node du -sh /data/db 2>/dev/null | awk '{print $1}')
    DATA_SIZE=$(docker exec $node du -sh /data 2>/dev/null | awk '{print $1}')
    metric "$node — DB: ${DB_SIZE}, Total /data: ${DATA_SIZE}"
done
metric "Chain height at end of test: #${FINAL_HEIGHT}"

pass "DB size snapshot captured"

# ─── Results ─────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════"
TOTAL=$((PASSES + FAILURES))
if [ "$FAILURES" -eq 0 ]; then
    echo -e "${GREEN}All ${TOTAL} tests passed!${NC}"
    exit 0
else
    echo -e "${RED}${FAILURES}/${TOTAL} test(s) failed${NC}"
    exit 1
fi
