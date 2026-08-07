#!/usr/bin/env bash
set -euo pipefail

fail() {
    echo "release source check failed: $1" >&2
    exit 1
}

COMMIT="${1:-}"
if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
    fail "expected a full 40-character commit SHA"
fi

CURRENT_COMMIT=$(git rev-parse HEAD) || fail "could not resolve current HEAD"
if [ "$CURRENT_COMMIT" != "$COMMIT" ]; then
    fail "HEAD moved after release validation (expected ${COMMIT}, found ${CURRENT_COMMIT})"
fi

if ! STATUS=$(git status --porcelain --untracked-files=normal); then
    fail "could not inspect the working tree"
fi
if [ -n "$STATUS" ]; then
    fail "working tree changed after release validation"
fi

echo "release source remains clean at ${COMMIT}"
