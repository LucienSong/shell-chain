#!/usr/bin/env bash
set -euo pipefail

fail() {
    echo "release tag check failed: $1" >&2
    exit 1
}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/release-version.sh"

REMOTE="${1:-origin}"
TAG="${2:-}"

if ! release_tag_is_valid "$TAG"; then
    fail "expected a semver tag with a leading 'v'"
fi

TAG_REF="refs/tags/${TAG}"
if git show-ref --verify --quiet "$TAG_REF"; then
    fail "tag '${TAG}' already exists locally"
fi

set +e
REMOTE_OUTPUT=$(git ls-remote --exit-code --refs "$REMOTE" "$TAG_REF" 2>&1)
REMOTE_STATUS=$?
set -e

case "$REMOTE_STATUS" in
    0)
        fail "tag '${TAG}' already exists on remote '${REMOTE}'"
        ;;
    2)
        ;;
    *)
        fail "could not verify tag '${TAG}' on remote '${REMOTE}': ${REMOTE_OUTPUT}"
        ;;
esac

echo "release tag '${TAG}' is available locally and on remote '${REMOTE}'"
