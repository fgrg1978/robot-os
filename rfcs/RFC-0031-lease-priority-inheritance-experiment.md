# RFC-0031: Lease/Capability Priority Inheritance (Experiment)

> **Status:** accepted (I3 — measured benefit, productized, gated default-off)
> **Authors:** Fernando Rodriguez
> **Created:** 2026-06-02
> **Last updated:** 2026-06-02
> **Type:** experiment (`_template_experiment.md` shape)
> **Companion:** Phase B item I3 (capability-aware scheduling)

---

## Summary

A high-priority task that blocks in `lease_wait_return()` waiting for a
lower-priority lessee to return a leased buffer suffers **priority inversion**:
the legacy bitmap scheduler runs every mid-priority task before the lessee, so
the buffer — and the blocked lessor — is held off for the full duration of that
mid-priority work (unbounded for a non-expiring lease, `expire_ticks = 0`).

This experiment makes `lease_wait_return()` apply **priority inheritance**:
while the lessor is blocked, the lessee inherits the lessor's priority and is
re-positioned in the ready queue, so it is scheduled ahead of mid-priority work
and returns promptly. The boost is undone on wake (return, exit, or expiry).

It touches the lease layer (`crates/ipc/src/lease.rs`) and the scheduler
(`crates/sched/src/scheduler.rs`). Compile-time gated by
`robot_os_limits::LEASE_PRIORITY_INHERITANCE` (default **n** → const-eliminated).

## Hypothesis

**Claim:** lease priority inheritance reduces the lessor's inversion hold-off by
a **gross factor (≥ 5×)** under a controlled cross-priority lease scenario.

**Primary metric:** kernel `[I3]` probe line, field `lease_inversion_cyc` —
cycles the lessor is blocked in `lease_wait_return`, emitted by
`i3_lease_inversion_probe` (`kernel/src/main.rs`, qemu-gated), build-time A/B
on the const (same pattern as RFC-0028 I2).

**Baseline number:** `lease_inversion_cyc = 7,032,000` (inheritance off,
4 spinners × 1,000,000-iter burst, 1-hart QEMU, n=1 one-shot probe per boot).

**Target number:** `≤ 1,400,000` (≥ 5× reduction).

**Confidence:** medium-high — the effect is gross (millions of cycles, far above
the 8–40 % TCG `rdcycle` noise floor that defeats microbench-scale experiments
here), measured as a ratio. Re-measure on real hardware (true counter) in 2026-07.

**Time horizon:** one-shot deterministic probe; verdict on first clean A/B (the
effect is structural, not statistical).

## What would make this fail (kill criteria)

The inversion ratio is gross **by construction** (the probe controls the
intervening work), so it is a *benefit demonstration*, NOT a kill criterion.
The experiment can fail on **cost and correctness**, where the criteria live:

- [ ] Uncontended lease acquire/release path regresses at all — the policy must
  be **zero-cost when off** (const-eliminated) and run only on the contended
  block path. ✗ did not fire (const-gated; uncontended ops untouched).
- [ ] Any of the 6 build configs fails to compile clean (0 errors/0 warnings).
  ✗ did not fire (6/6 clean: default/vf2/k1/no-ml/no-mmu/qemu).
- [ ] A correctness defect: lost wake or lost priority restore on **expiry**
  (the I-13 failure class). ✗ addressed — expiry path woken via `wq_wake_by_tid`
  so the lessor observes `Expired` and undoes the boost; `restore_ready_task`
  is a no-op if the lessee already exited.
- [ ] Boot regresses (panic / trap / no steady state). ✗ did not fire (0 panics,
  0 traps, probe completes, boot normal).

## Exit criteria

**Success → `accepted`:** primary metric beats target on a clean A/B; no kill
criterion fires; 6 configs clean. → **Met.** Mechanism kept, gated default-off.

**Failure → `rejected`:** any kill criterion fires → revert / gate off, document.

## Detailed design

### Interface
- Kconfig `LEASE_PRIORITY_INHERITANCE` (bool, default n) → `robot_os_limits`.
- `ipc::lease_wait_return(lease_id)` — the canonical lessor wait (previously a
  documented-but-absent pattern). Snapshots the lessee, boosts it (if enabled
  and the lessor outranks it), blocks until returned/expired, then restores.
- `ipc::lease_return` now wakes the lessor internally (`wq_wake_by_tid`).

### Data structures / scheduler primitives (`crates/sched`)
The naive `pi_boost_task` (field-only priority write) is **insufficient** for a
task already in the ready queue: the legacy bitmap scheduler buckets tasks by
priority at enqueue time, so a field write leaves a queued task in its old
bucket (measured: it produced B ≈ A, ~2 %). New primitives:
- `boost_ready_task(tid, new_prio)` — boost **and** re-bucket a ready task.
- `restore_ready_task(tid, prio)` — symmetric un-boost.
- `task_priority(tid)`, `tid_for_idx(idx)` — supporting getters.
- `cpu_remove(cpu, idx)` (private) — remove a specific idx from its ready bucket.

This is exactly the kind of base-level scheduler change a mature OS avoids;
PHANES can make it. (`pi_mutex` did not need it: its boosted owner is *running*,
not sitting in the ready queue.)

### Edge cases
- Lessor calls wait while lease is `Pending` (granted, not yet accepted): waits
  correctly (not only `Active`).
- Expiry while boosted: lessor woken, observes `Expired`, restores the boost.
- Lessee exits before restore: `restore_ready_task` is a no-op.

## Results

**Verdict: `accepted`.**

- **Final config:** measured via the production `lease_wait_return` path
  (the probe exercises the real mechanism, not an ad-hoc copy).
- **Primary metric (`lease_inversion_cyc`, 4×1M-iter spinners, 1-hart QEMU):**

  | inheritance | lease_inversion_cyc |
  |-------------|---------------------|
  | off (A)     | 7,032,000           |
  | on  (B)     |   810,000           |
  | **ratio**   | **≈ 8.7× reduction** |

- **Kill criteria fired:** none.
- **Cost:** zero when off (const-eliminated); when on, the boost is O(bucket
  length) once per contended block. Uncontended lease ops unchanged.
- **Builds:** 6/6 clean. **Boot:** 0 panics / 0 traps, probe completes.
- **What we learned:** priority inheritance for a *ready* resource holder
  requires re-bucketing it in the scheduler, not just writing its priority
  field — `pi_boost_task` alone yielded no benefit (B ≈ A). `boost_ready_task`
  unlocked the full ≈ 8.7×. Cross-class inheritance (class donation under the
  APS dispatcher) was scoped first but blocked: APS is not yet a live dispatcher
  (smoke-tested only), so the measurable, productizable win is priority-PI on
  the legacy scheduler (this RFC). Class donation is deferred until APS lands.

**Remaining (non-blocking):** userspace `SYS_IPC_LEASE_WAIT_RETURN` syscall (the
kernel function is wired today; the probe exercises it); re-measure on VF2/K1
hardware (true cycle counter), 2026-07.
