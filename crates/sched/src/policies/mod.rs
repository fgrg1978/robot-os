//! Scheduling policies — RFC-0004.
//!
//! Each policy is a self-contained module implementing the [`Policy`]
//! trait. The Adaptive Partitioning combinator
//! (`super::partitions::Aps`) selects a class; that class's policy
//! then picks the next runnable task within the class.
//!
//! ## Design notes
//!
//! - All policies use **bounded** queues (no `alloc`); the maximum
//!   number of runnable tasks per class is set at compile time.
//! - All time math is **integer microseconds**; no floats. This keeps
//!   us cert-safe (no FPU state in the safety scheduler path) and
//!   lets us run on harts without floating-point.
//! - Tasks are referenced by `tid: u32`. Per-task scheduler-relevant
//!   metadata (deadline, priority, time-slice remainder) lives in
//!   `super::task::Task`.

pub mod cfs;
pub mod edf_cbs;
pub mod fifo;
pub mod rr;
pub mod sporadic;

use crate::class::SchedClass;

/// Per-task metadata that policies consume. The scheduler maintains
/// this in the `Task` struct (W4.4 will extend `Task` with these
/// fields); for the standalone-policy unit tests we pass an explicit
/// `TaskMeta` instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskMeta {
    /// The task's unique identifier.
    pub tid: u32,
    /// Class assignment (from CAPS.TOML / SCHED.TOML).
    pub class: SchedClass,
    /// Static priority within the class (0 = highest, 31 = lowest).
    pub priority: u8,
    /// Earliest deadline in monotonic microseconds (only used by EDF).
    /// `None` ⇒ no deadline (the policy treats this as "lowest urgency"
    /// for EDF-class tasks; for non-EDF policies the field is ignored).
    pub deadline_us: Option<u64>,
    /// Time-slice quantum in microseconds (only used by RR).
    pub time_slice_us: u32,
    /// Virtual runtime in microseconds (only used by CFS).
    pub vruntime_us: u64,
}

impl TaskMeta {
    /// Construct a new metadata struct with all the optional fields
    /// at default values.
    pub const fn new(tid: u32, class: SchedClass, priority: u8) -> Self {
        Self {
            tid,
            class,
            priority,
            deadline_us: None,
            time_slice_us: 0,
            vruntime_us: 0,
        }
    }
}

/// Common interface implemented by every scheduling policy.
///
/// Policies are stateful (each owns its runqueue). The kernel holds
/// one instance of each policy per CPU; tasks register / deregister as
/// they become runnable / blocked.
pub trait Policy {
    /// Maximum tasks this policy can hold simultaneously. Static so
    /// the runqueue can be a fixed array.
    const CAPACITY: usize;

    /// Insert a runnable task. Returns `Err(meta)` echoing the input
    /// if the runqueue is full.
    fn enqueue(&mut self, meta: TaskMeta) -> Result<(), TaskMeta>;

    /// Remove `tid` from the runqueue. No-op if not present.
    fn dequeue(&mut self, tid: u32) -> Option<TaskMeta>;

    /// Pick the next task to run. `now_us` is the current monotonic
    /// time. Returns `None` if the runqueue is empty.
    fn pick_next(&mut self, now_us: u64) -> Option<TaskMeta>;

    /// Number of runnable tasks currently in this policy's runqueue.
    fn len(&self) -> usize;

    /// Convenience: `true` iff `len() == 0`.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Notify the policy that `dt_us` microseconds have elapsed since
    /// the last tick (called from the timer ISR). Default impl is a
    /// no-op; only RR / CFS override.
    fn tick(&mut self, _tid_running: u32, _dt_us: u32) {}
}
