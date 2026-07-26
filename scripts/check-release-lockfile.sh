#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="${1:-$(cd "$SCRIPT_DIR/.." && pwd)}"

fail() {
    echo "release lockfile check failed: $1" >&2
    exit 1
}

command -v cargo >/dev/null 2>&1 || fail "cargo is required"

if ! cargo metadata \
    --locked \
    --format-version 1 \
    --manifest-path "$ROOT_DIR/Cargo.toml" \
    >/dev/null; then
    fail "Cargo.lock does not match the workspace manifests"
fi

echo "release lockfile matches workspace manifests"
