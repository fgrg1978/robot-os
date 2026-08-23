# Multi-policy scheduler

> Authoritative spec: [RFC-0004](../appendix/rfcs.md).  
> Implementation: `crates/sched/src/{class,partitions,policies}` (W4) +
> `crates/sched/src/aps_state.rs` (W4-int).

PHANES splits CPU time across five **scheduling classes**, each
configured with its own policy. The [Adaptive Partitioning
Scheduler](#aps-the-adaptive-partitioning-combinator) — APS — picks
which class runs at any moment based on per-class budgets, then the
chosen class's policy picks the next task.

## The five classes

| Class             | Default budget | Default policy | Use case                                |
|-------------------|---------------:|----------------|-----------------------------------------|
| `SafetyCritical`  | 20 %           | EDF + CBS      | Geofence check, ESTOP, watchdog         |
| `HardRT`          | 30 %           | EDF + CBS      | Motor PID, IMU sample loop              |
| `SoftRT`          | 25 %           | RR             | Telemetry, network stack                |
| `BestEffort`      | 20 %           | CFS            | AI inference, logging                   |
| `Idle`            |  5 %           | Sporadic       | Background prefetch, opportunistic work |

The defaults sum to **exactly 100 %**. They're tunable in `SCHED.TOML`
per deployment — see [Static topology](./topology.md).

In code: `robot_os_sched::class::SchedClass` is a `#[repr(u8)]` enum;
the discriminant doubles as a slot index into `[ClassBudget; 5]`
arrays so the per-class lookup is a single load.

## The five policies

Each policy implements a common `Policy` trait
(`crates/sched/src/policies/mod.rs`):

```rust
pub trait Policy {
    const CAPACITY: usize;
    fn enqueue(&mut self, meta: TaskMeta) -> Result<(), TaskMeta>;
    fn dequeue(&mut self, tid: u32) -> Option<TaskMeta>;
    fn pick_next(&mut self, now_us: u64) -> Option<TaskMeta>;
    fn len(&self) -> usize;
    fn tick(&mut self, tid_running: u32, dt_us: u32) {}
}
```

All policies are bounded — runqueues are fixed-size arrays so the
implementation is `no_std` + alloc-free.

### FIFO (`policies::fifo::Fifo`)

Fixed-priority FIFO. Highest priority wins; ties broken by
insertion order. No quanta, no fairness — deterministic. Used by
`SafetyCritical` where every task is its own deadline.

### EDF + CBS (`policies::edf_cbs::EdfCbs`)

Earliest-Deadline-First with **Constant Bandwidth Server**. Picks the
runnable task with the smallest `deadline_us` whose CBS server still
has budget; on exhaustion the server pushes the deadline forward by
one period and refills.

CBS prevents a runaway task from stealing more than its declared
reservation: it gets *demoted* (later deadline) instead of *starving*
peers. Liu-Layland admission control rejects new tasks that would
push total utilisation past 100 %.

```rust
// Per-mille utilisation Σ(C_i × 1000 / T_i) ≤ 1000
if q.admission_check(new_budget_us, new_period_us) { … }
```

### Round-Robin (`policies::rr::RoundRobin`)

Each task carries its own `time_slice_us`. The timer ISR decrements
the head's remaining quantum; on exhaustion the head rotates to the
tail and the new head's quantum is refilled. Used by `SoftRT`.

### CFS-style (`policies::cfs::Cfs`)

Tracks `vruntime_us` per task; the pick returns the task with the
smallest virtual runtime. Priority is folded in via an inverse
weight (low numeric priority = small multiplier ⇒ vruntime grows
slower ⇒ task runs more). Used by `BestEffort`.

Deliberately much simpler than Linux CFS — no red-black tree, no
group scheduling, no load weighing beyond a single shift. The
cert-relevant scheduling classes are EDF/FIFO, not CFS.

### Sporadic (`policies::sporadic::Sporadic`)

A capacity-replenishment server: a single shared bucket per CPU
with `capacity_us` per period. Drained as the idle class consumes
time; refilled at the period boundary. Used by `Idle`.

## APS — the adaptive partitioning combinator

`partitions::Aps` is what stitches the five policies together. Each
tick:

1. **Time accounting** — the running class's `consumed_us` is
   incremented by `dt_us` (driven from the timer ISR via
   `aps_state::account`).
2. **Window roll-over** — if `now_us − window_start_us ≥ window_us`
   we reset every class's `consumed_us`; the implementation does
   **multi-window catch-up** in one step so a delayed first tick
   (e.g. at boot when `now_us` is in the millions and window starts
   at 0) doesn't burn the budget repeatedly.
3. **Class selection** (`pick_class`) — three phases:
   - **Phase 1**: pick the most-urgent class that is *runnable*
     **and** under its `min_pct` budget. Guarantees the minimum is
     served first.
   - **Phase 2**: if every runnable class is past `min_pct`, pick the
     most-urgent class still under its `max_pct` cap. Prevents any
     one class from hogging beyond its share.
   - **Phase 3** (degraded): every class is over `max_pct`. Pick the
     most-urgent runnable one anyway — better than an idle CPU.

The matching policy's `pick_next` then returns the actual task.

## The dispatch path

Real-world flow on each timer ISR (`scheduler::schedule()`):

```text
timer ISR
    ↓
schedule()                               (legacy scheduler entry)
    ↓
account current task's APS class         (aps_state::account)
    ↓
deadline / RT / time-slice bookkeeping   (legacy behaviour)
    ↓
do_schedule()
    ↓
if SCHED_USE_APS:
    aps_state::pick_next(cpu)            (APS path)
      ↓ class = aps.pick_class(...)
      ↓ task = policies[class].pick_next()
    idx_for_tid(task.tid)
else:
    find_earliest_deadline OR cpu_dequeue (legacy path)
    ↓
mark old Ready, enqueue both legacy + policy
mark new Running, set_current(class, tid)
context_switch()
```

The `SCHED_USE_APS` flag is an `AtomicBool` (default `false` in
Phase 1) so the dispatch path is a **runtime toggle**. The bookkeeping
runs underneath whether the flag is on or off — flipping to `true`
sees an already-warm APS state.

## Why this matters

- **Cert auditors** want proof that the safety class can always
  meet its budget when runnable. The APS Phase-1 + multi-window
  catch-up guarantees this; verified in `formal/tla/sched_aps.tla`
  (13,589 states explored, all invariants satisfied).
- **Real-time engineers** want a way to declare deadlines without
  hoping the scheduler honours them. EDF + CBS does — declared
  budgets are enforced, missed deadlines are surfaced via the
  `cbs.exhausted()` flag.
- **Best-effort code** (AI inference, telemetry) gets a fair share
  via CFS without starving safety. The window mechanism prevents
  the inverse too: best-effort never gets *less* than its declared
  minimum when runnable.

## See also

- [RFC-0004](../appendix/rfcs.md) — design rationale
- `crates/sched/src/{class,partitions,policies}.rs` — implementation
- `crates/sched-policy-tests/` — 44 host tests
- `formal/tla/sched_aps.tla` — formal spec (TLC verified)
- `formal/proofs/INVARIANTS.md` — INV-5 (budgets ≤ 100 %)
