# RFC-0020: Driver Migration Plan (Addendum to RFC-0002)

> **Status:** accepted (planned for post-AQ3 work)  
> **Authors:** Fernando Rodriguez
> **Created:** 2026-05-23
> **Last updated:** 2026-05-23
> **Supersedes:** —
> **Superseded by:** —

## Summary

RFC-0002 established `InKernel / UserProcess / Hypervisor` driver
isolation and RFC-0003 wired `SYS_DRV_INVOKE`. The ring-3 `gpio_drv`
proof-of-concept validates the IPC round-trip. This RFC defines the
migration order and validation gates for moving the remaining drivers
from `DriverIsolation::InKernel` to `UserProcess`.

## Motivation

Per RFC-0017, only InKernel code contributes to the ISO 26262 cert
scope. A userspace driver that faults triggers a fail-stop of that
process without killing the kernel or safety arbiter. Moving
appropriate drivers to UserProcess narrows cert scope and improves
fault isolation — but not every driver should move. Hard-RT loops that
require sub-millisecond jitter cannot tolerate cross-process IPC
latency.

## Detailed design

### Current baseline

All five registered drivers declare `DriverIsolation::InKernel`.
The `UserDriverProxy` and `SYS_DRV_INVOKE = 311` are live; the GPIO
smoke task validates the path. In-kernel adapters remain the registered
default; userspace promotion is gated by the validation sequence below.

### Migration order — project-specific reasoning

Hardware arrives July 2026; QEMU iteration dominates until then. UART
is the only diagnostic sightline — losing it silently is the worst
possible bring-up failure. Greenfield drivers (CSI, LIDAR) have zero
migration tax. I2C is the best first-wave candidate: real, fail-stop
testable in SITL before hardware.

**Wave 1 — before hardware arrives**

| Driver | Rationale |
|--------|-----------|
| CSI camera | Greenfield; write as UserProcess from day one. |
| LIDAR | Greenfield; same. |
| I2C | IMU-adjacent; fail-stop testable in SITL. ~450 LOC. |

**Wave 2 — after first hardware bring-up**

| Driver | Rationale |
|--------|-----------|
| PWM | Promote only after `SYS_DRV_INVOKE` RTT on VF2/K1 hardware is confirmed below `PWM_MAX_INVOKE_US`. ~500 LOC. |

**Stays InKernel — indefinitely**

| Driver | Rationale |
|--------|-----------|
| UART (debug console) | Sole diagnostic sightline during bring-up; silent loss is the worst possible failure. Never migrate. |
| Motor PID | 1 kHz loop; IPC deadline `< 50 µs` not yet proven on silicon. Stays InKernel until hardware measurement exists. |

### Validation gate

Each driver must clear all three before `default isolation = UserProcess`:

1. **In-kernel regression.** Existing test suite passes unchanged.
2. **UserDriverProxy round-trip.** All operations produce identical
   results through the proxy (QEMU smoke task, same shape as GPIO).
3. **HIL on real hardware.** Kill the userspace driver process; the
   kernel must continue and the behavior task must enter safe-fallback
   within `DRIVER_FAULT_TIMEOUT_MS`. Gate 3 blocks Wave 2 until July.

### Reversibility

Every driver keeps an `InKernel` impl behind a Cargo feature
(e.g. `i2c-in-kernel` vs. `i2c-user-process`) so any deployment can
roll back by changing the feature flag and reflashing.

## Drawbacks

- **IPC overhead per call.** One `SYS_DRV_INVOKE` round-trip per
  operation. Negligible for I2C (~400 Hz) and LIDAR (~10 Hz); must be
  measured before PWM promotion.
- **Two codepaths per driver** during the window. Mitigated by Cargo
  feature gating.
- **Heartbeat/restart plumbing.** ~50 LOC per driver; must be correct
  or isolation is worse than none.

## Rationale and alternatives

**Alternative A — migrate all drivers at once.** Rejected: Motor PID
and UART cannot safely move given current IPC latency guarantees.

**Alternative B — migrate UART first.** Rejected by project context:
the debug console is the only sightline during bring-up. Losing it
silently is the worst possible failure mode for hardware iteration.

**Alternative C (chosen) — staged waves, greenfield-first, explicit
never-migrate list.** Matches the project's binding constraint: hardware
arrive July 2026, bring-up iteration time is the priority.

## Prior art

- **Linux UIO** — in-kernel `platform_driver` vs. userspace `uio`.
  Same split; PHANES uses `SYS_DRV_INVOKE` + cap handles instead of
  `/dev/uioX` + mmap.
- **Hubris** — all drivers as tasks from day one. PHANES takes the
  middle path: safety-critical drivers stay InKernel until a latency
  guarantee exists.
- **seL4/Genode** — drivers as isolated processes is the default;
  PHANES converges toward this for non-RT drivers in Phases 2–3.

## Unresolved questions

- **`MOTOR_PID_HZ` and `PWM_MAX_INVOKE_US`** — must be determined from
  hardware measurement (July 2026). Wave 2 is blocked until then.
- **Heartbeat protocol.** Exact format of the heartbeat sent by a
  userspace driver to the kernel watchdog (packet type, period, miss
  threshold) needs a spec before Wave 1 ships.

## Future possibilities

- **Hot-swap at runtime.** Replace a userspace driver process without
  rebooting — requires `REGISTRY.switch_active()` from RFC-0002 Phase 4.
- **Motor PID promotion.** If a bounded-latency IPC primitive is
  formally verified on hardware, Motor PID could eventually move to
  UserProcess, removing the last safety-critical InKernel driver.
