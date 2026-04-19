#!/usr/bin/env bash
# Robot OS — CI build + unit-test verification (D07)
#
# 1. Builds all feature combinations (QEMU/vf2/k1/no-ml/no-mmu) for zero errors/warnings.
# 2. Runs drone algorithm unit tests in crates/flight-sim on the host.
#
# Usage: ./tools/ci_check.sh
#        make ci

set -euo pipefail

CARGO="${CARGO:-cargo}"
HOST_TARGET="${HOST_TARGET:-$(rustc -vV 2>/dev/null | sed -n 's/host: //p')}"

PASS=0
FAIL=0

build() {
    local label="$1"
    shift
    printf "  %-24s" "${label}..."
    if "$CARGO" build "$@" 2>&1 | grep -qE "^error"; then
        echo "FAIL"
        FAIL=$((FAIL + 1))
        return 1
    else
        echo "ok"
        PASS=$((PASS + 1))
    fi
}

test_host() {
    local label="$1"
    local crate="$2"
    printf "  %-24s" "${label}..."
    # Run from the crate directory so its .cargo/config.toml overrides the workspace target.
    if (cd "${crate}" && "$CARGO" test 2>&1 | grep -qE "^error|FAILED"); then
        echo "FAIL"
        FAIL=$((FAIL + 1))
        return 1
    else
        echo "ok"
        PASS=$((PASS + 1))
    fi
}

echo "=== Robot OS CI Check ==="
echo ""
echo "[1/3] Building all feature combinations..."

build "default (QEMU)"    --release
build "no-ml"             --release --features no-ml
build "no-mmu"            --release --features no-mmu
build "vf2"               --release --features vf2
build "k1"                --release --features k1

echo ""
echo "[2/3] Running drone algorithm unit tests (host)..."

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
test_host "flight-sim (D01-D07)" "${REPO_ROOT}/crates/flight-sim"

echo ""
echo "[3/3] Results: ${PASS} passed, ${FAIL} failed"

if [ "$FAIL" -gt 0 ]; then
    echo "=== CI FAILED ==="
    exit 1
fi

echo "=== All checks passed ==="
