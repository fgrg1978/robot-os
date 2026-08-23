//! Async-runtime microbench (RFC-0030 / Idea 12).
//!
//! Measures the cost to **resume a cooperative `async` task** — one `poll()`
//! of a yielding state machine — which is the operation that *replaces* a
//! preemptive context switch in a stackless async control plane. Compare
//! against `sched.task_yield` (the full preemptive yield → schedule →
//! `context_switch` → register-file save/restore path, ~2200 cyc measured).
//!
//! This is the **structural floor** of cooperative scheduling: a `poll()` is a
//! plain function call into a monomorphised state machine — no 31-GPR + FP +
//! CSR save/restore, no scheduler bookkeeping. Pure compute → runs in the
//! quiescent early-boot bench (clean rdcycle, no scheduler needed).

use crate::{BenchResult, report};
use robot_os_drivers::wcet::read_cycles;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// Minimal no-op waker (no executor / no wake list — we poll directly).
fn noop_raw_waker() -> RawWaker {
    fn no_op(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker { noop_raw_waker() }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
    RawWaker::new(core::ptr::null(), &VTABLE)
}

/// A future that yields (`Pending`) `n` times before completing. Each poll
/// resumes from the prior await point and yields again — modelling the
/// per-tick "resume a cooperative control task" the async plane would do.
struct CountdownYield {
    n: u64,
}

impl Future for CountdownYield {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.n == 0 {
            Poll::Ready(())
        } else {
            self.n -= 1;
            Poll::Pending
        }
    }
}

/// Per-resume cost: poll a yielding future `iters` times (each poll = one
/// cooperative resume).
pub fn bench_poll_resume(iters: u64) -> BenchResult {
    // SAFETY: noop waker holds no state; vtable is 'static.
    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = CountdownYield { n: iters + 1 };
    // SAFETY: `fut` lives on this stack frame and is never moved after pinning.
    let mut pinned = unsafe { Pin::new_unchecked(&mut fut) };

    let start = read_cycles();
    for _ in 0..iters {
        let _ = core::hint::black_box(pinned.as_mut().poll(&mut cx));
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

pub fn run(iters: u64) -> u32 {
    report("asyncrt.poll_resume", &bench_poll_resume(iters));
    1
}
