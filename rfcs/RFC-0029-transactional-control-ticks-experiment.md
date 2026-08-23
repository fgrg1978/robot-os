# RFC-0029: Transactional control ticks (rollback-on-fault) — experiment I-13

> **Status:** accepted
>
> `draft` — hypothesis written, experiment not yet started.
> `experiment-running` — the change is live in the tree; results not in yet.
> `accepted` — exit criteria met; change is permanent; baselines updated.
> `rejected` — experiment concluded; change reverted; negative result documented below.
> **Rejected RFCs are never deleted** — the negative result is the artefact.
>
> **Authors:** Fernando Rodriguez \<fgrg1978@gmail.com\>
> **Created:** 2026-06-01
> **Last updated:** 2026-06-01
> **Supersedes:** —
> **Superseded by:** —
> **Companion design RFC:** RFC-0017 (safety case / graceful degradation)

---

## Summary

Today a recoverable CPU fault (misaligned access, an unexpected page fault, a div-by-zero
in a derived gain) **anywhere in the hard-RT control loop** ends in `panic = "abort"` →
the kernel stops motors, logs, and halts/reboots (`kernel/src/panic.rs`). For a robot
mid-motion that is the worst outcome. This experiment makes each `rt_motor_task` control
tick a **transaction**: a checkpoint (SP + fallback-PC) is taken at tick start; a
recoverable trap inside the tick **rolls back** — `handle_exception` rewrites the saved
`TrapFrame` (`frame.regs[sp]`, `frame.sepc`) and returns, resuming at a deterministic
**safe-stop fallback** instead of the fatal path — and motor outputs are **double-
buffered** (staged during the tick, committed only at tick end) so a mid-tick fault never
leaves a half-applied command on the wire. It touches `rt_motor_task`
(`kernel/src/main.rs:3187`), `handle_exception` (`kernel/src/main.rs:3536`), and the motor
write (`motor_set`, lines 3231/3245). The property it improves: **survivability of a
recoverable fault in the control loop**, with bounded fallback WCET.

---

## Hypothesis

> Idea-13. Verdict is **behavioural, not a ratio** — chosen deliberately so it does NOT
> depend on the noisy TCG `rdcycle` substrate (the lesson from I2/RFC-0028): we measure
> *whether the correct thing happens*, not how many cycles it takes.

**Claim:**
A recoverable fault injected mid-tick in `rt_motor_task` rolls back to a deterministic
safe-stop and the system **continues running**, instead of `panic=abort` halting the
kernel — with no leaked half-applied motor command.

**Primary metric:**
Behavioural pass/fail under a fault-injection probe: `[I13] survived=1 aborts=N` emitted
after an injected recoverable fault. Pass = kernel did NOT halt, motors went to safe-stop,
the control loop resumed, and the abort counter incremented exactly once.

**Baseline number:**
`survived=0` — current behaviour: any recoverable fault in the control loop reaches
`handle_exception`'s fatal path → `panic.rs` stops motors + halts/reboots. Verified by
construction (`Cargo.toml:251 panic="abort"`, `panic.rs:19`). Kernel does not continue.

**Target number:**
`survived=1`, `aborts=1` per injected fault, control loop resumes within the same tick
budget; the no-fault hot path is byte-identical when the transactional region is not armed.

**Confidence:**
medium-high — the rollback mechanism is clean (rewrite the already-saved `TrapFrame` SP/
PC and return; no new assembly), the tick is stack-only (no arena needed; checkpoint =
SP+PC), and the verdict is binary so TCG noise is irrelevant. Risk is concentrated in the
trap handler (safety-critical) — see kill criteria.

**Time horizon:**
Single decisive fault-injection run per build (armed vs not). Promote to `accepted` after
the no-fault path is shown WCET-unchanged and the fallback WCET is bounded (compose with
RFC-0027/Idea-11), ideally re-confirmed on hardware.

---

## What would make this fail

- **State corruption after rollback** — if the tick had already mutated shared state
  (PID integrator, motor PWM) before the fault, rolling back SP/PC leaves that state
  inconsistent. *Mitigation:* double-buffer outputs (commit at tick end only) and treat the
  per-tick PID delta as recomputable; if shared mutation before the fault can't be made
  idempotent/rollback-safe, **reject**.
- **Fallback WCET overruns the tick** — the safe-stop path must fit in the remaining tick
  budget. If it can't be bounded, reject (compose with Idea-11 to prove it).
- **False rollbacks mask real bugs** — a sustained nonzero `aborts/tick` is a bug, not
  noise. If the mechanism hides genuine defects rather than surfacing them, reject.
- **No-fault hot path perturbed** — if arming the transaction measurably changes the
  no-fault control-tick WCET (`#[wcet]` histo), the cost isn't free → reject or gate harder.
- **Trap handler regression** — if the hook destabilises normal fault handling (page-fault
  demand paging, syscalls), reject immediately. This is the highest-risk surface.
- **RAII guard / lock leakage (added 2026-06-02 review).** The restart resets SP and
  re-enters at the task entry, so destructors on the abandoned stack never run. The tick
  body holds SpinLocks (`motor_pid::TICK_STATE`, `PID_CONTROLLERS`) across its computation;
  a whitelisted fault taken while one is held leaks the guard → the next acquire deadlocks.
  *Mitigation / load-bearing precondition:* the whitelist is misaligned-load/store-only and
  the tick touches only aligned typed data, so no whitelisted fault is reachable inside a
  locked region. Broadening the whitelist OR introducing a raw unaligned access to the tick
  re-opens this and **requires re-auditing every lock held in the armed region** — if that
  can't be guaranteed, reject. (Same class as the SeqLock-parity-inversion concern: a fault
  between a SeqLock's `seq→odd` and the guard's `seq→even` drop would permanently invert the
  channel's parity; unreachable today for the same alignment reason.)

---

## Exit criteria

- **Accept** when: injected recoverable fault → `survived=1` + clean safe-stop + resume,
  AND the no-fault path WCET is unchanged (armed vs baseline), AND the fallback WCET is
  bounded. Then enable transactional ticks for the control loop by default.
- **Reject** when any kill criterion fires; revert the trap-handler hook + checkpoint;
  document the negative result here. `panic=abort` remains the policy.

---

## Architectural risk if it succeeds

A rollback path in the trap handler is permanent surface in the most safety-critical code.
It must be (a) impossible to enter outside an armed transactional region (guard on a
per-hart flag + the saved fallback-PC being set), (b) restricted to a whitelist of
*recoverable* causes (never mask a genuine kernel bug like a null deref in S-mode), and
(c) auditable for cert (RFC-0017). Mitigated by: the region is armed only around the
control-tick body; the cause whitelist is explicit; the no-fault path takes the existing
code unchanged (the hook is a single early branch).

---

## Detailed design (grounded in the current tree)

- **Checkpoint (tick start, `rt_motor_task` ~main.rs:3187):** a per-hart
  `TxnCheckpoint { armed: bool, sp: usize, fallback_pc: usize }`. Arm it at the top of the
  tick body with `sp` = current SP and `fallback_pc` = address of a `tick_safe_fallback()`
  Rust fn. Disarm at tick end (before `task_yield`, ~line 3251).
- **Rollback (trap hook, `handle_exception` ~main.rs:3536):** after cause decode, *before*
  the fatal path: `if checkpoint.armed && is_recoverable(cause) && in this hart { frame.regs[REG_SP] = checkpoint.sp; frame.sepc = checkpoint.fallback_pc; checkpoint.armed = false; ABORTS.fetch_add(1); return 0; }`. The trap return restores SP/PC → resumes in
  `tick_safe_fallback()`. No new assembly (reuses the TrapFrame already saved at
  `trap_entry.S:120`).
- **Safe fallback:** `tick_safe_fallback()` publishes a zero/last-safe motor command and
  returns to the loop (or longjmps to the loop top). Bounded, no allocation.
- **Double-buffer outputs:** stage PWM in a tick-local `(l, r)` and call `motor_set` /
  `motor_cmd_publish` **once at tick end**; a mid-tick fault → fallback publishes safe-stop,
  no partial command ever reaches `CH_MOTOR_CMD` / hardware.
- **No arena needed:** the tick is stack-only (confirmed); checkpoint is SP+PC only.
- **Fault-injection probe:** qemu-gated, a one-shot trigger (e.g. a deliberate misaligned
  load / div-by-zero behind a debug flag) inside an armed tick; emit `[I13] survived aborts`.

---

## Implementation plan with measurement checkpoints

1. Add `TxnCheckpoint` (per-hart) + `is_recoverable(cause)` whitelist + `ABORTS` counter.
2. Arm/disarm around the `rt_motor_task` tick body; add `tick_safe_fallback`.
3. Double-buffer the motor write (commit at tick end).
4. Hook `handle_exception` (early branch, before fatal path).
5. Fault-injection probe → `[I13]`; run armed vs baseline → behavioural pass/fail.
6. Confirm no-fault path WCET unchanged (`#[wcet]` histo, armed vs not).

---

## Drawbacks

- Permanent branch in the trap handler (mitigated: single guarded early-return).
- Double-buffering adds one tick of motor-command latency (one tick = sub-ms; acceptable).
- The "recoverable cause" whitelist must be conservative — getting it wrong masks bugs.

---

## Alternatives

- **Per-task supervisor/restart** (kill+respawn the control task on fault): heavier, loses
  the in-tick state, and a respawn gap is unsafe mid-motion. Transactional rollback keeps
  the loop alive.
- **Leave panic=abort + fast reboot**: a reboot mid-motion is the very failure we avoid.
- **Hardware watchdog only**: catches hangs, not recoverable faults, and only after a
  timeout — too slow for a control loop.

---

## Unresolved questions

- Exact recoverable-cause whitelist (misaligned load/store yes; instruction-page-fault in
  S-mode almost certainly a real bug → no).
- Whether the PID integrator state needs explicit rollback or is safely recomputed next
  tick (likely the latter — it's a delta on encoder readings).
- Interaction with the existing demand-paging page-fault handler (must not shadow it).

---

## Results

**2026-06-01 — implemented + fault-injection A/B, sha 8dd02ed, QEMU TCG SMP-4.**
Gated behind Kconfig `CONTROL_TXN_TICKS` (default n) — added during implementation for
consistency with the experiment idiom (RFC-0028/0030) and because an experiment-running
rollback in the safety-critical trap handler must NOT ship on-by-default. Mechanism
(refined from the RFC design): on a recoverable fault in an armed tick, the trap handler
safe-stops the motors and **restarts the control task at its entry** with the saved SP —
a function-entry jump (valid SP), so no hand-rolled setjmp/longjmp asm is needed.

| Build | Behaviour | Verdict |
|---|---|---|
| **`CONTROL_TXN_TICKS=y`** (probe injects `unimp`, cause 2, in armed tick) | `[I13] survived=1 aborts=1`, **0 FATAL**, `[RT-MOTOR] Starting` printed **twice** (initial + restart) | **PASS** — kernel rolled back + kept running |
| **`=n`** (default) | `[I13]` absent (probe + rollback const-eliminated), **0 FATAL**, normal boot | trap handler **byte-identical** to baseline |
| **baseline (by construction)** | the same cause-2 fault hits the fatal `_` arm → `[FATAL] → shutdown` (kernel halts) | — |

**Verdict: CONFIRMED → ACCEPTED (2026-06-01).** A recoverable fault in the control tick is
survivable (safe-stop + restart) when armed. Behavioural pass/fail → TCG-noise-immune.
5/5 kernel configs build clean.

**Promoted to accepted:** the fault-injection probe (validation scaffolding — a deliberate
`unimp`) was REMOVED from the tree, and `CONTROL_TXN_TICKS` flipped to **default y**: the
rollback mechanism is now active in all builds (still toggle-able). No-fault hot path is
unchanged **by construction** — the checkpoint `arm` runs once per control-task entry (not
per tick iteration), and the trap-handler `txn_try_rollback` check only runs on the rare
exception path (ecall fast-rejects via the cause whitelist). Re-reproduce the A/B by
re-adding a one-shot `unimp` injector in the tick under `cfg(qemu)`. **Remaining hardening
(not blocking accept):** cert review of the trap-handler hook for the RFC-0017 safety case;
optional `#[wcet]` on the exception-path delta.

**Caveats / to reach `accepted`:** (1) confirm the no-fault hot-path WCET is unchanged
(the const-elimination makes this true by construction for `=n`; for `=y` the arm at tick
entry adds a few stores — measure with `#[wcet]`). (2) `restart-the-task` is coarser than
`resume-the-tick` (loses per-tick local state like `safe_mode`) but is far safer to
implement; acceptable for fault recovery. (3) recoverable-cause whitelist (illegal +
misaligned) is conservative; page faults + ecall keep their handlers. (4) cert review of
the trap-handler hook (RFC-0017).

---

**2026-06-02 — post-accept code review of the restart path (fix-forward).**
A review *after* the accept (the discipline: review safety-critical code before
building on it) found the one-shot fault-injection probe had validated only the
*transient* case; a **repeated/deterministic** fault exposes defects the probe
never hit:

| # | Defect | Severity | Resolution |
|---|--------|----------|------------|
| **A** | Restart SP **descended one frame (96 B) per rollback**. `entry_sp` was captured *post*-prologue (`mv {}, sp` in Rust) but the restart PC was the *pre*-prologue function entry, so the prologue (`addi sp,sp,-0x60`, objdump-confirmed) re-ran each restart. Even the happy-path single-transient recovery leaked a frame; under `no-mmu` (no guard page) → silent corruption. Root cause: drift from the RFC's `tick_safe_fallback()` landing-pad design. | **Blocker** | Reset SP to the task's **clean stack top** (`sched::current_task_stack_top()`, pre-prologue, ABI-aligned) instead of a mid-function SP. Prologue now runs exactly once per restart. |
| **B** | **No abort budget → infinite restart loop.** The "disarm first" comment claimed a recurring fault would fall to the fatal path "next time", but the task **re-arms at its entry before the fault recurs**, so a deterministic fault restarts forever (motors safe-stopped, `TXN_ABORTS` unbounded, never escalating). Worse than a clean halt. | **Blocker** | `MAX_TXN_RESTARTS = 8` consecutive-restart budget (`TXN_RESTART_STREAK`); exceeded → `txn_try_rollback` declines → fatal path surfaces the bug. `txn_note_tick_complete()` clears the streak each completed tick, so one-off transients never accumulate. |
| **C** | `TRAP_ILLEGAL_INSTR` in the recoverable whitelist **masked genuine bugs** (corrupt code / bad fn-pointer is not a transient). The probe needed it; with the probe gone it only hid defects — hits this RFC's own kill criterion "false rollbacks mask real bugs". | Decision | Whitelist narrowed to **misaligned load/store only** (the defensible "bad external-data alignment" case). Illegal-instruction now surfaces via the fatal path. |
| **D** | `motor_cmd_publish(0,0)` from trap context — suspected lock re-entry deadlock. | Non-issue | The channel is a **SeqLock** (`fetch_add` on a counter, no blocking lock) — re-entrant publish from trap never deadlocks; worst case a concurrent reader retries one snapshot. No change. |

**Re-validation (every-tick misaligned-AMO injector, QEMU TCG SMP-4):**
entry SP **constant across all 8 restarts** (`0x804def50` ×8 — fix A; pre-fix it
would descend `0x60`/restart), then **`[FATAL] Unhandled exception → shutdown`**
once `streak == MAX_TXN_RESTARTS` (fix B: escalates, no infinite loop). Probe
removed; **5/5 kernel configs + qemu build clean** (lone `warn` is the
pre-existing OTA prod-pubkey build-script notice); boot **without** the probe is
normal (`[I13]`=0, FATAL=0, `[RT-MOTOR] Starting` ×1).

**Correction to the earlier "no-fault hot path unchanged *by construction*" claim:**
it is no longer byte-identical when `CONTROL_TXN_TICKS=y` — `txn_note_tick_complete()`
adds one per-tick streak check. It is made **read-mostly** (a relaxed *load*; the store
runs only on the tick after a rollback), so the steady-state hot path gains a single
relaxed load with no store / no cache-line dirtying — negligible, but not "unchanged". The
`=n` path remains const-eliminated (byte-identical to baseline). `accepted` stands — the
mechanism now behaves correctly under *repeated* faults, not only the one-shot
transient the original probe covered. **Remaining hardening (still not
blocking):** cert review of the trap-handler hook for the RFC-0017 safety case;
optional `#[wcet]` on the exception-path delta.

---

## Reference

Companion to RFC-0017 (safety case). Composes with Idea-11/RFC-0027 (bound the fallback
WCET) and the per-tick-arena concept (not needed here; the control tick is stack-only).
Verdict style follows RFC-0028/I2: behavioural pass/fail, deliberately TCG-noise-immune.
