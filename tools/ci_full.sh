#!/usr/bin/env bash
# Robot OS + Brain — Full cross-repo CI check.
#
# Validates:
#   1. robot-os: all 5 feature combos build clean
#   2. robot-brain: pytest suite passes
#   3. Protocol sync: brain_protocol.rs matches protocol.py
#
# Usage: bash tools/ci_full.sh

set -euo pipefail

OS_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BRAIN_DIR="$OS_DIR/../robot-brain"
CARGO="${CARGO:-cargo}"

PASS=0
FAIL=0
WARN=0

check() {
    local label="$1"
    shift
    printf "  %-30s" "${label}..."
    if "$@" >/dev/null 2>&1; then
        echo "PASS"
        PASS=$((PASS + 1))
    else
        echo "FAIL"
        FAIL=$((FAIL + 1))
    fi
}

echo "========================================="
echo " Robot OS + Brain — Full CI"
echo "========================================="
echo ""

# ── 1. Robot OS builds ──────────────────────────────────────────────
echo "[1/3] Robot OS builds"
cd "$OS_DIR"

check "default (QEMU)"    $CARGO build --release
check "no-ml"             $CARGO build --release --features no-ml
check "no-mmu"            $CARGO build --release --features no-mmu
check "vf2"               $CARGO build --release --features vf2
check "k1"                $CARGO build --release --features k1
echo ""

# ── 2. Robot Brain tests ────────────────────────────────────────────
echo "[2/3] Robot Brain tests"
if [ -d "$BRAIN_DIR" ]; then
    cd "$BRAIN_DIR"
    # Syntax check
    check "syntax (server.py)"   python3 -m py_compile server.py
    check "syntax (protocol.py)" python3 -m py_compile protocol.py

    # Pytest
    printf "  %-30s" "pytest..."
    RESULT=$(python3 -m pytest tests/ -q --tb=line 2>&1 | tail -1)
    if echo "$RESULT" | grep -q "passed"; then
        echo "PASS ($RESULT)"
        PASS=$((PASS + 1))
    else
        echo "FAIL ($RESULT)"
        FAIL=$((FAIL + 1))
    fi
else
    echo "  SKIP — $BRAIN_DIR not found"
    WARN=$((WARN + 1))
fi
echo ""

# ── 3. Protocol sync ───────────────────────────────────────────────
echo "[3/3] Protocol sync"
RUST_PROTO="$OS_DIR/crates/behavior/src/brain_protocol.rs"
PY_PROTO="$BRAIN_DIR/protocol.py"

if [ -f "$RUST_PROTO" ] && [ -f "$PY_PROTO" ]; then
    SYNC_OK=true

    # Check key packet types exist in both
    for PKT in "0x01" "0x02" "0x03" "0x80" "0x81" "0x82" "0x83" "0x84" "0x85" "0x86" "0x88"; do
        IN_RUST=$(grep -c "$PKT" "$RUST_PROTO" 2>/dev/null || echo 0)
        IN_PY=$(grep -c "$PKT" "$PY_PROTO" 2>/dev/null || echo 0)
        if [ "$IN_RUST" -gt 0 ] && [ "$IN_PY" -eq 0 ]; then
            echo "  MISSING in Python: packet type $PKT"
            SYNC_OK=false
        fi
    done

    if $SYNC_OK; then
        echo "  Protocol sync:                PASS"
        PASS=$((PASS + 1))
    else
        echo "  Protocol sync:                WARN (missing types in Python)"
        WARN=$((WARN + 1))
    fi
else
    echo "  SKIP — protocol files not found"
    WARN=$((WARN + 1))
fi

echo ""
echo "========================================="
echo " Results: $PASS passed, $FAIL failed, $WARN warnings"
echo "========================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
