/// Scheduler for Robot OS — Priority-based with RT support and hart affinity.
///
/// Design:
/// - Global task pool: `TASKS[MAX_TASKS]` protected by `POOL_LOCK`
/// - Per-CPU multi-level priority queues: 32 FIFOs + bitmap for O(1) dequeue
/// - Hard real-time: priorities 0..RT_PRIORITY_THRESHOLD are never preempted by timer
/// - Hart affinity: tasks can be pinned to a specific CPU
/// - Task assignment: at creation time (unless pinned) to the CPU where a
///   task of that priority will actually be dispatched — see `find_best_cpu`
/// - Context switch: saves/restores callee-saved registers only (ra, sp, s0-s11, pc)
///
/// Invariants:
/// - `do_schedule()` is always called with interrupts disabled on the calling CPU
///   (either explicitly, or because it runs inside a trap handler where hardware
///   already disabled them)
/// - Cross-CPU ready-queue access is expected and routine — task creation
///   (priority-aware placement / explicit affinity), task wake-up
///   (`try_wake_task`, `wq_wake_by_tid`) and even `do_schedule()`'s own
///   same-CPU re-enqueue race the same target queue from other harts. Every
///   ready-queue read/write goes through `cpu_dequeue_locked` /
///   `cpu_enqueue_locked`, which take `CPU_LOCKS[cpu]` (the queue-owning
///   CPU's lock, IRQ-safe) for the duration of the single operation — never
///   held across `context_switch()`. `boost_ready_task`/`restore_ready_task`
///   are a known, gated-off exception (see their doc comments).
/// - `POOL_LOCK` protects `TASKS[]`, `TASK_VALID[]`, `NEXT_TID` during task creation
/// - `rebalance_from_offline_cpus` is boot-only (see its doc comment for the
///   exact window): it still goes through the same locked wrappers as
///   everything else above, it just runs before any task has been dispatched.

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use crate::task::{
    Task, TaskContext, TaskState, CtxReg, MAX_TASKS, STACK_SIZE,
    TIME_SLICE_TICKS, RT_TIME_SLICE_TICKS, NUM_PRIORITIES, is_rt_priority,
    WaitReason, DeadlineParams, SyscallFilter, TASK_NAME_CAPACITY,
    CpuLoad, EnqueueOutcome, enqueue_decision, pick_cpu_by_load,
};
use crate::smp::{current_cpu_id, NUM_ONLINE_CPUS};
use wcet_macro::wcet;

pub const MAX_CPUS: usize = 4;

/// Longest printable task name: `Task::name` is null-terminated, so a name
/// that fills the array leaves `TASK_NAME_CAPACITY - 1` usable bytes. Used as
/// the fallback length when no terminator is found (malformed name).
const TASK_NAME_MAX_LEN: usize = TASK_NAME_CAPACITY - 1;

/// Next ASID to allocate for user-space tasks.
/// ASID 0 is reserved for the kernel page table.
/// Sv39 supports 16-bit ASIDs (1..65535). Wraps to 1 on overflow.
static NEXT_ASID: AtomicU16 = AtomicU16::new(1);

/// Allocate a unique ASID for a user-space page table.
///
/// Memory ordering: AcqRel on success so the returned ASID is sequenced
/// after every prior allocator operation (no two CPUs can get the same
/// ASID). Acquire on failure so the next iteration sees the latest
/// value. Relaxed here would let two CPUs race and reuse an ASID before
/// TLB shootdown completes — a stale TLB entry for ASID N on hart A
/// then references hart B's freshly-allocated page table.
pub fn alloc_asid() -> u16 {
    loop {
        let current = NEXT_ASID.load(Ordering::Acquire);
        let next = if current == u16::MAX { 1 } else { current + 1 };
        if NEXT_ASID.compare_exchange(current, next,
            Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return current;
        }
    }
}

/// Magic value written at the bottom of each task stack (lowest address).
///
/// Stack grows downward — this 8-byte value is the first to be overwritten on
/// overflow.  Written during `task_create`; verified by `stack_canary_check()`.
pub const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_1234;

// ---- Global task pool (shared across all CPUs) ----

/// Task descriptors — valid slots tracked by TASK_VALID.
static mut TASKS: [Task; MAX_TASKS] = unsafe { core::mem::zeroed() };

/// Which slots in TASKS[] are in use.
static mut TASK_VALID: [bool; MAX_TASKS] = [false; MAX_TASKS];

/// Stack storage — each stack[i] is exclusively owned by TASKS[i].
/// Aligned to PAGE_SIZE (4 KiB) so that guard pages can unmap exact page
/// boundaries without affecting adjacent BSS data.
#[repr(align(4096))]
struct StackStorage([[u8; STACK_SIZE]; MAX_TASKS]);
static mut TASK_STACKS: StackStorage = StackStorage([[0u8; STACK_SIZE]; MAX_TASKS]);

/// RISC-V calling convention requires the stack pointer 16-byte aligned.
const STACK_ALIGN_BYTES: usize = 16;

/// Clean (pre-prologue, ABI-aligned) top of the stack at `stack_idx`.
///
/// This is the value a freshly dispatched task starts with, i.e. *before* any
/// function prologue has decremented SP. Used both at task creation and by the
/// I-13 transactional restart (`current_task_stack_top`).
///
/// SAFETY: `stack_idx` must be a valid task-pool index (`< MAX_TASKS`).
unsafe fn task_stack_top(stack_idx: usize) -> usize {
    let top = TASK_STACKS.0[stack_idx].as_mut_ptr() as usize + STACK_SIZE;
    top & !(STACK_ALIGN_BYTES - 1)
}

/// Monotonically increasing task ID counter.
static mut NEXT_TID: u32 = 1;

/// Spinlock protecting TASKS[], TASK_VALID[], and NEXT_TID.
static POOL_LOCK: AtomicBool = AtomicBool::new(false);

// ---- Per-CPU scheduler state (multi-level priority queue) ----

/// Per-priority-level FIFO queue (circular buffer of task indices).
#[derive(Copy, Clone)]
struct PrioQueue {
    buf:   [usize; MAX_TASKS],
    head:  usize,
    tail:  usize,
    count: usize,
}

const EMPTY_PRIO_QUEUE: PrioQueue = PrioQueue {
    buf:   [0; MAX_TASKS],
    head:  0,
    tail:  0,
    count: 0,
};

/// Per-CPU scheduling state with 32-level priority queue.
///
/// `current_idx = usize::MAX` means "no task running" (initial state, also after task_exit).
/// `ready_bitmap` bit `i` is set when `ready_queues[i]` is non-empty.
/// `trailing_zeros()` on the bitmap gives the highest-priority non-empty level in O(1).
#[derive(Copy, Clone)]
struct PerCpuSched {
    current_idx:  usize,
    ready_bitmap: u32,
    ready_queues: [PrioQueue; NUM_PRIORITIES],
}

/// Const initializer for PerCpuSched.
const EMPTY_CPU: PerCpuSched = PerCpuSched {
    current_idx:  usize::MAX,
    ready_bitmap: 0,
    ready_queues: [EMPTY_PRIO_QUEUE; NUM_PRIORITIES],
};

/// Per-CPU ready queues and current task index.
static mut PER_CPU: [PerCpuSched; MAX_CPUS] = [EMPTY_CPU; MAX_CPUS];

/// Per-CPU spinlocks for ready queue access.
/// Separate from PerCpuSched because AtomicBool is not Copy.
///
/// `MAX_CPUS`, not a literal: `PER_CPU` is `[_; MAX_CPUS]` and every index
/// used on one is used on the other — a hand-written 4 here compiled clean
/// while growing `MAX_CPUS` (the VF2 5-hart case is already documented) and
/// then indexed out of bounds. Same duplicated-constant class as the
/// `MAX_HARTS` injection into the asm.
static CPU_LOCKS: [AtomicBool; MAX_CPUS] =
    [const { AtomicBool::new(false) }; MAX_CPUS];

// ---- FFI: context_switch assembly ----

unsafe extern "C" {
    /// Switch from `old` task context to `new` task context.
    /// If `old` is null, just restores `new` (used for the very first task).
    fn context_switch(old: *mut Task, new: *mut Task);
}

// K-C23: `context_saving` used to be cleared by a `mark_context_saved`
// helper here, `call`ed from `context_switch.S` between saving `old`'s
// registers and restoring `new`'s. That call ran ON THE OLD TASK'S STACK,
// and the Release store inside it is precisely what publishes that stack as
// up for grabs — the helper only worked because rustc happened to compile it
// as a frameless leaf. Any future prologue/epilogue in it (a kprintln, a
// wcet probe, a dev-profile build) would have its epilogue racing the hart
// that already dispatched `old` and restored its sp. The clear now lives in
// the asm itself (`fence rw, w` + `sb zero` in `context_switch.S`, offset
// injected via `offset_of!` from kernel main.rs), where "no frame, last
// touch of the old stack" is true by construction instead of by accident.

// ---- Lock RAII guards ----

/// K-A13: IRQ-safe by construction (mirrors `CpuLockGuard` below): disables
/// `sstatus.SIE` before spinning for `POOL_LOCK` and restores the previous
/// interrupt state on drop. Without this, a timer tick on the same hart
/// while `task_exit()` holds this lock (it used to acquire it with
/// interrupts still enabled) could dispatch another task on that same hart
/// that then calls `task_create` (e.g. `fork()`) and spins on `POOL_LOCK`
/// forever with interrupts disabled — a same-hart deadlock, since only the
/// original holder (now preempted) could ever release it. Composes safely
/// with callers that already disable SIE themselves (`task_create_affinity`):
/// this guard just captures/restores whatever SIE was already at entry.
struct PoolGuard {
    prev_sstatus: usize,
}

impl PoolGuard {
    fn acquire() -> Self {
        let prev_sstatus = robot_os_arch::csr::read_sstatus();
        robot_os_arch::csr::write_sstatus(prev_sstatus & !robot_os_arch::csr::SSTATUS_SIE);

        while POOL_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        PoolGuard { prev_sstatus }
    }
}

impl Drop for PoolGuard {
    fn drop(&mut self) {
        POOL_LOCK.store(false, Ordering::Release);
        let current = robot_os_arch::csr::read_sstatus();
        let restored = (current & !robot_os_arch::csr::SSTATUS_SIE)
            | (self.prev_sstatus & robot_os_arch::csr::SSTATUS_SIE);
        robot_os_arch::csr::write_sstatus(restored);
    }
}

/// RAII guard for `CPU_LOCKS[cpu]`.
///
/// IRQ-safe by construction (mirrors `robot_os_sync::spinlock::lock_irqsave`):
/// disables `sstatus.SIE` on the local hart before spinning for the lock, and
/// restores the previous interrupt state on drop (lock released first, then
/// interrupts restored — a pending IRQ that fires right after re-enable must
/// see the lock as free).
///
/// This is the *only* acquisition path for `CPU_LOCKS`; every ready-queue
/// touch — same-CPU (`do_schedule`) or cross-CPU (`try_wake_task`,
/// `wq_wake_by_tid`, task creation) — goes through it. That is deliberate:
/// mixing a plain spin (`lock()`-style) with an irqsave spin on the *same*
/// lock reopens the deadlock this guard exists to close (an IRQ on the local
/// hart could preempt a plain-spin holder and then spin forever waiting for
/// itself). `do_schedule()` is reachable both from task context (interrupts
/// live) and from the timer ISR (`schedule()`), so plain `lock()` here would
/// not be safe.
struct CpuLockGuard {
    cpu: usize,
    prev_sstatus: usize,
}

impl CpuLockGuard {
    fn acquire(cpu: usize) -> Self {
        let prev_sstatus = robot_os_arch::csr::read_sstatus();
        robot_os_arch::csr::write_sstatus(prev_sstatus & !robot_os_arch::csr::SSTATUS_SIE);

        while CPU_LOCKS[cpu]
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        CpuLockGuard { cpu, prev_sstatus }
    }
}

impl Drop for CpuLockGuard {
    fn drop(&mut self) {
        CPU_LOCKS[self.cpu].store(false, Ordering::Release);
        // Restore only the SIE bit from the saved sstatus (same ordering
        // rationale as `IrqSaveGuard::drop` in crates/sync/src/spinlock.rs).
        let current = robot_os_arch::csr::read_sstatus();
        let restored = (current & !robot_os_arch::csr::SSTATUS_SIE)
            | (self.prev_sstatus & robot_os_arch::csr::SSTATUS_SIE);
        robot_os_arch::csr::write_sstatus(restored);
    }
}

/// Global lock serializing "scan `TASKS[]` for the earliest-deadline
/// candidate, then claim it" in `do_schedule()`'s deadline-pick branch.
///
/// Without this, two harts racing `do_schedule()` could both scan and
/// pick the same Ready deadline task before either writes `Running` —
/// see the task-lifecycle race fix. IRQ-safe for the same reason
/// `CpuLockGuard` is (mirrors `robot_os_sync::spinlock::lock_irqsave`):
/// `do_schedule()` is reachable from both task context and the timer ISR.
static DEADLINE_PICK_LOCK: AtomicBool = AtomicBool::new(false);

struct DeadlinePickGuard {
    prev_sstatus: usize,
}

impl DeadlinePickGuard {
    fn acquire() -> Self {
        let prev_sstatus = robot_os_arch::csr::read_sstatus();
        robot_os_arch::csr::write_sstatus(prev_sstatus & !robot_os_arch::csr::SSTATUS_SIE);

        while DEADLINE_PICK_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        DeadlinePickGuard { prev_sstatus }
    }
}

impl Drop for DeadlinePickGuard {
    fn drop(&mut self) {
        DEADLINE_PICK_LOCK.store(false, Ordering::Release);
        let current = robot_os_arch::csr::read_sstatus();
        let restored = (current & !robot_os_arch::csr::SSTATUS_SIE)
            | (self.prev_sstatus & robot_os_arch::csr::SSTATUS_SIE);
        robot_os_arch::csr::write_sstatus(restored);
    }
}

// ---- Internal helpers ----

/// Clamp a task's priority to a valid ready-queue bucket.
///
/// `ready_queues` has `NUM_PRIORITIES` (32) buckets and `ready_bitmap` is a
/// `u32`, so an out-of-range priority is an array index panic *and* a shift
/// overflow — under `panic = "abort"` that is a board reset, i.e. the loudest
/// possible failure for what is only ever a bad literal at a call site
/// (`task_create` takes `priority: u32` and never validates it). Clamping to
/// the lowest bucket degrades the task's scheduling instead of resetting the
/// board.
#[inline]
fn prio_bucket(priority: u32) -> usize {
    (priority as usize).min(NUM_PRIORITIES - 1)
}

/// Enqueue task `idx` at its priority level on CPU `cpu`.
/// Caller must hold `CPU_LOCKS[cpu]` or guarantee single-CPU access.
///
/// Returns `true` when this call put the task on the queue. `false` means
/// **this call** added nothing — see [`EnqueueOutcome`] for the two reasons
/// and why only one of them can lose work.
///
/// K-C12: the ring overflow used to be guarded by `debug_assert!`, which is
/// compiled out of the release profile this kernel ships. A full queue then
/// overwrote `q.buf[q.tail]` — silently discarding a *different* ready task —
/// and drove `q.count` past the ring so it no longer described the buffer.
/// Ready tasks disappeared with no error anywhere. See the `Task::queued`
/// doc for the invariant that now makes the full case unreachable, and
/// `enqueue_decision` for the policy itself.
unsafe fn cpu_enqueue(cpu: usize, idx: usize) -> bool {
    let task = task_mut(idx);
    let prio = prio_bucket(task.priority);
    // CLAIM, don't test-then-set. `CPU_LOCKS[cpu]` serializes enqueues onto
    // *one* CPU and nothing more, so two harts enqueueing the SAME task onto
    // DIFFERENT CPUs hold different locks and never exclude each other — and
    // that pair is reachable: `try_wake_task` and `wq_wake_by_tid` both flip
    // `state` without POOL_LOCK, and `wake_target_cpu` is deliberately
    // unlocked and approximate, so two concurrent wakers can legitimately
    // choose different targets for the same task. A plain load-then-store
    // would let both observe `false`, both append, and put the task in two
    // rings at once — which is exactly the duplicate this flag exists to
    // forbid, and it would void the counting argument that makes `Full`
    // unreachable. The atomic swap makes the claim itself the arbitration:
    // the loser reads `true` and refuses.
    let already = task.queued.swap(true, Ordering::AcqRel);
    let count = PER_CPU[cpu].ready_queues[prio].count;

    match enqueue_decision(already, count, MAX_TASKS) {
        // Still queued (elsewhere, or by the hart that won the swap) — leave
        // the claim standing, it belongs to that entry.
        // Still queued (elsewhere, or by the hart that won the swap) — leave
        // the claim standing, it belongs to that entry.
        //
        // MEASURED, not hypothetical (ring-3 ipctest, `-smp 4`): this fires
        // steadily for the periodic kernel tasks — `imu`, `odom`,
        // `sensor-slow`. The source is `wake_expired_timers`, which runs in
        // EVERY hart's timer ISR and reaches `try_wake_task`, whose
        // `state != Blocked` test and its `state = Ready` write have nothing
        // between them: two harts firing close together both see `Blocked`
        // and both enqueue. A probe that scanned every ready queue on each
        // refusal reported `present=true` 20 times out of 20 — the task
        // really was already queued, so refusing loses nothing — and
        // `user=false` every time, i.e. no ring-3 task was ever refused.
        //
        // Before `queued`, each of those duplicates added an entry and
        // incremented `count` toward the ring size. That is the second half
        // of K-C12: a queue that fills with duplicates starts overwriting
        // live entries, and the tasks it overwrites disappear in silence.
        EnqueueOutcome::AlreadyQueued => false,
        EnqueueOutcome::Full => {
            // We won the claim but cannot honour it: release it, or the task
            // becomes permanently un-enqueueable.
            task.queued.store(false, Ordering::Release);
            // Unreachable while the `queued` invariant holds; if it ever
            // fires, the invariant broke and that is worth a line in the log.
            // The task stays `Ready` and un-queued: whoever next wakes or
            // preempts it will enqueue it then. That is a delay; overwriting
            // a live entry, which is what the old code did here, was a
            // permanent, silent loss of somebody else's task.
            robot_os_drivers::kprintln!(
                "[SCHED] BUG: ready queue cpu{} prio{} full ({} entries) — task {} not enqueued",
                cpu, prio, count, task.tid,
            );
            false
        }
        EnqueueOutcome::Append => {
            // The claim was taken by the swap above.
            let q = &mut PER_CPU[cpu].ready_queues[prio];
            q.buf[q.tail] = idx;
            q.tail = (q.tail + 1) % MAX_TASKS;
            q.count += 1;
            PER_CPU[cpu].ready_bitmap |= 1 << prio;
            true
        }
    }
}

/// Dequeue the highest-priority ready task from CPU `cpu`.
/// Caller must hold `CPU_LOCKS[cpu]` or guarantee single-CPU access.
unsafe fn cpu_dequeue(cpu: usize) -> Option<usize> {
    let bitmap = PER_CPU[cpu].ready_bitmap;
    if bitmap == 0 {
        return None;
    }
    let prio = bitmap.trailing_zeros() as usize;
    if prio >= NUM_PRIORITIES {
        return None; // Impossible for a u32 bitmap; keeps the index provably safe.
    }
    let q = &mut PER_CPU[cpu].ready_queues[prio];
    if q.count == 0 {
        // Bitmap says occupied, ring says empty: they disagree. `count -= 1`
        // here would underflow, and `overflow-checks = true` turns that into
        // a board reset. Re-sync the bitmap and report "nothing ready".
        PER_CPU[cpu].ready_bitmap &= !(1 << prio);
        return None;
    }
    let idx = q.buf[q.head];
    q.head = (q.head + 1) % MAX_TASKS;
    q.count -= 1;
    if q.count == 0 {
        PER_CPU[cpu].ready_bitmap &= !(1 << prio);
    }
    // K-C12: this is the only pop, so it is the only place the `queued`
    // invariant is released. Leaving it set here would make the task
    // permanently un-enqueueable — silent starvation, the exact failure the
    // flag exists to prevent.
    if idx < MAX_TASKS {
        task_mut(idx).queued.store(false, Ordering::Release);
    }
    Some(idx)
}

/// Return the priority of the highest-priority ready task on `cpu`, or None.
///
/// Intentionally lock-free: called from `schedule()` as a preemption *hint*
/// (RT-task check) on `cpu == current_cpu_id()`. A stale read here (racing a
/// concurrent cross-CPU `cpu_enqueue_locked` targeting this CPU) only delays
/// or advances a preemption decision by up to one timer tick — the bitmap
/// write itself is always lock-protected, so this can observe an old-but-
/// consistent bitmap value, never a torn one. Same tolerance as the existing
/// `find_best_cpu()` (see its doc comment).
unsafe fn cpu_peek_highest_prio(cpu: usize) -> Option<u32> {
    let bitmap = PER_CPU[cpu].ready_bitmap;
    if bitmap == 0 { None } else { Some(bitmap.trailing_zeros()) }
}

/// Dequeue the highest-priority ready task from CPU `cpu`, taking
/// `CPU_LOCKS[cpu]` for the duration of the operation.
///
/// This is the cross-CPU-safe entry point — use this (never the raw
/// `cpu_dequeue`) from anywhere that isn't already holding `CPU_LOCKS[cpu]`.
/// The lock is released before returning, so it is never held across a
/// `context_switch()` call.
unsafe fn cpu_dequeue_locked(cpu: usize) -> Option<usize> {
    let _g = CpuLockGuard::acquire(cpu);
    cpu_dequeue(cpu)
}

/// Enqueue task `idx` on CPU `cpu`'s ready queue, taking `CPU_LOCKS[cpu]`
/// for the duration of the operation.
///
/// This is the cross-CPU-safe entry point — use this (never the raw
/// `cpu_enqueue`) from anywhere that isn't already holding `CPU_LOCKS[cpu]`.
/// Callers include waking a task pinned to (or placement-assigned to) a
/// CPU other than the caller's own — `try_wake_task`, `wq_wake_by_tid`,
/// `task_create_affinity` — as well as `do_schedule()` re-enqueuing the
/// outgoing task on its own CPU, which races the very same cross-CPU wakers.
unsafe fn cpu_enqueue_locked(cpu: usize, idx: usize) -> bool {
    let appended = {
        let _g = CpuLockGuard::acquire(cpu);
        cpu_enqueue(cpu, idx)
    }; // guard dropped before the SBI call below — never ecall holding a lock.

    // K-C15: tell the target hart it has work.
    //
    // **WHY the enqueue alone was not enough.** A hart with nothing ready runs
    // `idle_task`, which is `loop { wfi() }`, and the only thing that ever
    // preempts it is a timer interrupt. This kernel is *tickless*
    // (`nearest_timer_deadline` programs `mtimecmp` at the next real deadline
    // rather than at a fixed rate), so "the next tick" on an idle hart can be
    // arbitrarily far away. Making a task `Ready` on another hart's queue
    // therefore did not make it *run* — it made it eligible to run whenever
    // that hart happened to wake for some unrelated reason.
    //
    // Measured before this: a ring-3 fast-IPC round trip took on the order of
    // seconds per exchange, with both peers correct, awake and enqueued. It
    // reads as a hang and is a missing doorbell.
    //
    // `send_ipi` (`crates/arch-riscv64/src/api_impl.rs`) and the
    // `INT_SOFTWARE_S` trap arm both already existed; the arm was used only
    // for TLB shootdown and nothing ever called the sender. `tp` is the hart
    // id (`boot.S:30`), and CPU indices are hart ids, so `cpu` addresses the
    // hart directly.
    //
    // Self-enqueue needs no doorbell: this hart is by definition awake, and it
    // reaches `do_schedule` on its own path out. Sending to a hart that never
    // came up is harmless — SBI reports an error we deliberately ignore.
    // Only ring the doorbell when this call actually made the hart's queue
    // longer. A refused enqueue (K-C12: the task was already queued) has
    // nothing new to announce.
    if appended && cpu != current_cpu_id() {
        let _ = robot_os_arch::sbi::send_ipi(1, cpu);
    }
    appended
}

/// Allocate a free slot in TASKS[].
/// Caller must hold POOL_LOCK.
///
/// **The tid sentinel protocol (root fix for the `idx_for_tid` race).**
/// `TASK_VALID[i] = true` used to publish the slot while `TASKS[i].tid`
/// still carried the PREVIOUS occupant's TID; an unsynchronised scan
/// (`idx_for_tid` — cap_store's resolution path runs it without any lock)
/// could match a dead TID against a slot that now belongs to a brand-new
/// task, and the operation attributed to the dead TID then WIPED the live
/// task's cap table (`cap_store::claim_slot`'s owner-mismatch wipe). The
/// contained version (`resolve_only_untrusted`'s double scan) narrowed the
/// window; this closes it at the source: `tid` is cleared to 0 — the
/// "no task" sentinel `NEXT_TID` never issues — BEFORE the slot becomes
/// visible, with a Release fence ordering the two plain stores (RVWMO
/// allows store-store reordering; without the fence the fix is fiction).
/// The free sites do the mirror image (VALID=false, fence, tid=0), and
/// `idx_for_tid` revalidates a match behind an Acquire fence.
unsafe fn alloc_slot() -> Option<usize> {
    for i in 0..MAX_TASKS {
        if !TASK_VALID[i] {
            TASKS[i].tid = 0;
            core::sync::atomic::fence(Ordering::Release);
            TASK_VALID[i] = true;
            return Some(i);
        }
    }
    None
}

/// Get a mutable reference to TASKS[idx].
unsafe fn task_mut(idx: usize) -> &'static mut Task {
    &mut TASKS[idx]
}

/// K-C12 · Pick the CPU on which a task of priority `prio` will actually be
/// dispatched. Replaces the old `find_least_loaded_cpu()`.
///
/// See [`crate::task::pick_cpu_by_load`] for the policy and the ring-3
/// measurement behind it. Two things about *this* function are load-bearing
/// and were both learned the hard way:
///
/// **It scores residency, not the instantaneous queue state.** The first
/// version of this fix sampled `PER_CPU[c].ready_queues[..]` plus the task
/// running on `c`. That halved the loss rate and no more: `rt-motor` and
/// `flight-ctrl` are periodic, so there are windows in which both are
/// `Blocked` and hart 0 samples as completely idle. A child placed in one of
/// those windows is stuck behind them microseconds later, forever. What makes
/// a hart hostile to low-priority work is *which tasks live there*, not which
/// of them happen to be runnable at the instant of the sample.
///
/// **The ordering is `(rt_blocking, blocking, total)`, and the first key is
/// not decoration.** Ranking on `(blocking, total)` alone regressed once the
/// unpinned mid-priority tasks had spread out and every hart carried two
/// outranking residents: `total` then chose hart 0 — the hart dedicated to
/// `rt-motor` and `flight-ctrl` — precisely because all the earlier children
/// had gone elsewhere. See [`crate::task::pick_cpu_by_load`] for the measured
/// trace.
///
/// **`total` only breaks ties.** A hart with fifty same-priority tasks still
/// dispatches a newcomer (the ready queues are round-robin within a level);
/// a hart with one permanently runnable higher-priority task never does.
/// Liveness therefore outranks balance, and on this kernel's default layout that means
/// unpinned work concentrates on the harts without real-time residents. That
/// is the correct trade: an unbalanced-but-running task beats a
/// balanced-and-starved one.
///
/// A task's home is its pin (`cpu_affinity`) when it has one, otherwise the
/// saved `tp` that `context_switch` will restore — the same value every wake
/// path keeps in sync with the queue the task sits in.
///
/// `exclude` is the pool index of the task being placed, so it does not score
/// against its own placement. Pass `usize::MAX` for none.
///
/// **Cost, since this also runs from the timer ISR** (`wake_expired_timers` →
/// `try_wake_task` → `wake_target_cpu`, and `schedule()` carries
/// `#[wcet(30_us)]`): it is not a step up from what it replaces. The old
/// metric read `PER_CPU[c].ready_queues[p].count` for every `c` and every `p`
/// — `MAX_CPUS * NUM_PRIORITIES` = 128 reads, each `size_of::<PrioQueue>()`
/// (536 B) apart, i.e. 128 distinct cache lines. This reads at most
/// `MAX_TASKS` = 64 task slots, and everything it needs
/// (`state`/`priority`/`cpu_affinity` at offsets 132..145, `context.tp` at
/// 120) sits in two lines per slot — the same 128. Unlocked, exactly as the
/// old metric was: a stale sample costs one suboptimal placement, never
/// correctness.
///
/// Uses NUM_ONLINE_CPUS to limit the search. Acquire load pairs with the
/// SeqCst stores in kernel_main: the pre-wake_harts store (so secondaries
/// observe the published task pool) and the post-wake_harts correction (so a
/// hart that never started is excluded instead of looking permanently idle).
unsafe fn find_best_cpu(prio: u32, exclude: usize) -> usize {
    let num_online = NUM_ONLINE_CPUS.load(Ordering::Acquire).min(MAX_CPUS);
    if num_online <= 1 {
        return 0;
    }
    let bucket = prio_bucket(prio);
    let mut loads = [CpuLoad { rt_blocking: 0, blocking: 0, total: 0 }; MAX_CPUS];
    for i in 0..MAX_TASKS {
        if i == exclude || !TASK_VALID[i] {
            continue;
        }
        let t = &TASKS[i];
        // A Zombie is on its way out and will never contend again.
        if t.state() == TaskState::Zombie {
            continue;
        }
        let home = if t.cpu_affinity >= 0 {
            (t.cpu_affinity as usize).min(MAX_CPUS - 1)
        } else {
            (t.context.tp as usize).min(MAX_CPUS - 1)
        };
        if home >= num_online {
            continue;
        }
        loads[home].total = loads[home].total.saturating_add(1);
        if prio_bucket(t.priority) < bucket {
            loads[home].blocking = loads[home].blocking.saturating_add(1);
            if is_rt_priority(t.priority) {
                loads[home].rt_blocking = loads[home].rt_blocking.saturating_add(1);
            }
        }
    }
    pick_cpu_by_load(&loads[..num_online])
}

/// Pick target CPU respecting affinity.
/// If affinity >= 0, returns that hart directly; otherwise the hart where a
/// task of priority `prio` will actually get dispatched (K-C12).
unsafe fn pick_target_cpu(affinity: i8, prio: u32, exclude: usize) -> usize {
    if affinity >= 0 {
        // K-A13: clamp — an out-of-range affinity (a bad literal at a call
        // site; not reachable from userspace today) must not index
        // CPU_LOCKS/PER_CPU out of bounds and panic.
        (affinity as usize).min(MAX_CPUS - 1)
    } else {
        find_best_cpu(prio, exclude)
    }
}

// ---- Task entry wrapper ----

/// Entry point for every new task (called from context_switch assembly).
///
/// Reads `entry_fn` and `entry_arg` from the current task struct, calls them.
/// When entry_fn returns, calls `task_exit()`.
///
/// # Safety
/// Called from assembly with C ABI. No arguments passed via registers.
/// `tp` must contain the current hart_id.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn task_entry_wrapper() {
    let cpu = current_cpu_id();
    let idx = PER_CPU[cpu].current_idx;

    // Enable interrupts — required when first entered from a timer ISR
    // (hardware clears SIE on interrupt entry; sret would restore it, but we
    // jumped here via context_switch instead of returning via sret).
    // Also harmless when entered from start() which is not in interrupt context.
    let sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(sstatus | robot_os_arch::csr::SSTATUS_SIE);

    let entry_fn: fn(usize) = core::mem::transmute(task_mut(idx).entry_fn);
    let arg = task_mut(idx).entry_arg;
    entry_fn(arg);

    // Task function returned — clean up.
    task_exit();
}

// ---- Public API ----

/// PHANES Phase 1 W4-int.2 — global flag controlling dispatch path.
///
/// `false` (default) ⇒ legacy priority-queue scheduler picks tasks.
/// `true` ⇒ Adaptive Partitioning + per-class policies pick tasks.
///
/// Toggle at runtime via [`use_aps_dispatch`]. The legacy queue is
/// always maintained, so flipping the flag back to `false` returns
/// to legacy behaviour without loss of state.
static SCHED_USE_APS: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Enable (`true`) or disable (`false`) the W4 Adaptive Partitioning
/// dispatch. Returns the previous value.
///
/// Also mirrors the new state into the typed
/// [`crate::runtime::registry`] so consumers using the typed API
/// see a consistent value. The boolean here remains the hot-path
/// source for `aps_dispatch_enabled()`; the registry holds the
/// same fact in typed form for `procfs` + future hot-swap.
pub fn use_aps_dispatch(enable: bool) -> bool {
    let prev = SCHED_USE_APS.swap(enable, core::sync::atomic::Ordering::AcqRel);
    // Mirror into the typed registry. The unwrap is sound because
    // Legacy and Aps are both `is_supported_now()`.
    let _ = crate::runtime::registry::set_active(if enable {
        crate::runtime::registry::SchedulerHandle::Aps
    } else {
        crate::runtime::registry::SchedulerHandle::Legacy
    });
    prev
}

/// Returns the current state of the APS-dispatch flag.
#[inline]
pub fn aps_dispatch_enabled() -> bool {
    SCHED_USE_APS.load(core::sync::atomic::Ordering::Acquire)
}

/// Initialize the scheduler. Call once before `task_create` / `start`.
pub fn init() {
    // PER_CPU is initialized via EMPTY_CPU const (current_idx = usize::MAX).
    // Static storage is already zero-initialized (BSS).

    // PHANES Phase 1 W4-int.2 — mark the per-CPU APS state as ready
    // for use. The window stays anchored at 0; `Aps::tick` catches
    // up in one step when the first real timer tick arrives, so we
    // don't need a time-read dep in this crate. The dispatch path is
    // *not* yet driven by APS (`SCHED_USE_APS` is false); flipping
    // the flag later activates it without touching boot init.
    crate::aps_state::mark_initialised();
}

/// Create a new kernel task with CPU affinity.
///
/// `affinity`: -1 = auto-assign via `find_best_cpu`, 0..3 = pin to that hart.
/// Returns the task pool index.
///
/// Panics if the task pool is exhausted. Every existing caller of this
/// function is kernel-internal (boot-time or otherwise trusted), so pool
/// exhaustion here is a genuine system misconfiguration worth crashing
/// loudly for. **Any task-creation path reachable by unprivileged
/// userspace (e.g. `fork()`) MUST use [`try_task_create_affinity`]
/// instead** — with `panic = "abort"` in this profile a panic here is a
/// full board reset, so letting a user fork-bomb the pool would be a
/// remote/local DoS. K-A13.
pub fn task_create_affinity(
    name: &str,
    entry_fn: fn(usize),
    arg: usize,
    priority: u32,
    affinity: i8,
) -> usize {
    try_task_create_affinity(name, entry_fn, arg, priority, affinity)
        .expect("sched: task pool full")
}

/// Fallible variant of [`task_create_affinity`] — returns `None` instead of
/// panicking when the task pool is exhausted. See that function's doc for
/// when to use this one. K-A13.
pub fn try_task_create_affinity(
    name: &str,
    entry_fn: fn(usize),
    arg: usize,
    priority: u32,
    affinity: i8,
) -> Option<usize> {
    // Disable interrupts during task creation to prevent races on NEXT_TID.
    let sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(sstatus & !robot_os_arch::csr::SSTATUS_SIE);

    // K-C22(B): address spaces the claimed slot's PREVIOUS occupant left
    // behind, captured under POOL_LOCK and destroyed after interrupts are
    // back on (a full teardown walks and frees hundreds of frames — not
    // something to do with SIE off on an RT hart). See the reuse-time
    // comment at the capture site for why destroying here is safe at all.
    let mut stale_user_pt: u64 = 0;
    let mut stale_exec_old_pt: u64 = 0;

    let result = unsafe {
        // --- Allocate and initialize task under pool lock ---
        let alloc = {
            let _pool = PoolGuard::acquire();

            match alloc_slot() {
                None => None,
                Some(idx) => {
                    let task = task_mut(idx);

                    task.tid = NEXT_TID;
                    // Wrapping, and skipping 0: with `overflow-checks = true` a
                    // bare `+= 1` panics (= board reset) at the 2^32nd task
                    // creation — reachable on a long-lived robot under fork
                    // churn. TID 0 is skipped on wrap because idle/`NO_TID`
                    // conventions treat low sentinel values specially, and a
                    // colliding live TID after 4 billion creations is far less
                    // dangerous than a guaranteed panic.
                    NEXT_TID = NEXT_TID.wrapping_add(1);
                    if NEXT_TID == 0 { NEXT_TID = 1; }

                    let name_bytes = name.as_bytes();
                    let len = name_bytes.len().min(31);
                    task.name[..len].copy_from_slice(&name_bytes[..len]);
                    task.name[len] = 0;

                    task.priority      = priority;
                    task.base_priority = priority;
                    // Pool slots are reused: a stale count from the previous
                    // occupant would keep this task permanently "donated to".
                    task.donation_count.store(0, Ordering::Relaxed);
                    // Same slot-reuse hazard for the other cross-hart flags.
                    // A stale `wake_pending` (a wake that raced the previous
                    // occupant's exit and was never consumed) would hand this
                    // task a phantom wakeup on its first block. A stale
                    // `fork_ctx_ready` (previous occupant was a fork child
                    // that died without consuming its context) is far worse:
                    // a new fork child in this slot would swap it, read the
                    // PREVIOUS process's entry/user_sp/satp, and SRET into a
                    // freed address space. `context_saving` should already be
                    // false on every exit path; clearing it is free insurance
                    // against a future path that Zombifies mid-transition.
                    // (The wake stamp lives in `state_word` now — cleared
                    // together with the state reset below, atomically.)
                    // K-C12: a stale `queued` (previous occupant died while
                    // an entry of its own still sat in a ready queue) would
                    // make this brand-new task permanently un-enqueueable —
                    // `cpu_enqueue` would refuse it forever as a duplicate.
                    // That is the same silent starvation the flag prevents,
                    // inverted.
                    task.queued.store(false, Ordering::Relaxed);
                    task.fork_ctx_ready.store(false, Ordering::Relaxed);
                    task.fork_entry   = 0;
                    task.fork_user_sp = 0;
                    task.fork_satp    = 0;
                    // K-C21: same hygiene for the exec hand-off. A stale
                    // `exec_ctx_ready` (previous occupant published an exec
                    // it never consumed — no such path exists today, but the
                    // flag must not be the thing that assumption hangs on)
                    // would make this task's next ecall SRET into the dead
                    // process's image. If one IS pending, its `exec_old_pt`
                    // is an address space nothing else references any more —
                    // hand it to the same reuse-time reclaim as `user_pt`
                    // below instead of leaking it.
                    if task.exec_ctx_ready.swap(false, Ordering::Relaxed) {
                        stale_exec_old_pt = task.exec_old_pt;
                    }
                    task.exec_entry   = 0;
                    task.exec_user_sp = 0;
                    task.exec_sstatus = 0;
                    task.exec_satp    = 0;
                    task.exec_old_pt  = 0;
                    task.context_saving.store(false, Ordering::Relaxed);
                    task.time_slice    = if is_rt_priority(priority) {
                        RT_TIME_SLICE_TICKS
                    } else {
                        TIME_SLICE_TICKS
                    };
                    task.cpu_affinity  = affinity;
                    // One plain store resets state AND clears any stale wake
                    // stamp from the slot's previous occupant — the only
                    // place a plain (non-CAS) store of the word is correct,
                    // because the slot is not yet visible to any waker.
                    task.state_word.store(
                        crate::task::sched_word::pack(TaskState::Ready),
                        Ordering::Relaxed,
                    );
                    task.wait_reason    = WaitReason::None;
                    task.deadline       = DeadlineParams::default();
                    task.syscall_filter = SyscallFilter::disabled();
                    task.stack_idx      = idx;
                    task.entry_fn       = entry_fn as usize;
                    task.entry_arg      = arg;

                    // PHANES Phase 1 W4-int — multi-policy scheduler defaults.
                    // The legacy entry points (`task_create`, `task_create_affinity`)
                    // default to `BestEffort` to preserve current behaviour. The
                    // new fields are not yet consulted by the dispatch core; W4-int.2
                    // will wire them through `Aps::pick_class`.
                    task.sched_class_raw     = crate::task::DEFAULT_SCHED_CLASS_RAW;
                    task._sched_pad          = [0u8; 3];
                    task.sched_time_slice_us = 0;
                    task.sched_deadline_us   = crate::task::NO_DEADLINE;

                    // Stack grows down; top is the (ABI-aligned) end of the slice.
                    let stack_top = task_stack_top(idx);

                    let entry_addr = task_entry_wrapper as *const () as usize as CtxReg;
                    let target_cpu = pick_target_cpu(affinity, priority, idx);
                    task.context = TaskContext {
                        sp: stack_top as CtxReg,
                        pc: entry_addr,
                        ra: entry_addr,
                        tp: target_cpu as CtxReg,
                        ..Default::default()
                    };

                    // Phase 7: per-task SATP and user-space fields (requires MMU).
                    //
                    // K-C22(B): `task_exit()` cannot free the dying task's
                    // address space — the exiting hart is still EXECUTING
                    // under that satp (kernel mappings live inside the user
                    // PT), and stays on it until `do_schedule()` switches
                    // away. So teardown is deferred to REUSE time, i.e. right
                    // here: K-C6 frees the slot only in the `do_schedule()`
                    // call that is about to context-switch off the dying
                    // task's stack. Zeroing without destroying (the old code)
                    // leaked the whole address space of every exited user
                    // task. Capture under POOL_LOCK, destroy after SIE is
                    // restored (see `stale_user_pt` above).
                    //
                    // Residual window, argued exactly: the Zombie arm clears
                    // TASK_VALID a few dozen instructions BEFORE its
                    // `context_switch()` reaches `csrw satp`, so a claim can
                    // land while the freeing hart is still on the dying PT.
                    // In that tail the freeing hart runs straight-line,
                    // IRQ-off kernel code (spin-gate, a handful of stores,
                    // the register restore) touching only kernel text /
                    // stacks / .bss — all of which resolve through the
                    // kernel L1 tables the teardown BORROWS and never frees
                    // (`destroy_user_pagetable_skip_range` skips
                    // `k_l1 == u_l1`). The only frame that hart still needs
                    // is the PT ROOT, which the teardown frees LAST — after
                    // this claim finishes slot init + enqueue under two more
                    // locks, restores SIE, and walks all 512 L2 slots (a
                    // deliberate, load-bearing property; noted on the vmm
                    // fn). The satp switch therefore precedes the root free
                    // by orders of magnitude, and actual damage would still
                    // require the freed root to be re-allocated, overwritten
                    // AND the dying hart to miss in TLB for kernel text
                    // inside those same few instructions.
                    {
                        task.task_satp = robot_os_arch::csr::read_satp() as u64;
                        stale_user_pt  = task.user_pt;
                        task.user_pt   = 0;
                        task.user_brk  = 0;
                    }

                    // Phase 16: write stack canary at the bottom of the stack (lowest
                    // address).  Stack grows downward, so this is the first location
                    // overwritten on overflow.  Checked by `stack_canary_check()`.
                    // Skip when guard pages are active — the bottom page is unmapped
                    // and writing the canary would page fault.
                    if !GUARD_PAGES_ACTIVE.load(Ordering::Acquire) {
                        (TASK_STACKS.0[idx].as_mut_ptr() as *mut u64).write_volatile(STACK_CANARY);
                    }

                    Some((idx, target_cpu))
                }
            }
        }; // pool lock released here

        match alloc {
            None => None,
            Some((idx, target_cpu)) => {
                // --- Enqueue to target CPU under CPU lock ---
                // `target_cpu` may differ from the creating CPU (priority-aware
                // assignment or explicit affinity) — always go through the locked
                // wrapper.
                // K-C12: a brand-new task can never be a duplicate (its
                // `queued` flag was just cleared), so a refusal here can only
                // mean the ring is genuinely full. Fail the creation instead
                // of returning a task index for something that is in no ready
                // queue and will never run: `fork()` reports -1, which its
                // caller already handles, and the pool slot goes back.
                if !cpu_enqueue_locked(target_cpu, idx) {
                    // Inner scope on purpose: `PoolGuard::drop` re-applies the
                    // SIE bit it captured (interrupts already off here), so it
                    // MUST run before the caller's `sstatus` is restored —
                    // otherwise the guard drops last and leaves this hart with
                    // interrupts disabled all the way back up the stack.
                    {
                        let _pool = PoolGuard::acquire();
                        TASK_VALID[idx] = false;
                        // Mirror of alloc_slot's sentinel protocol: unpublish,
                        // fence, then clear the tid so no unsynchronised scan
                        // can ever again match this task's TID against the
                        // slot's next life.
                        core::sync::atomic::fence(Ordering::Release);
                        TASKS[idx].tid = 0;
                    }
                    robot_os_arch::csr::write_sstatus(sstatus);
                    // K-C22(B): the claim already zeroed the slot's `user_pt`
                    // — the captured stale address spaces must be destroyed
                    // on THIS exit too, or they become unreachable forever.
                    reclaim_stale_user_pts(stale_user_pt, stale_exec_old_pt);
                    return None;
                }

                // PHANES Phase 1 W4-int.2 — mirror into the per-class policy
                // runqueue so flipping `SCHED_USE_APS` later finds tasks
                // already populated. The class is read from task fields
                // (defaults to BestEffort for legacy callers).
                {
                    let task = task_mut(idx);
                    crate::aps_state::enqueue_task_for_class(
                        target_cpu,
                        task.tid,
                        task.sched_class_raw,
                        task.priority.min(255) as u8,
                        task.sched_time_slice_us,
                        task.sched_deadline_us,
                    );
                }

                Some(idx)
            }
        }
    };

    // Restore interrupts.
    robot_os_arch::csr::write_sstatus(sstatus);
    // K-C22(B): reuse-time reclaim of the previous occupant's address
    // space(s), with interrupts back on. Runs before the caller learns the
    // new index, but after the new task is enqueued — harmless: the new task
    // holds `user_pt = 0` and never references what is being freed.
    reclaim_stale_user_pts(stale_user_pt, stale_exec_old_pt);
    result
}

/// K-C22(B): destroy address spaces recovered from a reused task slot.
///
/// Callable ONLY with page tables that satisfy the reuse-time argument in
/// `try_task_create_affinity` (no hart can still hold them in satp/TLB).
/// Goes through `process::destroy_user_address_space` — not bare
/// `vmm::destroy_user_pagetable` — so shm/MMIO frames mapped into the dead
/// address space are spared (they are owned by the shm registry / the
/// hardware, not by the page table; see that function).
fn reclaim_stale_user_pts(user_pt: u64, exec_old_pt: u64) {
    if user_pt != 0 {
        crate::process::destroy_user_address_space(user_pt);
    }
    if exec_old_pt != 0 {
        crate::process::destroy_user_address_space(exec_old_pt);
    }
}

/// Create a new kernel task, auto-assigned to the CPU where a task of this
/// priority will actually get dispatched (K-C12 — see [`find_best_cpu`]).
///
/// Returns the task pool index (for debugging; rarely needed by callers).
pub fn task_create(name: &str, entry_fn: fn(usize), arg: usize, priority: u32) -> usize {
    task_create_affinity(name, entry_fn, arg, priority, -1)
}

/// PHANES Phase 1 W4-int — create a task with explicit scheduler-class
/// metadata (RFC-0004).
///
/// - `class_raw` — `SchedClass` discriminant (see
///   `robot_os_sched::class::SchedClass`).
/// - `deadline_us` — absolute monotonic-time deadline in microseconds.
///   Pass `crate::task::NO_DEADLINE` (= 0) for non-EDF tasks.
/// - `time_slice_us` — quantum for `Rr` / CBS budget seed. `0` ⇒
///   policy default.
///
/// The new fields are stored on the task; the live scheduler does not
/// yet consult them (W4-int.2). Callers can use this entry point
/// today so existing initialisation code is forward-compatible.
pub fn task_create_with_class(
    name: &str,
    entry_fn: fn(usize),
    arg: usize,
    priority: u32,
    affinity: i8,
    class_raw: u8,
    deadline_us: u64,
    time_slice_us: u32,
) -> usize {
    let idx = task_create_affinity(name, entry_fn, arg, priority, affinity);
    if idx >= MAX_TASKS {
        return idx;
    }
    // SAFETY: `idx` was just returned by a successful task_create_affinity
    // call; the slot is exclusively owned for the brief moment between
    // its return and the task first running.
    unsafe {
        if !TASK_VALID[idx] {
            return idx;
        }
        let task = task_mut(idx);
        // task_create_affinity already co-enqueued under the default
        // BestEffort class. Move the task to its real class by
        // dequeueing from the old policy and enqueueing into the new.
        let tid = task.tid;
        let target_cpu = task.context.tp as usize;
        let old_class = task.sched_class_raw;
        // Patch the task's class metadata.
        task.sched_class_raw     = class_raw;
        task.sched_deadline_us   = deadline_us;
        task.sched_time_slice_us = time_slice_us;
        // Move it in the per-class policy runqueues.
        crate::aps_state::dequeue_task_for_class(target_cpu, tid, old_class);
        crate::aps_state::enqueue_task_for_class(
            target_cpu,
            tid,
            class_raw,
            task.priority.min(255) as u8,
            time_slice_us,
            deadline_us,
        );
    }
    idx
}

/// Translate a `tid` to a slot index in the global `TASKS[]` array.
/// O(`MAX_TASKS`); acceptable since `MAX_TASKS = 64` on default builds.
/// Used by the APS dispatch path.
pub fn idx_for_tid(tid: u32) -> Option<usize> {
    // TID 0 is the sentinel `alloc_slot` parks in a slot before publishing
    // it (and the free sites park after unpublishing) — it never names a
    // task, so it must never match one.
    if tid == 0 {
        return None;
    }
    unsafe {
        for i in 0..MAX_TASKS {
            if TASK_VALID[i] && TASKS[i].tid == tid {
                // Pairs with the Release fences in `alloc_slot` and the free
                // sites: having (tentatively) observed a match, order the
                // re-reads after everything the publisher wrote before its
                // fence. A mid-allocation slot re-reads as tid 0 and is
                // rejected; only a slot whose tid was genuinely published
                // (or a torn first read that the re-read corrects) survives.
                // Fence-per-MATCH, not per-iteration — a resolution walks up
                // to 64 slots and this path is on every cap operation.
                core::sync::atomic::fence(Ordering::Acquire);
                if TASK_VALID[i] && TASKS[i].tid == tid {
                    return Some(i);
                }
            }
        }
    }
    None
}

/// Translate a slot index in `TASKS[]` to its `tid`. Inverse of
/// [`idx_for_tid`]. Returns `None` if the slot is not a valid task.
pub fn tid_for_idx(idx: usize) -> Option<u32> {
    if idx >= MAX_TASKS {
        return None;
    }
    unsafe {
        if TASK_VALID[idx] {
            Some(TASKS[idx].tid)
        } else {
            None
        }
    }
}

/// Update the scheduler-class metadata of an existing task. Intended
/// for tests and the topology-bind path (W5+). The live dispatch core
/// does not yet consult these fields.
pub fn task_set_class(idx: usize, class_raw: u8, deadline_us: u64, time_slice_us: u32) {
    if idx >= MAX_TASKS {
        return;
    }
    unsafe {
        if !TASK_VALID[idx] {
            return;
        }
        let task = task_mut(idx);
        task.sched_class_raw     = class_raw;
        task.sched_deadline_us   = deadline_us;
        task.sched_time_slice_us = time_slice_us;
    }
}

/// Create a task with a security profile pre-applied.
///
/// The filter is set before the task ever runs — it cannot call any
/// unauthorized syscall, not even during initialization.
///
/// An unknown `profile_id` FAILS CLOSED to `PROFILE_MINIMAL` (exit/yield/
/// sleep/write/brk only) and logs. This function returns a task index with
/// no error channel, so the old behaviour — `profile_to_filter` handing
/// back a disabled filter for any unrecognised id — created a fully
/// *unrestricted* child while the caller believed it had sandboxed one,
/// with no return code that could have revealed the difference. A child
/// that is too confined to work fails loudly during bring-up; a child that
/// is silently unconfined does not fail at all until it matters.
pub fn task_create_filtered(
    name: &str, entry_fn: fn(usize), arg: usize,
    priority: u32, profile_id: u64,
) -> usize {
    let idx = task_create(name, entry_fn, arg, priority);
    // Apply the filter to the newly created task.
    let filter = match crate::seccomp::profile_to_filter(profile_id) {
        Some(f) => f,
        None => {
            robot_os_drivers::kprintln!(
                "[SECCOMP] unknown profile {} for task '{}' — failing closed to MINIMAL",
                profile_id, name
            );
            // `PROFILE_MINIMAL` is a known-good id, so the `None` arm is
            // unreachable — but it must not be `unwrap()` (panic = board
            // reset) nor `SyscallFilter::disabled()` (that is the exact
            // silent-unrestricted bug being fixed). Deny-everything is the
            // only fallback that stays fail-closed without panicking.
            crate::seccomp::profile_to_filter(crate::seccomp::PROFILE_MINIMAL)
                .unwrap_or_else(|| {
                    let mut deny_all = crate::task::SyscallFilter::disabled();
                    deny_all.enabled = true;
                    deny_all
                })
        }
    };
    unsafe {
        if idx < MAX_TASKS && TASK_VALID[idx] {
            task_mut(idx).syscall_filter = filter;
        }
    }
    idx
}

/// Called when a task's entry function returns.
///
/// Marks the task as zombie and immediately tries to reschedule. If no tasks
/// are ready, enters a WFI idle loop (timer interrupts will call
/// `schedule()` to pick up future tasks).
///
/// K-C6: deliberately does NOT free the task's pool slot (`TASK_VALID`) or
/// clear `PER_CPU[cpu].current_idx` — this function is still executing ON
/// the exiting task's own stack, so doing either here would let another
/// hart's `alloc_slot()` (e.g. via `fork()`) reuse and dispatch a brand new
/// task onto that same physical stack while this hart is still running on
/// it. `do_schedule()` frees the slot itself, in the same call that
/// `context_switch()`s away from it — see its `old.state == Zombie` arm.
///
/// Hook invoked with the dying task's TID from [`task_exit`].
///
/// `crates/ipc` depends on `crates/sched`, so `sched` cannot call into it
/// directly without a dependency cycle — hence the same registered-callback
/// shape already used for priority inheritance (`pi_set_callbacks`). The
/// kernel registers `robot_os_ipc::handle_revoke_all` here at boot.
///
/// Stored as `AtomicUsize` (pointer-sized), like the PI callbacks.
static TASK_EXIT_HOOK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Register the [`TASK_EXIT_HOOK`]. Call once, during boot.
pub fn set_task_exit_hook(f: fn(u32)) {
    TASK_EXIT_HOOK.store(f as usize, Ordering::Release);
}

/// Never returns.
pub fn task_exit() -> ! {
    unsafe {
        let cpu = current_cpu_id();
        let idx = PER_CPU[cpu].current_idx;

        if idx != usize::MAX {
            // Read the dying task's identity BEFORE anything else touches it.
            // `exit_tid` — not `idx` — is what the driver manager and the
            // exit hook are keyed on: pool slots are recycled (`alloc_slot`
            // reuses the first free index, `do_schedule` frees this one), so
            // a slot number identifies "whoever sits here now", not the task
            // that is dying.
            let (exit_tid, exit_class) = {
                let t = task_mut(idx);
                (t.tid, t.sched_class_raw)
            };

            // AQ2: Notify driver manager — if this task was a registered driver,
            // record the crash so auto-restart can kick in. Keyed on the TID:
            // passing `idx` used to blame the crash on whichever driver last
            // occupied this pool slot.
            crate::driver::driver_on_crash(exit_tid);

            // PHANES Phase 1 W4-int.3b — remove the dying task from
            // its policy runqueue so APS pick_next never sees a Zombie.
            crate::aps_state::dequeue_task_for_class(cpu, exit_tid, exit_class);

            // Release resources keyed by this TID before the slot can be
            // reused. The legacy global handle table stores `owner_task` as a
            // plain field, so a handle left behind by a dead task is
            // inherited wholesale by the next task that draws the same TID.
            // `handle_revoke_all`'s own doc already claimed it was "called on
            // task_exit"; it had zero callers until now.
            //
            // ORDERING — DO NOT MOVE THIS BLOCK (W3-F7). The hook the kernel
            // registers is `robot_os_ipc::task_release_all`, which calls
            // `cap_store::reset`. That resolves the TID back to a pool slot
            // and only succeeds while `TASK_VALID[idx]` is still true. It
            // therefore MUST run before the `Zombie` marking below and long
            // before `do_schedule()` frees the slot. Move it later and typed
            // capabilities stop being revoked on exit — silently, with no
            // error and no failing test.
            {
                let raw = TASK_EXIT_HOOK.load(Ordering::Acquire);
                if raw != 0 {
                    let f: fn(u32) = core::mem::transmute(raw);
                    f(exit_tid);
                }
            }

            // K-C6: mark Zombie only — TASK_VALID stays true and
            // PER_CPU[cpu].current_idx keeps pointing at `idx` (see the
            // function doc above) until do_schedule() can safely free it.
            {
                let _pool = PoolGuard::acquire();
                task_mut(idx).set_state(TaskState::Zombie);
            } // pool lock released
        }

        // PHANES Phase 1 W4-int.4 — clear the APS current_class so
        // subsequent timer ticks don't keep crediting a dead task's
        // class until the next dispatch.
        let _ = crate::aps_state::with_cpu(cpu, |state| {
            state.aps.set_idle();
        });

        // Disable interrupts and try an immediate reschedule.
        robot_os_arch::csr::write_sstatus(
            robot_os_arch::csr::read_sstatus() & !robot_os_arch::csr::SSTATUS_SIE,
        );
        do_schedule(cpu);
        // do_schedule() only returns when there are no ready tasks on this CPU.
    }

    // No tasks remaining — idle until timer brings more work.
    loop {
        let sstatus = robot_os_arch::csr::read_sstatus();
        robot_os_arch::csr::write_sstatus(sstatus | robot_os_arch::csr::SSTATUS_SIE);
        robot_os_arch::cpu::wfi();
        // Timer interrupt → schedule() → do_schedule() → may context-switch away.
        // If not, we just loop again.
    }
}

/// Voluntarily yield the CPU to the next ready task.
pub fn task_yield() {
    let sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(sstatus & !robot_os_arch::csr::SSTATUS_SIE);

    let cpu = current_cpu_id();
    unsafe { do_schedule(cpu); }

    robot_os_arch::csr::write_sstatus(sstatus);
}

/// Microseconds per CLINT tick (10 MHz clock = 0.1 us per tick).
/// 1 us = 10 ticks, so to convert us to ticks: multiply by this value.
const TICKS_PER_US: u64 = 10;

/// PHANES Phase 1 W4-int.4 — microseconds per scheduler tick.
///
/// Matches the 100 Hz default in `crates/drivers/src/clint.rs::SCHED_HZ`
/// (10 ms per tick = 10 000 µs). The APS account path multiplies the
/// `DEADLINE_TICK_COUNTER` by this to get a monotonic-µs proxy without
/// adding a `drivers` dependency to this crate.
const SCHED_TICK_US: u32 = 10_000;

/// Fixed-point scale for deadline admission control (1.0 = this value).
/// Using 10000 gives 0.01% granularity for utilization checks.
const ADMISSION_SCALE: u64 = 10_000;

/// Called from the timer interrupt handler (interrupts already disabled by hardware).
///
/// RT tasks: never preempted by timer — only by a strictly higher-priority ready task.
/// Normal tasks: preempted when time slice expires (standard round-robin).
/// Deadline tasks: decrement remaining budget; replenish on deadline expiry.
#[wcet(30_us)]
pub fn schedule() {
    let cpu = current_cpu_id();
    // F03.4: If preemption is disabled (e.g. spinlock held, critical section),
    // skip the context switch but still let the caller's bookkeeping run.
    if PREEMPT_COUNT[cpu.min(MAX_CPUS - 1)].load(Ordering::Relaxed) > 0 {
        return;
    }
    unsafe {
        let current_idx = PER_CPU[cpu].current_idx;
        // K-C6: a lingering Zombie — task_exit() found no ready task and is
        // idling in its own WFI loop, still on its own stack — must not be
        // treated as "the task currently running" here. Crediting a dead
        // task's runtime/deadline stats is wrong, and worse: the RT/time-
        // slice preemption-avoidance branches below can `return` without
        // ever calling do_schedule(), which would permanently starve this
        // CPU of new dispatches once its last task happened to be
        // RT-priority. Treat it exactly like the genuinely-idle
        // (current_idx == MAX) case; do_schedule() frees the Zombie's slot
        // once it finds a real next task to switch to (see its `old.state
        // == TaskState::Zombie` arm).
        let current_idx = if current_idx != usize::MAX
            && task_mut(current_idx).state() == TaskState::Zombie
        {
            usize::MAX
        } else {
            current_idx
        };
        if current_idx != usize::MAX {
            let task = task_mut(current_idx);
            task.total_runtime += 1;

            // PHANES Phase 1 W4-int.4 — credit the APS class running on
            // this CPU. Drives per-class budget consumption and window
            // roll-over inside `Aps::tick`. Uses DEADLINE_TICK_COUNTER
            // (monotonic, incremented further down) × SCHED_TICK_US as
            // a microsecond proxy — exact enough for budget bookkeeping
            // without needing a `drivers` dep here. The call is cheap
            // (5 SpinLock acquire/release + a few atomic ops).
            let tid_snapshot = task.tid;
            let now_us = DEADLINE_TICK_COUNTER.load(Ordering::Relaxed)
                * SCHED_TICK_US as u64;
            crate::aps_state::account(cpu, now_us, SCHED_TICK_US, tid_snapshot);

            // Deadline task: track budget consumption and period expiry.
            if task.deadline.period_us > 0 {
                if task.deadline.remaining > 0 {
                    task.deadline.remaining -= 1;
                }
                // Check if deadline has expired (overrun).
                // Use total_runtime as a proxy for elapsed ticks since we cannot
                // access the CLINT directly from this crate.
                // The abs_deadline is checked against a monotonic tick counter
                // that is incremented each timer tick.
                let now_ticks = DEADLINE_TICK_COUNTER.load(Ordering::Relaxed);
                if now_ticks > task.deadline.abs_deadline {
                    // Deadline overrun — replenish for next period.
                    let period_ticks = task.deadline.period_us * TICKS_PER_US;
                    let runtime_ticks = task.deadline.runtime_us * TICKS_PER_US;
                    task.deadline.abs_deadline += period_ticks;
                    task.deadline.remaining = runtime_ticks;
                }
                // Always reschedule deadline tasks so EDF picks the earliest.
                DEADLINE_TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
                do_schedule(cpu);
                return;
            }

            if is_rt_priority(task.priority) {
                // RT task: only preempt if a higher-priority task is waiting.
                match cpu_peek_highest_prio(cpu) {
                    Some(ready_prio) if ready_prio < task.priority => {
                        // Higher-priority task ready — preempt.
                    }
                    _ => {
                        DEADLINE_TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
                        return; // No higher-priority task — keep running.
                    }
                }
            } else {
                // Normal task: standard time-slice expiry.
                if task.time_slice > 0 {
                    task.time_slice -= 1;
                    if task.time_slice > 0 {
                        DEADLINE_TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
                        return; // Still has remaining time — don't preempt.
                    }
                }
            }
        }
        DEADLINE_TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
        do_schedule(cpu);
    }
}

/// Monotonic tick counter for deadline scheduling.
/// Incremented every timer tick in `schedule()`.
static DEADLINE_TICK_COUNTER: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Start the scheduler on the calling CPU (boot CPU).
///
/// Picks the first ready task from this CPU's queue and switches to it.
/// Never returns.
pub fn start() -> ! {
    let cpu = current_cpu_id();
    // K-A12: SSTATUS.SIE is already on by the time start() runs (enabled
    // earlier in boot), so a timer tick firing in the window below would
    // enter schedule() → do_schedule() and mutate this same CPU's ready
    // queue / PER_CPU.current_idx concurrently with the dequeue+dispatch
    // this function is doing inline — corrupting the dispatch, or racing
    // `current_idx` between the dequeue and the assignment below. Not
    // restored here: task_entry_wrapper (the entry point of the task we're
    // about to switch to) unconditionally re-enables SIE.
    let sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(sstatus & !robot_os_arch::csr::SSTATUS_SIE);
    unsafe {
        // Locked: another hart may be racing us via `task_create` (least-
        // loaded assignment can still target a CPU that hasn't called
        // `start()` yet) or, in principle, a very early cross-CPU wake.
        let next_idx = match cpu_dequeue_locked(cpu) {
            Some(idx) => idx,
            None => panic!("sched_start: no tasks for CPU {}", cpu),
        };
        let next = task_mut(next_idx);
        next.set_state(TaskState::Running);
        next.time_slice = if is_rt_priority(next.priority) {
            RT_TIME_SLICE_TICKS
        } else {
            TIME_SLICE_TICKS
        };
        PER_CPU[cpu].current_idx = next_idx;

        // Switch to first task (no current task to save).
        context_switch(core::ptr::null_mut(), next as *mut Task);
    }
    unreachable!()
}

// ---- Boot-time offline-hart rescue ----

/// Move every ready task stranded on a hart that failed to start during SMP
/// bring-up onto a hart that actually came up.
///
/// Call exactly once, from the boot hart, strictly *after* `wake_harts()` has
/// returned and [`crate::smp::NUM_ONLINE_CPUS`] has been corrected to the
/// real `online` count, and strictly *before* the boot hart calls
/// [`start()`] (or enables its own timer interrupt). Returns the number of
/// tasks moved.
///
/// # Why this needs no extra synchronization on the *source* side
///
/// `task_create`/`task_create_affinity` calls made earlier in boot (before
/// `wake_harts()`) spread tasks across `0..num_cpus` using the *optimistic*
/// pre-boot `NUM_ONLINE_CPUS` estimate — some may have landed on a hart that
/// then failed `hart_start`. Those per-CPU ready queues
/// (`PER_CPU[online..total]`) are, by construction, never touched by anyone
/// but the boot hart: a hart that failed to start never executes a single
/// instruction, so it can never call `schedule()` / `do_schedule()` or
/// enqueue/dequeue anything on its own queue. That is not a narrow timing
/// window — it holds for as long as the kernel runs.
///
/// # Why the *destination* side still goes through the locked wrappers
///
/// A hart that *did* start runs `smp_secondary_start()` immediately and
/// independently of how far the boot hart has gotten through the rest of
/// `kernel_main` — it enables its own local timer right there and will call
/// `schedule()` on its first tick, which can land before the boot hart
/// reaches `start()`. So, unlike the source side, a lock-free enqueue onto
/// an *alive* CPU's queue here would not be provably race-free without also
/// reasoning about exactly how much boot-time code runs in between. Rather
/// than depend on that, every queue touch below — source and destination —
/// goes through `cpu_dequeue_locked` / `cpu_enqueue_locked` (never the raw
/// `cpu_dequeue`/`cpu_enqueue`), same as every other cross-CPU path in this
/// file. The extra lock cost is negligible: boot-time only, at most
/// `MAX_TASKS` operations total.
///
/// # Algorithm
///
/// For each dead hart `online..total`, repeatedly dequeue its
/// highest-priority ready task (this drains every priority level, via the
/// same bitmap dequeue used everywhere else) and hand it to
/// [`find_best_cpu`] — the same priority-aware balancer `task_create`
/// uses, not a bitmap popcount. It is naturally bounded to
/// `0..online` because the caller has already corrected `NUM_ONLINE_CPUS`
/// before calling this function.
///
/// Both the task's saved `tp` (`context.tp`, restored into the hardware
/// `tp` register by `context_switch` on dispatch — see its field doc) and,
/// if the task was pinned (`cpu_affinity >= 0`), its affinity are rewritten
/// to the new CPU. Skipping this would leave a moved task either running
/// with a stale `current_cpu_id()` (every `PER_CPU[current_cpu_id()]`
/// access inside it would then hit the wrong slot) or, once it blocks and
/// is later woken (`try_wake_task` / `wq_wake_by_tid` route pinned tasks
/// straight back to `cpu_affinity`), silently re-stranded on the dead hart.
/// Breaking an explicit pin is a decision the operator must see, so it is
/// logged per task.
///
/// No work-stealing is added at runtime — this is a one-shot rescue that
/// only ever runs during this boot-time window.
pub fn rebalance_from_offline_cpus(online: usize, total: usize) -> usize {
    let total = total.min(MAX_CPUS);
    if online == 0 || online >= total {
        return 0; // Nothing offline, or nothing online to rescue onto.
    }

    let mut moved = 0usize;
    let mut moved_per_dead = [0usize; MAX_CPUS];

    unsafe {
        for dead_cpu in online..total {
            loop {
                let idx = match cpu_dequeue_locked(dead_cpu) {
                    Some(idx) => idx,
                    None => break, // This dead hart's queue is fully drained.
                };

                let task = task_mut(idx);
                let target_cpu = find_best_cpu(task.priority, idx);

                if task.cpu_affinity >= 0 {
                    // Pinned task stranded on a hart that never came up —
                    // the pin cannot be honored without leaving it stuck
                    // forever, so move it anyway and make that visible.
                    let len = task.name.iter().position(|&b| b == 0).unwrap_or(TASK_NAME_MAX_LEN);
                    let name = core::str::from_utf8(&task.name[..len]).unwrap_or("<?>");
                    robot_os_drivers::kprintln!(
                        "[SMP] task '{}' (tid {}) was pinned to dead hart {} — \
                         reassigning to hart {} (affinity broken: hart never started)",
                        name, task.tid, dead_cpu, target_cpu
                    );
                    task.cpu_affinity = target_cpu as i8;
                }

                // Keep the saved tp consistent with the queue the task now
                // lives on: context_switch loads this straight into the
                // hardware tp register on dispatch, and current_cpu_id()
                // (hence every PER_CPU[current_cpu_id()] access inside the
                // task) trusts it completely.
                task.context.tp = target_cpu as CtxReg;

                cpu_enqueue_locked(target_cpu, idx);
                moved_per_dead[dead_cpu] += 1;
                moved += 1;
            }
        }
    }

    if moved > 0 {
        for dead_cpu in online..total {
            if moved_per_dead[dead_cpu] > 0 {
                robot_os_drivers::kprintln!(
                    "[SMP] rebalanced {} boot task(s) off dead hart {}",
                    moved_per_dead[dead_cpu], dead_cpu
                );
            }
        }
        robot_os_drivers::kprintln!(
            "[SMP] rebalance summary: {} task(s) moved off {} dead hart(s) onto {} online hart(s)",
            moved, total - online, online
        );
    }

    moved
}

/// Find the ready deadline task with the earliest absolute deadline on `cpu`.
///
/// Scans all valid tasks assigned to (or compatible with) `cpu` that are in
/// Ready state and have `period_us > 0` (deadline task) with remaining budget.
/// A task that is `Running` is only accepted if it's this `cpu`'s own current
/// task (self-continuation) — never a task running on a different hart.
/// Returns the task pool index of the winner, or `None`.
///
/// # Safety
/// Caller must ensure no concurrent mutation of TASKS/TASK_VALID (interrupts
/// disabled or pool lock held).
unsafe fn find_earliest_deadline(cpu: usize) -> Option<usize> {
    let mut best_idx: Option<usize> = None;
    let mut best_deadline: u64 = u64::MAX;

    for i in 0..MAX_TASKS {
        if !TASK_VALID[i] { continue; }
        let t = &TASKS[i];
        // Accept Ready tasks, or a task that's Running but only if it's
        // THIS cpu's own current task (self-continuation — see
        // next_idx == old_idx short-circuit below). A task Running on a
        // DIFFERENT hart must never be picked here: that's not a race,
        // it's a deterministic double-dispatch of any unpinned deadline
        // task the instant a second hart schedules.
        let st = t.state();
        let self_running = st == TaskState::Running && PER_CPU[cpu].current_idx == i;
        if st != TaskState::Ready && !self_running { continue; }
        if t.deadline.period_us == 0 { continue; }
        if t.deadline.remaining == 0 { continue; }
        // Check CPU affinity compatibility.
        let ok_cpu = if t.cpu_affinity >= 0 {
            t.cpu_affinity as usize == cpu
        } else {
            true
        };
        if !ok_cpu { continue; }
        if t.deadline.abs_deadline < best_deadline {
            best_deadline = t.deadline.abs_deadline;
            best_idx = Some(i);
        }
    }
    best_idx
}

/// Core scheduling logic: pick next task for `cpu` and context-switch to it.
///
/// Scheduling priority order:
/// 1. Deadline tasks (EDF — earliest absolute deadline first)
/// 2. RT + normal tasks (bitmap-based priority queue)
///
/// # Safety
/// Should be called with interrupts disabled on the calling CPU (all current
/// callers do, or run inside a trap handler where hardware already disabled
/// them) — but every ready-queue touch below goes through the IRQ-safe
/// `cpu_dequeue_locked` / `cpu_enqueue_locked` wrappers regardless, so this
/// function no longer *depends* on that invariant for correctness against
/// `CPU_LOCKS[cpu]`.
/// No `CPU_LOCKS` guard is held across `context_switch()` — each locked
/// helper acquires and releases `CPU_LOCKS[cpu]` internally, before
/// returning, so nothing is held when `context_switch` (which may not
/// return) is reached further down.
unsafe fn do_schedule(cpu: usize) {
    // PHANES Phase 1 W4-int.2 — if APS dispatch is enabled, consult
    // the per-class policies first. On any error (empty policies, tid
    // not in pool) fall back to the legacy bitmap queue so we never
    // wedge the kernel. While SCHED_USE_APS is false (default), the
    // branch is a single atomic load and the legacy path runs.
    let next_idx = if aps_dispatch_enabled() {
        let aps_pick = crate::aps_state::pick_next(cpu, 0)
            .and_then(|meta| idx_for_tid(meta.tid));
        match aps_pick {
            Some(idx) => idx,
            None => match cpu_dequeue_locked(cpu) {
                Some(idx) => idx,
                None => return,
            },
        }
    } else if let Some(dl_idx) = {
        // Scan-and-claim must be atomic across harts: without this lock,
        // two harts racing do_schedule() could both see the same Ready
        // deadline task as the earliest-deadline candidate and both
        // dispatch it (the old comment below was wrong — a bare state
        // write does NOT prevent double-selection without this lock).
        let _guard = DeadlinePickGuard::acquire();
        let picked = find_earliest_deadline(cpu);
        if let Some(idx) = picked {
            // Claim immediately, still under the lock, so the next
            // contender's scan (once it gets the lock) sees Running
            // and skips this task.
            task_mut(idx).set_state(TaskState::Running);
        }
        picked
    } {
        // Phase 1: deadline tasks have absolute priority over RT/normal.
        // Claimed above, under DeadlinePickGuard, so no double-selection.
        dl_idx
    } else {
        // Phase 2: No deadline tasks — use bitmap-based priority queue.
        match cpu_dequeue_locked(cpu) {
            Some(idx) => idx,
            None => return, // No ready tasks on this CPU — caller will idle.
        }
    };

    let old_idx = PER_CPU[cpu].current_idx;

    // Don't switch if we'd switch to ourselves.
    if next_idx == old_idx {
        return;
    }

    // PHANES Phase 1 W4-int.3a — if APS dispatch is active, the
    // picked task was a *peek*; commit by removing it from its
    // policy runqueue. (Mirrors the legacy `cpu_dequeue` which
    // already removed from the bitmap queue.)
    let aps_active = aps_dispatch_enabled();
    if aps_active {
        let next = task_mut(next_idx);
        crate::aps_state::dequeue_task_for_class(
            cpu,
            next.tid,
            next.sched_class_raw,
        );
    }

    // Re-enqueue old task if it is still runnable.
    // Set when the Zombie arm below frees the slot: from that instant the
    // slot is claimable by any hart's task_create, so nothing may write to
    // it anymore — see the `old_ptr` selection at the bottom.
    let mut old_slot_freed = false;
    if old_idx != usize::MAX {
        let old = task_mut(old_idx);
        if old.state() == TaskState::Running {
            // Same protection as block_current(): mark in-transit before
            // this task becomes visible/dispatchable via the ready queue.
            // Gated out under rvv — see the spin-gate comment below for why.
            #[cfg(not(feature = "rvv"))]
            old.context_saving.store(true, Ordering::Relaxed);
            old.set_state(TaskState::Ready);
            cpu_enqueue_locked(cpu, old_idx); // enqueues at old.priority level

            // PHANES Phase 1 W4-int.3a — also re-enqueue into the
            // matching policy so APS can pick it next time. Mirrors
            // the legacy cpu_enqueue above.
            if aps_active {
                crate::aps_state::enqueue_task_for_class(
                    cpu,
                    old.tid,
                    old.sched_class_raw,
                    old.priority.min(255) as u8,
                    old.sched_time_slice_us,
                    old.sched_deadline_us,
                );
            }
        } else if old.state() == TaskState::Zombie {
            // K-C6: `task_exit()` marks the task Zombie but deliberately does
            // NOT free its pool slot (TASK_VALID) or clear
            // PER_CPU[cpu].current_idx itself — at that point it is still
            // executing ON this exact task's own stack (task_exit() calls
            // do_schedule() directly, and if no task was ready yet, idles in
            // its own WFI loop on that same stack). Freeing the slot there
            // would let another hart's alloc_slot() (e.g. via fork()) reuse
            // and dispatch a brand new task onto that same physical stack
            // while this hart is still running on it.
            //
            // This is therefore the correct, and only safe, place to free
            // it: right here, in the SAME do_schedule() call that is about
            // to `context_switch()` away from it below — whether that
            // happens on the very tick task_exit() called us (a ready task
            // was immediately available) or many ticks later (task_exit()
            // idled in WFI until one appeared; schedule() treats a lingering
            // Zombie exactly like "nothing running" in the meantime, see its
            // K-C6 comment, so it keeps calling us every tick until we get
            // here). Either way, by the time this line runs we are
            // unconditionally about to leave `old`'s stack for good via the
            // context_switch() call below — no other hart can have reused
            // this slot in the interim, since TASK_VALID stayed true.
            let _pool = PoolGuard::acquire();
            TASK_VALID[old_idx] = false;
            // Sentinel protocol (see alloc_slot): unpublish, fence, clear the
            // tid, so a dead TID can never be matched against this slot's
            // next occupant by the lock-free `idx_for_tid` scan.
            core::sync::atomic::fence(Ordering::Release);
            old.tid = 0;
            old_slot_freed = true;
        } else if old.state() == TaskState::Blocked {
            // K-C24 tail: a stamp that landed while this task ran past an
            // unswitched block (see `sched_word::wake_transition`) is a
            // DELIVERED wake — parking the task asleep with it would lose
            // it forever, because no waker will fire again for a condition
            // it already announced. Convert stamp → Ready + enqueue as we
            // switch away: `context_saving` is already true (block_current
            // set it), so a hart that dequeues this task spins until our
            // context_switch below finishes the save — the gate's original
            // purpose. CAS loop because K-C24 stampers race this window.
            loop {
                use crate::task::sched_word::{pack, WAKE_STAMP};
                let curw = old.state_word.load(Ordering::Acquire);
                if curw & WAKE_STAMP == 0
                    || crate::task::sched_word::state_of(curw) != TaskState::Blocked
                {
                    break;
                }
                if old.state_word.compare_exchange_weak(
                    curw, pack(TaskState::Ready),
                    Ordering::AcqRel, Ordering::Relaxed,
                ).is_ok() {
                    old.wait_reason = WaitReason::None;
                    cpu_enqueue_locked(cpu, old_idx);
                    break;
                }
            }
        }
    }

    // Activate next task.
    let next = task_mut(next_idx);
    // If `next` is mid-transition (context_saving == true — e.g. this
    // exact task was just preempted/blocked on another hart and its
    // context_switch.S save hasn't finished yet), wait for it. The
    // window is a handful of instructions in the common case; see the
    // fence+store tail of the save path in context_switch.S / the
    // context_saving field doc.
    //
    // Gated out under `--features rvv`: context_switch_rvv.S (QEMU-only
    // vector-extension benchmark path, not used on any real hardware
    // target — JH7110/vf2 has no RVV) never clears context_saving,
    // so it would never drop back to false and this would spin forever.
    // The underlying race this closes is a pre-existing, documented gap
    // for that build only; see context_switch.S for the targets where it's
    // actually fixed.
    #[cfg(not(feature = "rvv"))]
    while next.context_saving.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    next.set_state(TaskState::Running);
    next.time_slice = if is_rt_priority(next.priority) {
        RT_TIME_SLICE_TICKS
    } else {
        TIME_SLICE_TICKS
    };
    PER_CPU[cpu].current_idx = next_idx;

    // PHANES Phase 1 W4-int.4 — tell the APS combinator which class
    // is now running on this CPU so subsequent timer ticks credit the
    // right budget. Always invoked (even when SCHED_USE_APS=false)
    // so the bookkeeping stays warm and a future flip-to-true sees
    // accurate consumption counters.
    if let Some(class) =
        crate::class::SchedClass::from_raw(next.sched_class_raw)
    {
        let _ = crate::aps_state::with_cpu(cpu, |state| {
            state.aps.set_current(class, next.tid);
        });
    }

    // A zombie whose slot was just freed gets NULL, exactly like "no old
    // task": its registers are dead by definition (the task can never run
    // again), and the slot is already claimable — `context_switch`'s save
    // path would write 128 bytes of registers plus the `context_saving`
    // clear into memory a concurrent `task_create` may be initializing.
    // The asm's `beqz a0, restore_new_task` skips the save entirely, which
    // also shrinks the K-C6 tail (this hart still runs on the zombie's
    // stack until the new sp is loaded) from the full save path to a few
    // instructions. With NULL the call never returns to this frame — there
    // is nothing after it, and no live state on this stack to return to.
    let old_ptr = if old_idx != usize::MAX && !old_slot_freed {
        task_mut(old_idx) as *mut Task
    } else {
        // No old task to save: first run, post-task_exit idle hand-off, or
        // a freed zombie slot (see above).
        core::ptr::null_mut()
    };

    // Update PI mutex identity so priority inheritance knows who we are.
    robot_os_sync::pi_mutex::CURRENT_TID.store(next.tid, core::sync::atomic::Ordering::Release);
    robot_os_sync::pi_mutex::CURRENT_PRIO.store(next.priority, core::sync::atomic::Ordering::Release);

    context_switch(old_ptr, next as *mut Task);
    // Returns here when the old task is rescheduled.
}

// ---- Deadline scheduling (AQ7) ----

/// Configure deadline scheduling for a task.
///
/// `period_us` and `runtime_us` > 0 enables deadline (EDF) mode.
/// The task will be scheduled with absolute priority over RT and normal tasks.
/// Uses admission control: rejects if total utilization would exceed 100%.
pub fn task_set_deadline(idx: usize, period_us: u64, runtime_us: u64) {
    if idx >= MAX_TASKS || period_us == 0 || runtime_us == 0 || runtime_us > period_us {
        return;
    }
    if !deadline_admission_check(period_us, runtime_us) {
        return; // Would exceed total bandwidth — reject.
    }
    unsafe {
        let _pool = PoolGuard::acquire();
        if !TASK_VALID[idx] { return; }
        let task = task_mut(idx);
        let now_ticks = DEADLINE_TICK_COUNTER.load(Ordering::Relaxed);
        let period_ticks = period_us * TICKS_PER_US;
        let runtime_ticks = runtime_us * TICKS_PER_US;
        task.deadline = DeadlineParams {
            period_us,
            runtime_us,
            abs_deadline: now_ticks + period_ticks,
            remaining: runtime_ticks,
        };
    }
}

/// Check if adding a deadline task would exceed total bandwidth.
///
/// Returns `true` if the task can be admitted (total utilization < 100%).
/// Uses fixed-point integer math (scale = `ADMISSION_SCALE`) to avoid floats.
pub fn deadline_admission_check(period_us: u64, runtime_us: u64) -> bool {
    if period_us == 0 { return false; }
    let mut total_util: u64 = 0;
    unsafe {
        for i in 0..MAX_TASKS {
            if !TASK_VALID[i] { continue; }
            let dl = &TASKS[i].deadline;
            if dl.period_us == 0 { continue; }
            // Utilization = runtime / period, scaled by ADMISSION_SCALE.
            total_util += (dl.runtime_us * ADMISSION_SCALE) / dl.period_us;
        }
    }
    // Add the candidate task's utilization.
    let candidate_util = (runtime_us * ADMISSION_SCALE) / period_us;
    total_util + candidate_util <= ADMISSION_SCALE
}

// ---- Query functions ----

/// Returns the name of the currently running task on this CPU.
pub fn current_task_name() -> &'static str {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX {
            return "<none>";
        }
        let task = &TASKS[idx];
        let len = task.name.iter().position(|&b| b == 0).unwrap_or(TASK_NAME_MAX_LEN);
        core::str::from_utf8(&task.name[..len]).unwrap_or("<?>")
    }
}

/// Returns the TID of the currently running task (0 if none).
pub fn current_task_tid() -> u32 {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { 0 } else { TASKS[idx].tid }
    }
}

/// Returns the clean (pre-prologue, ABI-aligned) stack top of the task running
/// on this CPU, or 0 if none.
///
/// Used by the I-13 transactional control-tick restart (RFC-0029): on a
/// recoverable fault the trap handler resets SP to this known-good base before
/// re-entering the control task, rather than to a mid-function SP — so the
/// entry prologue runs exactly once per restart and the stack does not descend
/// one frame on every rollback.
pub fn current_task_stack_top() -> usize {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX {
            return 0;
        }
        task_stack_top(TASKS[idx].stack_idx)
    }
}

/// Returns total runtime ticks of the currently running task.
pub fn current_runtime() -> u64 {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { 0 } else { TASKS[idx].total_runtime }
    }
}

/// Returns the user page-table physical address for the current task
/// (0 = kernel task / no user address space).
pub fn current_user_pt() -> usize {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { 0 } else { TASKS[idx].user_pt as usize }
    }
}

/// Update the task_satp, user_pt and user_brk of the current task.
/// Called by exec_user after a new user page table has been built.
pub fn set_current_user_info(task_satp: u64, user_pt: u64, brk: u64) {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx != usize::MAX {
            TASKS[idx].task_satp = task_satp;
            TASKS[idx].user_pt   = user_pt;
            TASKS[idx].user_brk  = brk;
        }
    }
}

/// Read + update user_brk for the current task (sys_brk).
/// Returns new brk value, or old brk if addr == 0.
pub fn update_user_brk(addr: u64) -> u64 {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { return 0; }
        if addr == 0 {
            TASKS[idx].user_brk
        } else {
            TASKS[idx].user_brk = addr;
            addr
        }
    }
}

/// Set user info for a specific task by pool index (used by fork).
pub fn set_task_user_info(idx: usize, task_satp: u64, user_pt: u64, brk: u64) {
    unsafe {
        if idx < MAX_TASKS && TASK_VALID[idx] {
            TASKS[idx].task_satp = task_satp;
            TASKS[idx].user_pt   = user_pt;
            TASKS[idx].user_brk  = brk;
        }
    }
}

/// K-A15: publish the fork hand-off context on the *child's* own task slot
/// (by pool index — unambiguous even with multiple concurrent forks).
/// Called once by the parent, as the last step of `sys_fork_impl()`. Writes
/// the payload fields first, then the `Release`-ordered ready flag — see
/// the `fork_ctx_ready` doc on `Task` for why the ordering matters.
///
/// `expected_tid` guards against slot reuse: a bare `TASK_VALID[idx]` check
/// would let a delayed parent publish another process's entry/user_sp/satp
/// onto whatever task now occupies the slot (if that task is itself a fork
/// child, it would SRET into the wrong address space). The check + payload
/// write happen under `POOL_LOCK` so they cannot interleave with
/// `do_schedule()` freeing the slot / `alloc_slot()` reusing it. Returns
/// `false` if the slot no longer belongs to `expected_tid` (i.e. the child
/// is gone) — nothing is written in that case.
pub fn set_task_fork_ctx(
    idx: usize,
    expected_tid: u32,
    entry: u64,
    user_sp: u64,
    satp: u64,
    regs: &[u64; 32],
) -> bool {
    if idx >= MAX_TASKS {
        return false;
    }
    unsafe {
        let _pool = PoolGuard::acquire();
        if !TASK_VALID[idx] || TASKS[idx].tid != expected_tid {
            return false;
        }
        let task = task_mut(idx);
        task.fork_entry   = entry;
        task.fork_user_sp = user_sp;
        task.fork_satp    = satp;
        // K-C11: the parent's full user register file. Written before the
        // publish flag below, like the other three payload fields — the child
        // reads all of it under the same Acquire/Release pairing.
        task.fork_regs    = *regs;
        task.fork_ctx_ready.store(true, Ordering::Release);
        true
    }
}

/// K-A15: consume (read-and-clear) the fork hand-off context for whichever
/// task is currently running on this CPU. Called only by `fork_child_entry`,
/// about itself — `Acquire` pairs with the `Release` in
/// [`set_task_fork_ctx`] so once this observes the flag set, the payload
/// fields it reads are guaranteed to be the parent's finished writes.
/// Returns `None` if the parent hasn't published it yet (caller retries).
///
/// **K-C11: the register file is returned by value, on purpose.** It lands on
/// the caller's (kernel) stack, and `fork_child_entry` hands the asm a pointer
/// to *that* copy rather than to `TASKS[idx].fork_regs`. The restore sequence
/// runs after `csrw satp` has already switched to the child's page table, and
/// while `copy_kernel_entries_to_user` does splice the kernel's entries into
/// every user PT, "the kernel's `.bss` is reachable through the child's PT" is
/// an assumption this code would then depend on silently — and getting it wrong
/// means an S-mode fault under `panic = "abort"`, i.e. a board reset on every
/// single fork. The kernel *stack* is unambiguously reachable after the switch;
/// the pre-existing `csrw sscratch, sp` already relies on exactly that. Copying
/// costs 256 bytes of stack on a path that runs once per fork.
pub fn take_current_task_fork_ctx() -> Option<(u64, u64, u64, [u64; 32])> {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { return None; }
        let task = task_mut(idx);
        if !task.fork_ctx_ready.swap(false, Ordering::Acquire) {
            return None;
        }
        Some((task.fork_entry, task.fork_user_sp, task.fork_satp, task.fork_regs))
    }
}

/// K-C21: publish the exec hand-off on the CURRENT task's own slot. Called
/// by `exec_user()` as its last step. Payload fields first, `Release`-ordered
/// ready flag last — same publish protocol as [`set_task_fork_ctx`].
///
/// No pool lock and no identity check, deliberately: unlike fork this never
/// writes across tasks — the writer IS the current task, its slot cannot be
/// freed or reused while it is still executing this function, and the only
/// reader is the same task later in the same syscall/trap. See the
/// `exec_ctx_ready` doc on `Task`.
pub(crate) fn set_current_task_exec_slots(
    entry: u64,
    user_sp: u64,
    sstatus: u64,
    satp: u64,
    old_pt: u64,
) {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { return; }
        let task = task_mut(idx);
        task.exec_entry   = entry;
        task.exec_user_sp = user_sp;
        task.exec_sstatus = sstatus;
        task.exec_satp    = satp;
        task.exec_old_pt  = old_pt;
        task.exec_ctx_ready.store(true, Ordering::Release);
    }
}

/// K-C21: consume (read-and-clear) the exec hand-off of the task currently
/// running on this CPU. Returns `(entry, user_sp, sstatus, satp, old_pt)`.
/// The `Acquire` swap pairs with the `Release` in
/// [`set_current_task_exec_slots`] — on this design it is belt-and-braces
/// (same task, same hart), but it keeps the protocol identical to fork's and
/// costs nothing. Callers must go through
/// `process::take_current_task_exec_ctx`, which owns the satp-switch /
/// destroy-old ordering (K-C22).
pub(crate) fn take_current_task_exec_slots() -> Option<(u64, u64, u64, u64, u64)> {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { return None; }
        let task = task_mut(idx);
        if !task.exec_ctx_ready.swap(false, Ordering::Acquire) {
            return None;
        }
        Some((
            task.exec_entry,
            task.exec_user_sp,
            task.exec_sstatus,
            task.exec_satp,
            task.exec_old_pt,
        ))
    }
}

/// Boost a task's priority by TID (for priority inheritance).
/// Only boosts if `new_prio` is higher (lower number) than current priority.
pub fn pi_boost_task(tid: u32, new_prio: u32) {
    unsafe {
        for i in 0..MAX_TASKS {
            if TASK_VALID[i] && TASKS[i].tid == tid {
                // Counted, like the lease path. This is safe only because
                // PiMutex's protocol is now edge-triggered: each waiter donates
                // at most once per acquisition and `PiMutex::release()` issues
                // exactly one restore per donation. It used to re-assert the
                // boost on a timer while spinning, which forced this function
                // to stay idempotent (uncounted) — and uncounted donations were
                // precisely why two contended PiMutexes could not compose. If
                // anyone reintroduces re-assertion, this counter drifts upward
                // without bound and the owner never returns to base priority.
                TASKS[i].donation_count.fetch_add(1, Ordering::Relaxed);
                if new_prio < TASKS[i].priority {
                    TASKS[i].priority = new_prio;
                }
                break;
            }
        }
    }
}

/// Restore a task's original priority by TID (after PI mutex release).
/// Uses `base_priority` from the task struct instead of the parameter for robustness.
pub fn pi_restore_task(tid: u32, _orig_prio: u32) {
    unsafe {
        for i in 0..MAX_TASKS {
            if TASK_VALID[i] && TASKS[i].tid == tid {
                // Symmetric with `pi_boost_task`: drop one donation and go
                // back to base only when the LAST one leaves. Both donation
                // sources (PiMutex here, leases via
                // `boost_ready_task`/`restore_ready_task`) now share this
                // counter, which is what makes them compose — neither can
                // clobber a boost the other still needs.
                let remaining = TASKS[i].donation_count
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| Some(c.saturating_sub(1)))
                    .unwrap_or(1)
                    .saturating_sub(1);
                if remaining == 0 {
                    TASKS[i].priority = TASKS[i].base_priority;
                }
                break;
            }
        }
    }
}

/// Remove a specific task `idx` from its current ready bucket, if present.
/// O(bucket length). Returns `true` if it was found and removed.
unsafe fn cpu_remove(cpu: usize, idx: usize) -> bool {
    let prio = prio_bucket(task_mut(idx).priority);
    let q = &mut PER_CPU[cpu].ready_queues[prio];
    // Clamped: `tmp` below holds MAX_TASKS entries, so a `count` that ever
    // exceeded the ring would index it out of bounds — a panic, i.e. a board
    // reset. The K-C12 `queued` invariant makes that unreachable; the clamp
    // is what keeps "unreachable" from meaning "resets the robot".
    let n = q.count.min(MAX_TASKS);
    let mut tmp = [0usize; MAX_TASKS];
    let mut k = 0;
    let mut found = false;
    for j in 0..n {
        let e = q.buf[(q.head + j) % MAX_TASKS];
        if e == idx && !found {
            found = true;
            continue;
        }
        tmp[k] = e;
        k += 1;
    }
    if found {
        for j in 0..k {
            q.buf[j] = tmp[j];
        }
        q.head = 0;
        q.tail = k % MAX_TASKS;
        q.count = k;
        if k == 0 {
            PER_CPU[cpu].ready_bitmap &= !(1 << prio);
        }
        // K-C12: the task no longer occupies a queue slot, so release the
        // invariant — otherwise the re-enqueue at its new priority (the whole
        // point of this call) is refused as a duplicate and the task is lost.
        task_mut(idx).queued.store(false, Ordering::Release);
    }
    found
}

/// Boost a **ready** task's priority and re-position it in the ready queue.
///
/// Unlike [`pi_boost_task`] — which only writes the `priority` field, correct
/// only for a running/blocked task that will be (re-)enqueued later — this also
/// moves a task that is ALREADY in the bitmap ready queue into its new
/// priority bucket. The legacy bitmap scheduler buckets tasks by priority at
/// enqueue time, so a field write alone leaves a queued task in its old bucket.
///
/// Needed for lease/capability priority inheritance (RFC-0031, experiment I3),
/// where the resource holder is a ready task being starved by mid-priority
/// work. No-op if `new_prio` is not an increase or the task is not ready.
///
/// KNOWN ISSUE (unverified, gated off by default — not addressed by the
/// CPU_LOCKS hardening applied to `cpu_dequeue`/`cpu_enqueue` elsewhere in
/// this file): this operates on `current_cpu_id()` — the *caller's* CPU —
/// not the target task's actual owning CPU, so on a system where the caller
/// and the boosted task's queue differ it silently touches the wrong ready
/// queue (`cpu_remove`/`cpu_enqueue` below are also unlocked). Fixing the
/// locking without first fixing the CPU-targeting would be misleading, since
/// callers on the wrong CPU would still corrupt data under a lock that
/// doesn't protect the queue they actually intended to touch. Left as-is;
/// needs its own fix (locate the task's real CPU, e.g. via its affinity /
/// `PER_CPU[owner].current_idx`, then route through
/// `cpu_dequeue_locked`/`cpu_enqueue_locked` on that CPU) before enabling.
pub fn boost_ready_task(tid: u32, new_prio: u32) {
    unsafe {
        let cpu = current_cpu_id();
        if let Some(idx) = idx_for_tid(tid) {
            // Count the donation even when it does not lower the priority (the
            // task may already sit at or above `new_prio` thanks to another
            // donor). The matching `restore_ready_task` decrements
            // unconditionally, so the pair must balance or the count drifts and
            // the task never returns to its base priority.
            task_mut(idx).donation_count.fetch_add(1, Ordering::Relaxed);

            if new_prio < task_mut(idx).priority {
                // Only a Ready task sits in a run queue keyed by priority, so
                // only that case needs the remove/re-enqueue dance. Blocked and
                // Running tasks still need the field written: a blocked lessee
                // must wake at the donated priority, otherwise donation silently
                // does nothing in the most common case (lessee waiting on I/O).
                let removed = if task_mut(idx).state() == TaskState::Ready {
                    cpu_remove(cpu, idx)
                } else {
                    false
                };
                task_mut(idx).priority = new_prio;
                if removed {
                    cpu_enqueue(cpu, idx);
                }
            }
        }
    }
}

/// Current priority of a task by TID, or `None` if no such task.
/// Diagnostic census of the task pool: `(ready, blocked, running)`, plus how
/// many `Ready` tasks sit on each CPU's queues.
///
/// **WHY this exists.** When a fast-IPC exchange wedges with the reply already
/// deposited in its slot, there are two very different explanations and the
/// logs cannot tell them apart: the client is `Blocked` and its wake was lost,
/// or the client is `Ready` and never gets picked. The second is starvation —
/// the residual K-C12 explicitly left open, since `find_best_cpu` is placement,
/// not an anti-starvation guarantee. One is a synchronisation bug, the other a
/// scheduling policy gap, and they need opposite fixes.
///
/// Unsynchronised on purpose: this is a diagnostic, it must not perturb the
/// race it is measuring, and an approximate count is enough to tell `Ready`
/// from `Blocked`.
/// Identify every task blocked on fast IPC: `(tid, is_client, payload,
/// state_word_raw, context_saving)`, where `payload` is the slot index for a
/// client and the server's own TID for a server. Returns how many were filled.
///
/// `state_word_raw` and `context_saving` are the two halves of the K-C24 wake
/// gate, read verbatim: bit 3 of the raw word is `WAKE_STAMP`, and a task
/// showing `Blocked` + `context_saving == true` for longer than a switch takes
/// is exactly the "wakes can only stamp, nobody sweeps" wedge this diagnostic
/// exists to catch — see `sched_word::wake_transition`'s `!saved` arm.
///
/// The other half of [`crate::scheduler::task_census`]'s story — see
/// `fast_ipc_slot_ids` for why the identities, not the counts, are what decide
/// between a lost wake and a coincidence.
pub fn blocked_fastipc_ids(out: &mut [(u32, bool, u32, u32, bool)]) -> usize {
    let mut n = 0usize;
    unsafe {
        for i in 0..MAX_TASKS {
            if n >= out.len() { break; }
            if !TASK_VALID[i] { continue; }
            let t = task_ref(i);
            // Acquire: seeing Blocked must make the published wait_reason
            // visible (K-C17 pairing, now via the Release commit CAS).
            if t.state_acquire() != TaskState::Blocked { continue; }
            let word = t.state_word.load(Ordering::Acquire);
            let saving = t.context_saving.load(Ordering::Acquire);
            match t.wait_reason {
                // The low 6 bits of the handle are the slot index by the
                // encoding contract in crates/ipc/src/fast_ipc.rs
                // (FAST_IPC_SLOT_BITS = 6) — this diagnostic reports the seat
                // so it can be correlated with the slot table; duplicating
                // the mask here is display-only and cannot corrupt anything.
                WaitReason::FastIpcClient(handle) => {
                    out[n] = (t.tid, true, (handle & 0x3F) as u32, word, saving);
                    n += 1;
                }
                WaitReason::FastIpcServer(tid)  => {
                    out[n] = (t.tid, false, tid, word, saving);
                    n += 1;
                }
                _ => {}
            }
        }
    }
    n
}

pub fn task_census() -> (u32, u32, u32, [u32; MAX_CPUS], u32, u32, [u32; 5]) {
    let mut ready = 0u32;
    let mut blocked = 0u32;
    let mut running = 0u32;
    let mut per_cpu = [0u32; MAX_CPUS];
    // Two states that must never occur, and that are exactly the failure modes
    // the `Task::queued` invariant can produce if it ever gets out of step:
    //
    //  * `Ready` with `queued == false` — the task is runnable and sits in no
    //    queue at all. Nothing will ever pick it. Lost.
    //  * `Blocked` with `queued == true` — a stale claim. `cpu_enqueue` will
    //    answer `AlreadyQueued` to the next wake and silently drop it, leaving
    //    the task asleep on a condition that has already been satisfied.
    //
    // Both are counted rather than asserted: this runs from a diagnostic task,
    // and a panic here would reset the board over a reporting bug.
    let mut ready_unqueued = 0u32;
    let mut blocked_queued = 0u32;
    // [FastIpcClient, FastIpcServer, Timer, WaitQueue, otros]
    let mut by_reason = [0u32; 5];
    unsafe {
        for i in 0..MAX_TASKS {
            if !TASK_VALID[i] { continue; }
            let t = task_ref(i);
            match t.state_acquire() {
                TaskState::Ready => {
                    ready += 1;
                    if !t.queued.load(Ordering::Acquire) { ready_unqueued += 1; }
                    let c = if t.cpu_affinity >= 0 {
                        (t.cpu_affinity as usize).min(MAX_CPUS - 1)
                    } else {
                        (t.context.tp as usize).min(MAX_CPUS - 1)
                    };
                    per_cpu[c] += 1;
                }
                TaskState::Blocked => {
                    blocked += 1;
                    if t.queued.load(Ordering::Acquire) { blocked_queued += 1; }
                    // Which wait is holding them matters more than the count:
                    // a client asleep on `FastIpcClient` with its reply already
                    // deposited is a lost wake; asleep on anything else means
                    // the model of the failure is wrong.
                    match t.wait_reason {
                        WaitReason::FastIpcClient(_) => by_reason[0] += 1,
                        WaitReason::FastIpcServer(_) => by_reason[1] += 1,
                        WaitReason::Timer(_)         => by_reason[2] += 1,
                        WaitReason::WaitQueue        => by_reason[3] += 1,
                        _                            => by_reason[4] += 1,
                    }
                }
                TaskState::Running => running += 1,
                _ => {}
            }
        }
    }
    (ready, blocked, running, per_cpu, ready_unqueued, blocked_queued, by_reason)
}

/// Diagnostic twin of [`task_census`]'s `ready_unqueued` counter: WHO is in
/// the impossible state, not just how many. Fills `(tid, priority, home_cpu,
/// name[0..8])` per victim; unsynchronised like the census, for the same
/// reason (must not perturb the race it measures).
pub fn ready_unqueued_ids(out: &mut [(u32, u32, u32, [u8; 8])]) -> usize {
    let mut n = 0usize;
    unsafe {
        for i in 0..MAX_TASKS {
            if n >= out.len() { break; }
            if !TASK_VALID[i] { continue; }
            let t = task_ref(i);
            if t.state() != TaskState::Ready { continue; }
            if t.queued.load(Ordering::Acquire) { continue; }
            let home = if t.cpu_affinity >= 0 {
                t.cpu_affinity as u32
            } else {
                t.context.tp as u32
            };
            let mut name = [0u8; 8];
            for (d, s) in name.iter_mut().zip(t.name.iter()) { *d = *s; }
            out[n] = (t.tid, t.priority, home, name);
            n += 1;
        }
    }
    n
}

pub fn task_priority(tid: u32) -> Option<u32> {
    unsafe { idx_for_tid(tid).map(|i| task_mut(i).priority) }
}

/// Restore a (possibly ready) task's priority to `prio` and re-position it in
/// the ready queue if needed. Counterpart of [`boost_ready_task`] used to undo
/// an inherited boost (RFC-0031). Unlike [`pi_restore_task`] (field-only), this
/// re-buckets a task that is still sitting in the ready queue.
pub fn restore_ready_task(tid: u32) {
    unsafe {
        let cpu = current_cpu_id();
        if let Some(idx) = idx_for_tid(tid) {
            // Saturating: an unbalanced restore (donor exited without a matching
            // boost) must not wrap to u32::MAX and pin the task at the donated
            // priority forever.
            let remaining = task_mut(idx).donation_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| Some(c.saturating_sub(1)))
                .unwrap_or(1)
                .saturating_sub(1);

            // Another donor is still waiting on this task — dropping the
            // priority now would reopen the inversion that donor is paying to
            // avoid. Stay boosted until the last one leaves.
            if remaining > 0 {
                return;
            }

            let base = task_mut(idx).base_priority;
            if task_mut(idx).priority == base {
                return;
            }
            let removed = if task_mut(idx).state() == TaskState::Ready {
                cpu_remove(cpu, idx)
            } else {
                false
            };
            task_mut(idx).priority = base;
            if removed {
                cpu_enqueue(cpu, idx);
            }
        }
    }
}

/// Whether guard pages have been set up (after vmm paging is enabled).
static GUARD_PAGES_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set up guard pages for all task stacks.
///
/// Unmaps the bottom page (4 KiB) of each stack slot so that stack overflow
/// triggers an immediate page fault instead of silently corrupting adjacent
/// stacks.  Must be called AFTER `vmm::enable_paging()`.
///
/// After this call, the effective usable stack per task is `STACK_SIZE - 4096`.
/// Stack canary checks are skipped when guard pages are active (the page fault
/// is a stronger guarantee than polling).
#[cfg(not(feature = "no-mmu"))]
pub fn setup_stack_guard_pages() {
    let kpt = robot_os_mm::vmm::kernel_pagetable();
    for i in 0..MAX_TASKS {
        let stack_bottom = unsafe { TASK_STACKS.0[i].as_ptr() as usize };
        robot_os_mm::vmm::unmap(kpt, stack_bottom);
    }
    GUARD_PAGES_ACTIVE.store(true, Ordering::Release);
}

/// Check stack canaries for all currently-valid task slots.
///
/// Returns `(intact, total)`:
/// - `intact` — slots where `STACK_CANARY` is still at the stack bottom.
/// - `total`  — number of valid slots inspected.
///
/// Called by the system watchdog task every ~1 s (Phase 16).
/// When guard pages are active, skips the check (page fault is stronger).
pub fn stack_canary_check() -> (usize, usize) {
    if GUARD_PAGES_ACTIVE.load(Ordering::Acquire) {
        // Guard pages active — overflow triggers immediate page fault.
        // Return (total, total) to indicate "all OK" to watchdog.
        let mut total = 0usize;
        unsafe {
            for i in 0..MAX_TASKS {
                if TASK_VALID[i] { total += 1; }
            }
        }
        return (total, total);
    }
    let mut ok    = 0usize;
    let mut total = 0usize;
    unsafe {
        for i in 0..MAX_TASKS {
            if TASK_VALID[i] {
                total += 1;
                let ptr = TASK_STACKS.0[i].as_ptr() as *const u64;
                if ptr.read_volatile() == STACK_CANARY {
                    ok += 1;
                }
            }
        }
    }
    (ok, total)
}

// ---- AQ11: Syscall filter accessor ----

/// Get the syscall filter of the current task.
pub fn current_syscall_filter() -> SyscallFilter {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { return SyscallFilter::disabled(); }
        TASKS[idx].syscall_filter
    }
}

/// Set the syscall filter for the current task.
pub fn set_current_syscall_filter(filter: SyscallFilter) {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx != usize::MAX {
            TASKS[idx].syscall_filter = filter;
        }
    }
}

/// Set the syscall filter for a specific task by pool index (fork inheritance).
pub fn set_task_syscall_filter(idx: usize, filter: SyscallFilter) {
    unsafe {
        if idx < MAX_TASKS && TASK_VALID[idx] {
            TASKS[idx].syscall_filter = filter;
        }
    }
}

// ---- AQ0: Block / Wake API (used by wait.rs) ----

/// Block the current task on `cpu` with the given reason.
/// Moves it from Running → Blocked, then reschedules.
pub fn block_current(cpu: usize, reason: WaitReason) {
    if cpu >= MAX_CPUS { return; }
    // K-A12: do_schedule() is not safe to reenter — a timer tick firing on
    // this hart mid-call would race this call's own queue/PER_CPU mutations
    // (the outer call's dequeued `next_idx` and this task's Blocked state get
    // corrupted by a nested dispatch). This is the exact hazard task_yield()
    // already guards against; block_current() was missing the same guard.
    // Disable SIE before do_schedule() and restore it only after it returns
    // (i.e. once this task has been rescheduled back in) — same pattern as
    // task_yield(), and both the "have a task to block" and early-return
    // paths below converge on the same restore.
    let sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(sstatus & !robot_os_arch::csr::SSTATUS_SIE);
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        // `< MAX_TASKS`, not `!= usize::MAX`: the scheduler only ever writes
        // in-range indices or MAX here, so the two are equivalent — but only
        // this form lets LLVM prove the `TASKS[idx]` index below in range.
        // With the sentinel comparison it emitted a `panic_bounds_check`
        // call, i.e. a board reset under `panic = "abort"` sitting on the
        // block path. Same provable-guard trick as `lease_tick`'s
        // `count < MAX_LEASES`.
        if idx < MAX_TASKS {
            let task = task_mut(idx);

            // Mark in-transit BEFORE the state change that makes this task
            // visible to wakers — closes the race where a waker sees Blocked
            // and redispatches before context_switch.S has actually saved
            // this task's registers (the save tail in context_switch.S clears
            // it). Gated out under rvv — see the spin-gate comment in
            // do_schedule() for why.
            #[cfg(not(feature = "rvv"))]
            task.context_saving.store(true, Ordering::Relaxed);

            // K-C17: publish the REASON before the state. A waker only reads
            // `wait_reason` after observing `Blocked` with Acquire, and
            // `Blocked` is only ever published by the Release CAS inside
            // `commit_blocked_or_consume_wake` below — that pairing is what
            // guarantees the reason a waker reads is this one, not the stale
            // value from when we were running (the mismatch path drops the
            // wake without stamping, so a torn read here was a permanent
            // sleep). The old explicit fence pair moved into the CAS
            // orderings.
            task.wait_reason = reason;

            // K-C9 + K-C19 — one conditional CAS replaces "consume the stamp,
            // then mark Blocked" (two independently-ordered cells, a measured
            // ~1-in-3 hang):
            //
            //  * If a wake is already stamped, the commit fails, the stamp is
            //    consumed, and we skip blocking entirely — the condition this
            //    caller is about to sleep on is already satisfied, and every
            //    caller re-checks its condition in a loop (see e.g.
            //    `lease_wait_return`), so returning is exactly "wait()
            //    returned because we were woken".
            //  * A waker can no longer stamp *after* we passed the check: its
            //    stamp CAS requires `state != Blocked`, our commit CAS
            //    requires the stamp clear. Whoever loses the CAS retries
            //    against the other's result. The double-check handshake that
            //    was tried and REVERTED (it could enqueue a task that was
            //    still "current", freezing boot) is unnecessary under this
            //    scheme — see `sched_word` in task.rs for the full history.
            if !crate::task::sched_word::commit_blocked_or_consume_wake(&task.state_word) {
                // We never became Blocked, so no waker can be dispatching
                // us — undo the in-transit mark and return to the caller's
                // condition re-check.
                #[cfg(not(feature = "rvv"))]
                task.context_saving.store(false, Ordering::Relaxed);
                robot_os_arch::csr::write_sstatus(sstatus);
                return;
            }

            // Don't re-enqueue — blocked tasks leave the ready queue.
            do_schedule(cpu);
            // Returns here when woken and rescheduled.
        }
    }
    robot_os_arch::csr::write_sstatus(sstatus);
}

/// Try to wake task `idx` if it matches the predicate.
/// Called from wait.rs wake_matching() — reachable from IRQ context (the
/// timer ISR and external-IRQ path in `kernel/src/main.rs` call
/// `wake_expired_timers()` / `wake_by_irq()`, which fan out here), and the
/// target CPU is frequently *not* the calling CPU (affinity-pinned task, or
/// the `0` fallback for unpinned tasks). Goes through `cpu_enqueue_locked` —
/// IRQ-safe and takes `CPU_LOCKS[target_cpu]`, so it can't race the target
/// CPU's own `do_schedule()`.
/// Which CPU should a task being woken be enqueued on?
///
/// **WHY this is not just `0` (K-C14).** All three wake paths
/// (`try_wake_task`, `wq_wake_by_tid`, `wake_task_by_tid`) used to send every
/// unpinned task to **CPU 0**, unconditionally. `find_least_loaded_cpu()` had
/// existed in this file all along and none of them called it.
///
/// The result is not "slightly unbalanced": it is starvation with a
/// reproducible victim. `cpu_dequeue` picks by `ready_bitmap.trailing_zeros()`,
/// i.e. strictly by priority, and on this kernel's default boot CPU 0 is where
/// the RT tasks live (`rt-motor` and `flight-ctrl` are both created with
/// affinity to hart 0). So every unpinned task that ever blocks and is woken —
/// which is every userspace task doing IPC, I/O or sleeping — is permanently
/// relocated behind two real-time tasks on a single hart, no matter how idle
/// the other three are.
///
/// Measured: a ring-3 fast-IPC round trip (`userspace/ipctest` phase A, 8
/// forked clients under `-smp 4`) completes exactly as many exchanges as there
/// were clients still sitting on their post-fork CPUs — four — and then wedges
/// forever. The kernel stays healthy throughout, RT tasks keep meeting their
/// deadlines, and nothing reports an error: the clients are `Ready`, correctly
/// enqueued, and simply never picked. That is why this reads as a lost wakeup
/// and is not one.
///
/// Pinned tasks (`cpu_affinity >= 0`) keep going exactly where they are
/// pinned; the operator asked for that.
///
/// `idx` is the waking task's own pool slot, so it does not score against its
/// own placement.
#[inline]
unsafe fn wake_target_cpu(idx: usize, task: &Task) -> usize {
    if task.cpu_affinity >= 0 {
        (task.cpu_affinity as usize).min(MAX_CPUS - 1)
    } else {
        // K-C12: "emptiest hart" is not the same question as "hart that will
        // dispatch this task". A woken task parked behind permanently
        // higher-priority work is a lost task, not a slow one — the same
        // defect K-C14 half-fixed by moving off the hardcoded CPU 0.
        // Approximate and unlocked, like the metric it replaces: a stale
        // sample costs one suboptimal placement, never correctness.
        find_best_cpu(task.priority, idx).min(MAX_CPUS - 1)
    }
}

pub fn try_wake_task(idx: usize, pred: &dyn Fn(&WaitReason) -> bool) {
    use crate::task::sched_word::{wake_transition, WakeTransition};
    if idx >= MAX_TASKS { return; }
    unsafe {
        if !TASK_VALID[idx] { return; }
        let task = task_mut(idx);
        // K-C19: the Blocked→Ready transition is a CAS — exactly one waker
        // can win it, and the Acquire/Release pairing inside guarantees the
        // `wait_reason` the predicate reads is the one the blocker published
        // (K-C17). `stamp_if_unblocked = false`: this is the broadcast path;
        // a sweep cannot tell its addressee from any other task about to
        // sleep, so it must never stamp (see wait.rs).
        // K-C24: `saved` gates dispatch — a Blocked task whose context is
        // not yet saved is still RUNNING (unswitched block) and must be
        // stamped, never enqueued. See `sched_word::wake_transition`.
        #[cfg(not(feature = "rvv"))]
        let saved = !task.context_saving.load(Ordering::Acquire);
        #[cfg(feature = "rvv")]
        let saved = true;
        let wt = wake_transition(&task.state_word, || pred(&task.wait_reason), false, saved);
        if wt != WakeTransition::Dispatched { return; }

        task.wait_reason = WaitReason::None;
        let target_cpu = wake_target_cpu(idx, task);
        // K-A11: keep the saved tp consistent with the CPU this task is enqueued
        // on (mirrors rebalance_from_offline_cpus): context_switch restores it
        // into the hardware tp and current_cpu_id() trusts it — a stale tp would
        // make the woken task corrupt another CPU's PER_CPU state.
        task.context.tp = target_cpu as CtxReg;
        cpu_enqueue_locked(target_cpu, idx);
    }
}

// ── WaitQueue support ───────────────────────────────────────────────────────

/// Block the current task on a WaitQueue.
/// Called via function pointer from `robot_os_sync::waitqueue`.
pub fn wq_block_current() {
    let cpu = current_cpu_id();
    block_current(cpu, WaitReason::WaitQueue);
}

/// Wake a blocked task by TID (used by WaitQueue/Completion).
/// Scans the task pool for a matching TID in WaitQueue-blocked state.
///
/// Reachable from IRQ context (timer ISR's lease-expiry path in
/// `kernel/src/main.rs` calls this directly) and, like `try_wake_task`, the
/// target CPU is often not the caller's own. See `try_wake_task` doc for the
/// locking rationale — same `cpu_enqueue_locked` wrapper, same guarantees.
pub fn wq_wake_by_tid(tid: u32) {
    use crate::task::sched_word::{wake_transition, WakeTransition};
    unsafe {
        for i in 0..MAX_TASKS {
            if !TASK_VALID[i] { continue; }
            let task = task_mut(i);
            if task.tid != tid { continue; }
            // K-C9 + K-C19, in one transition:
            //  * Not Blocked yet (the caller — WaitQueue::wait() /
            //    lease_wait_return() — registered itself as wake-able and is
            //    mid-way to wq_block_current()): stamp the wake so its
            //    commit consumes it. The stamp is a CAS conditioned on
            //    `state != Blocked`, so it can no longer land *after* the
            //    task committed (the old lost-wake half-window).
            //  * Blocked on WaitQueue: dispatch (CAS Blocked→Ready; the
            //    Acquire pairing guarantees the reason read is the published
            //    one — K-C17).
            //  * Blocked on something else (e.g. a Timer): genuine mismatch,
            //    not the K-C9 race — no stamp, this is not our task anymore.
            // K-C24: see try_wake_task — an unsaved Blocked target is still
            // running and gets a stamp, not an enqueue.
            #[cfg(not(feature = "rvv"))]
            let saved = !task.context_saving.load(Ordering::Acquire);
            #[cfg(feature = "rvv")]
            let saved = true;
            let wt = wake_transition(
                &task.state_word,
                || task.wait_reason == WaitReason::WaitQueue,
                true,
                saved,
            );
            if wt != WakeTransition::Dispatched { break; }

            task.wait_reason = WaitReason::None;
            let target_cpu = wake_target_cpu(i, task);
            // K-A11: keep saved tp consistent with the enqueue CPU (see try_wake_task).
            task.context.tp = target_cpu as CtxReg;
            cpu_enqueue_locked(target_cpu, i);
            break;
        }
    }
}

// ── K-C10: TID-directed wake with K-C9 pending-wake treatment ───────────────

/// Wake the task whose TID is `tid`, if its `wait_reason` satisfies `pred`.
///
/// Returns `true` **only** when this call actually transitioned the target
/// Blocked → Ready and enqueued it. The "stamped `wake_pending` instead"
/// path returns `false`: nothing was dispatched here, the target will
/// short-circuit its own `block_current()` instead. Callers must not read
/// `false` as "the wake was lost".
///
/// # K-C10: why this exists next to `try_wake_task`
///
/// `try_wake_task` selects tasks by a **predicate over `wait_reason`** and
/// bails out early when `state != Blocked`. That early exit is a lost-wakeup
/// bug for every wake whose target is a specific task that is still *on its
/// way* to `Blocked`. The concrete case is `SYS_IPC_FAST_CALL`
/// (`crates/syscall/src/dispatch.rs`), whose sequence is:
///
///   1. reserve a fast-IPC slot (now visible to the server),
///   2. `wake_fast_ipc_server(server_tid)`,
///   3. `task_block(WaitReason::FastIpcClient(slot))`.
///
/// On SMP the server can wake, accept and reply **between 2 and 3**. Its
/// `wake_fast_ipc_client*` then finds the client not yet `Blocked`,
/// `try_wake_task` returns early, and the client proceeds to block on a wake
/// that will never come again — sleeping forever while pinning the slot.
/// `SYS_IPC_FAST_ACCEPT` and `SYS_IPC_CALL`/`SYS_IPC_REPLY` are symmetric.
///
/// # Why this cannot be fixed inside `try_wake_task`
///
/// Stamping `wake_pending` from `try_wake_task` when the state is not
/// `Blocked` is **wrong** and must never be "simplified" into that. A task
/// that has not blocked yet has `wait_reason == WaitReason::None`, so the
/// predicate cannot possibly identify it: `try_wake_task` is called in a
/// sweep over all `MAX_TASKS` slots, so it would have to stamp *every*
/// not-yet-blocked task in the pool. That would make unrelated tasks skip
/// their next, entirely unrelated block — trading a hang for silent
/// cross-task corruption, which is strictly worse.
///
/// This function is safe to stamp because it selects by **TID**, which is
/// known independently of `state` and `wait_reason`. That is the invariant
/// to preserve: never widen this to a state-dependent selector. It is also
/// exactly why the broadcast wakes (`wake_by_irq`, `wake_by_channel`,
/// `wake_by_ring`, `wake_by_port`, `wake_expired_timers`) must NOT be
/// routed through here — they have no addressee TID.
///
/// # `pred` is a *cross-check*, not the selector
///
/// For all current callers the predicate is redundant with the TID by
/// construction: `WaitReason::Rpc(t)` and `WaitReason::FastIpcServer(t)`
/// both carry the blocked task's own TID, and `FastIpcClient(slot)` names a
/// slot whose owner is that TID. `pred` therefore only distinguishes "this
/// task is blocked where I expect" from "this task is blocked on something
/// else entirely", i.e. it detects a genuine mismatch.
///
/// # Genuine mismatch: `break` without stamping (same rule as `wq_wake_by_tid`)
///
/// If the task is `Blocked` but `pred` rejects its `wait_reason`, we leave
/// `wake_pending` untouched. Rationale specific to these callers: between
/// making itself wake-able and calling `task_block`, the target is running
/// a straight-line stretch of its own syscall — it can be preempted to
/// `Ready`, but it cannot become `Blocked` on a *different* reason, because
/// there is no other block in that stretch. So `Blocked` + non-matching
/// reason means this is not the task we are addressing at all (stale TID,
/// TID reuse after exit, or a confused/malicious replier). Stamping there
/// would make an unrelated task skip an unrelated block. Same conclusion as
/// `wq_wake_by_tid`, reached for the same reason.
///
/// # Spurious `wake_pending` — bounded, and audited per call site
///
/// If the target was simply running unrelated work, the stamp survives and
/// makes its *next* `block_current()` return immediately. `block_current`
/// documents this as acceptable because waiters re-check their condition.
/// Two caveats recorded honestly rather than hidden:
///   * the fast-IPC and RPC syscall paths re-check **once** and return `-1`
///     rather than looping, so a stale stamp surfaces as a bogus `-1` to
///     userspace, not as a hang or as corruption (see the K-C10 report);
///   * for the lease-expiry path in `kernel/src/main.rs` this adds no new
///     stale-stamp source at all: the line immediately above it already
///     calls `wq_wake_by_tid` on the same TID, which already stamps.
///
/// # First TID match wins — why that is safe
///
/// The scan stops at the first valid slot carrying `tid`. That is only
/// correct because TIDs are unique among valid slots: `NEXT_TID` increments
/// monotonically under `POOL_LOCK` and is never recycled (it wraps only after
/// 2^32 task creations, skipping 0). A `Zombie` slot does keep both
/// `TASK_VALID` and its TID until `do_schedule()` frees it — but its TID is
/// distinct from every live task's, so it can only be hit when the addressee
/// itself has exited. In that case it takes a `StampPending` on a slot that
/// is about to be released, which is inert: the old sweep also skipped
/// Zombies (`state != Blocked`), so this is not a regression.
///
/// # The selector invariant this depends on
///
/// Replacing "task blocked on reason R" with "task whose TID is `tid`" is
/// only sound while every `WaitReason` carrying a TID carries the *blocking
/// task's own* TID. Verified for all three construction sites in
/// `crates/syscall/src/dispatch.rs`: `Rpc(tid)` and `FastIpcServer(server_tid)`
/// and `FastIpcServer(lessee)` are each `current_task_tid()` of the task that
/// is about to block, and `FastIpcClient(slot)`'s slot was reserved by
/// `fast_ipc_call(caller_tid, ..)` for that same caller. If a future caller
/// blocks a task on another task's TID, it must NOT be woken through here.
///
/// # IRQ context and reentrancy
///
/// Reachable from IRQ context, like `try_wake_task` and `wq_wake_by_tid`:
/// the timer ISR's lease-expiry path calls `wake_fast_ipc_server`. It is
/// safe there for the same reasons — it never calls `do_schedule()`, it
/// touches the ready queue only via `cpu_enqueue_locked` (IRQ-safe, takes
/// `CPU_LOCKS[target_cpu]`, so it cannot race the target CPU's own
/// `do_schedule()`), and the target CPU is routinely not the caller's. The
/// stamp is a lock-free CAS on the task's own `state_word` (K-C19), so it
/// cannot deadlock against an interrupted critical section.
///
/// The policy lives in `wait::wake_action()` — a pure function with the full
/// truth table next to it — and since K-C19 the *transition* that enforces it
/// is host-tested too: `task::sched_word::wake_transition` is free code over
/// an `AtomicU32`, exercised directly by `sched-wake-tests`. Everything below
/// is the addressee scan plus the dispatch bookkeeping around that verdict.
/// Diagnostic counters for [`wake_task_by_tid`]: `(dispatched, stamped,
/// mismatched, absent)`.
///
/// `mismatched` is the one that matters. That branch drops a wake **without**
/// stamping, on the grounds that a blocked task whose reason does not match
/// is no longer the task the waker meant. If it ever fires for a wake that
/// WAS meant for that task, the result is a permanent sleep — the signature
/// K-C17/K-C19 chased (reply deposited, client blocked, nothing ready). Both
/// are closed; the counter stays because a nonzero value here under a hang is
/// still the fastest way to tell "predicate wrong" from "wake never sent".
///
/// `absent` counts wakes addressed to a TID with no valid task at all.
pub fn wake_counters() -> (u32, u32, u32, u32, u32, u32) {
    (
        WAKE_DISPATCHED.load(Ordering::Relaxed),
        WAKE_STAMPED.load(Ordering::Relaxed),
        WAKE_MISMATCHED.load(Ordering::Relaxed),
        WAKE_ABSENT.load(Ordering::Relaxed),
        WAKE_ENQ_REFUSED.load(Ordering::Relaxed),
        WAKE_LATE_DISPATCH.load(Ordering::Relaxed),
    )
}

static WAKE_DISPATCHED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static WAKE_STAMPED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static WAKE_MISMATCHED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static WAKE_ABSENT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static WAKE_ENQ_REFUSED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static WAKE_LATE_DISPATCH: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Diagnostic snapshot of what each hart is running right now: fills
/// `(tid, raw state_word, name)` per CPU (tid 0 = no current). Racy by
/// nature — reads without locks, display-only, same license as the rest of
/// the ipc-census family.
pub fn current_snapshot(out: &mut [(u32, u32, [u8; 8]); MAX_CPUS]) {
    unsafe {
        for cpu in 0..MAX_CPUS {
            let idx = PER_CPU[cpu].current_idx;
            out[cpu] = if idx < MAX_TASKS && TASK_VALID[idx] {
                let t = task_ref(idx);
                let mut name = [0u8; 8];
                let src = t.name;
                let n = src.iter().position(|&b| b == 0).unwrap_or(src.len()).min(8);
                name[..n].copy_from_slice(&src[..n]);
                (t.tid, t.state_word.load(Ordering::Acquire), name)
            } else {
                (0, 0, [0u8; 8])
            };
        }
    }
}

/// K-C25 reaper: deliver wakes that were stamped onto a task which then
/// parked (see `sched_word::reap_orphaned_stamp` for the full mechanism and
/// the measured wedge). Walks the pool once; called from the timer tick right
/// after `wake_expired_timers`, which already pays the same O(MAX_TASKS) walk
/// every tick — this adds one atomic load per valid task in the common case.
///
/// `Blocked + WAKE_STAMP` with `context_saving == false` is unambiguously an
/// orphaned delivered wake: with the context saved the target cannot consume
/// the stamp itself (it is not executing), `do_schedule`'s switch-away sweep
/// already ran (that is how the context got saved), and `wake_transition`
/// never stamps a saved task — so the stamp can only have landed in the
/// window between the sweep's check and context_switch.S clearing the flag.
/// While the flag is still true the target may be executing an unswitched
/// block and its own `commit_blocked_or_consume_wake` owns the stamp — those
/// are skipped and, if they park, caught on a later tick.
///
/// Recovery counts as `late_dispatch` in [`wake_counters`] — the counter has
/// been reserved (declared, read, never incremented) since K-C19 for exactly
/// this: a wake delivered later than its waker intended, by a third party.
pub fn reap_stamped_sleepers() {
    use crate::task::sched_word::{state_of, WAKE_STAMP};
    unsafe {
        for i in 0..MAX_TASKS {
            if !TASK_VALID[i] { continue; }
            let task = task_mut(i);
            let w = task.state_word.load(Ordering::Acquire);
            if w & WAKE_STAMP == 0 || state_of(w) != TaskState::Blocked {
                continue;
            }
            // K-C24 gate, same read the wakers use. Under `rvv` the flag is
            // never maintained (always false) — and `saved` is always passed
            // as true there, so a Blocked+stamp word is unreachable and this
            // loop never gets past the check above; the load is harmless.
            if task.context_saving.load(Ordering::Acquire) {
                continue;
            }
            // ABA guard (measured 2026-08-24, first reaper version): the
            // state word has no generation, so "Blocked+STAMP" can be the
            // SAME bit pattern twice with a full consume→run→re-block→
            // re-stamp cycle in between — and `context_saving` sampled in
            // that window reads false. A CAS alone then reaps a task that is
            // EXECUTING its unswitched-block loop, enqueueing a running task
            // (census signature: `READY-UNQUEUED name=autorun`, the exact
            // state K-C24 exists to prevent — the commit's defensive arm
            // then parks it Ready-in-no-queue forever). The discriminator a
            // recycled bit pattern cannot fake: a PARKED task is current on
            // no hart. A parked Blocked task can only become current again
            // by first leaving Blocked (dispatch CAS → our CAS fails), so
            // checking currency before the CAS closes the ABA.
            let mut is_current = false;
            for cpu in 0..MAX_CPUS {
                if PER_CPU[cpu].current_idx == i {
                    is_current = true;
                    break;
                }
            }
            if is_current {
                continue;
            }
            if crate::task::sched_word::reap_orphaned_stamp(&task.state_word) {
                WAKE_LATE_DISPATCH.fetch_add(1, Ordering::Relaxed);
                // Same dispatch-ownership contract as `wake_task_by_tid`'s
                // Dispatched arm: winner clears the reason and enqueues,
                // keeping tp consistent with the enqueue CPU (K-A11).
                task.wait_reason = WaitReason::None;
                let target_cpu = wake_target_cpu(i, task);
                task.context.tp = target_cpu as CtxReg;
                if !cpu_enqueue_locked(target_cpu, i) {
                    WAKE_ENQ_REFUSED.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

pub fn wake_task_by_tid(tid: u32, pred: &dyn Fn(&WaitReason) -> bool) -> bool {
    use crate::task::sched_word::{wake_transition, WakeTransition};
    unsafe {
        for i in 0..MAX_TASKS {
            // Non-addressees are skipped inline: an invalid slot's `Task` is
            // stale/zeroed memory, so it must not be read at all, and `pred`
            // must not be evaluated for non-addressees.
            if !TASK_VALID[i] { continue; }
            let task = task_mut(i);
            if task.tid != tid { continue; }

            // K-C19: decision and transition are one CAS protocol now —
            // `wake_transition` enforces the `wait::wake_action` truth table
            // (its doc still holds), with the two windows closed:
            // a stamp can no longer land after the addressee committed to
            // Blocked, and a dispatch CAS can be won by exactly one waker.
            // The Acquire pairing inside replaces the old K-C17 fence.
            // K-C24: `saved` keeps an unswitched-block target (Blocked but
            // still executing) out of the ready queues — stamp instead.
            #[cfg(not(feature = "rvv"))]
            let saved = !task.context_saving.load(Ordering::Acquire);
            #[cfg(feature = "rvv")]
            let saved = true;
            match wake_transition(&task.state_word, || pred(&task.wait_reason), true, saved) {
                WakeTransition::Stamped => {
                    WAKE_STAMPED.fetch_add(1, Ordering::Relaxed);
                    // K-C9/K-C10: the addressee had not committed to Blocked
                    // yet; its commit CAS will now fail, consume the stamp,
                    // and skip the block.
                    return false;
                }
                // Genuine mismatch: no stamp, and do NOT keep scanning —
                // TIDs are unique among valid tasks.
                WakeTransition::Mismatch => {
                    WAKE_MISMATCHED.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                WakeTransition::Dispatched => {
                    WAKE_DISPATCHED.fetch_add(1, Ordering::Relaxed);
                    task.wait_reason = WaitReason::None;
                    let target_cpu = wake_target_cpu(i, task);
                    // K-A11: keep the saved tp consistent with the enqueue CPU
                    // — context_switch restores it into hw tp and
                    // current_cpu_id() trusts it; a stale tp would corrupt
                    // another CPU's PER_CPU state.
                    task.context.tp = target_cpu as CtxReg;
                    // The return value matters: a refused enqueue leaves the
                    // task `Ready` and in no queue at all, which is worse than
                    // the sleep it was meant to end. Counted, not ignored.
                    if !cpu_enqueue_locked(target_cpu, i) {
                        WAKE_ENQ_REFUSED.fetch_add(1, Ordering::Relaxed);
                    }
                    return true;
                }
                // Unreachable with `stamp_if_unblocked = true`; kept so the
                // match stays exhaustive against the transition's contract.
                WakeTransition::NotBlocked => return false,
            }
        }
        // Every arm above returns, so reaching here means no valid slot
        // carried this TID.
        WAKE_ABSENT.fetch_add(1, Ordering::Relaxed);
    }
    false
}

// ── M03: Tickless scheduling helpers ────────────────────────────────────────

/// Return the nearest pending timer deadline across all blocked tasks.
///
/// Used by the tickless scheduler to program mtimecmp at the exact tick needed
/// rather than firing at a fixed periodic rate.  Returns `None` if no tasks
/// are currently sleeping on a timer.
pub fn nearest_timer_deadline() -> Option<u64> {
    let mut min_deadline: Option<u64> = None;
    unsafe {
        for i in 0..MAX_TASKS {
            if !TASK_VALID[i] { continue; }
            let task = task_ref(i);
            // Acquire: seeing Blocked publishes the task's wait_reason (K-C17).
            if task.state_acquire() != TaskState::Blocked { continue; }
            if let WaitReason::Timer(deadline) = task.wait_reason {
                min_deadline = Some(match min_deadline {
                    Some(cur) => cur.min(deadline),
                    None      => deadline,
                });
            }
        }
    }
    min_deadline
}

/// Read-only reference to task via raw pointer (avoids aliasing with task_mut).
/// # Safety
/// Caller must not hold a mutable reference to the same slot simultaneously.
#[inline(always)]
unsafe fn task_ref(idx: usize) -> &'static Task {
    unsafe { &*(core::ptr::addr_of!(TASKS[idx])) }
}

// ── Preemption control (F03.4) ────────────────────────────────────────────────
//
// Per-CPU preemption depth counter.  When > 0, `schedule()` returns immediately
// without performing a context switch.  Nesting is supported: each
// `preempt_disable()` must be matched by exactly one `preempt_enable()`.
//
// Typical use:
//   preempt_disable();
//   // ... critical section that must not be interrupted by the scheduler ...
//   preempt_enable();
//
// IRQ handlers are NOT affected — the hardware timer still fires, but
// `schedule()` checks the counter and bails early if preemption is disabled.

use core::sync::atomic::AtomicI32;

/// Per-CPU preemption depth. Positive means preemption is disabled.
/// We use `MAX_CPUS` slots indexed by `current_cpu_id()`.
static PREEMPT_COUNT: [AtomicI32; MAX_CPUS] = [
    AtomicI32::new(0), AtomicI32::new(0),
    AtomicI32::new(0), AtomicI32::new(0),
];

/// Increment the preemption disable count for the current CPU.
///
/// After this call, `schedule()` will not perform a context switch until a
/// matching `preempt_enable()` brings the count back to zero.
/// This function may be called from any context (task or IRQ handler).
#[inline]
pub fn preempt_disable() {
    let cpu = current_cpu_id().min(MAX_CPUS - 1);
    PREEMPT_COUNT[cpu].fetch_add(1, Ordering::Relaxed);
    // Compiler fence: prevent instruction reordering across the barrier.
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}

/// Decrement the preemption disable count for the current CPU.
///
/// If the count reaches zero, preemption is re-enabled and a deferred
/// `schedule()` may run immediately if a higher-priority task became runnable
/// while preemption was disabled.
///
/// # Panics
/// Panics in debug builds if called when preemption is already enabled
/// (i.e. the count would go negative), which indicates a mismatched pair.
#[inline]
pub fn preempt_enable() {
    let cpu = current_cpu_id().min(MAX_CPUS - 1);
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
    let prev = PREEMPT_COUNT[cpu].fetch_sub(1, Ordering::Relaxed);
    debug_assert!(prev > 0, "preempt_enable() without matching preempt_disable()");
    // If we just re-enabled preemption, run the scheduler in case a higher-
    // priority task became runnable while we held the disable.
    if prev == 1 {
        schedule();
    }
}

/// Returns true if preemption is currently disabled on this CPU.
#[inline]
pub fn preempt_disabled() -> bool {
    let cpu = current_cpu_id().min(MAX_CPUS - 1);
    PREEMPT_COUNT[cpu].load(Ordering::Relaxed) > 0
}
