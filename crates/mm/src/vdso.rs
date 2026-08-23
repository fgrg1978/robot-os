/// vDSO — Virtual Dynamic Shared Object (M01).
///
/// A single read-only physical page is shared into every user process at a
/// fixed virtual address (VDSO_USER_BASE).  The kernel writes monotonic timing
/// data to this page under a seqlock; user-space reads it without issuing an
/// ecall, eliminating syscall overhead for the most common time queries.
///
/// ## Seqlock protocol
/// Writer (kernel, timer ISR — see `vdso_update()` for how concurrent
/// writers from multiple harts are serialized):
///   1. seq += 1  →  odd   (write in progress)
///   2. store data fields
///   3. seq += 1  →  even  (data stable)
///
/// Reader (user-space via libsys):
///   loop:
///     seq1 = load seq;  if seq1 is odd → spin
///     read data fields
///     seq2 = load seq;  if seq2 != seq1 → retry
///     // data is consistent
///
/// A classic seqlock only tolerates a SINGLE writer. On SMP, the timer ISR
/// fires on every hart, so `vdso_update()` claims the right to write with a
/// compare-exchange on `seq` itself before touching any data field — see the
/// doc comment on `vdso_update()` for why that (rather than gating on hart
/// identity, or a lock) is the correct and cheapest way to serialize writers
/// here.
///
/// VDSO_USER_BASE is exported to libsys so it can read without a syscall.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use robot_os_arch::mmu::PAGE_SIZE;
use crate::pmm;

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// Fixed user-space virtual address of the vDSO page.
/// Placed at 0x5000_0000 — well within Sv39 user space and below the stack
/// (USER_STACK_TOP = 0x8000_0000), well above typical ELF load addresses.
pub const VDSO_USER_BASE: usize = 0x5000_0000;

/// Magic value stored at the start of the vDSO page.
pub const VDSO_MAGIC: u32 = 0x5644_534F; // "VDSO"

/// Kernel version encoded as (major << 16 | minor << 8 | patch).
pub const VDSO_KERNEL_VERSION: u32 = (0 << 16) | (1 << 8) | 0; // 0.1.0

// ---------------------------------------------------------------------------
// VdsoData — layout of the vDSO page (first 32 bytes)
// ---------------------------------------------------------------------------

/// Data written by the kernel into the vDSO page.
///
/// # Safety
/// This struct is placed at a physical address returned by `pmm::alloc_page`.
/// All fields are accessed through raw pointers with volatile semantics.
/// The seqlock (seq field) guards consistency.
#[repr(C, align(8))]
pub struct VdsoData {
    /// VDSO_MAGIC — lets userspace verify the page is mapped correctly.
    pub magic: AtomicU32,
    /// Kernel version (major.minor.patch packed into u32).
    pub kernel_version: AtomicU32,
    /// Seqlock counter.  Even = data stable, odd = write in progress.
    pub seq: AtomicU32,
    pub _pad: AtomicU32,
    /// Monotonic tick counter (incremented every timer IRQ).
    pub uptime_ticks: AtomicU64,
    /// Milliseconds since boot.
    pub uptime_ms: AtomicU64,
}

// ---------------------------------------------------------------------------
// Kernel-side state
// ---------------------------------------------------------------------------

/// Physical address of the vDSO page (0 = not initialised).
static VDSO_PHYS: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Kernel API
// ---------------------------------------------------------------------------

/// Allocate and initialise the vDSO page.  Called once during boot.
pub fn vdso_init() {
    if let Ok(page) = pmm::alloc_page() {
        let phys = page.as_usize();

        // Zero the page first.
        unsafe { core::ptr::write_bytes(phys as *mut u8, 0, PAGE_SIZE); }

        // Write the magic and version (seq = 0 = stable, no data yet).
        let data = unsafe { &*(phys as *const VdsoData) };
        data.magic.store(VDSO_MAGIC, Ordering::Release);
        data.kernel_version.store(VDSO_KERNEL_VERSION, Ordering::Release);

        VDSO_PHYS.store(phys as u64, Ordering::Release);
    }
}

/// Return the physical address of the vDSO page, or 0 if not initialised.
pub fn vdso_phys() -> usize {
    VDSO_PHYS.load(Ordering::Acquire) as usize
}

/// Update the vDSO timing data.  Called from the timer ISR — on every hart
/// in an SMP kernel, since each hart takes its own periodic timer interrupt.
///
/// Uses the seqlock write protocol: increment seq to odd, write, increment
/// to even.  This protocol is only sound with a SINGLE writer: two harts
/// racing the seq increment/store sequence can interleave and leave seq (and
/// the data fields) in an incoherent state that no reader-side retry can
/// detect.
///
/// Serializing writers by hart identity (e.g. "only hart 0 updates") was
/// considered and rejected: it would depend on `tp`/`hart_id()` correctly
/// identifying the running hart at the point this function is called from
/// deep inside the timer ISR. That is NOT a safe assumption in this kernel —
/// `crates/sched/src/task.rs` saves/restores `tp` as part of task context
/// (`CTX_TP`) so `current_cpu_id()` survives ordinary context switches, but
/// the EDF scheduler can migrate a task to a different physical hart, and the
/// migrated task's restored `tp` then reflects the hart it last ran on, not
/// the one it is running on now (tracked separately, see
/// `docs/KERNEL_REVIEW_NOTES.md`, `context_switch.S:83` entry). A hart whose
/// current task carries a stale `tp == 0` would wrongly believe itself to be
/// the sole writer, silently reopening the exact multi-writer race this
/// function exists to close — worse than not fixing the bug at all, because
/// the failure would be workload-dependent and invisible in easy testing.
///
/// Instead, writers are serialized without needing any hart identity: the
/// seqlock's own `seq` counter doubles as a claim ticket via
/// compare-exchange. A hart only proceeds to write if it wins the CAS that
/// flips `seq` from even to odd; every other hart (or a spurious re-entrant
/// call — see below) that loses the race, or sees `seq` already odd, simply
/// drops this tick's update and returns. That is harmless: the vDSO page is
/// refreshed again on the very next tick, by whichever hart gets there first
/// — there is no requirement that every tick be published, only that
/// published data is always internally consistent. This also protects
/// against IRQ-context re-entrancy on a single hart (e.g. a nested timer
/// interrupt while a write is still open): the odd-`seq` check makes a
/// reentrant call a no-op instead of corrupting an in-flight write.
///
/// Cost: one uncontended `compare_exchange` per timer tick on the common
/// path (no contention: SMP harts rarely race the exact same tick), which is
/// cheaper than an IRQ-safe lock (`SpinLock::lock_irqsave()` in
/// `crates/sync/src/spinlock.rs`) held across the write on every hart, every
/// tick, given this runs inside a WCET-budgeted ISR.
#[inline]
pub fn vdso_update(uptime_ticks: u64, uptime_ms: u64) {
    let phys = VDSO_PHYS.load(Ordering::Relaxed) as usize;
    if phys == 0 { return; }

    // SAFETY: phys is a valid page allocated at init time. Multiple harts
    // may call this concurrently; the CAS below ensures only the hart that
    // wins the even→odd transition of `seq` touches the data fields, making
    // this the sole writer for the duration of that write.
    let data = unsafe { &*(phys as *const VdsoData) };

    // Seqlock: claim the write by CAS'ing seq from even to even+1 (odd).
    // If seq is already odd, someone else's write is in flight — drop this
    // tick. If the CAS loses the race, someone else claimed it first —
    // drop this tick too. Either way the page is refreshed on the next tick.
    let seq = data.seq.load(Ordering::Acquire);
    if seq & 1 != 0 {
        return;
    }
    if data
        .seq
        .compare_exchange(seq, seq.wrapping_add(1), Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    // We won the claim: we are now the sole writer. The successful CAS used
    // Acquire ordering, so these stores cannot be hoisted above it.
    //
    // `uptime_ticks`/`uptime_ms` were sampled by the CALLER (main.rs, before
    // this hart necessarily won the race above) from two different sources —
    // a global `TICK_COUNT.fetch_add()` and `rdtime` respectively, read at
    // two different instants — so a hart that stalled between its own
    // sampling and winning a later CAS can carry a sample that is older in
    // one field but not necessarily the other. Publishing a sample that
    // regresses either field would walk the page's documented "monotonic"
    // data backwards for every user-space reader. Require the new sample to
    // dominate in BOTH fields before publishing; otherwise drop this tick's
    // update entirely (the seqlock still closes normally so no reader
    // spins) — the next tick that produces a fully-newer sample refreshes
    // the page. Both loads are Relaxed: we are the sole writer at this
    // point (we hold the claim), so no other write can race these reads.
    let published_ticks = data.uptime_ticks.load(Ordering::Relaxed);
    let published_ms = data.uptime_ms.load(Ordering::Relaxed);
    if uptime_ticks >= published_ticks && uptime_ms >= published_ms {
        data.uptime_ticks.store(uptime_ticks, Ordering::Release);
        data.uptime_ms.store(uptime_ms, Ordering::Release);
    }

    // Seqlock: close write (seq → even). Release ordering makes whichever
    // stores above ran visible to any reader whose seqlock retry-read
    // observes this new value (paired with the reader's Acquire fences in
    // `crates/libsys/src/lib.rs`).
    data.seq.store(seq.wrapping_add(2), Ordering::Release);
}
