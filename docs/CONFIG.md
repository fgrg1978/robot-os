# PHANES Build Configuration Reference

> **Audience:** developers onboarding to the PHANES kernel who need to build for a specific
> board, tune resource limits, add a new configuration option, or debug a build failure that
> traces back to a misconfigured `.config`.
>
> **RFC reference:** [RFC-0026 — PHANES Unified Build Configuration (Kconfig-style)](../rfcs/RFC-0026-phanes-config-kconfig-style.md)

---

## 1. TL;DR / Quick start

```bash
# Pick a named preset (edge is the default):
make defconfig-edge        # VF2 / K1 / RK3588 single-robot SBC
make defconfig-qemu        # QEMU virt riscv64 — local development

# Optionally tweak with an interactive terminal UI:
make menuconfig

# Build:
make
```

That's it.  One command to select a target, one to build.  No manual editing of
`Cargo.toml` files or scattered `pub const` sites.

### Named defconfigs at a glance

| Defconfig | Target / use case | When to use |
|-----------|-------------------|-------------|
| `embedded` | Generic <1 MiB SRAM microcontrollers | Tiny SoCs; minimal resource budget |
| `edge` | Single-robot SBC: VF2, K1, RK3588 **(default)** | Everyday single-robot development and production |
| `fleet` | Gateway/edge-server aggregating many downstream robots | Fleet orchestration nodes; 1000+ robot deployments |
| `vf2` | StarFive VisionFive 2 board-specific tuning | VF2 hardware; builds on top of `edge` |
| `k1` | SpacemiT K1 board-specific tuning | K1 hardware; enables RVV vector extension |
| `qemu` | QEMU virt riscv64 (TCG) | Local development; no hardware required |
| `qemu-aarch64` | QEMU virt aarch64 | Aarch64 development and CI |

> **First time on any board?**  Start with `make defconfig-<board>` (e.g. `defconfig-vf2`),
> which gives you all the board-specific defaults.  Use `make defconfig-qemu` if you have no
> hardware yet.

---

## 2. How configuration flows from `.config` to a built kernel

Before diving into individual options it helps to see how all the pieces connect.

```
Developer action
     │
     ▼
┌───────────────┐   make defconfig-*         ┌──────────────────────┐
│  defconfigs/  │ ──────────────────────────▶│       .config        │
│ edge.config   │                            │  (workspace root,    │
│ vf2.config    │                            │   gitignored)        │
│ qemu.config   │                            └──────────┬───────────┘
│   ...         │   make menuconfig                     │
└───────────────┘ ◀──────────────────────              │  read by
                                                        ▼
┌───────────────────────────┐              ┌────────────────────────────┐
│  Kconfig.*  (menu files)  │              │  crates/phanes-config/     │
│  Kconfig                  │              │  (workspace build.rs)      │
│  Kconfig.arch             │              └──────────┬─────────────────┘
│  Kconfig.platform         │                         │
│  Kconfig.profile          │                         │  emits
│  Kconfig.limits           │                         ▼
│  Kconfig.timing           │       ┌──────────────────────────────────────┐
│  Kconfig.network          │       │  crates/limits/src/generated.rs      │
│  Kconfig.security         │       │  pub const MAX_TASKS: usize = 512;   │
│  Kconfig.drivers          │       │  pub const TCP_MAX_CONNS: usize = 128;│
│  Kconfig.robot            │       │  pub const SCHED_HZ: u32 = 100;      │
│  Kconfig.brain            │       │  // ~150 constants total             │
│  Kconfig.ota              │       └──────────────────────────────────────┘
│  Kconfig.development      │                         │
└───────────────────────────┘                         │  also emits
                                                       ▼
                                       ┌──────────────────────────────┐
                                       │  tools/kconfig_to_cargo.py   │
                                       │  translates .config to:      │
                                       │  --features vf2,qemu,...     │
                                       │  --target riscv64imac-...    │
                                       └──────────────┬───────────────┘
                                                       │
                                                       ▼
                                            cargo build <args>
                                                       │
                                                       ▼
                                              kernel ELF binary
```

**Key facts:**

- `.config` is a flat text file (`CONFIG_FOO=value` one per line).  It is **gitignored** — only
  the `defconfigs/*.config` presets are committed.
- `build.rs` reads `.config` and writes `crates/limits/src/generated.rs` — a Rust source file
  containing every numeric and boolean option as a `pub const`.  Crates consume these via
  `pub use robot_os_limits::*`.
- Boolean Kconfig options that map to existing cargo features are passed through
  `tools/kconfig_to_cargo.py`, which assembles the `cargo build --features ...` arguments.
  Existing `#[cfg(feature = "vf2")]` guards in Rust code continue to work unchanged.
- `CONFIG.INI` (the runtime key-value store read at boot from the FAT32 partition) is a
  **separate** mechanism for per-deployment values that genuinely vary across identical kernel
  images: IP address, PID calibration, encoder offsets.  Kconfig is the source of
  *compile-time* defaults; `CONFIG.INI` is *runtime* override.

---

## 3. Configuration categories

The Kconfig menu is split across thirteen fragment files.  Each is described below.

### 3.1 General (`Kconfig.profile`)

Controls the top-level deployment profile and verbosity:

- **Profile** (`PROFILE_EMBEDDED` / `PROFILE_EDGE` / `PROFILE_FLEET`): a `choice` that selects
  default values for nearly every other option.  Pick `edge` for a single-robot SBC, `fleet`
  for a gateway node that aggregates many robots, `embedded` for microcontrollers.  Individual
  options can be overridden after choosing a profile (see §4).
- **Log level** (`LOG_LEVEL_ERROR` … `LOG_LEVEL_TRACE`): sets the verbosity of kernel
  `kprintln!` output.  `info` is the default; `trace` is very chatty and useful during bringup.

### 3.2 Architecture (`Kconfig.arch`)

Selects the ISA and per-ISA extension set:

- **ISA choice** (`ARCH_RISCV64` / `ARCH_AARCH64` / `ARCH_X86_64`): determines the Cargo
  target triple, FP context layout, cache-line size, MMU page size defaults, and WCET bounds.
- **RISC-V extensions** (`HAS_RVV`, `HAS_ZICBOM`, `HAS_ZICBOZ`, …): each maps to a hardware
  capability.  `HAS_RVV` is auto-enabled for the K1 board (which ships RVV 1.0 hardware) and
  disabled for VF2 (JH7110 has no V extension).
- **Aarch64 extensions** (`HAS_NEON`, `HAS_SVE`, `HAS_PAC`, `HAS_BTI`): auto-set from the
  board choice.  SVE and PAC are off by default; enable them for Armv8.2+ cores.
- **x86_64 mitigations** (`HAS_SMEP`, `HAS_SMAP`, `HAS_UMIP`, `KPTI`): each toggles a CPU
  security feature.  All are off by default; check your threat model.
- **FP context size** (`FP_HARDFLOAT_D` / `FP_NEON` / `FP_XSAVE_AVX2` / …): controls the
  floating-point save/restore area size in each task's TCB.  The choice is auto-derived from
  the ISA; override only if you know you need a different ABI.
- **Atomics and cache line** (`ATOMIC_MAX_BYTES`, `CACHE_LINE_BYTES`): computed from the ISA
  choice; not normally set manually.

### 3.3 Platform / Board (`Kconfig.platform`)

Selects the target board and its hardware constants:

- **Board choice** (`BOARD_QEMU` / `BOARD_VF2` / `BOARD_K1` / `BOARD_CUSTOM`):
  sets `UART_BASE`, `PLIC_BASE`, `TIMER_FREQ`, `RAM_BASE`, and `RAM_SIZE` to board-specific
  defaults.  For `BOARD_CUSTOM`, each constant is manually configurable.
- **Device tree source** (`DTB_NONE` / `DTB_FROM_FIRMWARE` / `DTB_BUILTIN`): controls how the
  kernel obtains its FDT.  QEMU passes the DTB in register `a1` at boot; VF2 and K1 use a
  built-in DTB linked into the kernel ELF.  `DTB_NONE` is for boards using hard-coded constants
  instead of a DTB.
- **MMU mode** (`MMU_SV39` / `MMU_SV48` / `NO_MMU`): Sv39 is the default for RISC-V.
  `NO_MMU` compiles out the MMU subsystem entirely (used for bare-metal debug and other
  no-MMU targets).
- **Bootloader contract** (`BOOTLOADER_OPENSBI` / `NO_OPENSBI` / `BOOTLOADER_UEFI`): selects
  whether the kernel expects OpenSBI below it, handles PMP itself, or is loaded by a UEFI
  firmware.

### 3.4 Resource limits (`Kconfig.limits`)

Every static array cap that existed as a scattered `pub const` is now a Kconfig integer option
here.  Defaults are set by the profile choice; individual values can be overridden.

See §4 for per-profile values and RFC-0023 for the full rationale of each cap.

Key options: `MAX_TASKS`, `TCP_MAX_CONNS`, `MAX_SOCKETS`, `MAX_FDS_PER_PROC`,
`MAX_FDS_TOTAL`, `MAX_CHANNELS`, `MAX_PIPES`, `MAX_PORTS`, `MAX_LEASES`,
`MAX_SERVICES`, `MAX_TOPICS`, `MAX_SUBS_PER_TOPIC`, `MAX_CAPS_TOTAL`,
`KERNEL_HEAP_SIZE`, `USER_STACK_SIZE_KB`, `KERNEL_STACK_SIZE_KB`.

All limits are **compile-time constants**.  There is no runtime resize.  Exceeding a limit
returns an error to the caller; there is no heap fallback.  This is intentional: it preserves
WCET predictability and ISO 26262 cert-eligibility (RFC-0017, RFC-0023).

### 3.5 Timing and scheduling (`Kconfig.timing`)

Controls the scheduler, WCET analysis, and watchdog:

- **`SCHED_HZ`**: tick rate.  250 Hz for VF2/K1 (1.5–1.6 GHz cores), 100 Hz for QEMU
  (TCG amplifies ISR cost ~5×).
- **`DEFAULT_QUANTUM_US`**: round-robin quantum.  10 ms on edge, 5 ms on fleet (more tasks →
  tighter slicing), 20 ms on embedded.
- **WCET bounds** (`WCET_BOUND_TIMER_ISR_US`, `WCET_BOUND_PID_US`, `WCET_BOUND_SENSOR_US`,
  `WCET_BOUND_CTX_US`, `WCET_BOUND_ACTUATOR_US`): upper bounds the WCET subsystem enforces.
  Each is per-arch and per-board because real hardware has different clock rates.
  `WCET_BOUND_TIMER_ISR_US = 0` under QEMU (disabled) because `rdcycle` under TCG-SMP
  measures wall-virtual-time rather than work-time, producing false violations.
- **Watchdog** (`WDT_TIMEOUT_MS`, `MOTOR_CMD_TIMEOUT_MS`, `SAFETY_COMMS_TIMEOUT_MS`):
  hardware and software watchdog timers.  All are profile-dependent.
- **Offline reconnect** (`OFFLINE_RECONNECT_INTERVAL_TICKS`): how often the kernel attempts to
  re-establish the brain link after a disconnect.

### 3.6 Network (`Kconfig.network`)

Wire protocol constants for the TCP/IP stack:

- `ETH_MTU`, `TCP_MSS`, `TCP_BUF_SIZE`, `ARP_CACHE_SIZE`
- Congestion control: `CWND_INITIAL`, `SSTHRESH_INITIAL`, `RTO_INITIAL_MS`, `RTO_MIN_MS`,
  `RTO_MAX_MS`, `RETX_MAX_ATTEMPTS`
- Keepalive: `KEEPALIVE_INTERVAL_TICKS`, `KEEPALIVE_MAX_PROBES`
- Brain link defaults: `BRAIN_SERVER_IP`, `BRAIN_SERVER_PORT`
- Multi-stream (RFC-0021): `MAX_STREAM_COUNT`, `CAMERA_STREAM_COUNT`

These values are compile-time defaults.  Most of the IP/port values are also readable at
runtime via `CONFIG.INI` so operators can reconfigure without a rebuild.

### 3.7 Security (`Kconfig.security`)

- **`SECURE_BOOT_ENFORCED`**: when `y`, the kernel refuses to boot any image whose Ed25519
  signature (see RFC-0011) doesn't match the built-in production public key.  Set to `n` only
  in development builds.
- **`OTA_SIGNATURE_MANDATORY`**: must be `y` whenever `SECURE_BOOT_ENFORCED` is `y`.  The
  `build.rs` validation pass rejects the combination `SECURE_BOOT_ENFORCED=y` +
  `OTA_SIGNATURE_MANDATORY=n`.
- **`LINK_AEAD_DEFAULT_ON`**: when `y`, the kernel negotiates AEAD link encryption (RFC-0019)
  by default on every brain-link connection.  Requires a PSK file at the path given by
  `LINK_PSK_PATH`.
- **`LINK_PSK_PATH`**: path on the FAT32 partition to the pre-shared key for AEAD.
- **Stack canary**, **W^X enforcement**, **PMP region budget**: all set conservatively by default.

### 3.8 Drivers (`Kconfig.drivers`)

Controls which driver subsystems are compiled in.  Most are enabled by default on platforms
that support them:

- Core: `DRIVER_UART`, `DRIVER_GPIO`, `DRIVER_I2C`, `DRIVER_PWM`, `DRIVER_MOTOR_PID`
- Sensors: `DRIVER_IMU`, `DRIVER_BARO`, `DRIVER_ADS1115`, `DRIVER_INA219`,
  `DRIVER_RANGEFINDER_US`, `DRIVER_RANGEFINDER_TOF`, `DRIVER_GPS`
- Imaging: `DRIVER_CSI_CAMERA` (platform-dependent)
- ML: `DRIVER_ML_CNN`, `DRIVER_NPU` (auto-disabled if no NPU present)
- USB: `DRIVER_USB_MSC`, `DRIVER_USB_DFU` (only when `BOARD_VF2` or `BOARD_K1`)
- VirtIO: `DRIVER_VIRTIO_BLK`, `DRIVER_VIRTIO_NET` (only when `BOARD_QEMU`)
- Isolation level defaults: most drivers start as `InKernel`; UART and Motor PID must stay
  `InKernel` (safety path).

### 3.9 Robot configuration (`Kconfig.robot`)

Sets the physical robot type and its default motion parameters:

- **Robot type** (`ROBOT_WHEELED` / `ROBOT_DRONE` / `ROBOT_HUMANOID` / `ROBOT_ACKERMANN`):
  selects which behavioral policies and skill sets are compiled in.
- **`ENCODER_TICKS_PER_M`**, **`WHEEL_BASE_MM`**, **`MOTOR_MAX_SPEED`**: mechanical defaults
  that can be overridden at runtime via `CONFIG.INI`.
- **PID defaults** (`PID_KP`, `PID_KI`, `PID_KD`, `PID_DT_MS`): compile-time PID constants;
  overridable at runtime.
- **IMU offsets** (`IMU_OFFSET_AX`, `IMU_OFFSET_AY`, `IMU_OFFSET_AZ`): default to 0;
  calibrated values go in `CONFIG.INI`.

### 3.10 Brain link (`Kconfig.brain`)

Default values for the kernel side of the brain ↔ kernel TCP link:

- `BEHAVIOR_SERVER_IP`, `BEHAVIOR_SERVER_PORT` (default `10.0.2.2:9000` for QEMU, board-
  specific for real hardware; these are the same keys as `behavior_server_ip` /
  `behavior_server_port` in `CONFIG.INI`)
- `BRAIN_LINK_AUTO_RECONNECT`: `y` by default
- `BRAIN_LINK_SUBSUMPTION_LAYERS`: bitmap of which subsumption layers (L0–L3) are active

These become `CONFIG.INI` defaults at boot and are typically overridden per-deployment.

### 3.11 OTA (`Kconfig.ota`)

Controls the over-the-air update system:

- `OTA_SLOT_COUNT` (default 2: A/B), `OTA_RECOVERY_SLOT` (1 extra slot)
- `OTA_MAX_IMAGE_SIZE` (default 8 MiB)
- `OTA_SIG_SCHEME` (`Ed25519`, the only option today)
- `OTA_BOOT_COUNT_THRESHOLD` (rollback trigger, default 3)
- `OTA_SIGNATURE_MANDATORY`: mirrors the security section flag

### 3.12 Development (`Kconfig.development`)

Debug and observability knobs that should be off in production:

- `DEV_TCP_TRACE_PROBES`: per-segment trace probe points in `tcp.rs`.  Off by default; useful
  for debugging the `#39` sensor-pump issue.
- `TRACE_BUF_SIZE`: size of the in-kernel trace ring buffer (default 512 events).
- `DEV_VERBOSE_KPRINTLN`: extra verbosity in kernel internals.
- `WCET_REPORT_INTERVAL`: `never` / `on_shutdown` / `periodic` — how often the WCET
  subsystem summarises violations.
- `PANIC_ACTION`: `halt` / `reboot_after_ms` / `dump_and_halt`.

---

## 4. Defconfigs — when to use which

Each defconfig in `defconfigs/` captures the complete set of options needed for a particular
deployment target.  They are committed to the repo, unlike `.config`.

### `embedded` — microcontrollers

**Hardware:** generic SoCs with ≤ 1 MiB SRAM.  No board-specific defconfig currently ships
for this profile — the ESP32-C3 build target was removed 2026-08-18 (never compiled, never
in CI); see `newfeatures/esp32c3/REVISAR.md` if reviving it.

**Memory budget (from RFC-0023):**

| Cap | Value | Memory cost |
|-----|-------|-------------|
| `MAX_TASKS` | 32 | 32 × 384 B = 12 KiB |
| `TCP_MAX_CONNS` | 4 | 4 × 1.2 KiB = ~5 KiB |
| `KERNEL_HEAP_SIZE` | 256 KiB | 256 KiB |
| Total static tables | — | ~30 KiB |

**Enabled:** UART, GPIO, basic networking, no-MMU, no-ML, no OTA.  
**Disabled:** CNN inference, NPU, USB MSC/DFU, VirtIO, AEAD, secure boot enforcement.  
**Note:** `PROFILE_FLEET` is incompatible with this defconfig; `build.rs` will reject the
combination with a compile-time error.

### `edge` — single-robot SBC (default)

**Hardware:** StarFive VisionFive 2, SpacemiT K1, Rockchip RK3588, or any ARM/RISC-V SBC
with ≥ 1 GiB RAM.

**Memory budget (from RFC-0023, edge profile):**

| Cap | Value | Memory cost |
|-----|-------|-------------|
| `MAX_TASKS` | 512 | 192 KiB |
| `TCP_MAX_CONNS` | 128 | 150 KiB |
| `MAX_CAPS_TOTAL` | 16 384 | 1 MiB |
| `KERNEL_HEAP_SIZE` | 32 MiB | 32 MiB |
| Lazy task stacks | demand-paged | ~8 MiB physical |
| **Total** | | **~45 MiB** |

**Enabled:** full driver set, ML/CNN, OTA (A/B), brain link, multi-stream (RFC-0021), AEAD
link opt-in, `LOG_LEVEL_INFO`.  
**Disabled:** secure boot enforcement (set `SECURE_BOOT_ENFORCED=y` before production).

### `fleet` — gateway / edge-server

**Hardware:** Any server-class board or x86_64 VM with ≥ 512 MiB RAM, acting as aggregator
for many downstream robots.

**Memory budget:**

| Cap | Value | Memory cost |
|-----|-------|-------------|
| `MAX_TASKS` | 4096 | ~1.5 MiB |
| `TCP_MAX_CONNS` | 1024 | ~1.2 MiB |
| `MAX_CAPS_TOTAL` | 131 072 | 8 MiB |
| `KERNEL_HEAP_SIZE` | 256 MiB | 256 MiB |
| **Total** | | **~270 MiB** |

**Enabled:** all edge features plus extended pub/sub mesh, full multi-stream, 1000-robot
simulation support.  
**Note:** WCET bounds for fleet require scheduler walk benchmarking at 4096 tasks (RFC-0023
phase L5).  If scheduler walks exceed budget, a B-tree priority queue is introduced.

### `vf2` — StarFive VisionFive 2 board

Extends `edge` with:
- `BOARD_VF2=y`, `ARCH_RISCV64=y`
- `HAS_RVV=n` (JH7110 has no V extension)
- `DRIVER_MACB_ETH=y` (VF2's Gigabit Ethernet MAC)
- `SCHED_HZ=250`
- `DTB_BUILTIN=y`, `DTB_BUILTIN_PATH="boards/vf2.dtb"`
- `WCET_BOUND_PID_US=80` (1.5 GHz, no vector)
- USB MSC gadget and DFU recovery enabled

### `k1` — SpacemiT K1 board

Extends `edge` with:
- `BOARD_K1=y`, `ARCH_RISCV64=y`
- `HAS_RVV=y` (K1 has RVV 1.0 hardware)
- `FP_RVV=y` (use vector context save/restore)
- `SCHED_HZ=250`
- `WCET_BOUND_PID_US=50` (1.6 GHz + RVV acceleration)
- `DRIVER_K1_GMAC=y`

### `qemu` — QEMU virt riscv64 (local dev)

**Hardware:** QEMU 7.0+ `virt` machine with `--cpu rv64`.

Key settings relative to `edge`:
- `BOARD_QEMU=y`
- `DTB_FROM_FIRMWARE=y` (QEMU passes DTB in `a1`)
- `SCHED_HZ=100` (TCG amplifies ISR cost)
- `WCET_BOUND_TIMER_ISR_US=0` (disabled — TCG rdcycle artefact, see MEMORY.md)
- `DRIVER_VIRTIO_BLK=y`, `DRIVER_VIRTIO_NET=y`
- `BRAIN_SERVER_IP="10.0.2.2"` (QEMU user-mode NAT gateway)
- `SECURE_BOOT_ENFORCED=n`, `OTA_SIGNATURE_MANDATORY=n` (dev build)
- `DEV_VERBOSE_KPRINTLN=y`

### `qemu-aarch64` — QEMU virt aarch64

Like `qemu` but:
- `ARCH_AARCH64=y`
- Target triple: `aarch64-unknown-none-softfloat` (switched to hardfloat once stable)
- `HAS_NEON=y`, `FP_NEON=y`
- `KERNEL_STACK_SIZE_KB=32`, `USER_STACK_SIZE_KB=32` (larger trap frame on aarch64)
- `CACHE_LINE_BYTES=128`

### `esp32c3` — parked

Removed 2026-08-18: the `esp32c3` defconfig, `BOARD_ESP32C3` Kconfig symbol, and the
`esp32c3` Cargo feature never compiled and were never in CI.  The linker script, boot
assembly, defconfig, and revival notes are parked in `newfeatures/esp32c3/` — see
`newfeatures/esp32c3/REVISAR.md` for what's needed to bring it back.

---

## 5. How to add a new configuration option

Follow this checklist when you want to introduce a new tunable constant to the kernel.

### Step 1: Identify the right Kconfig fragment

Choose the fragment that matches the subsystem:

| You are adding... | Edit this file |
|-------------------|----------------|
| A new resource cap (task count, buffer pool size) | `Kconfig.limits` |
| A timing or WCET budget | `Kconfig.timing` |
| A network protocol constant | `Kconfig.network` |
| A security toggle | `Kconfig.security` |
| A hardware driver on/off switch | `Kconfig.drivers` |
| A robot-type specific constant | `Kconfig.robot` |
| A debug/trace knob | `Kconfig.development` |
| A board/SoC constant (UART base, clock freq) | `Kconfig.platform` |

### Step 2: Write the Kconfig entry

For an **integer** option:

```kconfig
config MY_BUFFER_SIZE_KB
    int "My subsystem buffer size (KiB)"
    range 4 1024
    default 16  if PROFILE_EMBEDDED
    default 64  if PROFILE_EDGE
    default 256 if PROFILE_FLEET
    help
      Size of the my-subsystem DMA bounce buffer.  Each active
      transfer consumes one buffer slot.  Too small → transfer
      queuing latency; too large → wasted physical memory.
      Memory cost: MY_BUFFER_SIZE_KB × MY_MAX_TRANSFERS.
```

For a **boolean** option that maps to a cargo feature:

```kconfig
config MY_NEW_SUBSYSTEM
    bool "Enable my new subsystem"
    default y if PROFILE_EDGE || PROFILE_FLEET
    default n if PROFILE_EMBEDDED
    depends on !PROFILE_EMBEDDED   # too small
    help
      Compiles in the my-new-subsystem crate.  Disable to save
      ~20 KiB code size on constrained targets.
```

Rules for good Kconfig entries:
- Every option must have a `help` block.  The help text is the single source of truth for the
  option's purpose — it is also rendered in `make menuconfig` and (eventually) in auto-generated
  documentation.
- Every integer option must have a `range`.  The range is checked by kconfiglib and also
  serves as documentation of valid values.
- Use `default ... if ...` rather than `if ... default ...`.  The former composes better with
  multiple conditionals.
- If the value is derived from another option (e.g. `MY_TOTAL = MY_TASKS × MY_PER_TASK`),
  add a comment and a `build.rs` assertion rather than a Kconfig-level formula.

### Step 3: Propagate the value to Rust code

**For integer options:** the workspace `build.rs` reads `.config` and emits a `pub const` in
`crates/limits/src/generated.rs`.  Add a `pub use` import at the old hardcoded site:

```rust
// Before (in crates/my-crate/src/lib.rs):
pub const MY_BUFFER_SIZE: usize = 65536;

// After:
pub use robot_os_limits::MY_BUFFER_SIZE_KB;
pub const MY_BUFFER_SIZE: usize = MY_BUFFER_SIZE_KB * 1024;
```

**For boolean options that gate a cargo feature:** add a line to the mapping dictionary in
`tools/kconfig_to_cargo.py` (TBD pending C1 — the script is authored in phase C1 of RFC-0026):

```python
KCONFIG_TO_FEATURE = {
    # ... existing entries ...
    "MY_NEW_SUBSYSTEM": "my-new-subsystem",   # add this line
}
```

The build wrapper will then pass `--features my-new-subsystem` to cargo when
`CONFIG_MY_NEW_SUBSYSTEM=y` is set in `.config`.

### Step 4: Add a `build.rs` validation assertion if needed

If your option has cross-option constraints (e.g. `MY_BUFFER_SIZE ≥ MY_MAX_TRANSFERS × 4`),
add a check in the validation pass inside `crates/phanes-config/src/validate.rs`:

```python
# In the assertions list (pseudocode; adapt to actual build.rs structure):
assertions = [
    # ... existing assertions ...
    ("MY_BUFFER_SIZE_KB * 1024 >= MY_MAX_TRANSFERS * 4",
     "MY_BUFFER_SIZE_KB too small for MY_MAX_TRANSFERS"),
]
```

The build aborts with the quoted message if the constraint fails.

### Step 5: Verify

```bash
make oldconfig    # answers any new prompts with their defaults
make              # should build clean
```

Then check that `crates/limits/src/generated.rs` contains your new constant.

### Step 6: Update defconfigs if the default doesn't suit them

If the profile default is wrong for a particular board defconfig, open that
`defconfigs/<board>.config` and add or override the line:

```
CONFIG_MY_BUFFER_SIZE_KB=128
```

Then re-run `make defconfig-<board>` to propagate through `olddefconfig`.

---

## 6. How to derive a new defconfig from an existing one

If you have a new board or a custom deployment that differs from an existing defconfig:

```bash
# Start from the closest existing preset:
make defconfig-edge

# Open the interactive menu and tweak:
make menuconfig
# Navigate to the relevant category, change values, save and exit.

# Verify the build still works:
make

# Save the minimal diff as a new defconfig:
make savedefconfig
# This writes defconfigs/last_saved.config.

# Rename it:
mv defconfigs/last_saved.config defconfigs/my-board.config

# Add a Makefile target for it:
# defconfig-my-board is handled by the generic wildcard rule already;
# just ensure defconfigs/my-board.config exists in the repo.
```

The `savedefconfig` command writes only the options that differ from Kconfig defaults, not the
full `.config`.  This produces a minimal, readable diff that is easy to review and merge.

---

## 7. How to read a `.config` for safety-case auditing

This section is relevant for anyone preparing a safety argument for a PHANES kernel deployment
under ISO 26262 or DO-178C.

**What `.config` represents:**  Every line `CONFIG_FOO=value` is a compile-time decision
that is burnt into the binary.  The complete set of `.config` lines, combined with the git
SHA of the source tree used to build it, uniquely identifies the deployed kernel binary.

**What it does not cover:**  The brain (Python) is explicitly outside the cert scope
(RFC-0017).  The `.config` defines the *kernel* binary only.  The brain has its own
engineering rigour but a different risk profile; there is no `.config` for it.

**For a safety-case-bound release, the procedure is:**

1. Start from a named defconfig: `make defconfig-vf2`.
2. Apply any deployment-specific overrides (`make menuconfig` or manual `.config` edits).
3. Build: `make`.
4. Run the full test suite: `make test` (all 5 build configs; see `AGENTS.md`).
5. Tag the source tree: `git tag -s v1.2.3-vf2-production`.
6. Commit the defconfig used:
   ```bash
   make savedefconfig
   cp defconfigs/last_saved.config defconfigs/vf2-production-v1.2.3.config
   git add defconfigs/vf2-production-v1.2.3.config
   git commit -s -m "safety: pin defconfig for v1.2.3 VF2 production release"
   ```
7. The evidence bundle (for an auditor) is: git tag SHA + `defconfigs/vf2-production-v1.2.3.config`.
   That pair is sufficient to reproduce the exact binary.

**Reading individual options:**

| Line | What to check |
|------|---------------|
| `CONFIG_SECURE_BOOT_ENFORCED=y` | Production kernel enforces firmware signature |
| `CONFIG_OTA_SIGNATURE_MANDATORY=y` | No unsigned OTA images accepted |
| `CONFIG_LINK_AEAD_DEFAULT_ON=y` | Brain link is encrypted by default |
| `CONFIG_NO_MMU=n` | MMU-based isolation is active |
| `CONFIG_MAX_TASKS=512` | Scheduler table is bounded at 512; WCET walk is bounded |
| `CONFIG_WCET_BOUND_TIMER_ISR_US=10` | Timer ISR violation threshold is set |
| `CONFIG_PROFILE_EMBEDDED=n` | Edge profile (larger, more capable) is active |

An auditor reviewing a `.config` diff between two releases can immediately see which
security, resource, or timing decisions changed.

---

## 8. Per-arch / per-platform conditional defaults — how to read them

The `default ... if ...` syntax in Kconfig lets a single option have different defaults
depending on architecture, board, and profile.  This section walks through a concrete example.

### Worked example: `USER_STACK_SIZE_KB`

From `Kconfig.arch`:

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
```

**How to read this:**

- If you are building for `ARCH_RISCV64` (VF2, K1, QEMU), the default is 16 KiB.
- If you are building for `ARCH_AARCH64` (`qemu-aarch64`), the default is 32 KiB because
  the NEON register file (32 × 128-bit Q registers = 512 bytes) plus the exception frame is
  larger than the RISC-V D-extension save area (32 × 64-bit = 256 bytes).
- If you are building for `ARCH_X86_64`, the default is 48 KiB because the XSAVE area for
  AVX2 is ~1.2 KiB and for AVX-512 ~3 KiB.

The ordering matters: Kconfig evaluates the `default ... if ...` clauses in order and picks
the first one whose condition is true.  If none match (e.g. an unrecognised architecture),
there is no default and `menuconfig` will prompt.

### Worked example: `SCHED_HZ` — board × profile × architecture

```kconfig
config SCHED_HZ
    int "Scheduler tick frequency (Hz)"
    range 10 1000
    default 100  if PROFILE_EMBEDDED
    default 100  if PROFILE_EDGE && BOARD_QEMU
    default 250  if PROFILE_EDGE && BOARD_VF2
    default 250  if PROFILE_EDGE && BOARD_K1
    default 500  if PROFILE_FLEET
    default 100
```

Here the precedence is:
1. Profile + board combinations come first (a board's clock speed and ISR overhead matter
   more than the abstract profile when determining a safe tick rate).
2. Profile-only fallbacks next.
3. The bare `default 100` catches any combination none of the above conditions match.

### Worked example: `KERNEL_STACK_SIZE_KB` — nested conditions

```kconfig
config KERNEL_STACK_SIZE_KB
    int "Per-task kernel stack (KiB)"
    default 8   if ARCH_RISCV64 && PROFILE_EMBEDDED
    default 16  if ARCH_RISCV64
    default 32  if ARCH_AARCH64
    default 32  if ARCH_X86_64
    range 4 256
```

The `ARCH_RISCV64 && PROFILE_EMBEDDED` case is tested first so embedded RISC-V builds get
the smaller 8 KiB stack before the generic `ARCH_RISCV64` default of 16 KiB applies.

---

## 9. Validation invariants and what their error messages mean

`build.rs` runs a validation pass after reading `.config`.  If any invariant is violated,
the build aborts with an error like:

```
error: PHANES configuration invariant violated:
  MAX_SOCKETS (64) < TCP_MAX_CONNS (128) + 16
  Fix: increase CONFIG_MAX_SOCKETS to at least 144, or reduce CONFIG_TCP_MAX_CONNS.
```

The table below lists every invariant from RFC-0026's validation pass.  Error messages are
approximate; the actual text is in `crates/phanes-config/src/validate.rs` (TBD pending C1).

| Invariant | What it protects | How to fix |
|-----------|------------------|------------|
| `MAX_SOCKETS ≥ TCP_MAX_CONNS + 16` | Enough socket slots for all TCP conns plus reserved UDP | Increase `MAX_SOCKETS` or decrease `TCP_MAX_CONNS` |
| `MAX_FDS_TOTAL ≥ MAX_TASKS × MAX_FDS_PER_PROC / 4` | Global FD table has headroom for all processes | Increase `MAX_FDS_TOTAL` |
| `MAX_CAPS_TOTAL ≥ MAX_TASKS × 8` | Per-app capability budget doesn't overflow the pool | Increase `MAX_CAPS_TOTAL` |
| `KERNEL_HEAP_SIZE ≥ sum of static table footprints` (auto-computed) | Heap budget is consistent with table sizes | Increase `KERNEL_HEAP_SIZE` or reduce a cap |
| `PMP_REGIONS_RESERVED ≤ 12` | Leaves headroom within RV64 M-mode hardware limit (16) | Reduce `PMP_REGIONS_RESERVED` |
| `PROFILE_EMBEDDED ⇒ KERNEL_HEAP_SIZE_KB ≤ 512` | Microcontroller SRAM budget | Use `embedded` profile only for genuinely small targets |
| `BOARD_VF2 ⇒ ARCH_RISCV64` | JH7110 SoC is RISC-V only | Same as above |
| `BOARD_K1 ⇒ ARCH_RISCV64` | SpacemiT K1 is RISC-V only | Same as above |
| `FP_NEON ⇒ ARCH_AARCH64` | NEON is an aarch64 extension | Select `ARCH_AARCH64` or choose a different FP mode |
| `FP_RVV ⇒ HAS_RVV` | Can't save/restore RVV registers if the CPU has none | Disable `FP_RVV` or enable `HAS_RVV` (K1 only) |
| `NO_MMU ⇒ NOT PROFILE_EDGE AND NOT PROFILE_FLEET` | Edge/fleet profiles require virtual memory | Enable MMU, or switch to `embedded` profile |
| `LINK_AEAD_DEFAULT_ON ⇒ LINK_PSK_PATH != ""` | AEAD needs a key file | Set `LINK_PSK_PATH` to the key's FAT32 path |
| `SECURE_BOOT_ENFORCED ⇒ OTA_SIGNATURE_MANDATORY` | Boot verification and OTA verification must both be active | Enable `OTA_SIGNATURE_MANDATORY` when secure boot is enforced |
| `PMP_REGIONS_RESERVED + PMP_REGIONS_PER_USER × MAX_ISOLATED_PROCS ≤ 16` | RV64 hardware PMP limit | Reduce isolated process count or per-process PMP allocation |

---

## 10. Common workflows

### "I'm running QEMU dev — minimal config"

```bash
make defconfig-qemu
make
# In one terminal: run QEMU (see docs/DEV_WORKFLOW.md for the full command)
# In another terminal: run stub_brain or the full brain server
```

The `qemu` defconfig has `SECURE_BOOT_ENFORCED=n` and `OTA_SIGNATURE_MANDATORY=n` so you
don't need to sign binaries during development.  WCET is disabled (`WCET_BOUND_TIMER_ISR_US=0`)
to avoid false violations under TCG.

### "I'm flashing a VF2 for the first time"

```bash
make defconfig-vf2
# Review key security settings before production:
make menuconfig
# Navigate: Security → Secure boot → set SECURE_BOOT_ENFORCED=y
# Navigate: Security → OTA → set OTA_SIGNATURE_MANDATORY=y
make
# Build the signed binary:
python3 tools/sign_firmware.py target/riscv64imac-unknown-none-elf/release/robot-os \
    --key tools/keys/prod_priv.bin --out build/kernel-vf2-signed.bin
# Flash per docs/FLASH_PROCEDURE.md
```

For the first bring-up iteration you may keep `SECURE_BOOT_ENFORCED=n` and enable it once
the board is confirmed working.

### "I'm running soak tests with 1000 simulated robots — fleet profile"

```bash
make defconfig-fleet
# Optionally tune MAX_TASKS or KERNEL_HEAP_SIZE for your simulation host's RAM:
make menuconfig
make
# Run the soak harness:
python3 tools/fleet_soak.py --robots 1000 --duration 3600
```

The `fleet` defconfig sets `MAX_TASKS=4096` and `KERNEL_HEAP_SIZE=256 MiB`.  Confirm your
host has enough RAM.  The WCET walk time at 4096 tasks should be benchmarked per RFC-0023
phase L5 before treating fleet-profile results as performance-representative.

### "I'm building for ESP32-C3"

The `esp32c3` build target was removed 2026-08-18 — it never compiled and was never in CI.
It's parked in `newfeatures/esp32c3/`; see `newfeatures/esp32c3/REVISAR.md` for what's
needed to revive it before this workflow applies again.

### "I just added a new `pub const` for a new buffer — where does it go in Kconfig?"

Follow §5:
1. Add the option to the appropriate `Kconfig.*` fragment (usually `Kconfig.limits`).
2. The workspace `build.rs` automatically emits it in `crates/limits/src/generated.rs`.
3. Replace the hardcoded constant at its old site with `pub use robot_os_limits::MY_OPTION`.
4. Run `make oldconfig && make`.

### "CI is failing on `make oldconfig` after I rebased — what do I do?"

This happens when a rebase picks up a new Kconfig option that your local `.config` doesn't
yet know about.

```bash
# Option A: accept all new defaults silently (recommended for CI):
make olddefconfig

# Option B: answer each new question interactively:
make oldconfig

# Option C: regenerate from a named defconfig (discards local tweaks):
make defconfig-edge   # or whichever you were using
```

In CI the canonical flow is `make defconfig-<name> && make build`.  If a CI job runs
`make oldconfig` on a stale `.config` from a previous job artifact, it will ask questions.
Use `make olddefconfig` in CI scripts — it silently accepts all defaults.

---

## 11. Troubleshooting

### `make menuconfig` errors with `ImportError: No module named kconfiglib`

Install kconfiglib:

```bash
pip3 install kconfiglib
# Or pin the exact version used by the project:
pip3 install -r tools/requirements-dev.txt
```

kconfiglib has no compiled dependencies (pure Python, BSD-licensed) and runs on
macOS / Linux / Windows.  If you need to work offline or want to vendor it:

```bash
pip3 download kconfiglib -d tools/kconfiglib-vendor/
```

### `make oldconfig` asks me 80 questions

You have a `.config` from a much older build, and many new options have been added since.
Two options:

1. **Accept all defaults silently** (fastest): `make olddefconfig`
2. **Use a named defconfig** (recommended if you're unsure of your current settings):
   `make defconfig-edge` (or whichever matches your target), then `make menuconfig` to
   re-apply your customisations.

After a rebase that adds many new Kconfig options, the CI script pattern is:

```bash
make defconfig-${TARGET}
# Optional: apply deployment-specific overrides
make olddefconfig   # absorb any leftover unknown options
make build
```

### "My build picks the wrong target triple"

Check which architecture is selected:

```bash
grep 'CONFIG_ARCH_' .config
```

The expected output is one of:
```
CONFIG_ARCH_RISCV64=y
CONFIG_ARCH_AARCH64=y
CONFIG_ARCH_X86_64=y
```

If none is set, something went wrong with the defconfig application.  Run
`make defconfig-<target>` again.  If the wrong arch is set, either you applied the wrong
defconfig, or a `make menuconfig` session changed the Architecture choice.  Fix in
`make menuconfig` under `Architecture → ISA`.

### "Cargo says feature `vf2` not found"

The Kconfig → cargo feature translation happens in `tools/kconfig_to_cargo.py`.  If you
recently added a new board and the build command doesn't pass `--features vf2`, check:

1. That `CONFIG_BOARD_VF2=y` is in your `.config`.
2. That `kconfig_to_cargo.py` has a mapping entry for `BOARD_VF2` → feature `vf2`
   (TBD pending C1 — file is authored in RFC-0026 phase C1).
3. That the `vf2` feature is declared in `kernel/Cargo.toml`.

### "Out-of-memory at runtime (kernel panics in allocator)"

The kernel uses a fixed-size heap set at compile time by `KERNEL_HEAP_SIZE`.  If you see
an allocation failure panic, the heap is undersized for your workload.

Check your current setting:

```bash
grep CONFIG_KERNEL_HEAP_SIZE .config
```

If it shows a small value (e.g. 4 MiB), increase it in `make menuconfig` under
`Resource Limits → Kernel heap`.  Typical guidance: `edge` profile should have at least
32 MiB; `fleet` should have at least 256 MiB.

To diagnose *what* is consuming the heap, enable `DEV_VERBOSE_KPRINTLN=y` and look for
heap allocation log lines in the boot sequence, or use the procfs heap-stats endpoint if
available.

### "Build aborts with `PHANES configuration invariant violated`"

See §9 for the full table of invariants and how to fix each one.  The most common triggers:

- Enabling `PROFILE_FLEET` on a memory-constrained microcontroller defconfig — switch to
  `embedded` instead.
- Setting `LINK_AEAD_DEFAULT_ON=y` without setting `LINK_PSK_PATH` — add a key path.
- Raising `TCP_MAX_CONNS` without also raising `MAX_SOCKETS` — keep `MAX_SOCKETS ≥ TCP_MAX_CONNS + 16`.

---

## 12. Reference index

Alphabetical list of key Kconfig options referenced in this document and in RFC-0026's
audit tables.  This index covers user-tunable options; auto-computed options (e.g.
`ATOMIC_MAX_BYTES`, `CACHE_LINE_BYTES`) and driver on/off flags not explicitly named in
RFC-0026 are omitted — see RFC-0026 § Configuration categories for the full enumeration.

Options whose fragment is listed as **TBD pending C1** will be confirmed once phase C1
of RFC-0026 is committed.

| Option | Fragment | Description |
|--------|----------|-------------|
| `ARCH_AARCH64` | `Kconfig.arch` | ISA choice: Aarch64 |
| `ARCH_RISCV64` | `Kconfig.arch` | ISA choice: RISC-V 64-bit (default) |
| `ARCH_X86_64` | `Kconfig.arch` | ISA choice: x86_64 |
| `BEHAVIOR_SERVER_IP` | `Kconfig.brain` | Default brain server IP (= `behavior_server_ip` in CONFIG.INI) |
| `BEHAVIOR_SERVER_PORT` | `Kconfig.brain` | Default brain server TCP port (= `behavior_server_port` in CONFIG.INI) |
| `BOARD_K1` | `Kconfig.platform` | Target board: SpacemiT K1 |
| `BOARD_QEMU` | `Kconfig.platform` | Target board: QEMU virt |
| `BOARD_VF2` | `Kconfig.platform` | Target board: StarFive VisionFive 2 |
| `BOOTLOADER_OPENSBI` | `Kconfig.platform` | Boot with OpenSBI below kernel |
| `BRAIN_LINK_AUTO_RECONNECT` | `Kconfig.brain` | Kernel auto-reconnects brain link on drop |
| `DEFAULT_QUANTUM_US` | `Kconfig.timing` | Round-robin scheduler quantum in microseconds |
| `DEV_TCP_TRACE_PROBES` | `Kconfig.development` | Enable per-segment TCP trace probes |
| `DEV_VERBOSE_KPRINTLN` | `Kconfig.development` | Extra kernel verbosity |
| `DTB_BUILTIN` | `Kconfig.platform` | DTB blob linked into kernel ELF at build time |
| `DTB_BUILTIN_PATH` | `Kconfig.platform` | Path to .dtb file to embed |
| `DTB_FROM_FIRMWARE` | `Kconfig.platform` | DTB pointer passed in a1 register by firmware |
| `DTB_NONE` | `Kconfig.platform` | No device tree; hard-coded platform constants |
| `ENCODER_TICKS_PER_M` | `Kconfig.robot` | Default wheel encoder resolution (ticks per metre) |
| `ETH_MTU` | `Kconfig.network` | Ethernet maximum transmission unit |
| `FP_HARDFLOAT_D` | `Kconfig.arch` | RISC-V D extension (64-bit double) FP context |
| `FP_HARDFLOAT_F` | `Kconfig.arch` | RISC-V F extension (32-bit single) FP context |
| `FP_NEON` | `Kconfig.arch` | Aarch64 NEON FP/SIMD context |
| `FP_RVV` | `Kconfig.arch` | RISC-V V extension vector context |
| `FP_SOFTFLOAT` | `Kconfig.arch` | No FP save area (soft-float targets) |
| `FP_SVE` | `Kconfig.arch` | Aarch64 SVE context |
| `FP_XSAVE_AVX2` | `Kconfig.arch` | x86_64 AVX2 XSAVE region |
| `FP_XSAVE_AVX512` | `Kconfig.arch` | x86_64 AVX-512 XSAVE region |
| `FP_XSAVE_SSE` | `Kconfig.arch` | x86_64 SSE2 XSAVE region |
| `HAS_AVX2` | `Kconfig.arch` | x86_64: AVX2 available (requires Haswell+) |
| `HAS_NEON` | `Kconfig.arch` | Aarch64 NEON always present |
| `HAS_RVV` | `Kconfig.arch` | RISC-V V extension present on this board |
| `HAS_SSE2` | `Kconfig.arch` | x86_64 SSE2 always present |
| `INTERRUPT_STACK_SIZE_KB` | `Kconfig.arch` | Per-hart interrupt stack size |
| `KEEPALIVE_INTERVAL_TICKS` | `Kconfig.network` | TCP keepalive probe interval |
| `KEEPALIVE_MAX_PROBES` | `Kconfig.network` | TCP keepalive probe count before giving up |
| `KERNEL_HEAP_SIZE` | `Kconfig.limits` | Kernel heap size in bytes (profile-dependent) |
| `KERNEL_STACK_SIZE_KB` | `Kconfig.arch` | Per-task kernel-mode stack size |
| `LINK_AEAD_DEFAULT_ON` | `Kconfig.security` | AEAD link encryption enabled by default |
| `LINK_PSK_PATH` | `Kconfig.security` | FAT32 path to pre-shared key for AEAD |
| `LOG_LEVEL_*` | `Kconfig` | Kernel log verbosity (error/warn/info/debug/trace) |
| `MAX_CAPS_TOTAL` | `Kconfig.limits` | Total capability pool size |
| `MAX_CHANNELS` | `Kconfig.limits` | Maximum IPC channels |
| `MAX_FDS_PER_PROC` | `Kconfig.limits` | Maximum file descriptors per process |
| `MAX_FDS_TOTAL` | `Kconfig.limits` | System-wide global FD table size |
| `MAX_LEASES` | `Kconfig.limits` | Maximum IPC lease handles |
| `MAX_PIPES` | `Kconfig.limits` | Maximum IPC pipes |
| `MAX_PORTS` | `Kconfig.limits` | Maximum IPC ports |
| `MAX_SERVICES` | `Kconfig.limits` | Maximum named service registry entries |
| `MAX_SOCKETS` | `Kconfig.limits` | Maximum sockets (TCP + UDP) |
| `MAX_SUBS_PER_TOPIC` | `Kconfig.limits` | Maximum pub/sub subscribers per topic |
| `MAX_TASKS` | `Kconfig.limits` | Maximum schedulable tasks (TCB table size) |
| `MAX_TOPICS` | `Kconfig.limits` | Maximum pub/sub topics |
| `MMU_PAGE_4K` | `Kconfig.arch` | 4 KiB MMU page size |
| `MMU_PAGE_16K` | `Kconfig.arch` | 16 KiB MMU page size (Aarch64 only) |
| `MMU_PAGE_64K` | `Kconfig.arch` | 64 KiB MMU page size (Aarch64 only) |
| `MOTOR_CMD_TIMEOUT_MS` | `Kconfig.timing` | Motor command watchdog timeout |
| `MOTOR_MAX_SPEED` | `Kconfig.robot` | Default maximum motor speed |
| `NO_MMU` | `Kconfig.platform` | Compile out MMU subsystem entirely |
| `OFFLINE_RECONNECT_INTERVAL_TICKS` | `Kconfig.timing` | Brain-link reconnect interval |
| `OTA_BOOT_COUNT_THRESHOLD` | `Kconfig.ota` | Boot attempts before rollback |
| `OTA_MAX_IMAGE_SIZE` | `Kconfig.ota` | Maximum firmware image size |
| `OTA_SIGNATURE_MANDATORY` | `Kconfig.ota` | Reject unsigned OTA images |
| `OTA_SLOT_COUNT` | `Kconfig.ota` | Number of A/B OTA slots |
| `PANIC_ACTION` | `Kconfig.development` | Kernel behaviour on panic |
| `PID_DT_MS` | `Kconfig.robot` | PID loop timestep in milliseconds |
| `PID_KD` | `Kconfig.robot` | Default derivative gain |
| `PID_KI` | `Kconfig.robot` | Default integral gain |
| `PID_KP` | `Kconfig.robot` | Default proportional gain |
| `PMP_REGIONS_RESERVED` | `Kconfig.security` | PMP regions reserved for M-mode kernel |
| `PROFILE_EDGE` | `Kconfig.profile` | Deployment profile: single-robot SBC (default) |
| `PROFILE_EMBEDDED` | `Kconfig.profile` | Deployment profile: microcontrollers |
| `PROFILE_FLEET` | `Kconfig.profile` | Deployment profile: fleet gateway |
| `ROBOT_ACKERMANN` | `Kconfig.robot` | Robot type: Ackermann steering |
| `ROBOT_DRONE` | `Kconfig.robot` | Robot type: quadrotor drone |
| `ROBOT_HUMANOID` | `Kconfig.robot` | Robot type: humanoid joint control |
| `ROBOT_WHEELED` | `Kconfig.robot` | Robot type: differential-drive wheeled (default) |
| `RTO_INITIAL_MS` | `Kconfig.network` | TCP initial retransmission timeout |
| `RTO_MAX_MS` | `Kconfig.network` | TCP maximum retransmission timeout |
| `RTO_MIN_MS` | `Kconfig.network` | TCP minimum retransmission timeout |
| `SAFETY_COMMS_TIMEOUT_MS` | `Kconfig.timing` | Brain-link comms watchdog timeout |
| `SCHED_HZ` | `Kconfig.timing` | Scheduler tick frequency |
| `SECURE_BOOT_ENFORCED` | `Kconfig.security` | Reject unsigned boot images |
| `TCP_MAX_CONNS` | `Kconfig.limits` | Maximum simultaneous TCP connections |
| `TCP_MSS` | `Kconfig.network` | TCP maximum segment size |
| `TRACE_BUF_SIZE` | `Kconfig.development` | In-kernel trace event ring buffer size |
| `USER_STACK_SIZE_KB` | `Kconfig.arch` | Per-user-task virtual stack size |
| `WDT_TIMEOUT_MS` | `Kconfig.timing` | Hardware watchdog timeout |
| `WCET_BOUND_ACTUATOR_US` | `Kconfig.timing` | Actuator write WCET bound |
| `WCET_BOUND_CTX_US` | `Kconfig.timing` | Context switch WCET bound |
| `WCET_BOUND_PID_US` | `Kconfig.timing` | PID loop WCET bound |
| `WCET_BOUND_SENSOR_US` | `Kconfig.timing` | Sensor read WCET bound |
| `WCET_BOUND_TIMER_ISR_US` | `Kconfig.timing` | Timer ISR WCET bound (0 = disabled) |
| `WCET_REPORT_INTERVAL` | `Kconfig.development` | WCET violation reporting cadence |
| `WHEEL_BASE_MM` | `Kconfig.robot` | Default wheel base in millimetres |

> **Note:** options marked with **TBD pending C1** in the narrative sections above will have
> their fragment paths and exact option names confirmed once phase C1 of RFC-0026 is
> committed.  The entries in this index reflect the canonical names from RFC-0026's audit
> tables; they should not change during C1 authoring.

---

*Document reflects the design in RFC-0026 as of 2026-05-26.  For implementation status of
each migration phase (C1–C7), see RFC-0026 § Migration plan.*
