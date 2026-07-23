#!/usr/bin/env bash
set -euo pipefail

CHANGELOG=${1:?usage: changelog-excerpt.sh <changelog> <version> [max-lines]}
VERSION=${2:?usage: changelog-excerpt.sh <changelog> <version> [max-lines]}
MAX_LINES=${3:-30}

if [[ ! "$MAX_LINES" =~ ^[1-9][0-9]*$ ]]; then
    echo "changelog excerpt max lines must be a positive integer" >&2
    exit 1
fi

awk -v heading="## [${VERSION}]" -v max_lines="$MAX_LINES" '
    index($0, heading) == 1 {
        suffix = substr($0, length(heading) + 1, 1)
        if (suffix == "" || suffix ~ /[[:space:]]/) found = 1
        next
    }
    found && /^## \[/ { exit }
    found {
        print
        if (++lines >= max_lines) exit
    }
' "$CHANGELOG"
