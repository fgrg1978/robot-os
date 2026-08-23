#!/usr/bin/env python3
"""kconfig_to_cargo.py — RFC-0026 Phase C4 (updated from C1)

Reads a kconfiglib-generated `.config` file and emits `cargo build` arguments
to stdout:

  --features vf2,qemu     (based on bool options that map to cargo features)
  --target riscv64imac-unknown-none-elf  (based on ARCH_* selection)

The existing crate-level `#[cfg(feature = "vf2")]` gates continue to work
because the bridge translates Kconfig choices into the correct `--features`
arguments.

Usage:
    cargo build --release $(python3 tools/kconfig_to_cargo.py)
    cargo build --release $(python3 tools/kconfig_to_cargo.py .config)

If the .config file is absent the script emits nothing (allows existing manual
`--features` invocations to keep working untouched during the C1 phase).

Phase C4 changes vs C1:
  - Replaced the generic _ENABLED suffix path that produced phantom features
    (brain-l1, brain-l2, brain-l3) which do not exist in any Cargo.toml.
  - Built an explicit KCONFIG_TO_CARGO_FEATURE table that covers every cargo
    feature in kernel/Cargo.toml plus the one remaining nested crate feature
    (robot_os_mm/small-mem). secure-boot-enforced used to be emitted as the
    nested token robot_os_ota/secure-boot-enforced; now that kernel/src/main.rs
    itself gates the boot-halt on `#[cfg(feature = "secure-boot-enforced")]`
    (F18 wiring), the bare kernel feature is emitted instead — it forwards to
    robot_os_ota/secure-boot-enforced via kernel/Cargo.toml's own
    `secure-boot-enforced = ["robot_os_ota/secure-boot-enforced"]` entry, so
    both crates still end up in lockstep from one --features token.
  - Added INVERTED_FEATURES set for documentation: features whose Kconfig
    option is absent/n when the cargo feature is enabled.
  - Added ISA-correct --target selection (riscv64 / aarch64 / x86_64).
"""

import sys
import os
from typing import Optional

# ---------------------------------------------------------------------------
# Target triple mapping
# ---------------------------------------------------------------------------

# Maps CONFIG_ARCH_* key to cargo --target triple.
ARCH_TO_TARGET: dict[str, str] = {
    "CONFIG_ARCH_RISCV64": "riscv64imac-unknown-none-elf",
    "CONFIG_ARCH_AARCH64": "aarch64-unknown-none-softfloat",
    "CONFIG_ARCH_X86_64":  "x86_64-unknown-none",
}

# Default target when no ARCH_* is set (ARCH_RISCV64 is the Kconfig default).
DEFAULT_TARGET = "riscv64imac-unknown-none-elf"

# ---------------------------------------------------------------------------
# Kconfig option → kernel cargo feature.
#
# Rules:
#   - Bare feature names (e.g. "vf2") must appear in kernel/Cargo.toml
#     [features] block.
#   - "crate/feat" tokens (e.g. "robot_os_ota/secure-boot-enforced") must
#     appear in the named crate's [features] block AND the crate must be a
#     kernel dependency.
#   - Kconfig options not listed here produce pub const entries in
#     crates/limits, NOT cargo features.
#   - Options marked None have no direct feature; they affect generated
#     constants only.
#
# Valid kernel/Cargo.toml features (exhaustive, as of 2026-08-11):
#   vf2, k1, rvv, qemu, tftp-smoke, no-ml, no-mmu, no-opensbi,
#   uefi, secure-boot-enforced
#
# Nested crate features used as kernel --features tokens:
#   robot_os_mm/small-mem               (crates/mm/Cargo.toml)
# ---------------------------------------------------------------------------

KCONFIG_TO_CARGO_FEATURE: dict[str, Optional[str]] = {
    # Board / platform → bare kernel feature
    "CONFIG_BOARD_VF2":     "vf2",
    "CONFIG_BOARD_K1":      "k1",
    "CONFIG_BOARD_QEMU":    "qemu",

    # ISA extensions
    "CONFIG_HAS_RVV":       "rvv",

    # MMU mode: NO_MMU=y → enable no-mmu feature (direct, NOT inverted)
    "CONFIG_NO_MMU":        "no-mmu",

    # Boot path options.
    # NOTE: BOOT_NO_OPENSBI, TFTP_SMOKE_BOOT, BOOT_UEFI are not yet defined
    # in any Kconfig.* fragment (C1 scope; pending C4/C5 author work).
    # These rows are listed here so the mapping is complete and documented;
    # they will never fire until the matching Kconfig options are added.
    "CONFIG_BOOT_NO_OPENSBI":  "no-opensbi",
    "CONFIG_TFTP_SMOKE_BOOT":  "tftp-smoke",
    "CONFIG_BOOT_UEFI":        "uefi",

    # Security: bare kernel feature (gates the F18 boot-halt in
    # kernel/src/main.rs directly), which itself forwards to
    # robot_os_ota/secure-boot-enforced — see kernel/Cargo.toml.
    "CONFIG_SECURE_BOOT_ENFORCED": "secure-boot-enforced",
    # Same compile-time-policy pattern as secure boot: see kernel/Cargo.toml.
    "CONFIG_LINK_AUTH_ENFORCED": "link-auth-enforced",
    # K-C5 — kernel/Cargo.toml forwards to robot_os_behavior →
    # robot_os_encrypt_link, same pattern as secure-boot-enforced.
    "CONFIG_LINK_ENCRYPT_ENFORCED": "link-encrypt-enforced",

    # Profile: PROFILE_EMBEDDED activates small-mem in crates/mm
    "CONFIG_PROFILE_EMBEDDED": "robot_os_mm/small-mem",

    # BOARD_GENERIC and PROFILE_EDGE/FLEET have no direct cargo feature.
    # Their effect is entirely through pub const values in crates/limits.
    "CONFIG_BOARD_GENERIC":  None,
    "CONFIG_PROFILE_EDGE":   None,
    "CONFIG_PROFILE_FLEET":  None,
}

# Options that produce a cargo feature by their ABSENCE (inverted booleans).
# When CONFIG_FOO is absent or =n, the corresponding cargo feature is enabled.
# This set is intentionally small — most options are direct (presence = enable).
INVERTED_FEATURE_MAP: dict[str, str] = {
    # DRV_ML_INFERENCE=n (or not set) → enable no-ml feature.
    "CONFIG_DRV_ML_INFERENCE": "no-ml",
}

# Documentation-only set: cargo feature names whose Kconfig option is inverted.
# Used to label the semantics; the logic lives in INVERTED_FEATURE_MAP above.
INVERTED_FEATURES: frozenset[str] = frozenset({"no-ml", "no-mmu", "no-opensbi"})

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

DOT_CONFIG_DEFAULT = ".config"


def read_dot_config(path: str) -> Optional[dict[str, str]]:
    """Parse a kconfiglib .config file.

    Returns a dict of CONFIG_KEY → raw value string, or None if the file
    does not exist (silently; caller handles the missing-file case).
    """
    if not os.path.exists(path):
        return None

    result: dict[str, str] = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            # Skip blank lines and comment lines (including "# CONFIG_X is not set")
            if not line or line.startswith("#"):
                # Record "not set" patterns as "n" so INVERTED_FEATURE_MAP can see them.
                if line.startswith("# CONFIG_") and line.endswith(" is not set"):
                    key = line[2 : line.index(" is not set")]
                    result[key] = "n"
                continue
            if "=" not in line:
                continue
            key, _, val = line.partition("=")
            result[key.strip()] = val.strip()
    return result


def arch_target(cfg: dict[str, str]) -> str:
    """Return the --target triple for the selected ISA.

    Falls back to the riscv64 default if no ARCH_* key is set to "y"
    (ARCH_RISCV64 is the Kconfig default, so an all-defaults .config
    will not contain an explicit CONFIG_ARCH_RISCV64=y line).
    """
    for kconfig_key, triple in ARCH_TO_TARGET.items():
        if cfg.get(kconfig_key) == "y":
            return triple
    return DEFAULT_TARGET


def enabled_features(cfg: dict[str, str]) -> list[str]:
    """Return a deduplicated, ordered list of cargo features to enable.

    Only features that exist in kernel/Cargo.toml (bare names) or in a
    kernel-dependency crate (crate/feat notation) are produced.  The generic
    _ENABLED suffix path that existed in the C1 version has been removed to
    prevent phantom features like brain-l1/l2/l3.
    """
    features: list[str] = []
    seen: set[str] = set()

    def add(feat: Optional[str]) -> None:
        if feat and feat not in seen:
            features.append(feat)
            seen.add(feat)

    # --- Explicit forward mapping ---
    for kconfig_key, cargo_feat in KCONFIG_TO_CARGO_FEATURE.items():
        if cfg.get(kconfig_key) == "y":
            add(cargo_feat)

    # --- Explicit inverted mapping ---
    # Fire only when the option is explicitly disabled in the defconfig
    # ("# CONFIG_FOO is not set" → stored as "n").  We do NOT fire on absent
    # keys because minimal defconfigs omit options that are at their defaults,
    # and the default for DRV_ML_INFERENCE is y (ML enabled).  Only an explicit
    # disable line means "turn this feature off".
    for kconfig_key, cargo_feat in INVERTED_FEATURE_MAP.items():
        val = cfg.get(kconfig_key)
        if val == "n":
            add(cargo_feat)

    return features


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    dot_config_path = sys.argv[1] if len(sys.argv) > 1 else DOT_CONFIG_DEFAULT

    cfg = read_dot_config(dot_config_path)
    if cfg is None:
        # .config absent — emit nothing.  Existing `--features X` invocations
        # in the Makefile continue to work untouched.
        return 0

    parts: list[str] = []

    # --target (always emitted; defaults to riscv64 if not set)
    target = arch_target(cfg)
    parts.append(f"--target {target}")

    # --features
    feats = enabled_features(cfg)
    if feats:
        parts.append(f"--features {','.join(feats)}")

    if parts:
        print(" ".join(parts))

    return 0


if __name__ == "__main__":
    sys.exit(main())
