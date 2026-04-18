#!/usr/bin/env bash
# gen-report.sh — Generate a human-readable text report from a load-test CSV.
#
# Usage:
#   ./gen-report.sh <csv_file> [node_log_file]
#
# Output is written to stdout and optionally to
#   reports/<run_id>.txt
# where run_id is derived from the CSV filename.
#
# Example:
#   ./gen-report.sh /tmp/shell-load-test/load-test-20260415_205018.csv \
#                   /tmp/shell-local-test/node-loadtest3.log \
#                   > reports/run-20260415_205018.txt

set -euo pipefail

CSV="${1:-}"
NODE_LOG="${2:-}"

if [[ -z "$CSV" ]] || [[ ! -f "$CSV" ]]; then
    echo "Usage: $0 <csv_file> [node_log_file]" >&2
    exit 1
fi

RUN_ID=$(basename "$CSV" .csv | sed 's/load-test-//')
REPORT_FILE="$(dirname "$0")/reports/run-${RUN_ID}.txt"

# ── Helpers ──────────────────────────────────────────────────────────────────
awk_sum()  { awk -F',' -v col="$1" 'NR>1 { sum += $col } END { printf "%.0f", sum }' "$CSV"; }
awk_avg()  { awk -F',' -v col="$1" 'NR>1 { sum += $col; n++ } END { if(n>0) printf "%.1f", sum/n; else print "0" }' "$CSV"; }
awk_max()  { awk -F',' -v col="$1" 'NR>1 { if($col>m) m=$col } END { printf "%.1f", m }' "$CSV"; }
awk_min()  { awk -F',' -v col="$1" 'NR>1 { if(min==""||$col<min) min=$col } END { printf "%.1f", min }' "$CSV"; }
awk_last() { awk -F',' -v col="$1" 'NR>1 { v=$col } END { print v }' "$CSV"; }

PERIODS=$(awk -F',' 'NR>1 { n++ } END { print n }' "$CSV")
FIRST_TS=$(awk -F',' 'NR==2 { print $1 }' "$CSV")
LAST_TS=$(awk -F',' 'END { print $1 }' "$CSV")

# Column indices (1-based):
#  1=timestamp 2=period_secs 3=submitted 4=confirmed 5=errors
#  6=tps_submit 7=p50_ms 8=p95_ms 9=p99_ms 10=max_ms

TOTAL_SUBMIT=$(awk_sum 3)
TOTAL_ERRORS=$(awk_sum 5)
AVG_TPS=$(awk_avg 6)
PEAK_TPS=$(awk_max 6)
MIN_TPS=$(awk_min 6)
AVG_P50=$(awk_avg 7)
AVG_P95=$(awk_avg 8)
AVG_P99=$(awk_avg 9)
PEAK_LAT=$(awk_max 10)
LAST_TPS=$(awk_last 6)
LAST_ERR=$(awk_last 5)

SUCCESS_PCT=$(awk -v s="$TOTAL_SUBMIT" -v e="$TOTAL_ERRORS" \
    'BEGIN { total=s+e; if(total>0) printf "%.1f", s/total*100; else print "0" }')

# Node block stats
BLOCK_COUNT=""
BLOCK_TPS=""
AVG_TX_PER_BLOCK=""
if [[ -n "$NODE_LOG" ]] && [[ -f "$NODE_LOG" ]]; then
    BLOCK_COUNT=$(grep -c "Block #.*produced" "$NODE_LOG" 2>/dev/null || echo "0")
    AVG_TX_PER_BLOCK=$(grep "Block #.*produced" "$NODE_LOG" 2>/dev/null | \
        grep -oE '\([0-9]+ txs' | grep -oE '[0-9]+' | \
        awk '{sum+=$1;n++} END{if(n>0)printf "%.0f",sum/n;else print "0"}')
fi

# ── Report ───────────────────────────────────────────────────────────────────
generate_report() {
cat <<EOF
═══════════════════════════════════════════════════════════════════
Shell-chain Load Test Report
Run ID : ${RUN_ID}
Date   : $(date -u '+%Y-%m-%d %H:%M:%S UTC')
═══════════════════════════════════════════════════════════════════

Test Parameters
───────────────
  CSV source      : ${CSV}
  First timestamp : ${FIRST_TS}
  Last timestamp  : ${LAST_TS}
  Periods logged  : ${PERIODS}  (× 30s each)
  Elapsed (approx): $(echo "$PERIODS * 30 / 60" | bc) minutes

Transaction Submit Metrics (mempool-accept)
───────────────────────────────────────────
  Total submitted   : ${TOTAL_SUBMIT} txs
  Total errors      : ${TOTAL_ERRORS}
  Success rate      : ${SUCCESS_PCT}%
  Avg TPS           : ${AVG_TPS}
  Peak TPS          : ${PEAK_TPS}
  Min  TPS          : ${MIN_TPS}
  Latest period TPS : ${LAST_TPS}
  Latest period err : ${LAST_ERR}

Latency (submit → mempool accept)
──────────────────────────────────
  Avg p50  : ${AVG_P50} ms
  Avg p95  : ${AVG_P95} ms
  Avg p99  : ${AVG_P99} ms
  Peak max : ${PEAK_LAT} ms

Block Finalization (from node log)
───────────────────────────────────
  Blocks produced       : ${BLOCK_COUNT:-N/A}
  Avg txs / block       : ${AVG_TX_PER_BLOCK:-N/A}
  Est. committed TPS    : $(echo "${AVG_TX_PER_BLOCK:-0} / 2" | bc 2>/dev/null || echo "N/A") TPS  (block-time=2s)

Period-by-Period Detail
───────────────────────
$(printf "%-22s %8s %8s %8s %8s %6s %6s %6s\n" \
    "Timestamp" "Submit" "Errors" "TPS" "LastTPS" "p50ms" "p95ms" "p99ms")
$(awk -F',' 'NR>1 {
    printf "%-22s %8d %8d %8.1f %8.1f %6d %6d %6d\n",
        $1, $3, $5, $6, $6, $7, $8, $9
}' "$CSV")

═══════════════════════════════════════════════════════════════════
EOF
}

generate_report | tee "$REPORT_FILE"
echo "" >&2
echo "Report saved → ${REPORT_FILE}" >&2
