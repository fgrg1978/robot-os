# Post-hardware deferred work

This file documents the work that **cannot** be completed before the
Julio 2026 hardware arrival — and explains the rationale for each.
Closing one of these here is wrong; they need real boards or real
external toolchains. The list is updated when the rationale changes.

## P01 — Vigilance mode (suspend-to-RAM + wake-on-event)

**Estimated:** ~1700 LoC bare SoC, ~2300 with ESP32-C3 gatekeeper.

**Why deferred:**
- Requires SoC PMIC integration — VF2's JH7110 has WAKEUP_GPIO and
  RTC; K1's PMIC is undocumented for S3. Both need datasheet access.
- Wake-on-event from PIR/accel/mic requires the actual sensors
  wired and characterised (false-positive thresholds depend on
  noise floor in the robot's body — only measurable post-build).
- Power measurements (3 W → 150 mW → 30 mW targets) require a
  shunt/INA219 in the actual harness; bench numbers from QEMU mean
  nothing here.

**What we CAN do pre-Julio:** sketch the kernel state machine in
`crates/sched/src/power.rs` as `unimplemented!()` shells. We
already have F09 power-management init — that's the entry point.

## E11.AQ3 — Drivers to userspace

**Estimated:** large architectural refactor (~5–8 KLoC).

**Why deferred:**
- Requires the IPC fastpath from SC02 research (above) to be done
  first — without it, every driver call would be slow.
- Also requires PMP/MMU isolation guarantees we haven't fully
  verified for shared MMIO regions. Easier to audit on real boards.
- Migrating 30 drivers in the dark would mean a dead kernel until
  every one is ported. Better done driver-by-driver with hardware
  tests after each.

## F06 — Driver server syscalls

**Why deferred:** depends on AQ3.

## D04 / D07 — Drone-specific motor mixing + drone CI

**Why deferred:** there's no drone in the lab until Julio. Tests
without a real ESC and frame are decorative.

## UEFI EDK2 packaging real

**Why deferred:** the `crates/efi/` scaffolding exists and links
clean (UE01–UE04). To produce a real UEFI binary we need:
- `boot_efi.S` (currently a stub) wired to `EfiMain`
- `linker-efi.ld` matching the EDK2 PE/COFF layout
- A build step that runs EDK2's `GenFw` to convert the ELF to PE32+

EDK2 is heavyweight (~1 GB build env). We do this when we have
a target board that boots UEFI — until then the stub guards the
scaffolding from rotting.

## ARM64 and x86-64 — Resume After Hardware Tests

Parked in `newfeatures/` on 2026-08-20 per user decision: first close real RISC-V
hardware (VF2 / K1), then return to these two.

- Not parked for being broken: 5,322 lines of peripherals written (GIC/MMU/PSCI
  and ACPI/APIC/4-level paging), with 13 tests passing on the x86 side.
- Parked because **a kernel was never built with them**: missing boot assembler,
  kernel linker script, and CI entry.
- The `crates/arch-api` abstraction **stays in the tree** with its 17 tests,
  because `arch-riscv64` implements it. While the cross-ISA contract is still
  exercised against one real architecture, reactivating another is adding a
  crate.
- Order when resuming: (1) linker script + boot.S, (2) secondary hart start,
  (3) **entry in ci_check.sh before claiming anything supported**.

See `newfeatures/REVISAR-arch.md`.

## Userspace: Runtime and Fast Path kernel<->user

User decision 2026-08-20: **after hardware**. And a language constraint: **no C
will be used**, so porting glibc/newlib/picolibc or exposing a C ABI is ruled out.

### Userspace Runtime (No libc)

`crates/libsys` is 1,325 lines and 134 functions, but stops at syscall boundary:
zero `malloc`/`free`/`memcpy`/`strlen`/`printf`. Cost measured same day:
`captest` and `latbench` had to hand-implement integer-to-decimal conversion, and
`brain_client` its own INI parser and line buffer.

Direction: grow `libsys` toward Rust "std-lite" — a `GlobalAlloc` on `brk`/`mmap`
(both already exist as syscalls) gives `Vec`/`String`/`Box` in ring 3 without a
line of C, and an `impl core::fmt::Write` gives `write!`/`format!`.

Needed by RFC-0020 (drivers to userspace): ring 3 driver will want dynamic
buffers and today cannot have them.

### Fast path: vDSO + io_ring

**Measured starting point** (`userspace/latbench`, QEMU TCG, `rdtime`):
the floor of a syscall is **~3,300 ns**. That is the number a ring avoids.

**Already built and TURNED OFF** — the important finding:
- `crates/ipc/src/io_ring.rs` exists in full, with SQ/CQ, `dispatch_sqe` and
  opcodes (`OP_MOTOR_SPEED`, `OP_WRITE_GPIO`, `OP_I2C_WRITE`).
- The syscall numbers are reserved: `SYS_IO_SETUP` (503),
  `SYS_IO_SUBMIT` (504), `SYS_IO_SUBMIT_ASYNC` (519),
  `SYS_IORING_CREATE_TYPED` (536), `SYS_IORING_SUBMIT_TYPED`.
- But `io_ring_register_ops` has **zero callers**, so `OPS` is `None`
  and `io_ring_submit` returns `IO_ERR_NO_OPS`. It has never been turned on.
- The vDSO exists and exposes only three fields: `uptime_ticks`, `uptime_ms`,
  `kernel_version`.

**Do NOT turn on without fixing this first** (security audit 2026-08-20):
1. `IO_RINGS` is a `static mut` **without a lock** — the only table in the
   crate left out when the other four were fixed (`HANDLES`, `PORTS`,
   `SHM_REGIONS`, `LEASES`, all with a comment saying this race was closed
   for them). Two harts can claim the same slot; `io_ring_destroy`
   racing with `io_ring_submit` is a use-after-free.
2. `dispatch_sqe` executes `OP_MOTOR_SPEED` / `OP_WRITE_GPIO` / `OP_I2C_WRITE`
   **without any `cap_check`**. The day someone registers the ops table,
   io_ring becomes a complete bypass of the capability system.
3. `IoRingState::owner_task` is written and **never read**.

### Proposed order

1. Close the three points above (they are security, not performance).
2. Extend the vDSO: the cheap, risk-free part. Every read-only datum that
   costs a trap today — time, ids, counters, degraded state — onto the
   shared page. ~0 cost and it eliminates the whole trap.
3. Turn on io_ring for the actuation path, which is the one with a fixed
   cadence (the control loop runs at 40 Hz) and where amortizing the trap
   across several SQEs actually pays off.
4. **Measure with `userspace/latbench` before and after.** It already exists
   and separates the syscall floor from each layer's cost; extend it with a
   ring case.
5. A `ci_check.sh` scenario that exercises it. Without a gate, history
   repeats: this mechanism has been built for a long time and nobody has
   ever executed it.

## Decision log

| Date       | Decision                                         |
|------------|--------------------------------------------------|
| 2026-05-09 | Deferred P01, AQ3, F06, D04/D07, UEFI EDK2.      |
| 2026-05-09 | TS01-03 + DEV01/02/04 closed pre-Julio.          |
| 2026-05-09 | SC02 research notes added (gates AQ3).           |
| 2026-05-09 | Audit + 16 bug fixes; 103 regression tests.      |
| 2026-05-09 | Pre-Julio: API key, TG env var, TCP window/SYN, secure_channel HMAC envelope, CI nightly added. |
| 2026-05-09 | Pre-Julio modularization (per-robot-type Cargo features) deferred — multi-day refactor across ~40 files; not a blocker. |

## Cargo modularization (pre-Julio, deferred separately)

Currently every kernel build pulls in every behavior subsystem (drone
flight controller, humanoid joint mixer, ackermann steering, wheeled
diff-drive). Result: ~12 MB binary, every robot ships every feature.

Proper fix is to add `robot-wheeled` / `robot-drone` / `robot-humanoid`
/ `robot-ackermann` Cargo features and gate the relevant modules with
`#[cfg(feature = "robot-drone")]`. Affected files (estimated):

- `kernel/Cargo.toml` — feature definitions
- `kernel/src/main.rs` — gate `flight-ctrl` task creation, mode imports
- `crates/behavior/src/lib.rs` — gate per-type modules
- `crates/robot/src/lib.rs` — gate motor/payload abstractions
- `crates/flight/Cargo.toml` — make crate optional in workspace
- ~30 other files referencing types behind cfgs

Estimated 1–2 days of methodical refactor with `cargo check` after
each gate. Skipped in this audit because:
1. Not a security/safety blocker — extra modules just sit unused.
2. Real binary-size win comes only after the refactor is complete (no
   incremental value).
3. Better done once, with tests, not piecemeal.
