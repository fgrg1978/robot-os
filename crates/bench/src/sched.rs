//! Scheduler subsystem microbenchmarks.
//!
//! Today: task_yield (context switch round-trip).  Will grow to cover
//! block+wake (timer wait), priority-aware preemption, hart migration.

use crate::{BenchResult, report};
use robot_os_drivers::wcet::read_cycles;
use robot_os_sched as sched;

/// `task_yield()` — voluntary context switch.  Measures the round-trip
/// (yield from current → scheduler picks next → eventually picks current
/// again → return).  Under QEMU TCG SMP this includes cross-hart
/// scheduling time on the single host thread; on real hardware this is
/// pure context-switch cost.
///
/// Note: if no other task is ready on this hart, `do_schedule` returns
/// immediately without switching — the cost is then dominated by the
/// `cpu_dequeue` bitmap scan, not by actual register save/restore.
pub fn bench_task_yield(iters: u64) -> BenchResult {
    let start = read_cycles();
    for _ in 0..iters {
        sched::task_yield();
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

pub fn run(iters: u64) -> u32 {
    let mut n = 0u32;
    report("sched.task_yield", &bench_task_yield(iters)); n += 1;
    n
}
