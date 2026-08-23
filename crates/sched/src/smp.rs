//! SMP support for the Robot OS scheduler.
//!
//! Starts secondary harts via SBI HSM `hart_start` (same approach as the C kernel's
//! `sbi_hart_start()` in kernel/core/smp.c). OpenSBI parks secondary harts in M-mode
//! by default; they must be explicitly started via HSM, not via a polling flag.

use core::sync::atomic::AtomicUsize;

/// Pure liveness accounting for [`wake_harts`], in its own file only so the
/// host test runner can compile it (this module cannot leave the target: it
/// reads `tp` with inline assembly and links against `_secondary_start`).
#[path = "hart_set.rs"]
pub mod hart_set;

/// Number of CPUs currently considered online for task distribution.
///
/// Published in two steps by the boot CPU:
/// 1. Before task creation, set to the *expected* CPU count (from the DTB)
///    so that the boot-time `task_create` calls — which run before any
///    secondary hart exists and must pre-populate per-CPU ready queues that
///    can't be touched cross-CPU afterwards — spread across all intended
///    CPUs.
/// 2. After [`wake_harts`] reports how many secondary harts actually started,
///    corrected down to the real count. This is what matters for any task
///    created later (e.g. `fork()`): without the correction, a hart that
///    failed `hart_start` keeps an empty ready queue forever, which
///    `find_best_cpu` reads as the least contended CPU and routes tasks to
///    it forever — see [`hart_set`] for why the published value must be a
///    live *prefix* bound, not a plain headcount.
/// Secondary CPUs do NOT write this — the boot CPU owns it exclusively.
pub static NUM_ONLINE_CPUS: AtomicUsize = AtomicUsize::new(1);

// ---- External symbols ----

unsafe extern "C" {
    /// Secondary CPU entry point defined in kernel/src/asm/boot.S.
    /// OpenSBI will jump to this address (in S-mode) when `sbi::hart_start` is called.
    fn _secondary_start();
}

// ---- Hart wakeup via SBI HSM ----

/// Start secondary hart `hart_id` via SBI HSM `hart_start`.
///
/// Returns the raw SBI error code: `0` (`SBI_SUCCESS`) on success, negative
/// on failure (`SBI_ERR_INVALID_PARAM`, `SBI_ERR_ALREADY_AVAILABLE`, hart not
/// present, ...). Callers MUST check this — a hart that fails to start never
/// runs `_secondary_start`, so its per-CPU ready queue stays empty forever.
pub unsafe fn wake_hart(hart_id: usize) -> isize {
    let entry = _secondary_start as *const () as usize;
    robot_os_arch::sbi::hart_start(hart_id, entry, hart_id)
}

/// Start every secondary hart in `0..num_cpus` (boot hart excluded — it is
/// already running).
///
/// Returns the number of CPUs the caller should publish as
/// [`NUM_ONLINE_CPUS`]. `find_best_cpu` in `scheduler.rs` reads that value as
/// a live *prefix* bound (`for i in 0..num_online`, indexing `PER_CPU[i]`
/// directly), and `rebalance_from_offline_cpus(online, total)` reads
/// `online..total` as the dead harts — not a plain headcount of successes.
/// Every hart is still attempted regardless of earlier failures; only the
/// published prefix shrinks. See [`hart_set`] for the full contract, for why
/// the old `online = 1` seed silently assumed the boot hart was hart 0, and
/// for what that cost when it was hart 2.
///
/// The boot hart identifies itself through [`current_cpu_id`], which is
/// correct here without help: `wake_harts` runs on the boot hart inside
/// `kernel_main`, and `boot.S` sets `tp = a0 = hart_id` before calling it.
/// (`boot.S` also publishes a `boot_hart_id` word, but that exists for the
/// *trap* path, which has no `tp` it can trust after a trap from U-mode.
/// Nothing here needs it — the defect was the accounting, not the
/// identification.)
///
/// ASSUMPTION still standing (pre-existing, not fixed here): hart IDs are
/// contiguous `0..num_cpus`. True for QEMU virt; a real board's DTB may
/// enumerate non-contiguous hart IDs, which would need walking the DTB cpu
/// nodes instead of this range — and would also break the prefix contract
/// above, since `PER_CPU` is indexed by hart id. "The boot hart is 0" is no
/// longer assumed anywhere in this file.
pub unsafe fn wake_harts(num_cpus: usize) -> usize {
    let boot = current_cpu_id();
    let mut alive = hart_set::mark_alive(0, boot); // boot hart is already running

    for hart_id in 0..num_cpus {
        if hart_id == boot {
            continue;
        }
        let ret = wake_hart(hart_id);
        if ret == 0 {
            alive = hart_set::mark_alive(alive, hart_id);
        } else {
            robot_os_drivers::kprintln!(
                "[SMP] hart {} failed to start (sbi hart_start error {})",
                hart_id, ret
            );
        }
    }

    let online = hart_set::online_prefix(alive, num_cpus);

    // A hart that came up but sits past the first dead one gets no work at
    // all, because the published value is a prefix. That is the intended
    // conservative outcome, but silently idling a working CPU is exactly the
    // kind of thing that reads as "SMP is just slow" months later.
    let stranded = hart_set::stranded(alive, num_cpus);
    if stranded != 0 {
        robot_os_drivers::kprintln!(
            "[SMP] harts alive past the first dead one (mask {:#x}) will stay idle: \
             NUM_ONLINE_CPUS is a prefix bound, not a set",
            stranded
        );
    }

    // Two very different situations, both worth a line of UART because no
    // other message describes either one:
    //
    //  a) some hart below the boot hart failed to start. The boot hart keeps
    //     running the kernel, but it is outside `0..online`, so
    //     `find_best_cpu` will never place a task on it and
    //     `rebalance_from_offline_cpus` counts its queue as belonging to a
    //     dead hart.
    //
    //  b) `boot >= num_cpus` outright. That is the *dangerous* one and it
    //     needs no failure at all: `PER_CPU` is `[_; MAX_CPUS]` with
    //     `MAX_CPUS = 4`, every access is a raw `PER_CPU[current_cpu_id()]`
    //     with no clamp, and `boot.S` only range-checks *secondary* harts
    //     (against `MAX_HARTS = 8`, not `MAX_CPUS`) — the boot hart is
    //     whoever wins `boot_lock`, unchecked. On a board that enumerates
    //     more harts than `MAX_CPUS` (docs/KERNEL_REVIEW_NOTES.md records
    //     the VF2/JH7110 case: S7 as hart 0, U74s as 1..4, `num_cpus`
    //     clamped to 4) a boot hart of 4 indexes `PER_CPU` out of bounds and
    //     the board resets. Nothing in this crate can fix that — the clamp
    //     belongs in `boot.S`/`kernel_main` — so this print is the only
    //     warning that exists, and it is deliberately reached before
    //     `sched::start()`.
    if boot >= online {
        robot_os_drivers::kprintln!(
            "[SMP] WARNING: boot hart {} is outside the online prefix 0..{} \
             — it runs the kernel but will receive no balanced work",
            boot, online
        );
    }

    online
}

// ---- Current CPU identity ----

/// Returns the current CPU's hart ID by reading the `tp` (thread pointer) register.
///
/// `tp` is set to `hart_id` in boot.S for all CPUs (both primary and secondary).
/// Rust does not use `tp` in `no_std` bare-metal builds.
#[inline(always)]
pub fn current_cpu_id() -> usize {
    let id: usize;
    unsafe {
        core::arch::asm!(
            "mv {}, tp",
            out(reg) id,
            options(nostack, nomem)
        );
    }
    id
}
