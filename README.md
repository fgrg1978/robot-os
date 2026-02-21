# Robot OS

A bare-metal RISC-V kernel written in Rust for real-time physical robot control.

Robot OS is a `#![no_std]` operating system designed to run directly on RISC-V hardware with no runtime dependencies. It provides a complete robotics stack — from hardware drivers and sensor fusion to autonomous navigation and neural network inference — all in a single, deterministic kernel.

## Features

- **SMP support** — up to 8 hardware threads with per-core scheduling
- **Sv39 virtual memory** — 3-level page tables, 4 MB kernel heap
- **Preemptive multitasking** — 256 tasks, 8 KB stacks, priority-based scheduler with ELF loader
- **Full network stack** — Ethernet, ARP, IPv4, UDP, TCP, DHCP, socket API (32 sockets)
- **Filesystem** — RAM-based VFS + FAT32 from VirtIO block devices
- **60+ syscalls** — POSIX-inspired system call interface
- **Subsumption architecture** — 4-layer behavior system (L0 safety → L3 mission planning)
- **Flight controller** — cascaded PID loops, motor mixers (QuadX/+/Hex/Octo), multiple flight modes
- **On-board ML** — MLP inference + GGUF/GGML-nano model loading with Q8/Q4 quantization
- **Sensor fusion** — IMU (MPU-6050), barometer (BMP280), GPS (NMEA), integer-only AHRS
- **Navigation** — waypoint following, pure-pursuit controller, occupancy grid
- **Brain protocol** — binary protocol over UDP to offload high-level decisions to an external AI server
- **Interactive shell** — 40+ built-in commands for diagnostics and control
- **Zero floating-point** policy (integer arithmetic everywhere except ML inference)

## Supported Platforms

| Platform | Feature flag | SoC | Notes |
|---|---|---|---|
| **QEMU virt** | *(default)* | — | VirtIO net/blk, 4 CPUs, 128 MB RAM |
| **VisionFive 2** | `--features vf2` | JH7110 | MACB Ethernet, SDHCI, GPIO/PWM/I2C |
| **SpacemiT K1** | `--features k1` | BPI-F3 | RVV 1.0 (VLEN=256) |
| **ESP32-C3** | `--features esp32c3` | ESP32-C3 | RV32IMC, no MMU, flat memory, 384 KB |

## Prerequisites

- **Rust nightly** with `riscv64imac-unknown-none-elf` target
- **QEMU** (`qemu-system-riscv64`) for emulated testing
- **RISC-V toolchain** (`riscv64-unknown-elf-as`, `riscv64-unknown-elf-ld`) for user-space binaries
- **mtools** (`mcopy`, `mkfs.fat`) for FAT32 disk image generation
- **Python 3** for ML weight generation and companion tools

## Building

All configurations must compile with zero errors and zero warnings.

```bash
# QEMU (default)
cargo build --release

# VisionFive 2
cargo build --release --features vf2

# SpacemiT K1 (includes RVV)
cargo build --release --features k1

# Without ML/camera subsystems
cargo build --release --features no-ml

# Without MMU (ESP32-C3 class)
cargo build --release --features no-mmu
```

## Running in QEMU

```bash
# Single CPU
make qemu

# 4 CPUs (SMP)
make qemu-smp

# Full environment: 4 CPUs + VirtIO disk + network + user-space ELFs
make qemu-full-smp

# With RISC-V Vector extension (VLEN=128)
make qemu-full-smp-rvv

# GDB debug (starts paused, connect with gdb on :1234)
make qemu-gdb
```

Override QEMU path: `make qemu-smp QEMU=/path/to/qemu-system-riscv64`

## Flashing to Hardware

### VisionFive 2

```bash
make flash-vf2 VF2_SD=/dev/sdX
make vf2-console VF2_SERIAL=/dev/ttyUSB0
```

### SpacemiT K1

```bash
make flash-k1 K1_SD=/dev/sdX
make k1-console K1_SERIAL=/dev/ttyUSB0
```

## Crate Map

```
kernel/            Entry point, boot, main loop, behavior task, shell
crates/arch/       CSRs, SBI, PMP, trap codes, RVV SIMD intrinsics
crates/mm/         Physical/virtual memory (Sv39), heap allocator
crates/sched/      Task scheduler, SMP, ELF loader, processes
crates/sync/       SpinLock, priority-inheritance mutex
crates/drivers/    UART, PLIC, CLINT, GPIO, PWM, I2C, SPI, VirtIO,
                   MACB Ethernet, CSI camera, ESC, RC, rangefinder
crates/net/        Ethernet, ARP, IPv4, UDP, TCP, DHCP, sockets
crates/fs/         RAM filesystem (VFS), FAT32
crates/behavior/   Subsumption layers (L0-L3), brain protocol, types
crates/flight/     Cascaded PID, motor mixers, flight modes
crates/ml/         MLP neural network, GGUF inference, quantization
crates/imu/        MPU-6050 accelerometer/gyroscope driver
crates/baro/       BMP280 barometer driver
crates/gps/        NMEA sentence parser
crates/ahrs/       Attitude estimation (complementary filter, integer-only)
crates/nav/        Waypoints, pure-pursuit, occupancy grid
crates/channel/    Generic Channel<T> with watchdog age tracking
crates/ipc/        Pipes, signals, service registry
crates/syscall/    System call dispatch and handlers
crates/config/     Persistent CONFIG.INI, runtime AtomicU32 values
crates/shell/      Interactive shell (40+ commands)
crates/robot/      Motor control, encoders, odometry, trajectory
crates/telemetry/  Binary telemetry protocol
crates/libsys/     User-space syscall wrappers
```

## Brain Protocol

Robot OS communicates with an external AI brain server (see `robot-brain`) over UDP using a compact binary protocol:

```
MAGIC(2B "BR") | TYPE(1B) | LEN(2B LE) | PAYLOAD(0-1400B) | CRC8(1B)
```

- CRC-8/MAXIM (polynomial 0x31), little-endian
- Kernel side: `crates/behavior/src/brain_protocol.rs`
- Brain side: `robot-brain/protocol.py`

## Tools

| Tool | Description |
|---|---|
| `tools/ground_station.py` | Real-time telemetry dashboard |
| `tools/perception_server.py` | Camera perception via external GPU |
| `tools/slam_server.py` | SLAM mapping server |
| `tools/vla_server.py` | Vision-Language-Action model server |
| `tools/esp32_bridge/` | ESP32 WiFi/sensor bridge firmware (Arduino) |
| `tools/test_e2e_qemu.sh` | End-to-end QEMU integration test |

## License

All rights reserved.
