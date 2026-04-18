#!/usr/bin/env bash
# supervisor.sh — Keeps shell-node + shell-load-test alive for the 10-hour test.
#
# Usage:
#   bash supervisor.sh &
#   disown $!
#
# Logs: /tmp/shell-local-test/supervisor.log
# Stop: kill $(cat /tmp/shell-local-test/supervisor.pid)

set -euo pipefail

LOG=/tmp/shell-local-test/supervisor.log
CHAIN_DATA=/tmp/shell-local-test/chain-data
mkdir -p /tmp/shell-local-test /tmp/shell-load-test "$CHAIN_DATA"

log() {
    local msg="[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*"
    echo "$msg" >> "$LOG"
    echo "$msg" >&2
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SHELL_NODE="${REPO_ROOT}/target/release/shell-node"
LOAD_TEST="${REPO_ROOT}/target/release/shell-load-test"
OUT_DIR=/tmp/shell-load-test
TEST_DURATION=36000   # 10 hours

# Track when we started so we stop after TEST_DURATION even across restarts
START_TS=$(date +%s)

node_pid=0
lt_pid=0

start_node() {
    log "Starting shell-node…"
    (nohup "$SHELL_NODE" run \
        --db rocksdb \
        --datadir "$CHAIN_DATA" \
        --rpc-addr 127.0.0.1:8545 \
        --rpc-api eth,net,web3,shell,evm \
        --chain-id 1337 \
        --block-time 2000 \
        --rpc-cors '*' \
        --ws --ws-port 8546 \
        --unsafe-dev-exposed \
        --rpc-rate-limit 10000 \
        --mempool-max-size 50000 \
        >> /tmp/shell-local-test/node-supervised.log 2>&1) &
    node_pid=$!
    echo "$node_pid" > /tmp/shell-local-test/node.pid
    log "Node PID: $node_pid"
    # Wait for RPC to become ready
    for i in $(seq 1 30); do
        if curl -sf -X POST http://localhost:8545 \
               -H 'Content-Type: application/json' \
               -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
               > /dev/null 2>&1; then
            log "Node RPC ready"
            return 0
        fi
        sleep 1
    done
    log "ERROR: node RPC never became ready"
    return 1
}

start_load_test() {
    local elapsed=$(( $(date +%s) - START_TS ))
    local remaining=$(( TEST_DURATION - elapsed ))
    if [[ $remaining -le 0 ]]; then
        log "10-hour test duration reached — load test complete."
        return 1
    fi
    log "Starting load test (${remaining}s remaining)…"
    CSV_ID=$(date -u '+%Y%m%d_%H%M%S')
    (nohup "$LOAD_TEST" \
        --rpc http://127.0.0.1:8545 \
        --duration "$remaining" \
        --workers 20 \
        --fund-shell 1000000 \
        --out-dir "$OUT_DIR" \
        --report-interval 30 \
        --chain-id 1337 \
        >> "${OUT_DIR}/supervised-${CSV_ID}.log" 2>&1) &
    lt_pid=$!
    echo "$lt_pid" > "${OUT_DIR}/runner.pid"
    log "Load test PID: $lt_pid (CSV suffix: $CSV_ID)"
    return 0
}

cleanup() {
    log "Supervisor shutting down…"
    kill "$lt_pid" 2>/dev/null || true
    kill "$node_pid" 2>/dev/null || true
    exit 0
}
trap cleanup INT TERM

log "Supervisor started (PID $$, goal: ${TEST_DURATION}s of load testing)"
echo $$ > /tmp/shell-local-test/supervisor.pid

# Kill any pre-existing instances
existing_node=$(lsof -i :8545 | awk 'NR>1 {print $2; exit}' 2>/dev/null || true)
if [[ -n "$existing_node" ]]; then
    log "Killing existing node on :8545 (PID $existing_node)"
    kill "$existing_node" 2>/dev/null || true
    sleep 2
fi

start_node || { log "Failed to start node — exiting."; exit 1; }
start_load_test || { log "Load test duration already expired."; exit 0; }

# ── Main watchdog loop ─────────────────────────────────────────────────────
while true; do
    sleep 10

    elapsed=$(( $(date +%s) - START_TS ))
    if [[ $elapsed -ge $TEST_DURATION ]]; then
        log "Total test duration reached (${elapsed}s). Done."
        break
    fi

    # Check node health
    if ! kill -0 "$node_pid" 2>/dev/null; then
        log "Node (PID $node_pid) died — restarting…"
        # Load test will also need restart after node comes back up
        kill "$lt_pid" 2>/dev/null || true
        sleep 2
        start_node || { log "Node restart failed."; break; }
        start_load_test || break
        continue
    fi

    # Check load test health
    if ! kill -0 "$lt_pid" 2>/dev/null; then
        log "Load test (PID $lt_pid) ended — restarting if time remains…"
        start_load_test || break
    fi
done

log "Supervisor done. Generating final report…"
LATEST_CSV=$(ls -t "${OUT_DIR}"/load-test-*.csv 2>/dev/null | head -1 || true)
if [[ -n "$LATEST_CSV" ]]; then
    "${REPO_ROOT}/tools/load-test/gen-report.sh" \
        "$LATEST_CSV" /tmp/shell-local-test/node-supervised.log \
        > "${REPO_ROOT}/tools/load-test/reports/final-supervised.txt" 2>/dev/null
    log "Final report: tools/load-test/reports/final-supervised.txt"
fi
log "Supervisor exiting."
