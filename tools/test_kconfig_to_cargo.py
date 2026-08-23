#!/usr/bin/env python3
"""test_kconfig_to_cargo.py — RFC-0026 Phase C4 bridge-script unit tests.

Verifies that tools/kconfig_to_cargo.py:
  1. Emits a valid --target triple for every defconfig.
  2. Emits ONLY cargo features that exist in kernel/Cargo.toml [features] OR
     in a named sub-crate as a "crate/feat" token.
  3. Does NOT emit phantom features (brain-l1, brain-l2, brain-l3, etc.).
  4. Emits the correct target triple per architecture.
  5. Handles the inverted no-ml mapping correctly.

Run with:
    python3 tools/test_kconfig_to_cargo.py

Or via pytest:
    python3 -m pytest tools/test_kconfig_to_cargo.py -v
"""

import os
import re
import sys
import subprocess
from pathlib import Path
from typing import Optional

# ---------------------------------------------------------------------------
# Locate repo root relative to this script.
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).parent.parent.resolve()
TOOLS_DIR = REPO_ROOT / "tools"
DEFCONFIGS_DIR = REPO_ROOT / "defconfigs"
KERNEL_CARGO_TOML = REPO_ROOT / "kernel" / "Cargo.toml"

# ---------------------------------------------------------------------------
# Parse kernel/Cargo.toml to get the valid bare feature names.
# ---------------------------------------------------------------------------

def parse_kernel_features(cargo_toml_path: Path) -> frozenset[str]:
    """Extract feature names from the [features] block of kernel/Cargo.toml.

    Uses a regex; no toml dependency needed.  Returns a frozenset of bare
    feature names (e.g. {'vf2', 'k1', 'qemu', 'no-ml', ...}).
    """
    text = cargo_toml_path.read_text(encoding="utf-8")
    # Find the [features] block: everything between [features] and the next [...]
    features_block_match = re.search(
        r"^\[features\](.*?)^(?=\[)",
        text,
        re.MULTILINE | re.DOTALL,
    )
    if not features_block_match:
        raise RuntimeError(f"No [features] block found in {cargo_toml_path}")

    features_block = features_block_match.group(1)
    # Each feature line starts with an identifier (may contain hyphens/underscores).
    names = re.findall(r"^([a-zA-Z][a-zA-Z0-9_\-]+)\s*=", features_block, re.MULTILINE)
    return frozenset(names)


# ---------------------------------------------------------------------------
# Known valid sub-crate feature tokens (crate/feat form).
# These are verified manually against the crate Cargo.toml files.
# ---------------------------------------------------------------------------

VALID_CRATE_FEATURES: frozenset[str] = frozenset({
    "robot_os_mm/small-mem",              # crates/mm/Cargo.toml
})

# ---------------------------------------------------------------------------
# Valid target triples
# ---------------------------------------------------------------------------

VALID_TARGETS: frozenset[str] = frozenset({
    "riscv64imac-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "x86_64-unknown-none",
})

# ---------------------------------------------------------------------------
# Known phantom features that must NEVER appear in output.
# These existed in the C1 script's generic _ENABLED path.
# ---------------------------------------------------------------------------

PHANTOM_FEATURES: frozenset[str] = frozenset({
    "brain-l1",
    "brain-l2",
    "brain-l3",
    "auto-reconnect",
})

# ---------------------------------------------------------------------------
# Expected outputs per defconfig (target, features subset that must be present)
# ---------------------------------------------------------------------------

# Each entry: (defconfig_stem, expected_target, must_include_features,
#              must_exclude_features)
DEFCONFIG_EXPECTATIONS: list[tuple[str, str, set[str], set[str]]] = [
    # edge — all defaults, riscv64 QEMU, no features override
    (
        "edge",
        "riscv64imac-unknown-none-elf",
        set(),                              # no features required
        {"vf2", "k1", "no-ml", "no-mmu"},  # must not appear
    ),
    # qemu — identical to edge (explicit alias)
    (
        "qemu",
        "riscv64imac-unknown-none-elf",
        set(),
        {"vf2", "k1", "no-ml", "no-mmu"},
    ),
    # vf2 — must emit vf2 feature
    (
        "vf2",
        "riscv64imac-unknown-none-elf",
        {"vf2"},
        {"k1", "no-ml", "no-mmu"},
    ),
    # k1 — must emit k1 feature
    (
        "k1",
        "riscv64imac-unknown-none-elf",
        {"k1"},
        {"vf2", "no-ml", "no-mmu"},
    ),
    # embedded — must emit small-mem, no other board feature
    (
        "embedded",
        "riscv64imac-unknown-none-elf",
        {"robot_os_mm/small-mem"},
        {"vf2", "k1", "qemu", "no-ml", "no-mmu"},
    ),
    # fleet — all defaults, large caps via constants (no cargo features)
    (
        "fleet",
        "riscv64imac-unknown-none-elf",
        set(),
        {"vf2", "k1", "no-ml", "no-mmu"},
    ),
    # qemu-aarch64 — must emit aarch64 target
    (
        "qemu-aarch64",
        "aarch64-unknown-none-softfloat",
        set(),
        {"vf2", "k1", "no-ml", "no-mmu"},
    ),
]


# ---------------------------------------------------------------------------
# Helper: run the bridge script against a defconfig path
# ---------------------------------------------------------------------------

def run_bridge(defconfig_path: Path) -> str:
    """Run kconfig_to_cargo.py against the given defconfig and return stdout."""
    script = TOOLS_DIR / "kconfig_to_cargo.py"
    result = subprocess.run(
        [sys.executable, str(script), str(defconfig_path)],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, (
        f"kconfig_to_cargo.py exited {result.returncode} for {defconfig_path}:\n"
        f"{result.stderr}"
    )
    return result.stdout.strip()


def parse_output(output: str) -> tuple[Optional[str], list[str]]:
    """Split bridge output into (target_triple, feature_list).

    Returns (None, []) when output is empty (no .config present).
    Returns (target, [feat, ...]) otherwise.
    """
    if not output:
        return None, []

    target: Optional[str] = None
    features: list[str] = []

    # Split on whitespace tokens
    parts = output.split()
    i = 0
    while i < len(parts):
        if parts[i] == "--target" and i + 1 < len(parts):
            target = parts[i + 1]
            i += 2
        elif parts[i] == "--features" and i + 1 < len(parts):
            features = parts[i + 1].split(",")
            i += 2
        else:
            i += 1

    return target, features


# ---------------------------------------------------------------------------
# Test cases
# ---------------------------------------------------------------------------

def test_kernel_features_parseable() -> None:
    """Smoke test: we can parse kernel/Cargo.toml features block."""
    feats = parse_kernel_features(KERNEL_CARGO_TOML)
    # Known features that must be present
    for expected in ("vf2", "k1", "qemu", "no-ml", "no-mmu", "no-opensbi",
                     "rvv", "uefi", "tftp-smoke",
                     "secure-boot-enforced"):
        assert expected in feats, f"Expected feature '{expected}' missing from kernel/Cargo.toml"


def test_no_phantom_features() -> None:
    """Phantom features (brain-l1/l2/l3) must never appear in any output."""
    KERNEL_FEATURES = parse_kernel_features(KERNEL_CARGO_TOML)

    for defconfig_name, _expected_target, _must_include, _must_exclude in DEFCONFIG_EXPECTATIONS:
        defconfig_path = DEFCONFIGS_DIR / f"{defconfig_name}.config"
        assert defconfig_path.exists(), f"defconfig not found: {defconfig_path}"

        output = run_bridge(defconfig_path)
        _target, features = parse_output(output)

        for feat in features:
            assert feat not in PHANTOM_FEATURES, (
                f"defconfig={defconfig_name}: phantom feature '{feat}' emitted"
            )


def test_all_emitted_features_are_valid() -> None:
    """Every emitted feature must be in kernel/Cargo.toml or a known crate/feat token."""
    KERNEL_FEATURES = parse_kernel_features(KERNEL_CARGO_TOML)

    for defconfig_name, _expected_target, _must_include, _must_exclude in DEFCONFIG_EXPECTATIONS:
        defconfig_path = DEFCONFIGS_DIR / f"{defconfig_name}.config"
        output = run_bridge(defconfig_path)
        _target, features = parse_output(output)

        for feat in features:
            is_bare_valid = feat in KERNEL_FEATURES
            is_crate_valid = feat in VALID_CRATE_FEATURES
            assert is_bare_valid or is_crate_valid, (
                f"defconfig={defconfig_name}: feature '{feat}' is not in "
                f"kernel/Cargo.toml [features] and is not a known crate/feat token"
            )


def test_target_triples_are_valid() -> None:
    """Every defconfig must emit one of the 3 valid target triples."""
    for defconfig_name, expected_target, _must_include, _must_exclude in DEFCONFIG_EXPECTATIONS:
        defconfig_path = DEFCONFIGS_DIR / f"{defconfig_name}.config"
        output = run_bridge(defconfig_path)
        target, _features = parse_output(output)

        assert target is not None, (
            f"defconfig={defconfig_name}: no --target in output: '{output}'"
        )
        assert target in VALID_TARGETS, (
            f"defconfig={defconfig_name}: invalid target '{target}'"
        )
        assert target == expected_target, (
            f"defconfig={defconfig_name}: expected target '{expected_target}', "
            f"got '{target}'"
        )


def test_expected_features_present_and_absent() -> None:
    """For each defconfig, verify must_include and must_exclude feature sets."""
    for defconfig_name, _target, must_include, must_exclude in DEFCONFIG_EXPECTATIONS:
        defconfig_path = DEFCONFIGS_DIR / f"{defconfig_name}.config"
        output = run_bridge(defconfig_path)
        _target_triple, features = parse_output(output)
        feat_set = set(features)

        for feat in must_include:
            assert feat in feat_set, (
                f"defconfig={defconfig_name}: required feature '{feat}' not emitted. "
                f"Got: {sorted(feat_set)}"
            )
        for feat in must_exclude:
            assert feat not in feat_set, (
                f"defconfig={defconfig_name}: excluded feature '{feat}' was emitted. "
                f"Got: {sorted(feat_set)}"
            )


def test_missing_dot_config_emits_nothing() -> None:
    """When .config is absent, script must emit nothing (exit 0)."""
    script = TOOLS_DIR / "kconfig_to_cargo.py"
    result = subprocess.run(
        [sys.executable, str(script), "/nonexistent/path/.config"],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
    assert result.stdout.strip() == "", (
        f"Expected empty output for absent .config; got: '{result.stdout.strip()}'"
    )


def test_edge_no_ml_absent() -> None:
    """edge defconfig does not disable DRV_ML_INFERENCE → no-ml must NOT appear."""
    defconfig_path = DEFCONFIGS_DIR / "edge.config"
    output = run_bridge(defconfig_path)
    _target, features = parse_output(output)
    assert "no-ml" not in features, (
        f"edge defconfig should NOT emit no-ml; got features: {features}"
    )


def test_aarch64_target_selection() -> None:
    """qemu-aarch64 defconfig must select aarch64-unknown-none-softfloat."""
    defconfig_path = DEFCONFIGS_DIR / "qemu-aarch64.config"
    output = run_bridge(defconfig_path)
    target, _features = parse_output(output)
    assert target == "aarch64-unknown-none-softfloat", (
        f"Expected aarch64 target; got '{target}'"
    )


def test_default_target_is_riscv64() -> None:
    """Any defconfig without an explicit ARCH_ key defaults to riscv64."""
    # edge, qemu, embedded, fleet, vf2, k1 are all riscv64
    riscv64_defconfigs = ["edge", "qemu", "embedded", "fleet", "vf2", "k1"]
    for name in riscv64_defconfigs:
        defconfig_path = DEFCONFIGS_DIR / f"{name}.config"
        output = run_bridge(defconfig_path)
        target, _features = parse_output(output)
        assert target == "riscv64imac-unknown-none-elf", (
            f"defconfig={name}: expected riscv64 target; got '{target}'"
        )


# ---------------------------------------------------------------------------
# Simple self-runner (also works with pytest)
# ---------------------------------------------------------------------------

def _run_all_tests() -> int:
    """Run all test_* functions and report results."""
    tests = [
        test_kernel_features_parseable,
        test_no_phantom_features,
        test_all_emitted_features_are_valid,
        test_target_triples_are_valid,
        test_expected_features_present_and_absent,
        test_missing_dot_config_emits_nothing,
        test_edge_no_ml_absent,
        test_aarch64_target_selection,
        test_default_target_is_riscv64,
    ]

    passed = 0
    failed = 0
    for test_fn in tests:
        try:
            test_fn()
            print(f"  PASS  {test_fn.__name__}")
            passed += 1
        except AssertionError as exc:
            print(f"  FAIL  {test_fn.__name__}: {exc}")
            failed += 1
        except Exception as exc:  # noqa: BLE001
            print(f"  ERROR {test_fn.__name__}: {type(exc).__name__}: {exc}")
            failed += 1

    print(f"\n{passed + failed} tests: {passed} passed, {failed} failed")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(_run_all_tests())
