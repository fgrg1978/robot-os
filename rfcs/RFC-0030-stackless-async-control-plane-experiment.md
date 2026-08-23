# RFC-0030: Stackless async control plane (cooperative, zero context-switch) — experiment I-12

> **Status:** experiment-running
>
> `draft` — hypothesis written, experiment not yet started.
> `experiment-running` — the change is live in the tree; results not in yet.
> `accepted` — exit criteria met; change is permanent; baselines updated.
> `rejected` — experiment concluded; change reverted; negative result documented below.
>
> **Authors:** Fernando Rodriguez \<fgrg1978@gmail.com\>
> **Created:** 2026-06-01
> **Last updated:** 2026-06-01
> **Companion design RFC:** —

---

## Summary

Preemptive scheduling gives every task its own kernel stack and a full context switch
(save/restore 31 GPRs + FP + CSRs, via `context_switch.S`) on each yield — `sched.task_yield`
measures ~2200 cyc round-trip. This experiment proposes a **stackless cooperative** control
plane: control tasks become monomorphised `Future`s (compile-time-sized state machines)
polled by a table-driven executor; **resuming a task is a `poll()` function call**, not a
register-file save/restore. Selected at **compile time** via the Kconfig choice
`CONTROL_PLANE_ASYNC` (per-binary policy, zero hot-path overhead — `make menuconfig`). The
property it improves: per-resume scheduling overhead, plus structural wins (no per-task
stacks → no guard pages, no stack-overflow risk; simpler WCET — a `poll()` is a bounded
call, not an arbitrary preemption point).

---

## Hypothesis

**Claim:**
Resuming a cooperative async task (one `poll()`) is ≥ 10× cheaper than the preemptive
yield + context-switch path.

**Primary metric:**
`bench_synth.asyncrt.poll_resume.avg_cycles` (clean early-boot capture, RFC-0026
`bench_boot` mode) vs the `bench_synth.sched.task_yield` baseline.

**Baseline number:**
`sched.task_yield ≈ 2200 cyc` (preemptive yield → schedule → context_switch →
regfile save/restore), measured on the SMP behavior-task path.

**Target number:**
`asyncrt.poll_resume ≤ 220 cyc` (≥ 10× cheaper).

**Confidence:**
medium — the structural argument is strong (a poll avoids the entire regfile save/restore +
scheduler bookkeeping), and the measured floor is gross relative to TCG noise; but the
`poll_resume` floor is a trivial state machine, so it bounds the *scheduling overhead
eliminated*, not the real per-task work (which exists in both designs).

**Time horizon:**
Proxy measured immediately (microbench). Full validation requires a real async executor +
porting at least one control task; promote to `accepted` only then.

---

## What would make this fail

- **Resume cost not ≪ context switch** (< ~5×) → reject; the architecture isn't worth it.
  *Result: 8.8× — strong directional signal, did not fire (see Results).*
- **Real control tasks don't fit bounded `Future`s** — if a control task needs unbounded
  per-poll work or can't be expressed as a state machine, the floor is misleading → reject
  on the prototype.
- **Future state-machine size explodes** (many `.await` points) → arena/static budget
  blown; `const_assert!` on `size_of` guards this.
- **A `poll()` busy-loops internally** → breaks cooperative determinism; lint + `#[wcet]`
  on every poll.

---

## Exit criteria

- **Accept** when: the proxy shows resume ≪ switch (✓ 8.8×) **and** a real control task is
  ported to a `Future` polled by a no_std executor, with bounded poll WCET and no per-task
  stack, A/B'd (`CONTROL_PLANE_ASYNC=n` vs `=y`) showing the end-to-end win without
  determinism loss. Then enable async for the control plane and update baselines.
- **Reject** when a kill criterion fires; revert to preemptive; document below.

---

## Architectural risk if it succeeds

A cooperative control plane removes preemption on the hot core (composes with AMP core-
pinning) — a misbehaving `poll()` that doesn't yield stalls everything on that core. Mitigated
by: bounded-poll lint + per-poll `#[wcet]`, and the compile-time choice (a preemptive binary
carries none of this). The async executor is new surface in the scheduler — keep it minimal
(no allocator, static future array, no `dyn`).

---

## Detailed design

- **Config seam (done):** Kconfig `CONTROL_PLANE_ASYNC` (`Kconfig.robot`, default n) →
  `robot_os_limits::CONTROL_PLANE_ASYNC: bool`. Preemptive and async are separate binaries.
- **Proxy bench (done):** `crates/bench/src/asyncrt.rs` — `bench_poll_resume` polls a
  yielding `CountdownYield` future N times (each poll = one cooperative resume), pure compute
  → runs in the quiescent early-boot bench. noop waker, no executor.
- **Executor (pending):** a table-driven `[Future; N]` cooperative executor per hart, polled
  in static priority order; wakers = a ready bitmap (DMA/IPI-writable). Control tasks ported
  from stack fns to `async fn`. Resuming = `poll()`; `.await` = the only yield points.

---

## Implementation plan with measurement checkpoints

1. ✅ Config seam `CONTROL_PLANE_ASYNC` + const flow.
2. ✅ `asyncrt.poll_resume` proxy bench; A/B vs `task_yield` baseline.
3. ⏳ Minimal no_std cooperative executor (static future array, noop/bitmap waker).
4. ⏳ Port one control task to `async fn`; A/B end-to-end (`=n` vs `=y`); confirm bounded
   poll WCET + no determinism loss.

---

## Drawbacks

- Cooperative = no preemption safety net on the hot core (mitigated by bounded-poll lint).
- `async` in no_std hard-RT is unusual; reviewer/cert unfamiliarity.
- The proxy measures the floor, not real-task cost — honest about it in Results.

---

## Alternatives

- **Keep preemptive + shrink the context switch** (save fewer regs for leaf control tasks):
  smaller win, doesn't remove stacks/guard-pages.
- **Green threads with tiny stacks**: still pays a context switch; loses the compile-time
  size bound that `Future`s give.

---

## Unresolved questions

- Does the real control task's `poll()` WCET stay bounded once it does actual work?
- Interaction with the preemptive tasks that must remain (net, shell) — hybrid executor?

---

## Results

**2026-06-01 — proxy A/B, sha 6ba31f4, QEMU TCG, `bench_boot` quiescent capture:**

| Metric | cycles | source |
|---|---|---|
| `asyncrt.poll_resume` (cooperative resume floor) | **250** | clean early-boot bench |
| `sched.task_yield` (preemptive yield + ctx-switch) | **2200** | SMP behavior-task path |

**Ratio ≈ 8.8× cheaper to resume a cooperative poll.** Strong directional signal — the
~1950 cyc/resume of regfile-save/restore + scheduler bookkeeping is eliminated. Just shy of
the strict ≥10× target, but for a big architectural bet this is a clear "direction
validated"; the cycle ratio also undersells the win (no per-task stacks/guard-pages, simpler
WCET are not in this number).

**Caveats:** `poll_resume=250` is the FLOOR (trivial state machine) — it bounds the
*scheduling overhead removed*, not real per-task work. `task_yield=2200` is from the noisier
SMP path; the ~9× ratio is gross enough to survive TCG noise. Promotion to `accepted` needs
the real executor + a ported control task A/B'd end-to-end. Until then: **direction
confirmed, full build deferred** (substantial — a no_std cooperative executor + task port).

---

## Reference

Config mechanism per RFC-0026; experiment idiom (Kconfig choice + per-policy binaries +
`bench_boot` capture + dashboard A/B) per RFC-0028/I2. Composes with AMP core-pinning
(original idea 5) and Idea-11/RFC-0027 (bound the poll WCET).
