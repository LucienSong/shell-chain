#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
    echo "release preflight test failed: $1" >&2
    exit 1
}

"$SCRIPT_DIR/check-release-metadata.sh"

if ! grep -Fq 'cargo audit --file tools/tx-generator/Cargo.lock' "$SCRIPT_DIR/release.sh"; then
    fail "release audit does not cover the transaction generator lockfile"
fi
if ! grep -Fq 'cargo audit --file deps/libp2p-yamux/Cargo.lock' "$SCRIPT_DIR/release.sh"; then
    fail "release audit does not cover the patched libp2p-yamux lockfile"
fi
if ! grep -Fq 'check-release-ci.sh' "$SCRIPT_DIR/release.sh"; then
    fail "release preflight does not verify hosted CI for HEAD"
fi
if ! grep -Fq 'check-release-remote.sh' "$SCRIPT_DIR/release.sh"; then
    fail "release preflight does not verify the tag push remote"
fi
if ! grep -Fq 'git push "$RELEASE_REMOTE" "$TAG"' "$SCRIPT_DIR/release.sh"; then
    fail "release tag push does not use the validated remote"
fi

REMOTE_FIXTURE="$TMP_DIR/remote-fixture"
git -C "$TMP_DIR" init -q -b main remote-fixture
git -C "$REMOTE_FIXTURE" remote add canonical https://github.com/ShellDAO/shell-chain.git
git -C "$REMOTE_FIXTURE" remote add canonical-ssh git@github.com:ShellDAO/shell-chain.git
git -C "$REMOTE_FIXTURE" remote add fork https://github.com/example/shell-chain.git
git -C "$REMOTE_FIXTURE" remote add multi https://github.com/ShellDAO/shell-chain.git
git -C "$REMOTE_FIXTURE" remote set-url --add --push multi \
    https://github.com/ShellDAO/shell-chain.git
git -C "$REMOTE_FIXTURE" remote set-url --add --push multi \
    https://github.com/example/shell-chain.git

(cd "$REMOTE_FIXTURE" && "$SCRIPT_DIR/check-release-remote.sh" canonical >/dev/null)
(cd "$REMOTE_FIXTURE" && "$SCRIPT_DIR/check-release-remote.sh" canonical-ssh >/dev/null)
if REMOTE_OUTPUT=$(cd "$REMOTE_FIXTURE" && \
    "$SCRIPT_DIR/check-release-remote.sh" fork 2>&1); then
    fail "release remote check unexpectedly accepted a fork"
fi
if ! grep -Fq "does not target ShellDAO/shell-chain" <<<"$REMOTE_OUTPUT"; then
    fail "fork rejection did not explain the required release target: $REMOTE_OUTPUT"
fi
if REMOTE_OUTPUT=$(cd "$REMOTE_FIXTURE" && \
    "$SCRIPT_DIR/check-release-remote.sh" multi 2>&1); then
    fail "release remote check unexpectedly accepted multiple push URLs"
fi
if ! grep -Fq "must have exactly one push URL (found 2)" <<<"$REMOTE_OUTPUT"; then
    fail "multiple push URL rejection was not specific: $REMOTE_OUTPUT"
fi
if REMOTE_OUTPUT=$(cd "$REMOTE_FIXTURE" && \
    "$SCRIPT_DIR/check-release-remote.sh" missing 2>&1); then
    fail "release remote check unexpectedly accepted a missing remote"
fi
if ! grep -Fq "has no push URL" <<<"$REMOTE_OUTPUT"; then
    fail "missing push URL rejection was not specific: $REMOTE_OUTPUT"
fi

CHECK_SHA=1111111111111111111111111111111111111111
FAKE_GH="$TMP_DIR/fake-gh"
cat > "$FAKE_GH" <<'EOF'
#!/usr/bin/env bash
cat "$CHECK_RUNS_FIXTURE"
EOF
chmod +x "$FAKE_GH"

write_check_runs() {
    local test_status=$1
    local test_conclusion=$2
    local test_sha=${3:-$CHECK_SHA}
    cat > "$TMP_DIR/check-runs.json" <<EOF
{
  "check_runs": [
    {"name":"Check & Lint","head_sha":"$CHECK_SHA","status":"completed","conclusion":"success"},
    {"name":"Test","head_sha":"$test_sha","status":"$test_status","conclusion":$test_conclusion},
    {"name":"Supply Chain Security","head_sha":"$CHECK_SHA","status":"completed","conclusion":"success"}
  ]
}
EOF
}

assert_ci_fails_with() {
    local expected=$1
    local output
    if output=$(GH_BIN="$FAKE_GH" CHECK_RUNS_FIXTURE="$TMP_DIR/check-runs.json" \
        "$SCRIPT_DIR/check-release-ci.sh" "$CHECK_SHA" 2>&1); then
        fail "release CI check unexpectedly passed"
    fi
    if ! grep -Fq "$expected" <<<"$output"; then
        fail "expected '$expected' in CI check output: $output"
    fi
}

write_check_runs completed '"success"'
GH_BIN="$FAKE_GH" CHECK_RUNS_FIXTURE="$TMP_DIR/check-runs.json" \
    "$SCRIPT_DIR/check-release-ci.sh" "$CHECK_SHA" >/dev/null

write_check_runs in_progress null
assert_ci_fails_with "required check 'Test' has not succeeded"

write_check_runs completed '"success"' 2222222222222222222222222222222222222222
assert_ci_fails_with "required check 'Test' is associated with another commit"

printf '{"check_runs":[]}' > "$TMP_DIR/check-runs.json"
assert_ci_fails_with "required check 'Check & Lint' is missing"

make_fixture() {
    local changelog=$1
    local fixture="$TMP_DIR/fixture"

    rm -rf "$fixture"
    mkdir -p "$fixture/scripts"
    cp "$SCRIPT_DIR/release.sh" "$SCRIPT_DIR/check-release-ci.sh" \
        "$SCRIPT_DIR/check-release-remote.sh" \
        "$SCRIPT_DIR/check-release-metadata.sh" \
        "$SCRIPT_DIR/supply-chain-tool-versions.sh" "$fixture/scripts/"
    printf '[workspace.package]\nversion = "0.27.1"\n' > "$fixture/Cargo.toml"
    mkdir -p "$fixture/fuzz"
    printf '[package]\nname = "shell-fuzz"\nversion = "0.27.1"\n' > "$fixture/fuzz/Cargo.toml"
    printf '| v0.27.x | supported |\n| < v0.27.0 | end of life |\n\n**v0.27.x is the current supported release line.** v0.27.x receives security-only backports. Users older than v0.27.0 should upgrade.\n' > "$fixture/SECURITY.md"
    printf 'https://img.shields.io/badge/version-0.27.1-green.svg\n' > "$fixture/README.md"
    printf 'FROM example.invalid/base\n# ghcr.io/shelldao/shell-chain:v0.27.1\n' > "$fixture/Dockerfile"
    printf '%s\n' "$changelog" > "$fixture/CHANGELOG.md"
    git -C "$fixture" init -q -b main
    git -C "$fixture" config user.name "ShellDAO Release Test"
    git -C "$fixture" config user.email "release-test@shelldao.org"
    git -C "$fixture" remote add origin https://github.com/ShellDAO/shell-chain.git
    git -C "$fixture" add .
    git -C "$fixture" commit -qm "test fixture"
    printf '%s\n' "$fixture"
}

assert_fails_with() {
    local fixture=$1
    local version=$2
    local expected=$3
    local output

    if output=$(cd "$fixture" && ./scripts/release.sh "$version" 2>&1); then
        fail "release unexpectedly passed for version $version"
    fi
    if ! grep -Fq "$expected" <<<"$output"; then
        fail "expected '$expected' in output: $output"
    fi
}

assert_metadata_fails_with() {
    local fixture=$1
    local expected=$2
    local output

    if output=$("$fixture/scripts/check-release-metadata.sh" "$fixture" 2>&1); then
        fail "release metadata check unexpectedly passed"
    fi
    if ! grep -Fq "$expected" <<<"$output"; then
        fail "expected '$expected' in metadata output: $output"
    fi
}

fixture=$(make_fixture $'## [Unreleased]\n\n## [0.27.1] - test release')
assert_fails_with "$fixture" '0x27x1' "Version must be semver"

printf '| v0.24.x | stale support claim |\n' >> "$fixture/SECURITY.md"
assert_metadata_fails_with "$fixture" "exactly one supported release row (found 2)"

sed -i.bak 's/v0.27.x/v0.24.x/g' "$fixture/SECURITY.md"
rm "$fixture/SECURITY.md.bak"
git -C "$fixture" add SECURITY.md
git -C "$fixture" commit -qm "stale security policy"
assert_fails_with "$fixture" '0.27.1' "Public release metadata is stale"

touch "$fixture/untracked-release-input"
assert_fails_with "$fixture" '0.27.1' "uncommitted or untracked files"

fixture=$(make_fixture $'## [Unreleased]\n\n[0.27.1]: https://example.invalid/release')
assert_fails_with "$fixture" '0.27.1' "exactly one ## [0.27.1] release heading (found 0)"

fixture=$(make_fixture '## [0.27.1] - test release')
assert_fails_with "$fixture" '0.27.1' "exactly one ## [Unreleased] heading (found 0)"

fixture=$(make_fixture $'## [Unreleased]\n\n## [Unreleased]\n\n## [0.27.1] - test release')
assert_fails_with "$fixture" '0.27.1' "exactly one ## [Unreleased] heading (found 2)"

fixture=$(make_fixture $'## [Unreleased]\n\n## [0.27.1] - first\n\n## [0.27.1] - duplicate')
assert_fails_with "$fixture" '0.27.1' "exactly one ## [0.27.1] release heading (found 2)"

fixture=$(make_fixture $'## [Unreleased]\n\n## [0.27.1] - test release')
git -C "$fixture" switch -qc topic/release
assert_fails_with "$fixture" '0.27.1' "must run from 'main' or 'release/v0.27.1'"

fixture=$(make_fixture $'## [Unreleased]\n\n## [0.27.1] - test release')
git -C "$fixture" checkout -q --detach
assert_fails_with "$fixture" '0.27.1' "must run from 'main' or 'release/v0.27.1'"

fixture=$(make_fixture $'## [Unreleased]\n\n## [0.27.1] - test release')
git -C "$fixture" switch -qc release/v0.27.1
assert_fails_with "$fixture" '0.27.1' "cargo fmt check failed"

fixture=$(make_fixture $'## [Unreleased]\n\n## [0.27.1] - test release')
git -C "$fixture" switch -q --orphan release/v0.27.1
mkdir -p "$fixture/scripts"
cp "$SCRIPT_DIR/release.sh" "$SCRIPT_DIR/check-release-ci.sh" \
    "$SCRIPT_DIR/check-release-remote.sh" \
    "$SCRIPT_DIR/check-release-metadata.sh" \
    "$SCRIPT_DIR/supply-chain-tool-versions.sh" "$fixture/scripts/"
printf '[workspace.package]\nversion = "0.27.1"\n' > "$fixture/Cargo.toml"
mkdir -p "$fixture/fuzz"
printf '[package]\nname = "shell-fuzz"\nversion = "0.27.1"\n' > "$fixture/fuzz/Cargo.toml"
printf '| v0.27.x | supported |\n| < v0.27.0 | end of life |\n\n**v0.27.x is the current supported release line.** v0.27.x receives security-only backports. Users older than v0.27.0 should upgrade.\n' > "$fixture/SECURITY.md"
printf 'https://img.shields.io/badge/version-0.27.1-green.svg\n' > "$fixture/README.md"
printf 'FROM example.invalid/base\n# ghcr.io/shelldao/shell-chain:v0.27.1\n' > "$fixture/Dockerfile"
printf '## [Unreleased]\n\n## [0.27.1] - test release\n' > "$fixture/CHANGELOG.md"
git -C "$fixture" add .
git -C "$fixture" commit -qm "unrelated release history"
assert_fails_with "$fixture" '0.27.1' "must descend from 'main'"

echo "release preflight tests passed"
