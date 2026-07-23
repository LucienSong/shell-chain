#!/usr/bin/env bash
set -euo pipefail

fail() {
    echo "release remote check failed: $1" >&2
    exit 1
}

REMOTE="${1:-origin}"
if ! URLS=$(git remote get-url --push --all "$REMOTE" 2>/dev/null); then
    fail "remote '$REMOTE' has no push URL"
fi
URL_COUNT=$(printf '%s\n' "$URLS" | awk 'NF { count++ } END { print count + 0 }')
if [ "$URL_COUNT" -ne 1 ]; then
    fail "remote '$REMOTE' must have exactly one push URL (found $URL_COUNT)"
fi
URL="$URLS"

case "$URL" in
    https://github.com/ShellDAO/shell-chain | \
    https://github.com/ShellDAO/shell-chain.git | \
    git@github.com:ShellDAO/shell-chain | \
    git@github.com:ShellDAO/shell-chain.git | \
    ssh://git@github.com/ShellDAO/shell-chain | \
    ssh://git@github.com/ShellDAO/shell-chain.git)
        ;;
    *)
        fail "remote '$REMOTE' does not target ShellDAO/shell-chain"
        ;;
esac

echo "release remote '$REMOTE' targets ShellDAO/shell-chain"
