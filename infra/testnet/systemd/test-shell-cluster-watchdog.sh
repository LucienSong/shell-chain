#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
watchdog="$script_dir/shell-cluster-watchdog.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin"

cat >"$tmp/bin/curl" <<'EOF'
#!/usr/bin/env bash
endpoint="${*: -1}"
case "$endpoint" in
  http://ready/health) printf '%s\n' '{"production_ready":true,"syncing":false}' ;;
  http://unready/health) printf '%s\n' '{"production_ready":false,"syncing":false}' ;;
  http://syncing/health) printf '%s\n' '{"production_ready":false,"syncing":true}' ;;
  *) exit 22 ;;
esac
EOF

cat >"$tmp/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$WATCHDOG_TEST_ACTIONS"
if [[ "$1" == "is-active" ]]; then
  exit 0
fi
EOF

cat >"$tmp/bin/logger" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF

cat >"$tmp/bin/flock" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF

chmod +x "$tmp/bin/curl" "$tmp/bin/systemctl" "$tmp/bin/logger" "$tmp/bin/flock"

run_watchdog() {
  local endpoints="$1" services="$2" state_dir="$3" actions="$4"
  PATH="$tmp/bin:$PATH" \
    WATCHDOG_TEST_ACTIONS="$actions" \
    SHELL_WATCHDOG_ENDPOINTS="$endpoints" \
    SHELL_WATCHDOG_SERVICES="$services" \
    SHELL_WATCHDOG_FAILURE_THRESHOLD=1 \
    SHELL_WATCHDOG_STATE_DIR="$state_dir" \
    bash "$watchdog"
}

actions="$tmp/actions"
run_watchdog "http://ready,http://ready" "ready-a.service,ready-b.service" "$tmp/all-ready" "$actions"
[[ ! -e "$actions" ]]

run_watchdog "http://ready,http://missing" "ready.service,unreachable.service" "$tmp/one-unreachable" "$actions"
grep -qx 'restart unreachable.service' "$actions"
if grep -qx 'restart ready.service' "$actions"; then
  echo "watchdog restarted a production-ready service" >&2
  exit 1
fi

: >"$actions"
run_watchdog "http://syncing,http://unready" "syncing.service,unready.service" "$tmp/syncing" "$actions"
[[ ! -s "$actions" ]]

printf '%s\n' "shell cluster watchdog tests passed"
