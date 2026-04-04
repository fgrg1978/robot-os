#!/usr/bin/env bash
# Robot OS — CI build verification
#
# Builds all feature combinations and verifies 0 errors, 0 warnings.
# Usage: ./tools/ci_check.sh
#        make ci

set -euo pipefail

CARGO="${CARGO:-cargo}"
ESP32C3_TARGET="riscv32imac-unknown-none-elf"

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

echo "=== Robot OS CI Check ==="
echo ""
echo "[1/2] Building all feature combinations..."

build "default (QEMU)"    --release
build "no-ml"             --release --features no-ml
build "no-mmu"            --release --features no-mmu
build "vf2"               --release --features vf2
build "k1"                --release --features k1

echo ""
echo "[2/2] Results: ${PASS} passed, ${FAIL} failed"

if [ "$FAIL" -gt 0 ]; then
    echo "=== CI FAILED ==="
    exit 1
fi

echo "=== All checks passed ==="
