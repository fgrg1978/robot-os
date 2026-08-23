# RFC-0004: Multi-Policy Hierarchical Scheduler — Constitutional

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES adopts a hierarchical scheduler with **scheduling classes
that own CPU budgets** at the top level (Adaptive Partitioning style)
and **per-class scheduling policies** at the bottom level
(FIFO / EDF + CBS / RR / CFS / Sporadic Server). Each task is assigned
a class and per-policy parameters in `SCHED.TOML` at boot. The result
is a single OS that can serve a wheeled robot, a drone, a humanoid,
and an automotive ECU — the same kernel binary, different topology.

This is the **constitutional rule** for all task scheduling.

## Motivation

The current scheduler is a 32-priority preemptive RR with an RT
threshold. Good as a starting point; insufficient for cert and
multi-use:

- **No CPU budget** between task groups. A misbehaving normal-prio
  task can starve everyone above the RT threshold (we already saw
  this with the 15 stress-test workers crowding out OTA recv).
- **No deadlines.** Cyclic tasks (100 Hz IMU) are scheduled by
  priority, not deadline. Works most of the time; breaks under load.
- **No FFI proof for cert.** ISO 26262 ASIL-D needs a mechanical
  argument that a low-ASIL task cannot starve a high-ASIL one.
  Priority + RR is hand-waving; budgeted partitioning is proof.
- **No multi-use.** Today the same kernel binary serves all
  deployments through ad-hoc `task_create_affinity` calls. We need
  declarative deployment-specific config.

This RFC defines a single mechanism that solves all four problems and
maps cleanly onto AUTOSAR Adaptive's POSIX scheduling policies
(SCHED_FIFO / SCHED_RR / SCHED_DEADLINE).

## Detailed design

### Two-layer hierarchy

```
                    +------------------------------------------+
                    |   Adaptive Partitioning (top layer)      |
                    |   classes own CPU budget percentages     |
                    +-------------------+----------------------+
                                        |
        +-----------+-------------------+---------+----------------+
        |           |                   |         |                |
   SafetyCritical HardRT             SoftRT    BestEffort        Idle
   (FIFO)         (EDF + CBS)        (RR)      (CFS-like)        (Sporadic)
   prio 0–7       deadline-driven    prio 8–15 prio 16–30        prio 31
   min 15% CPU   min 30% CPU         min 20%   min 5% / max ∞   min 0%
```

### Five scheduling classes (top layer)

| Class | Use | Min budget per CPU | Max budget |
|-------|-----|--------------------|-----------:|
| `SafetyCritical` | motor PID, ESTOP, watchdog | 15% | unbounded |
| `HardRT` | sensor fusion 100 Hz, AHRS | 30% | 50% |
| `SoftRT` | behavior, telemetry | 20% | 40% |
| `BestEffort` | shell, logging, perception | 5% | unbounded |
| `Idle` | analytics, garbage collection | 0% | 5% |

A class's "minimum budget" is **guaranteed**: even if every other
class is hot, this class still gets that share over a budget window
(default: 1 ms). This is the FFI mechanism for cert: a low-ASIL
class cannot drag a high-ASIL class below its guaranteed share.

A class's "maximum budget" is a **cap**: even if other classes are
idle, this class doesn't consume more than its cap. Prevents a soft
task from monopolising CPU by busy-looping.

Budget accounting uses Constant Bandwidth Server (CBS) tokens
replenished at the start of each window.

### Five scheduling policies (per-class, bottom layer)

#### `SchedFifo` — for `SafetyCritical`

Strict priority preemptive. A higher-priority task immediately
preempts a lower one. No time slicing. Within a priority level, FIFO
order. Run-to-block.

Use: motor PID, ESTOP. Determinism > fairness.

#### `SchedEdf` — for `HardRT`

Earliest-Deadline-First with Constant Bandwidth Server. Each task
declares `(deadline, period_us, budget_us)`. The scheduler picks the
task with the closest absolute deadline; CBS bounds CPU consumption.

Admission control: when a task is added (via topology or runtime),
the scheduler checks `Σ(budget_us / period_us) ≤ U_max` (Liu-Layland;
U_max = 1.0 for EDF). If exceeded, the new task is rejected.

Use: 100 Hz IMU, 50 Hz odometry, 1 kHz joint control.

#### `SchedRr` — for `SoftRT`

Priority + time slice. Time slice = 10 ms (configurable). Within a
priority, round-robin.

Use: behavior, telemetry, network polling.

#### `SchedCfs` — for `BestEffort`

Completely Fair Scheduler-like. Tasks share CPU time proportionally
to their `nice` value. No priority preemption within the class.

Use: shell, logging, non-critical perception.

#### `SchedSporadic` — for `Idle`

Sporadic server: bursty work allowed but bounded. A task is given a
budget that replenishes after a period. Once exhausted, the task
yields until replenishment.

Use: analytics, GC, opportunistic computation.

### Trait API

```rust
// crates/sched/src/api.rs

pub trait Scheduler: Sync {
    /// Pick the next task to run on `cpu` at time `now`.
    fn pick_next(&self, cpu: usize, now: u64) -> Option<TaskIdx>;

    /// Insert a ready task. `params` carries policy-specific data
    /// (priority, deadline, period, budget).
    fn enqueue(&self, idx: TaskIdx, params: SchedParams);

    /// Remove a task (block, exit).
    fn dequeue(&self, idx: TaskIdx);

    /// Per-CPU tick: replenish budgets, advance deadlines, check
    /// time-slice expiry.
    fn tick(&self, cpu: usize, now: u64);

    /// Admission control: would adding this task violate utilisation
    /// bounds for its class?
    fn admit(&self, params: &SchedParams) -> bool;

    /// Stats for /proc/sched, telemetry.
    fn stats(&self) -> SchedStats;
}

#[derive(Clone, Copy, Debug)]
pub struct SchedParams {
    pub class:    SchedClass,
    pub priority: u8,           // within class
    pub deadline_us: u32,       // EDF only
    pub period_us:   u32,       // EDF / Sporadic
    pub budget_us:   u32,       // EDF / Sporadic / CFS share
    pub affinity:    CpuMask,   // CPU pinning, if any
}

pub enum SchedClass {
    SafetyCritical,
    HardRT,
    SoftRT,
    BestEffort,
    Idle,
}
```

### Per-policy implementation skeleton

```rust
// crates/sched/src/policies/edf.rs

pub struct EdfScheduler {
    queues: [SortedReadyQueue; MAX_CPUS],   // sorted by abs_deadline
    cbs:    [CbsState; MAX_TASKS],          // budget bookkeeping
}

impl Scheduler for EdfScheduler {
    fn pick_next(&self, cpu: usize, now: u64) -> Option<TaskIdx> {
        let q = &self.queues[cpu];
        let candidate = q.peek_min_deadline()?;
        if self.cbs[candidate].has_budget(now) {
            Some(candidate)
        } else {
            // CBS exhausted; demote to Idle until next replenishment.
            None
        }
    }
    // ...
}
```

EDF queue is a sorted heap (binary or pairing); insertion O(log N),
peek O(1). We use a fixed-size no-alloc heap from `heapless` or
hand-rolled.

### Top-level dispatch (the partition combinator)

When `feature = "sched-partition"` is active, the active scheduler is
a `PartitionScheduler` that:

1. Tracks each class's budget consumption per CPU per window.
2. Picks a class that has remaining budget and ready work.
3. Delegates to that class's `Scheduler::pick_next`.

```rust
pub struct PartitionScheduler {
    classes: [Box<dyn Scheduler>; 5],     // one per class
    budgets: [BudgetState; 5],            // per-class accounting
}

impl Scheduler for PartitionScheduler {
    fn pick_next(&self, cpu: usize, now: u64) -> Option<TaskIdx> {
        // Step 1: ensure SafetyCritical's minimum is met.
        if self.budgets[SC].under_minimum(cpu, now) {
            if let Some(t) = self.classes[SC].pick_next(cpu, now) { return Some(t); }
        }
        // Step 2: serve other classes by priority, capped by budget.
        for class in [HardRT, SoftRT, BestEffort, Idle] {
            if !self.budgets[class].over_maximum(cpu, now) {
                if let Some(t) = self.classes[class].pick_next(cpu, now) {
                    self.budgets[class].consume(cpu, /* until next pick */);
                    return Some(t);
                }
            }
        }
        None
    }
    // ...
}
```

### `SCHED.TOML` (declarative deployment config)

Each class and each task is declared at boot. See RFC-0005 for the
full schema. Excerpt:

```toml
[class.safety_critical]
cpu_budget_min_pct = 15
cpu_budget_max_pct = 100
policy = "fifo"
priority_range = [0, 7]

[class.hard_rt]
cpu_budget_min_pct = 30
cpu_budget_max_pct = 50
policy = "edf"
admission_control = true

[task.rt_motor]
class = "safety_critical"
priority = 4
pinned_cpu = 0

[task.sensor_ahrs]
class = "hard_rt"
period_us = 10_000          # 100 Hz
deadline_us = 9_000         # must finish in 9 ms
budget_us = 2_000           # CBS reserves 2 ms / 10 ms

[task.behavior]
class = "soft_rt"
priority = 12

[task.shell]
class = "best_effort"
priority = 20
```

### Performance model

Hot path (`pick_next`):

- Partition scheduler: 1 budget check per class (5 cheap atomic
  reads) + 1 inner-class call.
- EDF inner: peek-min on sorted queue, O(1).
- FIFO inner: pop-front on per-priority queue, O(1).

Total: ~15–25 cycles per `pick_next`. Comparable to current scheduler.

### Migration plan

| Step | Action | Effort |
|------|--------|--------|
| 1 | Add `crates/sched/src/api.rs` with `trait Scheduler` | 1 week |
| 2 | Refactor existing scheduler into `policies/priority.rs` (one impl, satisfies new trait) | 2 weeks |
| 3 | Implement `policies/edf.rs` with CBS | 4 weeks |
| 4 | Implement `policies/rr.rs` (trivial wrapper around current logic) | 1 week |
| 5 | Implement `policies/cfs.rs` (vruntime-based) | 3 weeks |
| 6 | Implement `policies/sporadic.rs` | 2 weeks |
| 7 | Implement `policies/partition.rs` (combinator) | 4 weeks |
| 8 | `SCHED.TOML` loader (depends on RFC-0005) | 2 weeks |
| 9 | Migrate task creation sites in `kernel_main` to declarative | 2 weeks |
| 10 | Tests + Kani invariants | 4 weeks |

**Total: ~25 engineer-weeks** ≈ 1 senior engineer × 6 months OR 2
engineers × 3 months.

## Drawbacks

- **EDF + CBS is non-trivial.** Replenishment, hard / soft
  reservations, blocking. The literature (Buttazzo, Lipari) is
  mature but easy to get subtly wrong. Tests are critical.
- **Admission control may reject legitimate task sets.** If a
  user-supplied `SCHED.TOML` doesn't pass the utilisation bound, the
  kernel refuses to boot. We need a clear error path with diagnostic.
- **Multi-policy increases verification surface.** Each policy is a
  separate proof obligation in TLA+ / Kani. RFC-0006 covers this.
- **Per-CPU budget tracking adds atomic ops in the tick path.**
  Bounded; estimated 10–20 cycles overhead per timer tick.

## Rationale and alternatives

**Alternative A — keep current priority + RR.** Insufficient (motivation).

**Alternative B — pure EDF for everything.** Hard for non-periodic
work (shell, logger, OTA recv). Multi-policy is unavoidable.

**Alternative C — CFS only (Linux-style).** No deadline guarantees.
Bad fit for hard-RT.

**Alternative D (chosen) — multi-policy + adaptive partitioning.**
QNX has shipped this for 20 years. Sound, well-understood.

**Alternative E — borrow from RTEMS' MrsP / DPMS.** Even more
sophisticated (mixed-criticality scheduling). Overkill for our phase
1; could revisit Phase 4.

## Prior art

- **QNX Adaptive Partitioning** (commercial, 2007+). Production
  reference for partition + per-partition policy.
- **AUTOSAR Adaptive** scheduling: SCHED_FIFO + SCHED_RR +
  SCHED_DEADLINE, with restricted budgets via CGroups.
- **RTEMS SMP** schedulers: EDF + CBS + Priority + Round-Robin
  selectable per-task-set.
- **Buttazzo** (2011), *Hard Real-Time Computing Systems*. The
  textbook for EDF / CBS internals.
- **Lipari & Bini** (2003), *Resource Reservation in Real-Time
  Systems*. CBS implementation details.

## Unresolved questions

- **Migration of existing tasks** — currently every task gets
  priority + RR. We default-map to `SoftRT` class, but some need
  manual review (e.g. should `rt_motor` go to `SafetyCritical` with
  FIFO and explicit priority 4?). Working list to be assembled
  during impl.
- **Locking + priority inheritance** — how do priority inheritance
  protocols interact with classes? When a `BestEffort` task holds a
  lock that a `SafetyCritical` task needs, does the holder
  temporarily promote? Working assumption: yes (PIP), but needs
  careful design to avoid budget violations.
- **`MAX_CPUS`** — current code assumes 4. EDF queues per CPU at
  64 KB each = 256 KB total. Acceptable. If we add more CPUs (Phase
  3+), revisit.
- **Window size** — 1 ms granularity for budget accounting.
  Smaller = more accurate but more overhead. Working assumption:
  configurable per-class, default 1 ms.

## Future possibilities

- **Mixed-Criticality EDF** (Vestal, 2007) — change behavior under
  high system load: at "low criticality" mode, all tasks run; at
  "high criticality" mode, low-ASIL tasks are dropped to free CPU
  for high-ASIL ones. Phase 4 research.
- **Energy-aware scheduling** — extend partitions to also bound
  energy budget per class. Phase 4 (after P01 vigilance mode).
- **Heterogeneous Multi-Processing** (big.LITTLE-style) — different
  CPU types for different classes. Phase 5.
