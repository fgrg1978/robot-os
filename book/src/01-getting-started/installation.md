# Installation

PHANES is built and tested on **macOS** (Apple Silicon) and **Linux**
(Debian / Ubuntu). Windows + WSL2 works for the build but UART tooling
is platform-specific.

## Prerequisites

- **Rust** — install via `rustup`. PHANES tracks the most-recent
  stable plus an MSRV ≥ 12 months old (currently MSRV ≥ 1.75).
- **QEMU** — `qemu-system-riscv64` and `qemu-system-aarch64` for
  development without hardware. Get it from your package manager:
  - macOS: `brew install qemu`
  - Debian / Ubuntu: `apt install qemu-system-misc`
- **Cross-toolchain** — the workspace targets bare-metal:
  `rustup target add riscv64gc-unknown-none-elf`
- **Python 3.11+** for the brain.

## Cloning

```bash
git clone https://github.com/phanes-project/phanes.git phanes
cd phanes
```

(Phase 0 note: the public repo is currently `robot-os`. Migration to
`phanes-project/phanes` happens at end of plan.)

## Building the kernel

```bash
# Default config (QEMU virt machine, RV64)
cargo build --release

# Other supported configs
cargo build --release --features vf2     # StarFive VisionFive 2
cargo build --release --features k1      # SpacemiT K1 (Banana Pi BPI-F3)
cargo build --release --features no-ml   # without ML stack
cargo build --release --features no-mmu  # MMU-less variant
```

All five configs **must** build with zero errors and zero warnings.
That's a project-level rule, not a suggestion.

## Running tests

```bash
cargo test --all                # kernel + crates
python -m pytest tests/         # brain (when phanes-brain repo ready)
```

## Running in QEMU

```bash
scripts/qemu.sh           # default
scripts/qemu.sh smp       # 4-core
scripts/qemu.sh full      # full integration with brain
```

## Verifying your build

```bash
cargo build --release && ls -la target/riscv64gc-unknown-none-elf/release/kernel
```

A successful build produces a multi-megabyte ELF; the `kernel.bin`
target produces the flat binary used for QEMU and real hardware.

## Next

[Hello robot in QEMU](./hello-robot-qemu.md).
