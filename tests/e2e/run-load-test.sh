#!/usr/bin/env bash
# Shell-chain Load Test
# Sends sustained transaction load against the 3-node testnet and measures
# throughput and latency.
#
# Usage: ./tests/e2e/run-load-test.sh [--reuse]
#   --reuse  Skip build/start, use already-running containers
set -euo pipefail

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

AA_ADDR_1="pq1qyqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqy0vusna"
AA_ADDR_2="pq1qyqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqg7j66z6"
AA_ADDR_3="pq1qyqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqv3ccudq"

# M10 production target: 500 TPS for 1 hour (1_800_000 transactions).
# For CI/smoke runs use: TX_COUNT=500 DURATION=30
# For production soak use: TX_COUNT=1800000 DURATION=3600
TX_COUNT=${TX_COUNT:-500}
DURATION=${DURATION:-30}
LATENCY_FILE="$PROJECT_DIR/tests/e2e/.load-test-latencies.txt"

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

cleanup() {
    rm -f "$LATENCY_FILE"
    if [ "$REUSE" != "true" ]; then
        info "Tearing down containers..."
        docker compose down -v --remove-orphans 2>/dev/null || true
    fi
}
trap cleanup EXIT

REUSE=false
if [ "${1:-}" = "--reuse" ]; then
    REUSE=true
fi

echo ""
echo "╔══════════════════════════════════════════════╗"
echo "║   Shell-chain Load Test Suite                ║"
echo "╚══════════════════════════════════════════════╝"
echo ""
metric "Target: ${TX_COUNT} transactions over ${DURATION}s"
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

# ─── Pre-test: Capture memory baseline ───────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Pre-Load Baseline"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

MEM_BEFORE=$(docker stats --no-stream --format "{{.Name}}\t{{.MemUsage}}" 2>/dev/null)
echo "$MEM_BEFORE" | while IFS=$'\t' read -r name mem; do
    metric "BEFORE — $name: $mem"
done

PRE_HEIGHT_HEX=$(rpc 8545 eth_blockNumber)
PRE_HEIGHT=$((16#${PRE_HEIGHT_HEX#0x}))
metric "Block height before load: #${PRE_HEIGHT}"

PENDING_BEFORE=$(rpc 8545 shell_pendingCount)
PENDING_BEFORE_DEC=$((16#${PENDING_BEFORE#0x}))
DRAIN_ALLOWANCE=${DRAIN_ALLOWANCE:-4}
if [ "$PENDING_BEFORE_DEC" -eq 0 ]; then
    DRAIN_TARGET=0
else
    DRAIN_TARGET=$((PENDING_BEFORE_DEC + DRAIN_ALLOWANCE))
fi
metric "Pending tx before load: ${PENDING_BEFORE}"
metric "Drain target after load: <= ${DRAIN_TARGET} pending txs"

# ─── Send sustained load ─────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Sending ${TX_COUNT} Transactions"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Calculate delay between transactions to spread over DURATION seconds.
# Use microseconds for better precision.
DELAY_US=$(( (DURATION * 1000000) / TX_COUNT ))

# Use canonical Shell addresses even in negative-path RPC load tests so the
# payload shape matches the native AA model.
pick_addr() {
    case $(( $1 % 3 )) in
        0) echo "$AA_ADDR_1" ;;
        1) echo "$AA_ADDR_2" ;;
        *) echo "$AA_ADDR_3" ;;
    esac
}

> "$LATENCY_FILE"
SUBMITTED=0
ERRORS=0

# Rotate across all three nodes for load distribution.
PORTS=(8545 8546 8547)

info "Starting load at $(date '+%H:%M:%S')..."
LOAD_START=$(date +%s%N)

for i in $(seq 1 "$TX_COUNT"); do
    PORT=${PORTS[$((i % 3))]}
    TO=$(pick_addr "$i")
    NONCE=$(printf "0x%x" "$i")

    # Measure per-request latency.
    REQ_START=$(date +%s%N)

    RESULT=$(curl -sf "http://127.0.0.1:${PORT}" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{
            \"jsonrpc\":\"2.0\",\"id\":${i},
            \"method\":\"shell_sendTransaction\",
            \"params\":[{
                \"from\":\"${AA_ADDR_1}\",
                \"to\":\"${TO}\",
                \"value\":\"0x1\",
                \"nonce\":\"${NONCE}\",
                \"gas\":\"0x5208\",
                \"gasPrice\":\"0x3b9aca00\",
                \"data\":\"0x\"
            }]
        }" 2>/dev/null || echo "")

    REQ_END=$(date +%s%N)
    LATENCY_NS=$((REQ_END - REQ_START))
    LATENCY_MS=$((LATENCY_NS / 1000000))

    echo "$LATENCY_MS" >> "$LATENCY_FILE"

    if [ -z "$RESULT" ]; then
        ERRORS=$((ERRORS + 1))
    fi
    SUBMITTED=$((SUBMITTED + 1))

    # Progress indicator every 50 txs.
    if [ $((i % 50)) -eq 0 ]; then
        info "  Sent ${i}/${TX_COUNT} transactions..."
    fi

    # Throttle to match target rate.
    if [ "$DELAY_US" -gt 1000 ]; then
        sleep "0.$(printf '%03d' $((DELAY_US / 1000)))"
    fi
done

LOAD_END=$(date +%s%N)
TOTAL_TIME_MS=$(( (LOAD_END - LOAD_START) / 1000000 ))
TOTAL_TIME_S=$((TOTAL_TIME_MS / 1000))

info "Load completed at $(date '+%H:%M:%S') (${TOTAL_TIME_S}s elapsed)"

# ─── Measure TPS ─────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Throughput Results"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

if [ "$TOTAL_TIME_S" -gt 0 ]; then
    TPS=$((SUBMITTED / TOTAL_TIME_S))
else
    TPS=$SUBMITTED
fi

metric "Total submitted: ${SUBMITTED}"
metric "RPC errors: ${ERRORS}"
metric "Elapsed time: ${TOTAL_TIME_S}s"
metric "Effective TPS (submit rate): ${TPS} tx/s"

if [ "$ERRORS" -lt $((TX_COUNT / 10)) ]; then
    pass "Transaction submission: ${SUBMITTED} sent, ${ERRORS} errors (<10% error rate)"
else
    fail "Transaction submission: too many errors (${ERRORS}/${SUBMITTED})"
fi

# ─── Latency percentiles ─────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Latency Percentiles"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

if [ -f "$LATENCY_FILE" ] && [ -s "$LATENCY_FILE" ]; then
    SORTED=$(sort -n "$LATENCY_FILE")
    TOTAL_LINES=$(echo "$SORTED" | wc -l | tr -d ' ')

    percentile() {
        local pct=$1
        local idx=$(( (TOTAL_LINES * pct + 99) / 100 ))
        [ "$idx" -lt 1 ] && idx=1
        echo "$SORTED" | sed -n "${idx}p"
    }

    P50=$(percentile 50)
    P95=$(percentile 95)
    P99=$(percentile 99)
    MIN=$(echo "$SORTED" | head -1)
    MAX=$(echo "$SORTED" | tail -1)

    metric "Min latency:  ${MIN}ms"
    metric "P50 latency:  ${P50}ms"
    metric "P95 latency:  ${P95}ms"
    metric "P99 latency:  ${P99}ms"
    metric "Max latency:  ${MAX}ms"

    # P99 under 5 seconds is acceptable for a testnet.
    if [ "$P99" -lt 5000 ]; then
        pass "P99 latency under 5s (${P99}ms)"
    else
        fail "P99 latency too high (${P99}ms >= 5000ms)"
    fi
else
    fail "No latency data collected"
fi

# ─── Wait for receipts ───────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Drain & Receipt Check"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

info "Waiting up to 30s for mempool to drain..."
DRAIN_OK=false
for i in $(seq 1 15); do
    PENDING=$(rpc 8545 shell_pendingCount)
    PENDING_DEC=$((16#${PENDING#0x}))
    if [ "$PENDING_DEC" -le "$DRAIN_TARGET" ]; then
        DRAIN_OK=true
        break
    fi
    info "  Pending: ${PENDING_DEC} tx remaining..."
    sleep 2
done

if [ "$DRAIN_OK" = "true" ]; then
    pass "Mempool drained back to target window (baseline ${PENDING_BEFORE_DEC}, target <= ${DRAIN_TARGET})"
else
    PENDING=$(rpc 8545 shell_pendingCount)
    fail "Mempool did not drain back to target window after 30s (${PENDING}, target <= ${DRAIN_TARGET})"
fi

# ─── Post-load memory ────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Post-Load Memory"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

MEM_AFTER=$(docker stats --no-stream --format "{{.Name}}\t{{.MemUsage}}" 2>/dev/null)
echo "$MEM_AFTER" | while IFS=$'\t' read -r name mem; do
    metric "AFTER — $name: $mem"
done

POST_HEIGHT_HEX=$(rpc 8545 eth_blockNumber)
POST_HEIGHT=$((16#${POST_HEIGHT_HEX#0x}))
BLOCKS_DURING=$((POST_HEIGHT - PRE_HEIGHT))

metric "Blocks produced during load: ${BLOCKS_DURING} (#${PRE_HEIGHT} → #${POST_HEIGHT})"

pass "Memory snapshot captured"

# ─── Summary ─────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════"
echo "  Load Test Summary"
echo "════════════════════════════════════════════════"
metric "Transactions submitted: ${SUBMITTED}"
metric "RPC errors:             ${ERRORS}"
metric "Effective TPS:          ${TPS} tx/s"
metric "P50 / P95 / P99:        ${P50:-?}ms / ${P95:-?}ms / ${P99:-?}ms"
metric "Blocks produced:        ${BLOCKS_DURING}"
echo ""

TOTAL=$((PASSES + FAILURES))
if [ "$FAILURES" -eq 0 ]; then
    echo -e "${GREEN}All ${TOTAL} load tests passed!${NC}"
    exit 0
else
    echo -e "${RED}${FAILURES}/${TOTAL} load test(s) failed${NC}"
    exit 1
fi
