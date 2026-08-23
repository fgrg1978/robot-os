# RFC-0036: Brain-Triggered Degraded Mode (Capability Containment)

> **Status:** accepted (design RFC — capability + by-construction cost; no
> measured ratio. AI-native roadmap Fase 3, item #4 — reframed; the "semantic
> capability class" half is a documented half-KILL, deferred to RFC-0020.)
> **Authors:** Fernando Rodriguez
> **Created:** 2026-06-14
> **Last updated:** 2026-06-14
> **Type:** design (correctness/safety by construction)
> **Builds on:** RFC-0003 (capability-typed IPC — extends `CapTable::get`),
> RFC-0033 (bounded runtime safety monitor), RFC-0035 (confidence-aware
> real-time). **Defers to:** RFC-0020 (user-mode drivers — where the criticality
> class becomes load-bearing).

---

## Summary

The brain can put the kernel into a **degraded mode** in which every
write/actuation through a user-task **capability** is denied at the single
`CapTable::get` chokepoint, while the in-kernel control loop keeps running and
safe-stops as normal. The brain arms it over a new `PKT_DEGRADE` packet when it
detects a *situational hazard only it can perceive* — most concretely, **when it
goes blind** (perception/VLM fails for several cycles): knowledge the kernel
cannot derive from its local sensors.

This is a graceful-degradation tier **between** normal operation and ESTOP.
ESTOP is a blunt motor halt; degraded mode *contains the blast radius of
untrusted userspace activity* (no IPC sends, no socket/file writes, no actuation
syscalls) without depending on the AI, while the kernel's own safety layer
(L0 + RFC-0033/0035 envelope + watchdog) remains the authority on physical
motion. The deliverable is a safety property by construction, not a perf ratio.

## Motivation — the honest gap (and what was killed)

Roadmap item #4 was "semantic capabilities — caps with meaning from the brain →
caching / protection / priority." Applying the project's own filters, **most of
#4 collapses**, recorded here as a negative result rather than papered over:

- **Priority → KILLED (re-tread).** RFC-0031 (lease priority inheritance) and
  RFC-0028 (stream priority) already cover this, and *measuring* a cap-priority
  hits the QEMU-TCG substrate wall that killed I1/I6.
- **Caching → KILLED (vague).** No concrete cache, no concrete decision.
- **Semantic criticality class → DEFERRED to RFC-0020 (half-KILL, see below).**
- **Protection (containment) → SURVIVES.** Putting *policy* — what may be done
  with a cap under a runtime condition — at the cap chokepoint is precisely what
  a mature capability OS (seL4) refuses to do (mechanism/policy separation;
  TCB bloat). PHANES co-designs brain + kernel and is young enough to change its
  base (the I+D thesis), so it can.

The traced gap (verified, not assumed):

- `CapTable::get` (`crates/ipc/src/cap.rs`) is the single dereference chokepoint
  (Kani-proven forgery resistance, RFC-0003). It validates kind + generation +
  perms; it has **no notion of a system-wide degraded state**.
- The cap system is **live**, not future scaffolding: `crates/syscall/handlers.rs`
  dereferences `Cap<Channel|Port|Shm|Gpio|I2c|Pwm|Motor|IoRing>` with `WRITE` on
  the real resource syscalls. A containment check in `get` therefore fires for
  real user-task operations across all of these — including actuation syscalls.
- Today the kernel reacts to *physical* hazards locally (L0 `safety_check`) and
  to *per-command* uncertainty (RFC-0035 confidence). It has **no channel** for
  the brain to say "the *situation* is hazardous — contain," and no mechanism to
  contain userspace short of a blunt ESTOP.

## Why the criticality class is deferred (the half-KILL, documented)

The original plan kept a `SafetyCritical` cap class live in degraded mode so the
*safe-stop path* would stay usable. Checked against the code, that is the wrong
call **today**:

- Safe-stop is **in-kernel**: `rt_motor_task` reads `CH_MOTOR_CMD` directly and
  the ESTOP handler calls `motor_stop()` directly — **neither goes through
  `CapTable::get`**. So freezing user-task caps cannot disable safe-stop; the
  class exemption protects a path that the cap layer does not gate.
- Worse, exempting `Cap<Motor>`/`Cap<Pwm>`/… (which *are* on the live user
  syscall path) would *weaken* containment: a buggy/compromised user task could
  keep actuating in degraded mode. Freezing **all** user-task writes is both
  simpler and strictly more restraining.
- A kind-derived class would do **zero** differentiating work today and is dead
  code under the dev-phase no-dead-code rule.

The class becomes genuinely load-bearing only once a **user-mode driver
(RFC-0020)** holds the `Motor` cap and *is* the safe-stop path — then it must be
exempted from the freeze. The criticality class is therefore deferred to that
RFC, where it has a real job. v1 ships the containment without it.

## What is genuinely novel here (passing the filter)

Two composed pieces, neither a re-tread:

1. **The trigger** — a *runtime situational-hazard* signal from the brain. Not
   in static topology (perceived live), not RFC-0035 confidence (that is
   per-command trust; this is a global "I cannot assess the world"), and it only
   ever *constrains*. The kernel cannot derive this from local sensors —
   perception is the brain's job.
2. **The mechanism** — *containment at the capability chokepoint*. A blunt ESTOP
   cannot express "freeze the userspace blast radius while the in-kernel safety
   loop keeps the robot safe." A mature OS will not put this policy in its cap
   layer.

## Design

### Degraded mode (`crates/ipc/src/cap.rs`)

A single global, co-located with the enforcement it drives (mirrors the
`estop` global in `safety.rs`):

```rust
pub fn degraded_set(on: bool);
pub fn degraded_active() -> bool;
```

The containment rule, added to `CapTable::get` **after** the
kind/generation/perms checks (so forgery resistance is unchanged — a
forged/stale/wrong-kind/under-permissioned cap still fails first):

```rust
// Degraded mode (RFC-0036): contain the userspace blast radius. Any
// write/actuation through a user-task cap is denied; READ stays live so
// tasks can still observe. The in-kernel control loop and safe-stop do
// not go through get(), so they are unaffected.
if degraded_active() && need.contains(CapPerms::WRITE) {
    return Err(CapError::Contained);
}
```

`CapError::Contained` is a new, distinct error (not folded into `MissingPerms`)
so callers/tests/Kani can tell containment from a permission bug. The check is
skipped entirely (one relaxed atomic load) when not degraded — **zero** effect
on the normal path.

### Wire (`PKT_DEGRADE = 0x8A`)

Brain → robot, 1-byte payload = *reason* code, mirroring `PKT_ESTOP`:

- `DEGRADE_CLEAR = 0` — exit degraded mode (brain recovered).
- `DEGRADE_REASON_PERCEPTION_BLIND = 1` — perception failed N cycles.
- `DEGRADE_REASON_SENSOR_INCOHERENT = 2` — fused sensor state is inconsistent.
- `DEGRADE_REASON_UNMODELLED_HAZARD = 3` — situational anomaly.

`protocol.py` and `brain_protocol.rs` byte-pinned. Decoded in both rx paths
(TCP + UART) in `kernel/src/main.rs`: reason 0 → `degraded_set(false)` + log;
reason > 0 → `degraded_set(true)` + log. Also cleared on any `MODE_CMD`
(operator override), exactly like ESTOP. Degraded mode is otherwise sticky —
fail-safe; the watchdog safe-stops on comms loss regardless.

### Brain (`server.py`, `protocol.py`)

`DegradeCmd(reason)` + `_send_degrade_cmd`. The trigger is **real**, grounded in
the existing perception-failure path (`_perception_cycle`'s `except` block, which
already STOPs on VLM/LLM error): a consecutive-failure counter raises
`DEGRADE_REASON_PERCEPTION_BLIND` after
`DEGRADE_PERCEPTION_FAIL_THRESHOLD` (= 3) failures in a row — i.e. the brain is
*persistently* blind, not a single dropped frame — and clears (`DEGRADE_CLEAR`)
on the first successful cycle afterwards. Idempotent (a `_degraded_sent` flag
avoids re-sending). Never raised speculatively; ESTOP stays the response to a
hard hazard.

### Why this placement

Reuses the RFC-0003 cap chokepoint (the single `get`), so containment is
structurally unbypassable — exactly as RFC-0033 reuses the motor chokepoint. The
degraded flag is a global set at ingest and read at the chokepoint: coarse
(system-wide), which is correct and conservative for a containment mode.

## Security — constrain-only, the brain cannot elevate

Load-bearing invariant (consistent with RFC-0035): a brain-supplied signal may
only *remove* capability, never add it. Degraded mode strictly *denies*; it
grants nothing. A compromised or hallucinating brain can at worst over-contain
(spurious degrade) — which fails safe — and can never widen access through this
path. ESTOP and the in-kernel envelope remain the authority on physical motion,
independent of the brain.

## Cost — by construction

The containment check is one relaxed atomic load (skipped when not degraded)
plus one compare on the already-hot `get` path. O(1), no allocation, no I/O —
bounded by construction, no measurement needed. A runtime-assurance gate, **not**
a formally verified component ("verified" is the Phase-5 horizon, RFC-0017).
Forgery resistance is preserved: containment is a *post*-validation denial; the
Kani harnesses still hold and a new harness asserts the containment property
(degraded + WRITE ⇒ never `Ok`).

## Limitations / future

- **No criticality class in v1** — deferred to RFC-0020 (user-mode drivers),
  where exempting the safe-stop driver's `Motor` cap from the freeze is real.
  Until then, degraded mode freezes *all* user-task writes (stronger, simpler).
- **Containment is userspace-scoped.** The in-kernel control loop is unaffected;
  physical motion stays governed by L0 + the RFC-0033/0035 envelope + watchdog.
  Live actuation reach grows with driver migration (RFC-0020).
- **WRITE-only, system-wide, sticky.** READ stays live; degraded is global and
  clears on `DEGRADE_CLEAR` / `MODE_CMD`. Per-task or auto-decaying containment
  is future work.
- Composes with RFC-0034: do not speculate (apply predicted commands) while
  contained.

## Results

Capability landed (v1 = containment + trigger; criticality class deferred to
RFC-0020). Validated 2026-06-14:

- **Kernel build:** 5/5 configs clean, 0 warnings (default, vf2, k1, no-ml,
  no-mmu). The new `CapError::Contained` arm was threaded through all 8
  `errno_for_*_err` mappings (→ `EAGAIN`, transient).
- **Boot:** QEMU default boots to steady state, 0 panics/faults (degraded path
  is inert until armed — one relaxed atomic load on the `get` hot path).
- **Brain:** 1318 pytest passed, 0 failures (8 new `test_degrade.py`, incl. an
  integration test driving the *real* `_perception_cycle` trigger: blind streak
  → `PKT_DEGRADE`/PERCEPTION_BLIND once → recovery → `DEGRADE_CLEAR`). The lone
  collection error is a missing optional `hypothesis` dep, unrelated.
- **Protocol byte-sync:** `PKT_DEGRADE = 0x8A` and `DEGRADE_*` reason codes
  (0/1/2/3) identical in `protocol.py` and `brain_protocol.rs`.
- **Cap-layer test + Kani harness** (`degraded_mode_contains_writes`,
  `cap_contained_when_degraded`) added following the existing `cap.rs` test
  convention. Like the pre-existing `cap.rs` unit tests, they are not wired into
  the host `regression-tests` harness (cap.rs's `robot_os_sync` chain is
  RISC-V-only on host).
- **Kani — NOT re-run (caveat).** `get` carries RFC-0003's forgery-resistance
  proofs, and this RFC inserts a branch into it. `cargo kani` is **not installed
  in this environment**, so the proofs were **not re-verified by the prover**.
  Argued safe by **inspection, not verification**: the containment branch is
  placed *after* every forgery check (null/range/occupied/generation/kind/perms),
  so it cannot alter the outcome of any scenario those proofs cover — control
  returns `Err` before reaching it — and the `DEGRADED` static initialises
  `false`, so existing harnesses run with containment off. A `cargo kani` run
  (confirming the forgery proofs survived + exercising the new harness) is a
  **required pre-merge step** wherever Kani is available; until then the
  "proofs hold" claim is by inspection only.
- **Python lint not run here.** `ruff` / `black` are not installed in this
  environment; `ruff check` + `black --line-length 100` on the three changed
  files (`protocol.py`, `server.py`, `tests/test_degrade.py`) is a pre-commit
  step. `mypy --strict` adds **zero** new errors from `DegradeCmd` (the 5
  pre-existing errors are at lines 342/388/433, all before the insert — version
  skew in the local mypy, not this change).

Live actuation reach grows with RFC-0020 (user-mode drivers); the in-kernel
control loop remains the safe-stop authority regardless of degraded mode.
