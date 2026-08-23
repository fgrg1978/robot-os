# RFC-0033: Bounded Runtime Safety Monitor (Motor Output Envelope)

> **Status:** accepted (design RFC — capability + by-construction cost; no
> measured ratio. AI-native roadmap Fase 1.)
> **Authors:** Fernando Rodriguez
> **Created:** 2026-06-04
> **Last updated:** 2026-06-04
> **Type:** design (`_template.md` shape — correctness by construction)
> **Relates to:** RFC-0017 (brain role / safety case), RFC-0034 (speculative
> actuation — Fase 2, builds on this gate).

---

## Summary

Add a **bounded runtime safety monitor** at the single `MotorCmd→PID→PWM`
chokepoint (`rt_motor_task`): the last line of defence between *any* motor
command and the PWM hardware. It enforces a hard ESTOP override and the
per-robot-type speed cap on the command **magnitude** — limits that were
declared in `safety.rs` (`SAFETY_*_MAX_SPEED_PCT`) but never enforced at the
output. This is a runtime-assurance gate (Simplex pattern), **not** a formally
verified component; "verified" is reserved for the Phase-5 horizon.

This is Fase 1 of the AI-native roadmap and the safety foundation that makes
Fase 2 (speculative actuation, RFC-0034) safe to attempt.

## Motivation — the gap (traced, not assumed)

Tracing both brain-command ingest sites (`decode_actuator_cmd`, TCP @ main.rs
~2738 and UART ~2890) to motor output:

- Non-emergency commands already route through `arbitrate()` (L0–L3
  subsumption); a previous direct-publish bypass bug was fixed, so L0 *is* in
  the path. **Good — but L0 is sensor-reactive** (obstacle, tilt, battery): it
  reacts to the world, it does not bound the command's own magnitude.
- `ActuatorCmd::diff_drive()` clamps to ±100 (the protocol range), **not** to
  the documented safety cap (`SAFETY_WHEELED_MAX_SPEED_PCT = 80`).
- At the actual chokepoint `rt_motor_task` (the single reader of `CH_MOTOR_CMD`
  → PID → PWM), there was **no command-magnitude check and no ESTOP re-check**
  on the output. The watchdog there covers comms-loss, not magnitude.

So an over-limit command (or any future command source that writes
`CH_MOTOR_CMD`) reaches PWM at full magnitude. The gap is real.

## Design

### Placement — the chokepoint, not the ingest sites
Gating at the two decode sites is bypass-prone (miss a path → "always active"
is a lie). The monitor sits at the **single** point where any command becomes
PWM (`rt_motor_task`, right after `motor_cmd_read()`), so it is structurally
unbypassable: every command source — brain (TCP/UART), local behaviors,
reflexes, future speculation — funnels through it.

### The monitor
`behavior::safety::motor_envelope(speed_l, speed_r) -> (i32, i32)`:
1. `estop_is_active()` → return `(0, 0)` (hard stop overrides everything).
2. Per-type magnitude cap: wheeled/ackermann → `SAFETY_WHEELED_MAX_SPEED_PCT`;
   drone/humanoid pass through (their actuation has its own envelope path).
3. Clamp both channels to `±cap`.

`rt_motor_task` applies it to the command before encoder/PID/PWM (both the
closed-loop and open-loop branches).

### Why this is not redundant with L0
L0 = "is the *world* unsafe right now?" (sensor-reactive). The monitor = "is
this *command* within the actuator envelope?" (magnitude). Orthogonal; both
needed. The monitor is also the enforcement point for the previously-unenforced
`SAFETY_*_MAX_SPEED_PCT` constants.

## Cost — by construction, not measured

`motor_envelope` is one branch plus two `clamp`s: O(1), no loop over unbounded
data, no allocation, no I/O. Its cost is bounded **by construction**, not by a
cycle measurement — deliberately so: this session established that WCET/jitter
under QEMU TCG are noise (RFC-0027), so a measured WCET gate here would be
theatre. The added work on the control path is a handful of instructions on a
path that already runs the arbiter; "~0 added jitter" is a structural claim.

Catch behavior (an out-of-range command is clamped; an ESTOP zeroes output) is
true by construction — a property of the gate, not a benchmark.

## Naming

Called a **bounded runtime safety monitor** / runtime-assurance gate, NOT
"verified". Nothing here is formally verified; in a cert-adjacent context
(RFC-0017) "verified" is a loaded, specific claim. Formal verification of the
safety envelope is a Phase-5 item.

## Limitations / future

- v1 enforces ESTOP + speed cap. **Not yet**: acceleration/jerk limits (need
  previous-output state), geofence-predictive checks on the commanded *motion*
  (need position + command projection), drone/humanoid output envelopes.
- The cap uses the per-type constant; a future version may take limits from
  Kconfig / CONFIG.INI for per-deployment tuning.
- Foundation for **RFC-0034 (speculative actuation)**: speculative motor
  commands commit only if they pass this same gate, so speculation can never
  drive the actuators outside the envelope; mispredictions roll back via I-13.

## Alternatives considered

- **Do nothing** (rely on L0 + ±100 clamp): rejected — leaves the documented
  speed cap unenforced and no magnitude/ESTOP gate at the output chokepoint.
- **Gate at the decode sites**: rejected — bypass-prone (two+ sites), not the
  Simplex chokepoint pattern.
