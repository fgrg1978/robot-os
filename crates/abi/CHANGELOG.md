# `robot_os_abi` — Changelog

All notable changes to the PHANES frozen ABI crate. Versioning is
strict SemVer:

- **PATCH**: documentation, additional `#[doc]` comments, internal
  refactoring that doesn't change any `pub` item.
- **MINOR**: new `pub` items (e.g., a new syscall number at the next
  unused slot, a new `CapKind` variant, a new errno).
- **MAJOR**: removing or changing the wire format of any existing
  `pub` item. Requires an RFC supersede.

## [1.0.0] — Phase 1 release (2026-05-14)

**Initial stable ABI freeze**. Everything `pub` in this crate is
locked for the entirety of the `v1.x` series.

### Public surface frozen

- **`ABI_VERSION: u32 = 1`** — the ABI generation tag.
- **`cap::CapHandle`** — `#[repr(transparent)] u32` wire-format
  capability handle. Bitfield: 4-bit kind, 4-bit perms, 8-bit
  generation, 16-bit slot.
- **`cap::CapKind`** — `#[repr(u8)]` enum, 16 variants:
  `Null` `Channel` `Shm` `Port` `Irq` `MmioRegion` `IoRing` `Sensor`
  `Gpio` `I2c` `Pwm` `Motor` `File` `Socket` `Task` `AiSession`.
- **`cap::CapPerms`** — `#[repr(transparent)] u8` bitfield:
  `READ=0b0001` `WRITE=0b0010` `EXEC=0b0100` `DUP=0b1000`.
- **`cap::CAP_NULL`** — the all-zeros invalid handle.
- **`error::Errno`** — `#[repr(i64)]` enum with POSIX-aligned codes
  in `1..=99` and PHANES-specific codes in `200..=299`. Notable
  PHANES additions: `ECAPKIND=200` `ECAPPERMS=201` `ECAPSTALE=202`
  `ETOPOLOGY=203` `ESAFETY=204` `EAUTH=205` `EREPLAY=206`
  `EOTASIG=207` `EROLLBACK=208` `EQUOTA=209` `EABIVERSION=210`.
- **`syscall_nr::*`** — 70+ frozen syscall numbers:
  - 0..=19 process control
  - 20..=29 file I/O
  - 100..=119 IPC
  - 200..=229 GPIO/PWM/I2C
  - 230..=249 motor + sysinfo
  - 250..=269 filesystem + network
  - 270..=299 system control / disk / FDT
  - 300..=319 driver-server
  - 320..=349 robot control + platform
  - 350..=369 signals + pipes
  - 370..=389 sockets
  - 390..=399 service manager
  - 400..=429 memory + ADC + buzzer
  - 430..=499 security (seccomp + future)
  - 500..=529 IO ring / channels / MMIO / IRQ / ports / handles /
    trace / drivers
  - **528..=549 reserved for cap-typed migrations**
    (`SYS_CHAN_WRITE_TYPED=528`, `SYS_CHAN_READ_TYPED=529`)
- **`types::*`** — `#[repr(C)]` size-stable structs:
  `SensorState` (48 B), `MotorOutput` (12 B), `RobotInfo` (8 B),
  `SafetyProfile` (24 B).
- **`SYS_NR_RESERVED_UPPER: u64 = 600`** — bound below which new
  numbers will be allocated.

### Verification

- `crates/abi-tests/` host suite: 18 tests covering pack/unpack,
  errno round-trip, size stability, frozen number assignments.
- All sizes asserted at compile time via `core::mem::size_of`.

### Phase 1 lineage

This crate emerged from Wave 1 of Phase 1 (RFC-0008). The bitfield
layout of `CapHandle` and the `Cap<T>` typed wrapper that uses it
trace to RFC-0003.

## Upgrade discipline

- A v1.x release **may** add new syscall numbers, new `CapKind`
  variants, new `Errno` codes, or new `#[repr(C)]` types.
- A v1.x release **must not**:
  - Remove or rename any existing `pub` item.
  - Change the numeric value of any `Errno` or syscall number.
  - Change the size or layout of any `#[repr(C)]` type.
  - Repurpose any bit in `CapHandle`.
- ABI breakage requires an RFC supersede, a major version bump, and
  a 12-month deprecation window (RFC-0016).
