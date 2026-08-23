//! Synthetic microbenchmarks for kernel subsystems.
//!
//! # Goal
//!
//! Give every subsystem (IPC, MM, sched, net, fs, crypto, auth, …) a tight-
//! loop microbenchmark that measures per-operation latency in CPU cycles.
//! Run from the kernel shell (`bench <subsystem>` or `bench all`) and from
//! the bench harness (which injects `bench all` automatically and parses
//! the `[BENCH-RES]` lines into the JSON sidecar consumed by
//! `bench_compare.py`).
//!
//! # Why synthetic vs `#[wcet(...)]` runtime instrumentation?
//!
//! - `#[wcet(...)]` measures the OPPORTUNISTIC cost: the function gets
//!   sampled whenever the live workload happens to call it.  Coverage is
//!   uneven (cap_get + channel_* never fire if no userspace task uses
//!   them) and per-sample wall time includes cross-hart contention that
//!   varies run-to-run.
//! - Synthetic microbenches run a tight loop INSIDE the bench function
//!   itself.  N=1000 iterations measured by a single `rdcycle` delta
//!   amortises the per-call jitter and gives a stable
//!   "this primitive costs X cycles avg" number.
//!
//! Both layers coexist: `#[wcet(...)]` for runtime distribution under
//! real load; `bench_*` for synthetic baselines that don't depend on
//! workload shape.
//!
//! # Wire format
//!
//! Each bench emits one line:
//!
//!     [BENCH-RES] <subsystem>.<name> iters=<N> min_cycles=<X>
//!                 max_cycles=<X> avg_cycles=<X> total_cycles=<X>
//!
//! Brain-side `parse_bench` (in `tools/bench_e2e_collect.py`) ingests
//! these and emits a top-level `bench_synth` dict into the result JSON:
//!
//!     {"bench_synth": {"ipc.channel_send_recv": {iters, min_c, max_c,
//!                                                avg_c}, …}}
//!
//! Same direction-aware regression gate as `wcet_per_fn` (smaller-is-
//! better for `.avg_cycles`, `.max_cycles`).

#![no_std]
// Bench names carry byte-size suffixes (`_64B`, `_1K`, `_256B`, `_1500B`)
// that read far better than the snake-case the linter wants
// (`_64_b`/`_1_k`).  The suffix IS the spec — it names the payload size the
// number applies to — so we keep it and silence the lint crate-wide.
#![allow(non_snake_case)]

use core::sync::atomic::{AtomicU64, Ordering};

/// Default iteration count per bench.  Picked so a single bench takes
/// ~ms on a 2024 host (10× that under QEMU TCG).  Override via the
/// `iters` argument to each `bench_*` function.
pub const DEFAULT_ITERS: u64 = 1000;

/// Result of one microbenchmark.  All cycle counts come from
/// `robot_os_drivers::wcet::read_cycles()`.
#[derive(Copy, Clone)]
pub struct BenchResult {
    /// Number of iterations executed.
    pub iters: u64,
    /// Smallest single-iter cycle delta observed (within the inner loop).
    /// Note: in tight-loop benches we measure ONLY the total around the
    /// whole loop — per-iter min/max would require N×2 rdcycle reads
    /// which dominates the measurement.  We store `total/iters` as both
    /// min and max in that case, surfaced as `avg`.
    pub min_cycles: u64,
    /// Largest single-iter cycle delta observed.  See `min_cycles`.
    pub max_cycles: u64,
    /// Average cycles per iteration (`total_cycles / iters`).
    pub avg_cycles: u64,
    /// Total cycles around the entire N-iteration loop.
    pub total_cycles: u64,
}

impl BenchResult {
    /// Build from a single bracketing measurement (start cycle, end
    /// cycle, iters).  Sets min=max=avg=total/iters.
    pub fn from_total(start: u64, end: u64, iters: u64) -> Self {
        let total = end.wrapping_sub(start);
        let avg = if iters > 0 { total / iters } else { 0 };
        BenchResult {
            iters,
            min_cycles: avg,
            max_cycles: avg,
            avg_cycles: avg,
            total_cycles: total,
        }
    }

    /// Build from per-iteration min/max accumulation (when the caller
    /// times each iteration individually — more accurate min/max,
    /// higher rdcycle overhead).
    pub fn from_per_iter(
        iters: u64,
        min_cycles: u64,
        max_cycles: u64,
        total_cycles: u64,
    ) -> Self {
        let avg = if iters > 0 { total_cycles / iters } else { 0 };
        BenchResult { iters, min_cycles, max_cycles, avg_cycles: avg, total_cycles }
    }
}

/// Print one `[BENCH-RES]` line for the brain collector to ingest.
///
/// `name` should be `<subsystem>.<bench_name>` (e.g. `ipc.channel_send_recv`)
/// — the dot becomes the JSON nesting key separator on the brain side.
pub fn report(name: &str, r: &BenchResult) {
    robot_os_drivers::kprintln!(
        "[BENCH-RES] {} iters={} min_cycles={} max_cycles={} avg_cycles={} total_cycles={}",
        name, r.iters, r.min_cycles, r.max_cycles, r.avg_cycles, r.total_cycles,
    );
}

/// Round-up u64 saturating divide.  Used in benches that need to
/// expose "ops per millisecond" derived numbers without floating point.
#[inline]
pub fn cycles_to_ns(cycles: u64) -> u64 {
    let freq = robot_os_drivers::clint::TIMER_FREQ;
    if freq == 0 { return 0; }
    cycles.saturating_mul(1_000_000_000) / freq
}

// ── Subsystems (gated by features) ───────────────────────────────────────────

#[cfg(feature = "ipc")]
pub mod ipc;

#[cfg(feature = "mm")]
pub mod mm;

#[cfg(feature = "sched")]
pub mod sched;

#[cfg(feature = "net")]
pub mod net;

#[cfg(feature = "fs")]
pub mod fs;

#[cfg(feature = "crypto")]
pub mod crypto;

#[cfg(feature = "auth")]
pub mod auth;

#[cfg(feature = "cap")]
pub mod cap;

#[cfg(feature = "protocol")]
pub mod protocol;

#[cfg(feature = "ota")]
pub mod ota;

#[cfg(feature = "asyncrt")]
pub mod asyncrt;

// ── Master entrypoint ────────────────────────────────────────────────────────

/// One-shot guard to make sure shell-`bench all` invocations don't recurse
/// or interleave on multi-hart shell access.  Best-effort; the bench
/// machinery itself is not re-entrant safe.
static BENCH_IN_PROGRESS: AtomicU64 = AtomicU64::new(0);

/// Run every enabled subsystem's bench suite, in declared order.
///
/// Returns the total number of `[BENCH-RES]` lines emitted.  Used by the
/// kernel shell `bench all` command; the bench harness scrapes the lines
/// out of the qemu.log directly.
pub fn run_all(iters: u64) -> u32 {
    if BENCH_IN_PROGRESS.swap(1, Ordering::AcqRel) != 0 {
        robot_os_drivers::kprintln!("[BENCH-RES] busy — another bench run in progress, skipping");
        return 0;
    }
    robot_os_drivers::kprintln!("[BENCH-RES] ── run_all start iters={} ──", iters);
    let mut emitted: u32 = 0;

    #[cfg(feature = "ipc")]
    { emitted = emitted.saturating_add(ipc::run(iters)); }

    #[cfg(feature = "mm")]
    { emitted = emitted.saturating_add(mm::run(iters)); }

    #[cfg(feature = "sched")]
    { emitted = emitted.saturating_add(sched::run(iters)); }

    #[cfg(feature = "net")]
    { emitted = emitted.saturating_add(net::run(iters)); }

    #[cfg(feature = "fs")]
    { emitted = emitted.saturating_add(fs::run(iters)); }

    #[cfg(feature = "crypto")]
    { emitted = emitted.saturating_add(crypto::run(iters)); }

    #[cfg(feature = "auth")]
    { emitted = emitted.saturating_add(auth::run(iters)); }

    #[cfg(feature = "cap")]
    { emitted = emitted.saturating_add(cap::run(iters)); }

    #[cfg(feature = "protocol")]
    { emitted = emitted.saturating_add(protocol::run(iters)); }

    #[cfg(feature = "ota")]
    { emitted = emitted.saturating_add(ota::run(iters)); }

    #[cfg(feature = "asyncrt")]
    { emitted = emitted.saturating_add(asyncrt::run(iters)); }

    robot_os_drivers::kprintln!("[BENCH-RES] ── run_all done emitted={} ──", emitted);
    BENCH_IN_PROGRESS.store(0, Ordering::Release);
    emitted
}

/// Run every subsystem suite EXCEPT `sched` — for the early-boot capture path
/// (`CFG_BENCH_BOOT`), which runs before `scheduler::start()`, so
/// `sched.task_yield` has no live scheduler to yield into.  Every other
/// subsystem only needs its data structures initialised (done by this point
/// in boot: ipc, fs/tmpfs, net/arp, crypto, auth) and is pure compute.
///
/// Runs in a quiescent single-active-hart, timer-OFF context → the cleanest
/// `rdcycle` measurement available under QEMU TCG.  See [`crate`] docs and
/// `CFG_BENCH_BOOT`.
pub fn run_all_quiescent(iters: u64) -> u32 {
    if BENCH_IN_PROGRESS.swap(1, Ordering::AcqRel) != 0 {
        return 0;
    }
    robot_os_drivers::kprintln!("[BENCH-RES] ── run_all start iters={} (boot/quiescent) ──", iters);
    let mut emitted: u32 = 0;

    #[cfg(feature = "ipc")]
    { emitted = emitted.saturating_add(ipc::run(iters)); }
    #[cfg(feature = "mm")]
    { emitted = emitted.saturating_add(mm::run(iters)); }
    // sched intentionally skipped — no live scheduler this early in boot.
    #[cfg(feature = "net")]
    { emitted = emitted.saturating_add(net::run(iters)); }
    #[cfg(feature = "fs")]
    { emitted = emitted.saturating_add(fs::run(iters)); }
    #[cfg(feature = "crypto")]
    { emitted = emitted.saturating_add(crypto::run(iters)); }
    #[cfg(feature = "auth")]
    { emitted = emitted.saturating_add(auth::run(iters)); }
    #[cfg(feature = "cap")]
    { emitted = emitted.saturating_add(cap::run(iters)); }
    #[cfg(feature = "protocol")]
    { emitted = emitted.saturating_add(protocol::run(iters)); }
    #[cfg(feature = "ota")]
    { emitted = emitted.saturating_add(ota::run(iters)); }

    #[cfg(feature = "asyncrt")]
    { emitted = emitted.saturating_add(asyncrt::run(iters)); }

    robot_os_drivers::kprintln!("[BENCH-RES] ── run_all done emitted={} ──", emitted);
    BENCH_IN_PROGRESS.store(0, Ordering::Release);
    emitted
}
