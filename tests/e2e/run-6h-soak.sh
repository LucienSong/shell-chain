#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_DIR"

TOTAL_SECONDS=21600
REUSE=false
REBUILD_IMAGES=false
OUTPUT_DIR=""
TX_ACCOUNTS=12
TX_MIN_INTERVAL=100
TX_MAX_INTERVAL=800
CHAIN_ID=1337
OVERALL_FAIL=0
STARTED_TESTNET=0
HEALTH_PID=""
TX_SOAK_PID=""
STOP_MONITOR_FILE=""

usage() {
    cat <<'EOF'
Usage: ./tests/e2e/run-6h-soak.sh [--reuse] [--total-seconds N] [--output-dir DIR]
                                 [--rebuild-images]

Runs a long-lived shell-chain testnet soak with:
  - baseline smoke
  - continuous tx soak
  - AA mixed usage
  - load / security / chaos / extended suites
  - AA-specific attack injection
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --reuse)
            REUSE=true
            shift
            ;;
        --rebuild-images)
            REBUILD_IMAGES=true
            shift
            ;;
        --total-seconds)
            TOTAL_SECONDS="$2"
            shift 2
            ;;
        --output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [[ -z "$OUTPUT_DIR" ]]; then
    OUTPUT_DIR="${TMPDIR:-/tmp}/shell-chain-soak-$(date +%Y%m%d-%H%M%S)"
fi
mkdir -p "$OUTPUT_DIR"
STOP_MONITOR_FILE="$OUTPUT_DIR/.stop-monitor"
rm -f "$STOP_MONITOR_FILE"

TX_GEN_MANIFEST="$PROJECT_DIR/tools/tx-generator/Cargo.toml"
TX_GEN_BIN="$PROJECT_DIR/tools/tx-generator/target/debug/shell-tx-generator"
AA_BIN="$PROJECT_DIR/tools/tx-generator/target/debug/shell-aa-injector"
SUMMARY_SCRIPT="$PROJECT_DIR/tests/e2e/summarize-6h-soak.py"
FUNDING_KEY_FILE="$OUTPUT_DIR/dev-authority.json"
FUND_RPC_ARGS=(
    --fund-rpc-url http://127.0.0.1:8545
    --fund-rpc-url http://127.0.0.1:8546
    --fund-rpc-url http://127.0.0.1:8547
)

log() {
    local message="$1"
    echo "[$(date -Iseconds)] $message" | tee -a "$OUTPUT_DIR/orchestrator.log"
}

rpc_result() {
    local port="$1"
    local method="$2"
    local params="${3:-[]}"
    curl -sf "http://127.0.0.1:${port}" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
        2>/dev/null | jq -r '.result // empty'
}

wait_for_rpc() {
    local port="$1"
    local timeout="${2:-90}"
    for _ in $(seq 1 "$timeout"); do
        local chain_id
        chain_id="$(rpc_result "$port" eth_chainId 2>/dev/null || true)"
        if [[ -n "$chain_id" && "$chain_id" != "null" ]]; then
            return 0
        fi
        sleep 1
    done
    return 1
}

capture_snapshot() {
    local label="$1"
    docker compose ps > "$OUTPUT_DIR/compose-ps-${label}.log" 2>&1 || true
    docker compose logs --tail=300 > "$OUTPUT_DIR/compose-logs-${label}.log" 2>&1 || true
}

export_dev_authority_key() {
    if docker exec shell-node1 test -f /data/dev-authority.json; then
        docker exec shell-node1 cat /data/dev-authority.json > "$FUNDING_KEY_FILE"
        chmod 600 "$FUNDING_KEY_FILE"
        log "exported dev authority key to $(basename "$FUNDING_KEY_FILE") for canonical funding"
    else
        log "dev authority key not found in shell-node1"
        return 1
    fi
}

monitor_health() {
    while [[ ! -f "$STOP_MONITOR_FILE" ]]; do
        local ts b1 b2 b3 p1 p2 p3 stats
        ts="$(date -Iseconds)"
        b1="$(rpc_result 8545 eth_blockNumber || echo "")"
        b2="$(rpc_result 8546 eth_blockNumber || echo "")"
        b3="$(rpc_result 8547 eth_blockNumber || echo "")"
        p1="$(rpc_result 8545 net_peerCount || echo "")"
        p2="$(rpc_result 8546 net_peerCount || echo "")"
        p3="$(rpc_result 8547 net_peerCount || echo "")"
        printf '%s block1=%s block2=%s block3=%s peer1=%s peer2=%s peer3=%s\n' \
            "$ts" "$b1" "$b2" "$b3" "$p1" "$p2" "$p3" >> "$OUTPUT_DIR/periodic-health.log"
        stats="$(docker stats --no-stream --format '{{.Name}} cpu={{.CPUPerc}} mem={{.MemUsage}}' 2>/dev/null | tr '\n' ';' || true)"
        printf '%s %s\n' "$ts" "$stats" >> "$OUTPUT_DIR/periodic-stats.log"
        sleep 60
    done
}

run_phase() {
    local name="$1"
    shift
    local log_file="$OUTPUT_DIR/${name}.log"
    local start_ts end_ts elapsed rc status
    start_ts="$(date +%s)"
    log "PHASE_START name=${name}"
    set +e
    "$@" >"$log_file" 2>&1
    rc=$?
    set -e
    end_ts="$(date +%s)"
    elapsed=$((end_ts - start_ts))
    if [[ $rc -eq 0 ]]; then
        status="ok"
    else
        status="fail"
        OVERALL_FAIL=1
    fi
    log "PHASE_END name=${name} status=${status} rc=${rc} duration_s=${elapsed} log=$(basename "$log_file")"
    return 0
}

scaled_seconds() {
    local nominal="$1"
    echo $(( nominal * TOTAL_SECONDS / 21600 ))
}

wait_until_elapsed() {
    local target="$1"
    local now elapsed sleep_for
    now="$(date +%s)"
    elapsed=$((now - RUN_START_EPOCH))
    if (( target > elapsed )); then
        sleep_for=$((target - elapsed))
        log "waiting ${sleep_for}s until next scheduled phase"
        sleep "$sleep_for"
    fi
}

run_tx_soak_loop() {
    local remaining="$1"
    local nominal_chunk chunk round rc had_fail
    nominal_chunk="$(scaled_seconds 3600)"
    if (( nominal_chunk < 60 )); then
        nominal_chunk=60
    fi
    round=1
    had_fail=0
    while (( remaining > 0 )); do
        if (( remaining < nominal_chunk )); then
            chunk="$remaining"
        else
            chunk="$nominal_chunk"
        fi
        if (( chunk <= 0 )); then
            break
        fi
        log "TX_SOAK_START round=${round} duration_s=${chunk}"
        set +e
        "$TX_GEN_BIN" \
            --rpc-url http://127.0.0.1:8545 \
            "${FUND_RPC_ARGS[@]}" \
            --accounts "$TX_ACCOUNTS" \
            --duration "$chunk" \
            --min-interval "$TX_MIN_INTERVAL" \
            --max-interval "$TX_MAX_INTERVAL" \
            --funding-key-file "$FUNDING_KEY_FILE" \
            --chain-id "$CHAIN_ID" \
            >"$OUTPUT_DIR/tx-soak-round-${round}.log" 2>&1
        rc=$?
        set -e
        if [[ $rc -ne 0 ]]; then
            had_fail=1
        fi
        log "TX_SOAK_END round=${round} rc=${rc} log=tx-soak-round-${round}.log"
        remaining=$((remaining - chunk))
        round=$((round + 1))
    done
    return "$had_fail"
}

build_docker_images() {
    if docker compose build > "$OUTPUT_DIR/compose-build.log" 2>&1; then
        return 0
    fi

    log "docker compose build failed, falling back to legacy docker build"
    DOCKER_BUILDKIT=0 docker build -t shell-chain-node1 . \
        > "$OUTPUT_DIR/legacy-image-build.log" 2>&1
    docker tag shell-chain-node1 shell-chain-node2
    docker tag shell-chain-node1 shell-chain-node3
}

cleanup() {
    touch "$STOP_MONITOR_FILE" 2>/dev/null || true
    if [[ -n "$HEALTH_PID" ]]; then
        kill "$HEALTH_PID" 2>/dev/null || true
    fi
    if [[ -n "$TX_SOAK_PID" ]]; then
        kill "$TX_SOAK_PID" 2>/dev/null || true
        wait "$TX_SOAK_PID" 2>/dev/null || true
    fi
    rm -f "$FUNDING_KEY_FILE"
    if [[ "$REUSE" != "true" && $STARTED_TESTNET -eq 1 ]]; then
        docker compose down -v --remove-orphans > "$OUTPUT_DIR/compose-teardown.log" 2>&1 || true
    fi
}
trap cleanup EXIT

log "6h soak orchestrator start output_dir=$OUTPUT_DIR total_seconds=$TOTAL_SECONDS"

if [[ "$REUSE" == "true" ]]; then
    log "reusing existing docker testnet"
else
    log "starting docker compose testnet"
    log "resetting existing docker compose stack"
    docker compose down -v --remove-orphans > "$OUTPUT_DIR/compose-reset.log" 2>&1 || true
    if [[ "$REBUILD_IMAGES" == "true" ]]; then
        log "rebuilding docker images before startup"
        build_docker_images
        docker compose up -d --no-build > "$OUTPUT_DIR/compose-up.log" 2>&1
    elif docker image inspect shell-chain-node1 shell-chain-node2 shell-chain-node3 >/dev/null 2>&1; then
        log "reusing existing local docker images"
        docker compose up -d --no-build > "$OUTPUT_DIR/compose-up.log" 2>&1
    else
        log "building docker images before startup"
        build_docker_images
        docker compose up -d --no-build > "$OUTPUT_DIR/compose-up.log" 2>&1
    fi
    STARTED_TESTNET=1
fi

for port in 8545 8546 8547; do
    if wait_for_rpc "$port" 120; then
        log "rpc port ${port} is ready"
    else
        log "rpc port ${port} failed to become ready"
        capture_snapshot failed-startup
        exit 1
    fi
done

capture_snapshot initial

log "building tx-generator and AA injector binaries"
cargo build \
    --manifest-path "$TX_GEN_MANIFEST" \
    --bin shell-tx-generator \
    --bin shell-aa-injector \
    > "$OUTPUT_DIR/tool-build.log" 2>&1

monitor_health &
HEALTH_PID="$!"
log "health monitor pid=$HEALTH_PID"

RUN_START_EPOCH="$(date +%s)"

BASELINE_END="$(scaled_seconds 1200)"
AA_MIXED_START="$(scaled_seconds 4800)"
AA_MIXED_DURATION="$(scaled_seconds 2400)"
LOAD_START="$(scaled_seconds 7200)"
SECURITY_START="$(scaled_seconds 9600)"
AA_ATTACK_START="$(scaled_seconds 12000)"
AA_ATTACK_DURATION="$(scaled_seconds 3600)"
CHAOS_START="$(scaled_seconds 15600)"
FINAL_START="$(scaled_seconds 19200)"

run_phase baseline-smoke "$PROJECT_DIR/tests/e2e/run-e2e.sh" --reuse
if ! export_dev_authority_key; then
    capture_snapshot funding-key-missing
    exit 1
fi
wait_until_elapsed "$BASELINE_END"

CURRENT_ELAPSED=$(( $(date +%s) - RUN_START_EPOCH ))
if (( TOTAL_SECONDS > CURRENT_ELAPSED )); then
    TX_REMAINING=$((TOTAL_SECONDS - CURRENT_ELAPSED))
    run_tx_soak_loop "$TX_REMAINING" &
    TX_SOAK_PID="$!"
    log "tx soak pid=$TX_SOAK_PID remaining_s=$TX_REMAINING"
fi

wait_until_elapsed "$AA_MIXED_START"
run_phase aa-mixed \
    "$AA_BIN" \
    --mode mixed \
    --duration "$AA_MIXED_DURATION" \
    --rpc-url http://127.0.0.1:8545 \
    "${FUND_RPC_ARGS[@]}" \
    --funding-key-file "$FUNDING_KEY_FILE" \
    --chain-id "$CHAIN_ID"

wait_until_elapsed "$LOAD_START"
run_phase load-test "$PROJECT_DIR/tests/e2e/run-load-test.sh" --reuse

wait_until_elapsed "$SECURITY_START"
run_phase security-audit "$PROJECT_DIR/tests/e2e/run-security-audit.sh" --reuse

wait_until_elapsed "$AA_ATTACK_START"
run_phase aa-attack \
    "$AA_BIN" \
    --mode attack \
    --duration "$AA_ATTACK_DURATION" \
    --rpc-url http://127.0.0.1:8545 \
    "${FUND_RPC_ARGS[@]}" \
    --funding-key-file "$FUNDING_KEY_FILE" \
    --chain-id "$CHAIN_ID"

wait_until_elapsed "$CHAOS_START"
run_phase chaos "$PROJECT_DIR/tests/e2e/run-chaos-test.sh" --reuse

wait_until_elapsed "$FINAL_START"
run_phase extended "$PROJECT_DIR/tests/e2e/run-extended.sh" --reuse
run_phase final-burst "$PROJECT_DIR/tests/e2e/run-load-test.sh" --reuse

wait_until_elapsed "$TOTAL_SECONDS"
if [[ -n "$TX_SOAK_PID" ]]; then
    wait "$TX_SOAK_PID" || OVERALL_FAIL=1
fi

capture_snapshot final
touch "$STOP_MONITOR_FILE"
if [[ -n "$HEALTH_PID" ]]; then
    kill "$HEALTH_PID" 2>/dev/null || true
    wait "$HEALTH_PID" 2>/dev/null || true
fi

if [[ -x "$SUMMARY_SCRIPT" ]]; then
    python3 "$SUMMARY_SCRIPT" --output-dir "$OUTPUT_DIR" | tee "$OUTPUT_DIR/summary.txt"
else
    log "summary script not executable, skipping"
fi

log "6h soak orchestrator complete overall_fail=$OVERALL_FAIL"
exit "$OVERALL_FAIL"
