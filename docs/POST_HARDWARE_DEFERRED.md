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
