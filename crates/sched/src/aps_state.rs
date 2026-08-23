//! Per-CPU state for the W4 multi-policy scheduler — RFC-0004.
//!
//! Mirrors the per-CPU pattern used by the legacy scheduler
//! (`scheduler::PER_CPU`), but holds the new Adaptive Partitioning
//! Scheduler plus the five policy runqueues from `crate::policies`.
//!
//! In W4-int.1 (this wave) the state is **constructed but not yet
//! consulted** by the live dispatch core. The legacy priority-queue
//! scheduler continues to drive boot. W4-int.2 will atomically switch
//! the dispatch path to read from this module.

use core::sync::atomic::{AtomicBool, Ordering};

use robot_os_sync::spinlock::SpinLock;

use crate::partitions::Aps;
use crate::policies::cfs::Cfs;
use crate::policies::edf_cbs::EdfCbs;
use crate::policies::fifo::Fifo;
use crate::policies::rr::RoundRobin;
use crate::policies::sporadic::Sporadic;
use crate::scheduler::MAX_CPUS;

/// Per-CPU multi-policy scheduler state.
pub struct CpuSchedV2 {
    /// Adaptive Partitioning combinator.
    pub aps: Aps,
    /// Safety-critical class runqueue (default `Fifo`).
    pub fifo: Fifo,
    /// Hard real-time class runqueue (default `EdfCbs`).
    pub edf_cbs: EdfCbs,
    /// Soft real-time class runqueue (default `RoundRobin`).
    pub rr: RoundRobin,
    /// Best-effort class runqueue (default `Cfs`).
    pub cfs: Cfs,
    /// Idle class runqueue (default `Sporadic`).
    pub sporadic: Sporadic,
}

impl CpuSchedV2 {
    /// Construct an empty per-CPU state with RFC-0004 default budgets.
    pub const fn new() -> Self {
        Self {
            aps: Aps::default_config(),
            fifo: Fifo::new(),
            edf_cbs: EdfCbs::new(),
            rr: RoundRobin::new(),
            cfs: Cfs::new(),
            sporadic: Sporadic::new(),
        }
    }
}

impl Default for CpuSchedV2 {
    fn default() -> Self {
        Self::new()
    }
}

/// Sentinel: `true` once `init()` has populated all per-CPU slots.
/// Read by tests; the dispatch core does not yet consult this.
static INIT_DONE: AtomicBool = AtomicBool::new(false);

/// Const initializer for the static array. The `[item; N]` syntax
/// repeats a `const` value, which avoids the `Copy` requirement.
const FRESH_CPU_STATE: SpinLock<CpuSchedV2> = SpinLock::new(CpuSchedV2::new());

/// Per-CPU multi-policy scheduler state.
///
/// IRQ-safe: `account()` runs from the timer ISR (via `schedule()`), while
/// `task_exit()`/admission run in task context on the same hart with interrupts
/// enabled. `with_cpu`/`for_each_cpu` therefore lock with `lock_irqsave()` so a
/// timer tick can't re-enter and deadlock on `V2_STATE[cpu]`.
static V2_STATE: [SpinLock<CpuSchedV2>; MAX_CPUS] =
    [FRESH_CPU_STATE; MAX_CPUS];

/// Mark the V2 scheduler state as initialised. Called once during
/// boot from `crate::scheduler::init()` (W4-int.2 will wire this in).
pub fn mark_initialised() {
    INIT_DONE.store(true, Ordering::Release);
}

/// Returns `true` iff `mark_initialised` has been called.
#[inline]
pub fn is_initialised() -> bool {
    INIT_DONE.load(Ordering::Acquire)
}

/// Borrow a CPU's state and run a closure on it.
///
/// Returns `None` if `cpu` is out of range.
pub fn with_cpu<R>(cpu: usize, f: impl FnOnce(&mut CpuSchedV2) -> R) -> Option<R> {
    if cpu >= MAX_CPUS {
        return None;
    }
    let mut state = V2_STATE[cpu].lock_irqsave();
    Some(f(&mut *state))
}

/// Run the same closure on every CPU's state in turn (used by the
/// window-anchor path at boot).
pub fn for_each_cpu(mut f: impl FnMut(usize, &mut CpuSchedV2)) {
    for cpu in 0..MAX_CPUS {
        let mut state = V2_STATE[cpu].lock_irqsave();
        f(cpu, &mut *state);
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Co-enqueue helpers — called by `scheduler::task_create_affinity` so the
// new policy runqueues stay populated even when SCHED_USE_APS is false.
// Flipping the flag mid-run then finds the policies already holding the
// current task set.
// ──────────────────────────────────────────────────────────────────────────

use crate::class::SchedClass;
use crate::policies::{Policy, TaskMeta};

/// Insert a task into the policy that matches its `class_raw`.
/// Silent drop on full runqueues (W4-int.2; W4-int.3 will surface
/// the error path once dispatch consults the policies).
pub fn enqueue_task_for_class(
    cpu: usize,
    tid: u32,
    class_raw: u8,
    priority: u8,
    time_slice_us: u32,
    deadline_us: u64,
) {
    let class = match SchedClass::from_raw(class_raw) {
        Some(c) => c,
        None => return,
    };
    let mut meta = TaskMeta::new(tid, class, priority);
    meta.time_slice_us = time_slice_us;
    if deadline_us != 0 {
        meta.deadline_us = Some(deadline_us);
    }
    let _ = with_cpu(cpu, |state| match class {
        SchedClass::SafetyCritical => {
            let _ = state.fifo.enqueue(meta);
        }
        SchedClass::HardRT => {
            let _ = state.edf_cbs.enqueue(meta);
        }
        SchedClass::SoftRT => {
            let _ = state.rr.enqueue(meta);
        }
        SchedClass::BestEffort => {
            let _ = state.cfs.enqueue(meta);
        }
        SchedClass::Idle => {
            let _ = state.sporadic.enqueue(meta);
        }
    });
}

/// Pick the next task to run on `cpu` via the APS combinator.
///
/// Algorithm:
///   1. `Aps::pick_class` chooses the class whose budget is most
///      under-served and that has a runnable task.
///   2. The matching policy's `pick_next` returns a [`TaskMeta`].
///   3. The caller (the dispatch core) translates `meta.tid` to a
///      task slot index via [`crate::scheduler::idx_for_tid`].
///
/// Returns `None` if every policy's runqueue is empty.
pub fn pick_next(cpu: usize, now_us: u64) -> Option<TaskMeta> {
    if cpu >= MAX_CPUS {
        return None;
    }
    with_cpu(cpu, |state| {
        let class = state.aps.pick_class(|c| match c {
            SchedClass::SafetyCritical => !state.fifo.is_empty(),
            SchedClass::HardRT => !state.edf_cbs.is_empty(),
            SchedClass::SoftRT => !state.rr.is_empty(),
            SchedClass::BestEffort => !state.cfs.is_empty(),
            SchedClass::Idle => !state.sporadic.is_empty(),
        })?;
        match class {
            SchedClass::SafetyCritical => state.fifo.pick_next(now_us),
            SchedClass::HardRT => state.edf_cbs.pick_next(now_us),
            SchedClass::SoftRT => state.rr.pick_next(now_us),
            SchedClass::BestEffort => state.cfs.pick_next(now_us),
            SchedClass::Idle => state.sporadic.pick_next(now_us),
        }
    })?
}

/// PHANES Phase 1 W4-int.2 — boot-time smoke test for the APS
/// dispatch path.
///
/// Returns `Ok(())` if at least one task is enqueued in each of the
/// classes specified by `classes_required`, and if `pick_next` on
/// the given CPU returns a `Some(meta)`. Otherwise returns
/// `Err(reason)`.
///
/// Used by the kernel to print a one-line PASS/FAIL line during
/// boot without flipping the dispatch flag permanently.
pub fn smoke_test(cpu: usize) -> Result<u32, &'static str> {
    if cpu >= MAX_CPUS {
        return Err("cpu out of range");
    }
    let pick = pick_next(cpu, 0).ok_or("no class returned a task")?;
    Ok(pick.tid)
}

/// Account for `dt_us` microseconds of runtime on `cpu`, charging
/// the currently-running class. Called from the timer ISR.
pub fn account(cpu: usize, now_us: u64, dt_us: u32, running_tid: u32) {
    let _ = with_cpu(cpu, |state| {
        // Drive the APS window roll-over and class-budget accounting.
        state.aps.tick(now_us, dt_us);
        // Forward to each policy's tick — only the policy that owns
        // `running_tid` reacts; others no-op.
        state.fifo.tick(running_tid, dt_us);
        state.edf_cbs.tick(running_tid, dt_us);
        state.rr.tick(running_tid, dt_us);
        state.cfs.tick(running_tid, dt_us);
        state.sporadic.tick(running_tid, dt_us);
    });
}

/// Remove a task from whichever policy holds it. Called from
/// `task_exit` so the per-class runqueues stay in sync.
pub fn dequeue_task_for_class(cpu: usize, tid: u32, class_raw: u8) {
    let class = match SchedClass::from_raw(class_raw) {
        Some(c) => c,
        None => return,
    };
    let _ = with_cpu(cpu, |state| match class {
        SchedClass::SafetyCritical => {
            state.fifo.dequeue(tid);
        }
        SchedClass::HardRT => {
            state.edf_cbs.dequeue(tid);
        }
        SchedClass::SoftRT => {
            state.rr.dequeue(tid);
        }
        SchedClass::BestEffort => {
            state.cfs.dequeue(tid);
        }
        SchedClass::Idle => {
            state.sporadic.dequeue(tid);
        }
    });
}
