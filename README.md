# Robot OS

A bare-metal RISC-V kernel written in Rust for real-time physical robot control.

Robot OS is a `#![no_std]` operating system designed to run directly on RISC-V hardware with no runtime dependencies. It provides a complete robotics stack — from hardware drivers and sensor fusion to autonomous navigation and on-device neural network inference — in a single, deterministic kernel.

## Why this exists

Commercial robotics platforms force a choice: Linux + ROS 2 (powerful, but 200 ms jitter and 4 GB RAM), Zephyr/FreeRTOS (tiny but no networking / ML), or a proprietary stack (black box). This repo takes a fourth path:

- **Real-time control lives in the kernel** — the PID loop, safety layer and motor driver never leave ring 0. No IPC round-trip, no scheduler jitter. Sensor→actuator path is a direct function call at 1 kHz.
- **AI where it belongs** — on-device inference (MLP + CNN + INT8) for reflexive obstacle avoidance, off-board VLM/LLM (via the companion brain server) for cognition.
- **Auditable** — ~50k lines of `#![no_std]` Rust across 34 crates, zero magic numbers, every constant named. One person can read the whole kernel in a weekend.
- **Single binary, four targets** — same Rust source → QEMU / VisionFive 2 / SpacemiT K1 / ESP32-C3 via feature flags. No per-board fork.
- **Companion brain, optional** — the robot has its own offline autonomy (waypoint patrol, reconnect, battery cutoff). If the brain disconnects, the robot keeps working.

This is a single-author hobby / learning project. The goal is not to compete with ROS; it's to own every layer of a real autonomous robot.

## State (2026-04)

**All 63 phases of the master plan implemented.** 50 000+ lines of Rust across 34 crates.

| Check | Status |
|-------|--------|
| 5-config build (QEMU / vf2 / k1 / no-ml / no-mmu) | ✅ 0 errors, 0 warnings |
| End-to-end smoke (kernel in QEMU + brain server) | ✅ 5/5 checks |
| Brain-side pytest | ✅ 1115/1115 |
| Protocol sync (kernel ↔ brain) | ✅ verified |

**Not yet validated (pre-hardware)** — UEFI boot on real EDK2 firmware, userspace driver migration (framework ready), LoRa/RF physical modules, production Ed25519 key rotation.

### Comparison

| Capability | **Robot OS** | Linux + ROS 2 | Zephyr | FreeRTOS |
|---|---|---|---|---|
| Worst-case IRQ latency | **< 10 µs** | 50–500 µs | < 10 µs | < 10 µs |
| Boot time | **< 1 s** | 5–30 s | < 1 s | < 1 s |
| RAM footprint | **8–128 MB** | 256 MB+ | 64 KB–2 MB | 4–256 KB |
| Memory safety | **Rust (static)** | C/C++ (unsafe) | C (unsafe) | C (unsafe) |
| On-board ML inference | **CNN + INT8 + NPU** | Via ROS node | No | No |
| Full TCP/IP stack | **Yes (RTO, cwnd, keepalive)** | Yes | Yes (LwIP) | Partial |
| SMP / multi-core | **Yes (4 CPUs)** | Yes | Partial | No |
| ELF user-space | **Yes** | Yes | No | No |
| Capability-based IPC | **Yes (handles + lease + zerocopy)** | No | No | No |
| Flight controller built-in | **Yes (PID + mixer + EKF + SITL)** | Via ArduPilot | No | No |
| Secure boot | **Ed25519 sidecar (F18)** | Via shim | Optional | No |
| UEFI-bootable | **Yes (opt-in feature)** | Yes | Partial | No |

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                  robot-brain (Python/macOS)                    │
│    VLM · LLM · task planner · fleet mgmt · dashboard · SITL    │
└──────────────────────────┬───────────────────────────────────┘
              Brain Protocol (binary TCP/UDP, CRC-8)
┌──────────────────────────▼───────────────────────────────────┐
│                  Robot OS  (Rust / RISC-V S-mode)              │
│ ┌─────────────────────────────────────────────────────────┐  │
│ │  L3 Mission   A* nav · waypoints · geofence · skills    │  │
│ │  L2 Deliberate task planner · remote VLA · MAVLink      │  │
│ │  L1 Reactive  PID · AHRS · SLAM · obstacle avoid        │  │
│ │  L0 Safety    ESTOP · per-type profiles · watchdog      │  │
│ └─────────────────────────────────────────────────────────┘  │
│ ┌──────────┐┌──────────┐┌──────────┐┌──────────┐┌─────────┐ │
│ │  net/    ││  ml/     ││  nav/    ││ flight/  ││ fs/     │ │
│ │TCP cwnd  ││CNN INT8  ││A* + grid ││PID+mixer ││FAT32    │ │
│ │IPv6 NTP  ││GGUF NPU  ││SLAM 2D   ││EKF SITL  ││tmpfs    │ │
│ │multilink ││model mgr ││spec wp   ││terrain   ││procfs   │ │
│ └──────────┘└──────────┘└──────────┘└──────────┘└─────────┘ │
│ ┌─────────────────────────────────────────────────────────┐  │
│ │ sched/    SMP · 32-level priorities · tickless · RT     │  │
│ │ mm/       Sv39 · PMP · COW fork · demand paging · vDSO  │  │
│ │ ipc/      io_ring · channels · SHM · fast-IPC · lease   │  │
│ │           zero-copy pipeline · pubsub                    │  │
│ │ crypto/   AES · SHA256 · X25519 · Ed25519 · secure_chan │  │
│ │ ota/      A/B slots · CRC-32 · Ed25519 signed (F18)     │  │
│ │ trace/    F27 ring buffer + UART/FAT32 dump             │  │
│ │ efi/      UE01-UE04 BootInfo + PE/COFF scaffolding      │  │
│ │ driver_server/ AQ3 userspace driver framework           │  │
│ └─────────────────────────────────────────────────────────┘  │
│ ┌─────────────────────────────────────────────────────────┐  │
│ │ drivers/ UART · GPIO · PWM · I²C · SPI · DMA · CAN      │  │
│ │          VirtIO blk/net · MACB eth · CSI camera · NPU   │  │
│ │          IMU · GPS · baro · rangefinder · USB xHCI      │  │
│ │          INA219 · ADS1115 · encoders · PLIC (SMP-safe)  │  │
│ └─────────────────────────────────────────────────────────┘  │
│    QEMU virt · VisionFive 2 (JH7110) · SpacemiT K1 · ESP32-C3 │
└──────────────────────────────────────────────────────────────┘
```

## Key design decisions

1. **No magic numbers** — every numeric value has a named constant. Timeouts, sizes, offsets, thresholds, port numbers, PWM limits — all declared once and referenced everywhere.
2. **Zero-copy hot paths** — camera frames and LiDAR scans flow through DMA-backed shared buffers (F15 `ipc/zerocopy.rs`); the CPU only touches data when computing.
3. **Capability-based security** — IPC handles carry permission bits. A crashed sensor driver cannot forge motor commands, by construction.
4. **Layered RT guarantees** — safety reflex in timer ISR @ 1 kHz (L0); reactive control @ 400 Hz (L1); deliberate planning as best-effort (L2/L3). No layer can starve a lower one.
5. **Feature-gated targets** — same source compiles to QEMU, VF2, K1 and ESP32-C3. Adding a board means adding a feature flag, not forking the kernel.
6. **PLIC SMP-safe** — the interrupt controller's `complete()` re-reads the enable register before writing (CVE-2026-23287 fix); no silent completion loss on multi-core.

## Supported platforms

| Platform | Feature flag | SoC | Notes |
|---|---|---|---|
| **QEMU virt** | *(default)* | — | VirtIO net/blk, 4 CPUs, 128 MB RAM |
| **VisionFive 2** | `--features vf2` | JH7110 | MACB Ethernet, SDHCI, GPIO/PWM/I²C |
| **SpacemiT K1** | `--features k1` | BPI-F3 | RVV 1.0 (VLEN=256) |
| **ESP32-C3** | `--features esp32c3` | ESP32-C3 | RV32IMC, no MMU, flat memory, 384 KB |
| **UEFI** *(opt-in)* | `--features uefi` | any | PE/COFF stub, BootInfo handoff |

## Building

All configurations must compile with **zero errors, zero warnings**.

```bash
# Full matrix (5 configs)
./scripts/build.sh

# Individual targets
cargo build --release                        # QEMU
cargo build --release --features vf2         # VisionFive 2
cargo build --release --features k1          # SpacemiT K1
cargo build --release --features no-ml       # without ML subsystem
cargo build --release --features no-mmu      # ESP32-C3 flat memory
cargo build --release --features uefi        # UEFI PE/COFF stub
```

## Running in QEMU

```bash
./scripts/qemu.sh              # default (1 CPU)
./scripts/qemu.sh smp          # 4 CPUs
./scripts/qemu.sh full         # 4 CPUs + VirtIO disk + net (port 8080 fwd)
./scripts/qemu.sh full-rvv     # 4 CPUs + RVV 1.0 vector extension
./scripts/qemu.sh gdb          # paused at start, GDB on :1234
```

End-to-end with brain:

```bash
./tools/test_e2e_auto.sh       # automated smoke: kernel + brain + 5 checks
```

## Flashing hardware

See [`docs/FLASH_PROCEDURE.md`](docs/FLASH_PROCEDURE.md) for the full SD-card / signing / first-boot procedure on VisionFive 2.

Quick form:

```bash
cargo build --release --features vf2
/opt/homebrew/opt/llvm/bin/llvm-objcopy -O binary \
  target/riscv64imac-unknown-none-elf/release/kernel target/kernel.bin

# Optional F18 signing
python3 tools/sign_ota.py target/kernel.bin

# Copy to SD (see FLASH_PROCEDURE.md for BOOTMETA layout)
cp target/kernel.bin /Volumes/ROBOT_FAT/KERN_A.BIN
```

## Deploying via OTA

```bash
./scripts/deploy.sh <robot-ip>   # build + sign + upload + watch boot
```

See [`docs/DEPLOY.md`](docs/DEPLOY.md) for the OTA protocol, key rotation and rollback procedure.

## UEFI boot (opt-in)

```bash
./scripts/build_uefi.sh          # → target/uefi/BOOTRISCV64.EFI + esp.img
qemu-system-riscv64 -M virt -bios edk2-riscv64.fd \
  -drive file=target/uefi/esp.img,format=raw,if=virtio
```

Scaffolding is in place; actual EDK2 validation pending hardware availability.

## Crate map

```
kernel/              Boot, init, behavior task, shell, panic, ASM
crates/arch/         CSRs, SBI, PMP, trap codes, RVV SIMD
crates/mm/           Sv39 VMM · heap · COW fork · demand paging · vDSO
crates/sched/        SMP scheduler · 32-level priorities · tickless · RT
crates/sync/         SpinLock · PI-mutex
crates/ipc/          io_ring · channels · SHM · fast-IPC · lease · zerocopy
crates/syscall/      60+ syscalls (POSIX-inspired + driver_server)
crates/fs/           VFS · FAT32 (full file API) · tmpfs · procfs
crates/net/          TCP cwnd · UDP · IPv6 · DNS · NTP · DHCP · multilink
crates/drivers/      UART · GPIO · PWM · I²C · SPI · DMA · CAN · USB xHCI
                     VirtIO · MACB · CSI · NPU · optical flow · PLIC (SMP)
crates/behavior/     Subsumption L0-L3 · safety · offline · logger · habits
                     skill profiles · world state · payload · balance
crates/flight/       Cascaded PID · mixers · EKF · SITL · SLAM · path3d
crates/flight-sim/   Host-side flight simulation (std crate, out-of-tree)
crates/ml/           MLP · CNN · INT8 · GGUF · model manager · pipeline
crates/nav/          A* · occupancy grid · pure-pursuit · speculative cache
crates/crypto/       AES · SHA256 · X25519 · Ed25519 · secure_channel
crates/ota/          A/B slots · Ed25519 secure boot · boot metadata
crates/trace/        F27 ring buffer + UART/FAT32 dump
crates/efi/          UE01-UE04 EFI BootServices + PE/COFF scaffolding
crates/driver_server/AQ3 userspace driver framework (registry + IRQ route)
crates/pubsub/       Robot OS SDK pub/sub
crates/channel/      Generic Channel<T> with watchdog age tracking
crates/service/      Service manager (register / discover / heartbeat)
crates/shell/        40+ interactive shell commands
crates/imu/          MPU-6050 accelerometer + gyroscope
crates/baro/         BMP280 barometer
crates/gps/          NMEA parser
crates/ahrs/         Integer-only attitude estimation
crates/robot/        Motor PID · encoders · odometry · trajectory
crates/telemetry/    Binary telemetry protocol
crates/camera/       CSI driver integration
crates/config/       CONFIG.INI persistence + runtime AtomicU32 config
crates/common/       Shared types (error codes, WCET)
crates/dtb/          Flattened Device Tree parser
crates/libsys/       User-space syscall wrappers
```

## Brain protocol

Binary over TCP (primary) or UART (ESP32 bridge fallback):

```
MAGIC(2B "BR") | TYPE(1B) | LEN(2B LE) | PAYLOAD(0-1400B) | CRC8(1B)
```

CRC-8/MAXIM polynomial 0x31, little-endian. Packet types synced between:
- Kernel: `crates/behavior/src/brain_protocol.rs`
- Brain:  `robot-brain/protocol.py`

## Tools

| Tool | Description |
|---|---|
| `scripts/build.sh` | Build all 5 configs and verify clean |
| `scripts/qemu.sh [mode]` | Launch kernel in QEMU (default/smp/full/rvv/gdb) |
| `scripts/deploy.sh <ip>` | OTA update: build + sign + upload + watch boot |
| `scripts/build_uefi.sh` | Build BOOTRISCV64.EFI + ESP FAT32 image |
| `tools/test_e2e_auto.sh` | Automated E2E smoke test (kernel + brain, 5 checks) |
| `tools/gen_dev_key.py` | Generate Ed25519 dev key pair for F18 secure boot |
| `tools/sign_ota.py` | Sign kernel binary for secure-boot verification |
| `tools/esp32_bridge/` | Arduino firmware for ESP32 WiFi bridge |

## Companion repo

- **[robot-brain](../robot-brain/)** — Python AI server (VLM + LLM + fleet + dashboard + MAVLink + SITL)

## Documentation

- [`docs/FLASH_PROCEDURE.md`](docs/FLASH_PROCEDURE.md) — SD-card flashing, signing, first-boot checklist
- [`docs/DEPLOY.md`](docs/DEPLOY.md) — OTA workflow, key rotation, rollback
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — historical master plan
- [`docs/PLAN_BRAIN.md`](docs/PLAN_BRAIN.md) — brain-side plan

## License

MIT
