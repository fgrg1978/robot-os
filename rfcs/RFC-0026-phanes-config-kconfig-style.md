# RFC-0026: PHANES Unified Build Configuration (Kconfig-style)

> **Status:** implemented  
> **Authors:** Fernando Rodriguez
> **Created:** 2026-05-26
> **Last updated:** 2026-05-26
> **Supersedes:** parts of RFC-0023 (centralised limits) — this RFC subsumes
> RFC-0023's profile mechanism into a single global configuration system.
> **Superseded by:** —

## Summary

PHANES today has **six distinct configuration surfaces** scattered across
the codebase: cargo features per crate, target triples, hardware platform
constants, scattered `pub const` caps, CONFIG.INI runtime keys, and
`.cargo/config.toml` build settings.  Onboarding a new deployment ("make
this build for VF2 with the fleet profile and AEAD link enabled")
requires editing 4-7 files in separate locations.

This RFC introduces a **single Kconfig-style configuration system**
modelled on Linux's `make menuconfig` / Zephyr's `prj.conf`:

- One source of truth: `.config` at workspace root (generated from a
  declarative Kconfig menu).
- An interactive TUI (`make menuconfig`) to edit it.
- Pre-built profiles (`make defconfig-{embedded,edge,fleet,vf2,k1,qemu}`)
  for common deployments.
- A workspace `build.rs` that reads `.config` and emits both:
  - `crates/limits/src/generated.rs` — every numeric cap, timing
    constant, buffer size as a `pub const`.
  - Cargo feature env vars (`CARGO_FEATURE_*`) so existing
    `#[cfg(feature = …)]` gates Just Work.
- Every existing `pub const` and every cargo feature becomes Kconfig-driven.

Result: changing from a demo build to a fleet-edge build is **one
command** (`make defconfig-fleet && make`), not a 7-file edit.  Same
zero-alloc-runtime guarantee.  Same WCET predictability.  Same safety
case (RFC-0017) — every value is still compile-time constant.

## Motivation

### Today's configuration surfaces (audit, 2026-05-26)

| # | Surface | Locations | Examples |
|---|---------|-----------|----------|
| 1 | **Cargo features** (compile-time on/off) | 14 `Cargo.toml` files across kernel + 13 crates | `qemu`, `vf2`, `k1`, `esp32c3`, `no-ml`, `no-mmu`, `no-opensbi`, `rvv`, `uefi`, `tftp-smoke`, `secure-boot-enforced`, `small-mem` |
| 2 | **Target triple** (architecture choice) | 10 `.cargo/config.toml` files | `riscv64imac-unknown-none-elf`, `aarch64-unknown-none-softfloat`, `x86_64-unknown-none`, host triples for tests |
| 3 | **Hardware platform consts** | `crates/drivers/src/platform.rs` | UART_BASE, PLIC_BASE, TIMER_FREQ (10 MHz qemu / 4 MHz esp32c3 / 24 MHz k1 / 16 MHz vf2), RAM_BASE, RAM_SIZE |
| 4 | **Resource caps** | ~15 sites across `crates/sched`, `crates/net`, `crates/ipc`, `crates/fs`, `crates/topology`, `crates/pubsub`, `crates/service`, `kernel/src/main.rs` | `MAX_TASKS`, `TCP_MAX_CONNS`, `MAX_FDS`, `MAX_CHANNELS`, `MAX_TOPICS`, `MAX_SERVICES`, `MAX_CAPS_TOTAL`, `KERNEL_HEAP_SIZE` |
| 5 | **Timing / WCET / buffer constants** | ~30 sites across `drivers`, `sched`, `net`, `behavior`, `ipc`, `ota` | `SCHED_HZ`, `WCET_BOUND_*`, `RTO_INITIAL_MS`, `KEEPALIVE_INTERVAL_TICKS`, `OFFLINE_RECONNECT_INTERVAL_TICKS`, `SAFETY_COMMS_TIMEOUT_TICKS`, `PID_DT_MS`, `DEFAULT_QUANTUM_US`, `PIPE_BUF_SIZE`, `TCP_BUF_SIZE`, `TRACE_BUF_SIZE`, `ZEROCOPY_BUF_SIZE`, `OTA_RECV_BUF_SIZE`, `JPEG_MAX_SIZE`, `RF_MAX_PAYLOAD`, `ETH_MTU`, `TCP_MSS` |
| 6 | **CONFIG.INI runtime values** | `crates/config/src/lib.rs` + brain `protocol.py` | `behavior_server_ip`, `behavior_server_port`, `net_ip/gateway/mask`, `dhcp`, `link_encrypt`, `ml_enabled`, `motor_max_speed`, `watchdog_ms`, `pid_kp/ki/kd`, `ticks_per_m`, `wheel_base_mm`, `estop_gpio_pin`, `panic_reboot_ms`, `imu_offset_a{x,y,z}` |

**Real complexity**: building PHANES for "VF2 hardware + fleet profile + AEAD
link + secure-boot enforced" requires touching:
- `kernel/Cargo.toml` — features `vf2 + profile-fleet`
- `crates/drivers/Cargo.toml` — feature `vf2`
- `crates/ota/Cargo.toml` — feature `secure-boot-enforced` + `vf2`
- `crates/net/Cargo.toml` — propagation
- `~15 pub const` sites (or RFC-0023's `crates/limits/`)
- `build/CONFIG.INI` — `link_encrypt=1` etc.
- Verify nothing collides

This is the kind of pain Linux Kconfig was invented to solve.

### Why Linux-style Kconfig specifically

The mature alternatives:
- **Cargo features alone**: booleans only, no integers/strings, no
  validation rules, no inter-feature constraints, no presets.
- **Bare `build.rs` + env vars**: works but ad-hoc, no UI, no menu, no
  documentation generation.
- **Custom TOML reader**: same drawbacks, plus we'd reinvent menus.
- **CMake-style**: heavy, not Rust-native.
- **Bazel / Buck**: requires a complete build system migration.

**Linux Kconfig** (also used by Zephyr, U-Boot, OpenWrt, Buildroot) is the
proven approach for safety-critical embedded with:
- Declarative menu hierarchy in plain text.
- Constraints (`depends on`, `select`, `imply`).
- Per-option help text — single source of truth for documentation.
- Multiple frontends (terminal `menuconfig`, JSON output for tooling).
- Defconfigs (named presets) for common deployments.
- `oldconfig` upgrade path when options are added.

We adopt this verbatim, using the Python implementation [`kconfiglib`]
(BSD-licensed, no compiled deps, used by Zephyr).

## Detailed design

### Top-level structure

```
robot-os/
  Kconfig                       # root menu — sources sub-menus
  Kconfig.arch                  # architecture / ISA choice
  Kconfig.platform              # board / SoC choice
  Kconfig.profile               # embedded / edge / fleet
  Kconfig.limits                # caps from RFC-0023 (now driven by .config)
  Kconfig.timing                # WCET, scheduler, RTO, watchdog
  Kconfig.network               # MTU, MSS, buffers, ARP/keepalive
  Kconfig.security              # secure-boot, link-encrypt
  Kconfig.drivers               # which subsystems compiled in
  Kconfig.robot                 # wheeled/drone/humanoid/ackermann + defaults
  Kconfig.brain                 # default behavior_server values
  Kconfig.ota                   # slot count, max image, sig enforcement
  Kconfig.development           # debug probes, trace ring size

  defconfigs/
    embedded.config             # ESP32-C3 / microcontroller preset
    edge.config                 # default: VF2 / K1 / RK3588
    fleet.config                # gateway edge-server preset
    vf2.config                  # board: StarFive VisionFive 2
    k1.config                   # board: SpacemiT K1
    qemu.config                 # QEMU virt riscv64 dev
    qemu-aarch64.config         # QEMU virt aarch64 dev
    esp32c3.config              # ESP32-C3 specific

  .config                       # current selection (generated; gitignored)
  .config.old                   # backup from previous build

  Makefile                      # adds `config`, `menuconfig`, `defconfig-*`,
                                # `oldconfig` targets
  build.rs (workspace-level)    # reads .config → emits Rust consts + features

  crates/limits/                # consumes generated.rs
  crates/phanes-config/         # build-time reader + emitter
```

### Example Kconfig fragment

```kconfig
# Kconfig.profile
choice
    prompt "Deployment profile"
    default PROFILE_EDGE
    help
      Selects the default static cap sizes.  See RFC-0023 for the table.
      Individual caps can be overridden after picking a profile.

    config PROFILE_EMBEDDED
        bool "Embedded — microcontrollers (ESP32-C3 etc.)"
        select MAX_TASKS_32
        select TCP_MAX_CONNS_4
        select KERNEL_HEAP_SIZE_KB_256
        help
          For tiny SoCs with <1 MiB SRAM.  Designed for a single
          robot instance with minimal multi-app capability.

    config PROFILE_EDGE
        bool "Edge — single-robot SBC (VF2 / K1 / RK3588)"
        select MAX_TASKS_512
        select TCP_MAX_CONNS_128
        select KERNEL_HEAP_SIZE_MB_32
        help
          Default.  Sized for ~200 userspace apps + 100 sensor streams
          + brain link + OTA.  Memory budget ~45 MiB.

    config PROFILE_FLEET
        bool "Fleet — gateway / edge-server (many robots aggregated)"
        select MAX_TASKS_4096
        select TCP_MAX_CONNS_1024
        select KERNEL_HEAP_SIZE_MB_256
        help
          For a single PHANES instance acting as gateway for multiple
          downstream robots.  Memory budget ~256 MiB.
endchoice

# Allow per-option override of the profile defaults.
config MAX_TASKS
    int "Maximum schedulable tasks"
    range 16 16384
    default 32   if PROFILE_EMBEDDED
    default 512  if PROFILE_EDGE
    default 4096 if PROFILE_FLEET
    help
      Size of the task table.  Each task costs ~384 B of TCB plus its
      stack (USER_STACK_SIZE).  Affects scheduler walk WCET.
```

### Per-architecture and per-platform conditional configuration

The Kconfig hierarchy isn't flat — most values depend on architecture
**and** board.  We model this with the standard Kconfig `if` / `depends
on` / `default ... if ...` constructs (same idiom Linux uses for `if
ARM`, `if ARM64`, `if X86`).

**Example — stack sizes per architecture**:

```kconfig
config USER_STACK_SIZE_KB
    int "User-task stack size (KiB)"
    default 16  if ARCH_RISCV64
    default 32  if ARCH_AARCH64    # NEON/SVE save area is bigger
    default 48  if ARCH_X86_64     # XSAVE region (AVX/AVX-512) is bigger
    range 4 1024
    help
      Each user task gets this much virtual address space for its
      stack.  Lazy-allocated via demand paging — only the touched
      pages cost physical memory.  Sized to fit the largest FP
      register save context on the ISA plus ~4 KiB of guard.

config KERNEL_STACK_SIZE_KB
    int "Per-task kernel stack (KiB)"
    default 8   if ARCH_RISCV64 && PROFILE_EMBEDDED
    default 16  if ARCH_RISCV64
    default 32  if ARCH_AARCH64    # bigger trap frame
    default 32  if ARCH_X86_64
    range 4 256
    help
      Kernel-mode stack used when a user task syscalls into the
      kernel.  Sized for the deepest syscall handler chain.

config INTERRUPT_STACK_SIZE_KB
    int "Per-hart interrupt stack (KiB)"
    default 4   if ARCH_RISCV64
    default 8   if ARCH_AARCH64
    default 8   if ARCH_X86_64
    range 2 64
```

**Example — DTB / device-tree handling per platform**:

```kconfig
choice
    prompt "Device tree source"
    default DTB_BUILTIN if BOARD_VF2 || BOARD_K1
    default DTB_FROM_FIRMWARE if BOARD_QEMU
    default DTB_NONE if BOARD_ESP32C3
    help
      How the kernel obtains its FDT/DTB at boot.

    config DTB_NONE
        bool "No device tree (hard-coded platform constants)"
        depends on BOARD_ESP32C3 || BOARD_CUSTOM_NODT
        help
          Platforms without a DTB use the `platform.rs` cfg block
          for UART/PLIC/RAM addresses.

    config DTB_FROM_FIRMWARE
        bool "Pointer passed from firmware (a1 register on riscv64)"
        depends on ARCH_RISCV64
        help
          QEMU virt and most OpenSBI-style boots pass the DTB
          physical address in a1.  Kernel walks the FDT at boot.

    config DTB_BUILTIN
        bool "DTB embedded in kernel ELF (link-time blob)"
        help
          The board's .dts is compiled and linked into the kernel
          binary at a known symbol.  Boot ignores firmware-provided
          pointers.  Used for known-fixed hardware (VF2, K1).
endchoice

config DTB_BUILTIN_PATH
    string "Path to .dtb to embed"
    depends on DTB_BUILTIN
    default "boards/vf2.dtb" if BOARD_VF2
    default "boards/k1.dtb"  if BOARD_K1
    help
      Build-time path resolved relative to the kernel crate.
```

**Example — scheduler tick frequency depends on platform clock**:

```kconfig
config SCHED_HZ
    int "Scheduler tick frequency (Hz)"
    default 50    if BOARD_ESP32C3                     # 160 MHz CPU
    default 100   if PROFILE_EMBEDDED                  # minimal load
    default 250   if PROFILE_EDGE && BOARD_VF2         # JH7110 1.5 GHz
    default 250   if PROFILE_EDGE && BOARD_K1          # K1 1.6 GHz
    default 100   if PROFILE_EDGE && BOARD_QEMU        # TCG can't handle high
    default 500   if PROFILE_FLEET                     # gateway boards
    range 10 1000
    help
      Periodic timer interrupt rate, used for:
        - RR scheduler preemption tick
        - WDT kick
        - tickless lower bound (set_next_tick_smart)
      Higher SCHED_HZ = lower task-switch latency, more ISR cost.
      QEMU TCG amplifies ISR cost ~5×, so we keep it lower there.

config DEFAULT_QUANTUM_US
    int "Round-robin default quantum (μs)"
    default 10000 if PROFILE_EDGE
    default 5000  if PROFILE_FLEET    # tighter quantum at more tasks
    default 20000 if PROFILE_EMBEDDED
    range 100 1000000
```

**Example — cache line size per architecture** (matters for
`#[repr(align(N))]` on lock-free structures):

```kconfig
config CACHE_LINE_BYTES
    int
    default 64  if ARCH_RISCV64
    default 64  if ARCH_X86_64
    default 128 if ARCH_AARCH64    # Cortex-A72 L1D is 64, but L2 prefetcher works on 128
    help
      Sets `#[repr(align(N))]` on cross-CPU-shared atomics to avoid
      false sharing.  Not user-visible; `crates/sync/` reads it.
```

**Example — atomic guarantees per architecture**:

```kconfig
config ATOMIC_MAX_BYTES
    int
    default 8  if ARCH_RISCV64                  # rv64 amoswap.d = 8B max
    default 16 if ARCH_AARCH64 && HAS_LDXP_STXP # ARMv8.1+ has 16B atomics
    default 8  if ARCH_AARCH64
    default 16 if ARCH_X86_64                   # cmpxchg16b
    help
      Largest atomic-CAS the ISA supports.  Used by lock-free
      structures to decide between u64 packing and `SpinLock`-guarded
      wider state.

config HAS_RVV
    bool "RISC-V Vector Extension (V) present"
    default y if BOARD_K1                       # K1 has V1.0 hardware
    default n if BOARD_VF2                      # JH7110 does not
    default n if BOARD_ESP32C3
    default n if BOARD_QEMU                     # opt-in via --cpu rv64,v=true
    depends on ARCH_RISCV64
    help
      Selects RVV codegen paths in ml/, camera/, etc.

config HAS_NEON
    bool
    default y if ARCH_AARCH64
    depends on ARCH_AARCH64

config HAS_SSE2
    bool
    default y if ARCH_X86_64

config HAS_AVX2
    bool "x86_64: AVX2 (requires Haswell+)"
    depends on ARCH_X86_64
    default n
```

**Example — MMU page size per architecture**:

```kconfig
choice
    prompt "MMU page size"
    depends on !NO_MMU

    config MMU_PAGE_4K
        bool "4 KiB pages"
        default y
        help
          The only size riscv64 Sv39 supports.  Also the common
          choice on aarch64 and x86_64.

    config MMU_PAGE_16K
        bool "16 KiB pages (Aarch64 only)"
        depends on ARCH_AARCH64

    config MMU_PAGE_64K
        bool "64 KiB pages (Aarch64 only)"
        depends on ARCH_AARCH64
endchoice
```

**Example — Floating-point register set save area**:

```kconfig
choice
    prompt "Floating-point context size"
    default FP_HARDFLOAT_D if ARCH_RISCV64
    default FP_NEON        if ARCH_AARCH64
    default FP_XSAVE_AVX2  if ARCH_X86_64

    config FP_SOFTFLOAT
        bool "No FP save area (soft-float)"
        help
          Smallest TCB; only valid if no userspace code uses FP.

    config FP_HARDFLOAT_F     # 32 × f32 = 128 B
        bool "RISC-V F extension (32-bit single)"
        depends on ARCH_RISCV64

    config FP_HARDFLOAT_D     # 32 × f64 = 256 B
        bool "RISC-V D extension (64-bit double, default)"
        depends on ARCH_RISCV64

    config FP_RVV             # 32 × vlen-bit + control
        bool "RISC-V V extension (Vector)"
        depends on ARCH_RISCV64 && HAS_RVV

    config FP_NEON            # 32 × q-reg = 512 B
        bool "Aarch64 NEON"
        depends on ARCH_AARCH64

    config FP_SVE             # variable, up to 2048-bit
        bool "Aarch64 SVE"
        depends on ARCH_AARCH64

    config FP_XSAVE_SSE       # ~512 B
        bool "x86_64 SSE2 XSAVE region"
        depends on ARCH_X86_64

    config FP_XSAVE_AVX2      # ~1.2 KiB
        bool "x86_64 AVX2 XSAVE region"
        depends on ARCH_X86_64

    config FP_XSAVE_AVX512    # ~3 KiB
        bool "x86_64 AVX-512 XSAVE region"
        depends on ARCH_X86_64
endchoice
```

The FP save-area choice **automatically resizes the TCB** in
`crates/sched/src/task.rs` via a generated const:

```rust
// generated.rs
pub const FP_CTX_SIZE_BYTES: usize = 256;  // = FP_HARDFLOAT_D
```

**Example — WCET bounds per architecture × profile**:

```kconfig
config WCET_BOUND_TIMER_ISR_US
    int "Timer ISR worst-case execution time bound (μs)"
    default 10  if !BOARD_QEMU                       # real hw
    default 0   if BOARD_QEMU                        # TCG SMP artefact, disabled
    range 0 100000
    help
      Above this, the WCET subsystem logs a violation.  0 = disabled
      (used under QEMU because rdcycle measures wall-virtual-time,
      not work-time, under TCG SMP).

config WCET_BOUND_PID_US
    int "PID loop bound (μs)"
    default 50   if ARCH_RISCV64 && BOARD_K1         # 1.6 GHz + RVV
    default 80   if ARCH_RISCV64 && BOARD_VF2        # 1.5 GHz, no V
    default 200  if ARCH_RISCV64 && BOARD_ESP32C3    # 160 MHz
    default 30   if ARCH_AARCH64                     # cortex-a72 1.5 GHz
    default 25   if ARCH_X86_64                      # modern cores
    range 1 100000
```

### Hardware-aware constraints (compile-time refuse if invalid)

Kconfig + `build.rs` validation rejects nonsense combinations:

```python
# build.rs Python helper validation pass
assertions = [
    # ARCH × BOARD must be consistent
    ("BOARD_ESP32C3", "ARCH_RISCV64", "ESP32-C3 is RISC-V only"),
    ("BOARD_VF2",     "ARCH_RISCV64", "VF2 is RISC-V only"),
    ("BOARD_K1",      "ARCH_RISCV64", "K1 is RISC-V only"),
    # Profile × board memory closure
    ("PROFILE_FLEET",     not "BOARD_ESP32C3", "Fleet profile won't fit on ESP32-C3 SRAM"),
    ("PROFILE_EMBEDDED",  "KERNEL_HEAP_SIZE_KB <= 512", "Embedded profile demands ≤ 512 KiB heap"),
    # FP × arch
    ("FP_NEON",  "ARCH_AARCH64", "NEON requires Aarch64"),
    ("FP_RVV",   "HAS_RVV",      "RVV save area requires RVV present"),
    # MMU
    ("NO_MMU",   not "PROFILE_EDGE", "edge/fleet profiles require MMU"),
    # Crypto
    ("LINK_AEAD_DEFAULT_ON", "LINK_PSK_PATH != \"\"", "AEAD enabled needs PSK path"),
    # PMP
    ("PMP_REGIONS_RESERVED + PMP_REGIONS_PER_USER * MAX_ISOLATED_PROCS <= 16",
     "PMP region budget exceeds RV64 M-mode hardware limit"),
]
```

If any fails, `make build` aborts with a clear error pointing at the
violating Kconfig option pair.

### Updated configuration categories (full list)

Adding what was missing in the first pass — per-arch / per-platform
items the previous draft glossed over:

#### Architecture details (new section)

```
Architecture
  ISA: [RISC-V 64 | Aarch64 | x86_64]
  Atomic max bytes (auto-computed)
  Cache line bytes (auto-computed)

  RISC-V specifics (if ARCH_RISCV64)
    Base extensions: I, M, A, C (always)
    Optional: F, D, V, Zaamo, Zalrsc, Zicbom, Zicboz
    Privilege levels: M+S+U (default) | M+U (no-mmu mode)
    PMP regions (hardware-fixed at 16): [reserved_for_kernel: 4]

  Aarch64 specifics (if ARCH_AARCH64)
    Page size: [4 KiB | 16 KiB | 64 KiB]
    Exception level boot entry: [EL3 | EL2 (default) | EL1]
    PAC: [off | y]
    BTI: [off | y]
    SVE: [off | y]

  x86_64 specifics (if ARCH_X86_64)
    Page size: 4 KiB (mandatory)
    XSAVE region: [SSE2 | AVX2 | AVX-512]
    SMEP / SMAP / UMIP: y/n each
    KPTI (Meltdown mitigation): y/n
    LA57 (5-level paging): n (default)
```

#### Memory layout (new section)

```
Memory Layout
  Kernel link address: 0x80200000 (riscv64) / per-arch
  Page size: [4 KiB | 16 KiB | 64 KiB]
  Kernel heap size: <profile-dependent>
  Kernel stack per hart: <arch-dependent>
  Interrupt stack per hart: <arch-dependent>
  User task default stack: <arch-dependent>
  User stack guard pages: 1
  Boot identity-map: enabled
  HHTM (High-Half Trampoline) addr: 0xffff_ffff_8020_0000
```

#### Boot path (new section)

```
Boot
  Bootloader contract: [OpenSBI (default) | no-opensbi | UEFI | direct-from-rom]
  Multi-hart boot: [primary-only (default) | all-harts-park-WFI]
  Boot ELF format: [embed-mlp/policy | strip-debug | full-debug]
  Initial PMP install: [yes (no-opensbi) | no (OpenSBI does it)]
  Auto-MMU enable at boot: [yes if !NO_MMU]
```

#### Filesystem (new section)

```
Filesystem
  Root FS: [FAT32 (default) | TMPFS | none]
  FAT32 cluster size: 4 KiB
  Disk image path: /dev/virtio-blk-0 (qemu) | /dev/mmcblk0 (vf2)
  Max open files (system-wide): <profile-dependent>
  Max open files per process: <profile-dependent>
  Mount-point reserve: 4 mount slots
```

#### Power management (new section)

```
Power
  Idle policy: [WFI (default) | poll-with-pause | sleep-with-wake-on-IRQ]
  Tickless idle: y
  Power gating regions: depends on BOARD
  Suspend-to-RAM (P01): n (default; opt-in)
```

#### Per-driver configuration (expanded)

```
Drivers
  UART
    Driver: [16550 (qemu) | DesignWare DW8250 (vf2) | K1 UART (k1)]
    Baud: 115200 (default) | 9600 | 38400 | 230400
    RX buffer: 128 B
    TX FIFO threshold: 4 B

  GPIO
    Driver: [SiFive (qemu/vf2) | K1 GPIO (k1) | ESP32C3 GPIO]
    Max pins: 64 (default) | per-board

  Camera (CSI)
    Driver: [None | JH7110 CSI | K1 ISP | mock]
    Default resolution: 320×240 | 640×480 | 1920×1080 | 4K
    Default format: GRAY8 | JPEG | NV12
    Frame rate cap: 30 fps

  Network
    PHY driver: [VirtIO net | MACB (vf2) | K1 GMAC | ESP32C3 WiFi+TCP]
    RX descriptor ring size: 64
    TX descriptor ring size: 64
    Promiscuous mode: n
```

The point is: **the same option name can take different defaults
across (arch, board, profile) triples**, and Kconfig's `default ... if
...` syntax captures exactly that without scattering the logic across
13 Cargo.toml files.

### Categories in detail

#### Architecture & platform

```
General Settings
  Profile: [embedded | edge (default) | fleet]
  Log level: [error | warn | info | debug | trace]

Architecture
  ISA: [riscv64 (default) | aarch64 | x86_64]
  RISC-V extensions: [V (Vector) | Zaamo | Zalrsc | Zicbom | Zicboz]
  Aarch64 extensions: [NEON | SVE | PAC]

Platform / Board
  Board: [QEMU virt (default) | VF2 | K1 | ESP32-C3 | RK3588 (TBD) | Generic]
  if Generic:
    UART base address: 0x...
    PLIC/CLINT base: 0x...
    TIMER_FREQ (Hz): 10000000
    RAM base / size: 0x80000000 / 0x... MiB
  Bootloader: [None / OpenSBI / UEFI / no-opensbi]
  MMU: [Sv39 | Sv48 | no-mmu]
```

#### Resource limits (subsumes RFC-0023)

Every cap from RFC-0023 becomes a Kconfig integer option with:
- profile-derived default
- range constraint (min, max)
- help text explaining what determines the size
- inter-option constraints where relevant
  (e.g. `MAX_FDS_TOTAL ≥ MAX_TASKS × MAX_FDS_PER_PROC / 4`)

```
Resource Limits
  Schedulable tasks (MAX_TASKS): 512
  TCP connections (TCP_MAX_CONNS): 128
  UDP sockets (MAX_SOCKETS): 256
  FDs per process (MAX_FDS_PER_PROC): 64
  Global FD table (MAX_FDS_TOTAL): 2048
  IPC channels (MAX_CHANNELS): 512
  IPC pipes (MAX_PIPES): 512
  IPC ports (MAX_PORTS): 256
  IPC leases (MAX_LEASES): 128
  Named services (MAX_SERVICES): 256
  Pub/sub topics (MAX_TOPICS): 256
  Subscribers per topic (MAX_SUBS_PER_TOPIC): 32
  Capability pool (MAX_CAPS_TOTAL): 16384
  Capability kinds (MAX_CAP_KINDS): 16  # 4-bit until RFC-0025 ABI v2
  Kernel heap (KERNEL_HEAP_SIZE): 32 MiB
  User task stack (USER_STACK_SIZE): 16 KiB
```

#### Timing & scheduling

```
Timing
  Scheduler tick rate (SCHED_HZ): 100 Hz
  Default RR quantum (DEFAULT_QUANTUM_US): 10000
  Time slice ticks (TIME_SLICE_TICKS): 1
  RT priority levels: 32

WCET Budgets (per RFC-0004 SoftRT class)
  pid_loop bound: 50 μs
  sensor_read bound: 100 μs
  ctx_switch bound: 5 μs
  timer_isr bound (hw): 10 μs
  timer_isr bound (qemu): 0 (disabled — TCG artefact)
  actuator_write bound: 10 μs

Watchdog
  Hardware WDT timeout: 500 ms
  RT motor cmd timeout: 500 ms
  Safety comms timeout: 5 s
  Drone comms timeout: 3 s
  Panic reboot delay: 5 s

Offline / Reconnect
  Reconnect interval: 5 s
  Buzzer beep on alert: 500 ms
```

#### Network

```
Network
  Ethernet MTU: 1500
  TCP MSS: 1460
  TCP receive buffer per conn: 128 KiB
  TCP advertised window: 64 KiB (header limit)
  CWND initial: 2 × MSS
  ssthresh initial: TCP_BUF_SIZE
  RTO initial: 1000 ms
  RTO min: 200 ms
  RTO max: 60000 ms
  Retx max attempts: 8
  Keepalive interval: 30 s
  Keepalive max probes: 3
  Dup-ACK fast-retransmit threshold: 3
  Out-of-order reassembly slots: 4
  TIME_WAIT duration: 2000 ms
  ARP cache size: 16

  Brain link defaults
    Default server IP / port: 10.0.2.2 : 9000
    Auto-reconnect: yes
    Subsumption layers enabled: L0+L1+L2+L3

  Multi-stream (RFC-0021)
    Max stream count: 16
    Camera stream count: 8
    LiDAR / audio stream slots: reserved

  Crypto link (RFC-0019)
    AEAD enabled by default: no
    PSK file path: /fat/LINK.KEY
    PSK rotation cadence: annual + 24h overlap (TBD)
```

#### Drivers

```
Drivers
  UART [y]
  GPIO [y]
  I²C [y]
  PWM [y]
  Motor PID [y]
  IMU (MPU-6050/9250/ICM-20948) [y]
  Barometer (BMP280/BMP388) [y]
  ADS1115 [y]
  INA219 [y]
  Buzzer [y]
  CSI camera [y]   (depends on board)
  GPS UART [y]
  ML / CNN inference [y]   (Disable with --features no-ml)
  NPU accelerator [y]      (auto-disabled if not present)
  Rangefinder ultrasonic [y]
  Rangefinder ToF VL53L0X [y]
  USB MSC gadget (DEV03) [y if board has USB-OTG]
  USB DFU recovery (DEV02) [y if board has USB-OTG]
  VirtIO blk/net (QEMU) [y if PLATFORM_QEMU]
  MACB Ethernet (VF2) [y if PLATFORM_VF2]

Driver isolation defaults
  Default isolation level: InKernel | UserProcess
  Critical drivers (must stay InKernel): UART, Motor PID
```

#### Robot configuration

```
Robot
  Type: [Wheeled (default) | Drone | Humanoid | Ackermann]
  Encoder ticks per metre: 1000   (overridable runtime)
  Wheel base: 200 mm
  Motor max speed (default): 100
  IMU offsets default: 0,0,0

  PID defaults
    Kp: 1.0  Ki: 0.0  Kd: 0.0
```

#### Security

```
Security
  Secure boot
    Enforce signature verification: [ y | n (dev only) ]
    Production key fingerprint: <hex from prod_pub.bin>
    Anti-rollback floor (CFG_FW_VERSION_MIN): 1

  Link layer
    AEAD encryption (RFC-0019): [ enabled | opt-in (default) | disabled ]
    Auth envelope (HMAC) required: yes
    Replay window: strict-monotonic

  PMP regions reserved for M-mode: 4
  Stack canary: on
  W^X enforcement: on
```

#### OTA

```
OTA
  Active slots: 2 (A/B)
  Recovery slot: 1
  Max image size: 8 MiB
  Signature scheme: Ed25519
  Signature mandatory: [ yes / no (dev) ]
  Boot count threshold (rollback): 3
```

#### Development / Debug

```
Development
  TCP per-segment trace probes (RFC-0021 debug): [ off | qemu-only | always ]
  Trace event ring buffer size: 512
  Verbose kprintln: [ off (default) | on ]
  WCET reporting interval: never | on shutdown | periodic
  Panic action: halt | reboot after Nms | dump and halt
```

### Generated artefacts

`build.rs` reads `.config` and emits:

1. **`crates/limits/src/generated.rs`** — every numeric / string option:
   ```rust
   // GENERATED — do not edit.  Source: .config (run `make config` to change).
   pub const MAX_TASKS: usize = 512;
   pub const TCP_MAX_CONNS: usize = 128;
   pub const SCHED_HZ: u32 = 100;
   pub const KERNEL_HEAP_SIZE: usize = 32 * 1024 * 1024;
   // … ~150 constants total …
   ```

2. **Cargo `cfg` flags** via env: each `bool` Kconfig option becomes a
   `cfg(option_name)` attribute consumable by Rust source:
   ```rust
   #[cfg(feature_aead_enabled_by_default)]
   fn default_link_mode() -> LinkMode { LinkMode::Encrypted }
   ```

3. **`Cargo.toml` features**: existing crate-level features (`vf2`,
   `qemu`, `esp32c3`, …) are activated by Kconfig via a small build-script
   shim that sets `CARGO_FEATURE_*` env variables.  Crates that already
   use `#[cfg(feature = "vf2")]` keep working — they're now driven by the
   Kconfig choice, not by direct `cargo build --features` invocation.

4. **CONFIG.INI defaults** at boot: a small subset of options is also
   emitted as a CONFIG.INI default template for runtime override (the
   keys today in `cfg_get_u32`).  Kconfig becomes the **source of
   compile-time defaults**; CONFIG.INI is **per-deployment runtime
   override** for things that genuinely vary (IP address, calibration).

### Makefile changes

```makefile
KCONFIG_CONFIG ?= .config
KCONFIG_DEFCONFIG_DIR := defconfigs

# Interactive config menus (ncurses)
.PHONY: menuconfig
menuconfig:
	@python3 -m kconfiglib.menuconfig

.PHONY: config
config:
	@python3 -m kconfiglib.guiconfig 2>/dev/null \
		|| python3 -m kconfiglib.menuconfig

.PHONY: nconfig oldconfig olddefconfig
nconfig:
	@python3 -m kconfiglib.menuconfig --style=nconfig
oldconfig:
	@python3 -m kconfiglib.oldconfig
olddefconfig:
	@python3 -m kconfiglib.olddefconfig

# Named profile presets
.PHONY: defconfig-embedded defconfig-edge defconfig-fleet \
        defconfig-vf2 defconfig-k1 defconfig-qemu defconfig-esp32c3
defconfig-%:
	@cp $(KCONFIG_DEFCONFIG_DIR)/$*.config $(KCONFIG_CONFIG)
	@python3 -m kconfiglib.olddefconfig
	@echo "[CONFIG] active = $*"

.PHONY: savedefconfig
savedefconfig:
	@python3 -m kconfiglib.savedefconfig --out defconfigs/last_saved.config

# The build now depends on .config existing; absent → fall back to edge
$(KCONFIG_CONFIG):
	@$(MAKE) defconfig-edge

build: $(KCONFIG_CONFIG)
	@$(CARGO) build $(CARGO_FLAGS) $$(python3 tools/kconfig_to_cargo.py)
```

`tools/kconfig_to_cargo.py` translates the `.config` to cargo args
(`--features vf2,qemu --target riscv64imac-unknown-none-elf` etc.).

### Validation & invariants

Kconfig syntax supports constraints — we use them:

```kconfig
config TCP_MAX_CONNS
    int "TCP connections"
    range 1 65536
    default 128
    help
      Must be ≤ MAX_SOCKETS.

config MAX_SOCKETS
    int "All sockets"
    range 1 65536
    default 256
    help
      Must be ≥ TCP_MAX_CONNS + reserved UDP slots.

# Cross-option check via build.rs:
# if MAX_SOCKETS < TCP_MAX_CONNS:
#     fail compilation
```

`build.rs` runs a validation pass — invariants we'll enumerate:
- `MAX_SOCKETS ≥ TCP_MAX_CONNS + 4` (4 UDP reserve slots, per Kconfig help)
- `MAX_FDS_TOTAL ≥ MAX_TASKS × 4` (average FDs/task budget)
- `MAX_FDS_PER_PROC ≤ MAX_FDS_TOTAL` (no task can starve the pool)
- `MAX_CAPS_TOTAL ≥ MAX_TASKS × 8` (per-app cap budget)
- `MAX_CAPS_PER_TASK ≤ MAX_CAPS_TOTAL` (cap-table parallel to the FD ceiling)
- `KERNEL_HEAP_SIZE ≥` sum of static-table footprints (auto-computed)
- `PMP_REGIONS_RESERVED ≤ 12` (hardware caps at 16, leave headroom)
- `BOARD_VF2 / BOARD_K1 / BOARD_ESP32C3 ⇒ ARCH_RISCV64`
- `BOARD_ESP32C3 ⇒ KERNEL_HEAP_SIZE ≤ 512 KiB` (ESP32-C3 SRAM)
- `PROFILE_FLEET ⊻ BOARD_ESP32C3` (fleet profile won't fit ESP32-C3 SRAM)
- `FP_NEON ⇒ ARCH_AARCH64` (NEON is an Aarch64 feature)
- `FP_RVV ⇒ HAS_RVV` (RVV needs the V extension on the target SoC)
- `LINK_AEAD_DEFAULT_ON ⇒ LINK_PSK_PATH non-empty`
- `SECURE_BOOT_ENFORCED ⇒ OTA_SIG_MANDATORY` (anti-downgrade)
- `WCET_MAX_POINTS ≥ 9` (kernel hardcodes 9 fixed points pid_loop..path_plan)
- `MAX_SUBS_PER_TOPIC ≤ MAX_TASKS` (more sub slots than tasks just wastes BSS)

The list reflects the actual `crates/limits/build.rs` implementation
(landed across RFC-0026, RFC-0027 and follow-ups).  Adding a new
invariant: drop another `if/panic!` block into `run_validations()`
and append the rule here.

### Migration plan

Done in **6 phases over ~5-7 working days**.  Each phase is committable
on its own; the build never breaks.

| Phase | Scope | Effort | Risk |
|-------|-------|--------|------|
| **C1 Skeleton** | Create `crates/phanes-config/` build-time crate.  Add `tools/kconfig_to_cargo.py`.  Add `make menuconfig` target.  Author the **Kconfig menu file** as the *single source of truth*, populated initially with all currently-existing options at their current values.  Build still uses the old constants — phase 1 is purely additive. | 1.5 d | low |
| **C2 Generated consts** | Add the workspace-level `build.rs` that reads `.config` and emits `crates/limits/src/generated.rs`.  Generate *but don't yet use* — old `pub const`s in 15 files are kept side-by-side.  Verify byte-identical output via `diff <generated> <expected>`. | 1 d | low |
| **C3 Source migration** | Replace each of the ~15 existing `pub const` sites with `pub use robot_os_limits::*`.  One PR per subsystem (sched, net, ipc, fs, …) so reverts are clean.  After this phase, every cap is Kconfig-driven. | 1.5 d | medium (test churn) |
| **C4 Feature unification** | Migrate cargo features (`vf2`, `qemu`, etc.) to Kconfig bools.  `tools/kconfig_to_cargo.py` emits the `--features` arg list.  Existing `#[cfg(feature = "vf2")]` gates keep working unchanged. | 1 d | medium (cross-crate feature propagation) |
| **C5 Defconfigs** | Author `defconfigs/{embedded,edge,fleet,vf2,k1,qemu,qemu-aarch64,esp32c3}.config`.  Each captures the current "this build works on this target" knowledge that's today implicit in the cargo invocation. | 0.5 d | low |
| **C6 CI integration** | Replace the `cargo build --features X` matrix in `.github/workflows/ci.yml` with `make defconfig-X && make build` invocations.  All 6 build configs become 6 defconfigs.  Add a `make oldconfig` consistency check to catch stale `.config`. | 1 d | low |
| **C7 Documentation** | Write `docs/CONFIG.md` — a tutorial on `make menuconfig`, the option tree, and how to add a new option to Kconfig.  Update `AGENTS.md` to point at it. | 0.5 d | none |

**Total: ~7 days** for a single developer.  Parallelisable across the
subsystem boundaries (sched, net, ipc, fs could go to different agents
in C3).

### What this RFC does NOT change

- **Runtime CONFIG.INI**: stays as is.  Used for per-deployment values
  that genuinely vary across identical kernel images (IP address,
  calibration offsets).  This RFC adds Kconfig as the source of
  **compile-time defaults**, not a replacement for runtime config.
- **Brain (Python)**: brain side has no static-array problem; it stays
  on pyproject + env vars.  This RFC is kernel/Rust only.
- **Target triple selection mechanism**: still through Cargo, but now
  picked by Kconfig and emitted as a `--target` arg by the build
  wrapper.  Cleaner than `.cargo/config.toml` files scattered across
  10 crates.
- **The safety case (RFC-0017)**: every Kconfig value is still a
  compile-time constant burnt into the binary.  No runtime selection,
  no heap, no WCET surprise.  Auditors can review one `.config` file
  to know everything about a deployed binary.

## Drawbacks

- **Dependency on `kconfiglib`** (Python).  Lightweight (single
  package, no compiled deps, BSD-licensed, used by Zephyr and Buildroot),
  but it's a developer-tool dependency.  Mitigation: vendor it under
  `tools/kconfiglib/` if the upstream maintenance ever lapses.
- **Cognitive load for new contributors**: "where do I add this knob?"
  becomes "edit the right Kconfig fragment".  Mitigation: `docs/CONFIG.md`
  + per-option `help` text.
- **`.config` is gitignored** so deployment configs aren't tracked
  per-tree.  Mitigation: the `defconfigs/*.config` files ARE tracked,
  and `make savedefconfig` produces a minimal patch.  Each deployment
  pins its `.config` (and the SHA of the binary it produced).
- **Cargo features still exist underneath**: this RFC unifies the
  *interface*, not the *plumbing*.  Cargo features remain the
  underlying mechanism Rust's compiler honours.  We're translating
  Kconfig → Cargo args, not eliminating Cargo features.
- **Migration churn**: ~15 const sites edited in C3, ~14 Cargo.toml
  files edited in C4.  Each is a small diff but the total is
  noticeable.  CI must stay green throughout each phase.

## Rationale and alternatives

**Alternative A — pure cargo features**: rejected.  No integer
options, no validation, no help text, no menu, no presets.  Doesn't
solve the audit problem.

**Alternative B — TOML config + bespoke reader**: tempting but
reinvents Kconfig poorly.  Same effort to author menus, but without
the mature TUI / validation toolchain.

**Alternative C — Bazel/Buck**: heavy, requires migrating the entire
build system away from Cargo.  Disproportionate.

**Alternative D — JSON Schema + custom UI**: same shape as B.

**Alternative E (chosen) — Linux Kconfig via kconfiglib**: proven,
documented, used by safety-critical projects (Zephyr, NXP MCUXpresso,
ChromeOS EC).  Tool is plain Python, runs on macOS / Linux / Windows.
Output is plain text (`.config`) auditable in PR diffs.

## Unresolved questions

- **Should `.config` be checked in or always gitignored?**  Linux
  gitignores `.config`; Buildroot ships per-board `defconfig` files
  and gitignores `.config`.  Proposal: gitignore `.config`, track only
  `defconfigs/*.config`.
- **How does this interact with brain repo?**  Brain is Python; no
  Kconfig needed there.  But the brain *consumes* certain kernel
  defaults (e.g. `MAX_CHANNELS` for ActuatorCmd format).  Resolution: a
  small `tools/export_kernel_consts.py` runs as part of the build and
  emits a Python file `protocol_consts.py` the brain imports.
- **Per-app sub-caps**: RFC-0023 mentioned per-app cap budgets.  Should
  those also be Kconfig-driven?  Probably yes — same mechanism.  Defer
  to RFC-0027 or fold into this.
- **Hot-reload of CONFIG.INI**: today some values are runtime.  Should
  Kconfig generate the set of "this is runtime-overridable" vs
  "this is compile-time only"?  Yes — proposed semantics:
    - `int "X" if RUNTIME_OVERRIDABLE` → also emits CONFIG.INI key
    - `int "X"` without that suffix → compile-time only.

## Future possibilities

- **Per-board pinned defconfigs in CI**: every supported board has its
  defconfig committed; CI builds each one nightly.  A new board
  contribution = adding a `defconfigs/{board}.config` plus
  `Kconfig.platform` entry.
- **Auto-generated documentation**: kconfiglib has a `--genrst` mode
  that emits ReStructuredText from the menu.  We get the kernel
  configuration reference doc for free.
- **Cross-build matrix expansion**: `make ci-full` could iterate over
  all defconfigs automatically and build them all in parallel.
- **Brain-side mirror**: extend the Python `tools/export_kernel_consts.py`
  to also drive brain-side timeouts, packet size limits, etc. so the
  two repos stay byte-aligned by construction.

## Prior art

- **Linux**: `make menuconfig` since v2.5 (2003).  Kconfig syntax is the
  canonical reference for this design.
- **Zephyr RTOS**: identical model (`prj.conf` is auto-generated from
  Kconfig).  Heavily used in safety-critical embedded.
- **U-Boot**: same; per-board defconfigs.
- **OpenWrt**: same; many thousands of options.
- **Buildroot**: same.
- **ChromiumOS EC** firmware: same, with Zephyr underneath.
- **NXP MCUXpresso**, **STM32CubeMX**: same idea, vendor-specific GUI.

[`kconfiglib`]: https://github.com/ulfalizer/Kconfiglib
