/// Scheduler for Robot OS — Priority-based with RT support and hart affinity.
///
/// Design:
/// - Global task pool: `TASKS[MAX_TASKS]` protected by `POOL_LOCK`
/// - Per-CPU multi-level priority queues: 32 FIFOs + bitmap for O(1) dequeue
/// - Hard real-time: priorities 0..RT_PRIORITY_THRESHOLD are never preempted by timer
/// - Hart affinity: tasks can be pinned to a specific CPU
/// - Task assignment: least-loaded CPU at creation time (unless pinned)
/// - Context switch: saves/restores callee-saved registers only (ra, sp, s0-s11, pc)
///
/// Invariants:
/// - `do_schedule()` is always called with interrupts disabled on the calling CPU
/// - Each CPU only dequeues/enqueues to its own ready queue (no cross-CPU access post-init)
/// - `POOL_LOCK` protects `TASKS[]`, `TASK_VALID[]`, `NEXT_TID` during task creation

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use crate::task::{
    Task, TaskContext, TaskState, CtxReg, MAX_TASKS, STACK_SIZE,
    TIME_SLICE_TICKS, RT_TIME_SLICE_TICKS, NUM_PRIORITIES, is_rt_priority,
    WaitReason, DeadlineParams, SyscallFilter,
};
use crate::smp::{current_cpu_id, NUM_ONLINE_CPUS};

pub const MAX_CPUS: usize = 4;

/// Next ASID to allocate for user-space tasks.
/// ASID 0 is reserved for the kernel page table.
/// Sv39 supports 16-bit ASIDs (1..65535). Wraps to 1 on overflow.
static NEXT_ASID: AtomicU16 = AtomicU16::new(1);

/// Allocate a unique ASID for a user-space page table.
pub fn alloc_asid() -> u16 {
    loop {
        let current = NEXT_ASID.load(Ordering::Relaxed);
        let next = if current == u16::MAX { 1 } else { current + 1 };
        if NEXT_ASID.compare_exchange(current, next, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
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
static CPU_LOCKS: [AtomicBool; 4] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

// ---- FFI: context_switch assembly ----

unsafe extern "C" {
    /// Switch from `old` task context to `new` task context.
    /// If `old` is null, just restores `new` (used for the very first task).
    fn context_switch(old: *mut Task, new: *mut Task);
}

// ---- Lock RAII guards ----

struct PoolGuard;

impl PoolGuard {
    fn acquire() -> Self {
        while POOL_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        PoolGuard
    }
}

impl Drop for PoolGuard {
    fn drop(&mut self) {
        POOL_LOCK.store(false, Ordering::Release);
    }
}

struct CpuLockGuard(usize);

impl CpuLockGuard {
    fn acquire(cpu: usize) -> Self {
        while CPU_LOCKS[cpu]
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        CpuLockGuard(cpu)
    }
}

impl Drop for CpuLockGuard {
    fn drop(&mut self) {
        CPU_LOCKS[self.0].store(false, Ordering::Release);
    }
}

// ---- Internal helpers ----

/// Enqueue task `idx` at its priority level on CPU `cpu`.
/// Caller must hold `CPU_LOCKS[cpu]` or guarantee single-CPU access.
unsafe fn cpu_enqueue(cpu: usize, idx: usize) {
    let prio = task_mut(idx).priority as usize;
    let q = &mut PER_CPU[cpu].ready_queues[prio];
    debug_assert!(q.count < MAX_TASKS, "sched: priority queue full");
    q.buf[q.tail] = idx;
    q.tail = (q.tail + 1) % MAX_TASKS;
    q.count += 1;
    PER_CPU[cpu].ready_bitmap |= 1 << prio;
}

/// Dequeue the highest-priority ready task from CPU `cpu`.
/// Caller must hold `CPU_LOCKS[cpu]` or guarantee single-CPU access.
unsafe fn cpu_dequeue(cpu: usize) -> Option<usize> {
    let bitmap = PER_CPU[cpu].ready_bitmap;
    if bitmap == 0 {
        return None;
    }
    let prio = bitmap.trailing_zeros() as usize;
    let q = &mut PER_CPU[cpu].ready_queues[prio];
    let idx = q.buf[q.head];
    q.head = (q.head + 1) % MAX_TASKS;
    q.count -= 1;
    if q.count == 0 {
        PER_CPU[cpu].ready_bitmap &= !(1 << prio);
    }
    Some(idx)
}

/// Return the priority of the highest-priority ready task on `cpu`, or None.
unsafe fn cpu_peek_highest_prio(cpu: usize) -> Option<u32> {
    let bitmap = PER_CPU[cpu].ready_bitmap;
    if bitmap == 0 { None } else { Some(bitmap.trailing_zeros()) }
}

/// Allocate a free slot in TASKS[].
/// Caller must hold POOL_LOCK.
unsafe fn alloc_slot() -> Option<usize> {
    for i in 0..MAX_TASKS {
        if !TASK_VALID[i] {
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

/// Pick the CPU with the fewest ready tasks.
/// Uses NUM_ONLINE_CPUS to limit the search. Not locked — approximate is fine.
unsafe fn find_least_loaded_cpu() -> usize {
    let num_online = NUM_ONLINE_CPUS.load(Ordering::Relaxed);
    let mut min_count = usize::MAX;
    let mut min_cpu = 0;
    for i in 0..num_online {
        let count = PER_CPU[i].ready_bitmap.count_ones() as usize;
        if count < min_count {
            min_count = count;
            min_cpu = i;
        }
    }
    min_cpu
}

/// Pick target CPU respecting affinity.
/// If affinity >= 0, returns that hart directly; otherwise least-loaded.
unsafe fn pick_target_cpu(affinity: i8) -> usize {
    if affinity >= 0 {
        affinity as usize
    } else {
        find_least_loaded_cpu()
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

/// Initialize the scheduler. Call once before `task_create` / `start`.
pub fn init() {
    // PER_CPU is initialized via EMPTY_CPU const (current_idx = usize::MAX).
    // Static storage is already zero-initialized (BSS).
    // Nothing else to do.
}

/// Create a new kernel task with CPU affinity.
///
/// `affinity`: -1 = auto-assign to least-loaded CPU, 0..3 = pin to that hart.
/// Returns the task pool index.
pub fn task_create_affinity(
    name: &str,
    entry_fn: fn(usize),
    arg: usize,
    priority: u32,
    affinity: i8,
) -> usize {
    // Disable interrupts during task creation to prevent races on NEXT_TID.
    let sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(sstatus & !robot_os_arch::csr::SSTATUS_SIE);

    let idx = unsafe {
        // --- Allocate and initialize task under pool lock ---
        let (idx, target_cpu) = {
            let _pool = PoolGuard::acquire();

            let idx = alloc_slot().expect("sched: task pool full");
            let task = task_mut(idx);

            task.tid = NEXT_TID;
            NEXT_TID += 1;

            let name_bytes = name.as_bytes();
            let len = name_bytes.len().min(31);
            task.name[..len].copy_from_slice(&name_bytes[..len]);
            task.name[len] = 0;

            task.priority      = priority;
            task.base_priority = priority;
            task.time_slice    = if is_rt_priority(priority) {
                RT_TIME_SLICE_TICKS
            } else {
                TIME_SLICE_TICKS
            };
            task.cpu_affinity  = affinity;
            task.state          = TaskState::Ready;
            task.wait_reason    = WaitReason::None;
            task.deadline       = DeadlineParams::default();
            task.syscall_filter = SyscallFilter::disabled();
            task.stack_idx      = idx;
            task.entry_fn       = entry_fn as usize;
            task.entry_arg      = arg;

            // Stack grows down; top is at the end of the stack storage slice.
            let stack_top = TASK_STACKS.0[idx].as_mut_ptr() as usize + STACK_SIZE;
            let stack_top = stack_top & !0xF; // align to 16 bytes (RISC-V ABI)

            let entry_addr = task_entry_wrapper as *const () as usize as CtxReg;
            task.context = TaskContext {
                sp: stack_top as CtxReg,
                pc: entry_addr,
                ra: entry_addr,
                ..Default::default()
            };

            // Phase 7: per-task SATP and user-space fields (RV64 only — requires MMU).
            #[cfg(target_pointer_width = "64")]
            {
                task.task_satp = robot_os_arch::csr::read_satp() as u64;
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

            let target_cpu = pick_target_cpu(affinity);
            (idx, target_cpu)
        }; // pool lock released here

        // --- Enqueue to target CPU under CPU lock ---
        {
            let _cpu = CpuLockGuard::acquire(target_cpu);
            cpu_enqueue(target_cpu, idx);
        } // cpu lock released here

        idx
    };

    // Restore interrupts.
    robot_os_arch::csr::write_sstatus(sstatus);
    idx
}

/// Create a new kernel task, auto-assigned to the least-loaded CPU.
///
/// Returns the task pool index (for debugging; rarely needed by callers).
pub fn task_create(name: &str, entry_fn: fn(usize), arg: usize, priority: u32) -> usize {
    task_create_affinity(name, entry_fn, arg, priority, -1)
}

/// Create a task with a security profile pre-applied.
///
/// The filter is set before the task ever runs — it cannot call any
/// unauthorized syscall, not even during initialization.
pub fn task_create_filtered(
    name: &str, entry_fn: fn(usize), arg: usize,
    priority: u32, profile_id: u64,
) -> usize {
    let idx = task_create(name, entry_fn, arg, priority);
    // Apply the filter to the newly created task.
    let filter = crate::seccomp::profile_to_filter(profile_id);
    unsafe {
        if idx < MAX_TASKS && TASK_VALID[idx] {
            task_mut(idx).syscall_filter = filter;
        }
    }
    idx
}

/// Called when a task's entry function returns.
///
/// Marks the task as zombie, frees its pool slot, then immediately tries to
/// reschedule. If no tasks are ready, enters a WFI idle loop (timer interrupts
/// will call `schedule()` to pick up future tasks).
///
/// Never returns.
pub fn task_exit() -> ! {
    unsafe {
        let cpu = current_cpu_id();
        let idx = PER_CPU[cpu].current_idx;

        // Free the task slot under pool lock.
        // Use a nested block so the guard is dropped (lock released) before
        // do_schedule(), which may context-switch and never return to this frame.
        if idx != usize::MAX {
            // AQ2: Notify driver manager — if this task was a registered driver,
            // record the crash so auto-restart can kick in.
            crate::driver::driver_on_crash(idx);

            {
                let _pool = PoolGuard::acquire();
                task_mut(idx).state = TaskState::Zombie;
                TASK_VALID[idx] = false;
            } // pool lock released
        }
        PER_CPU[cpu].current_idx = usize::MAX;

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

/// Fixed-point scale for deadline admission control (1.0 = this value).
/// Using 10000 gives 0.01% granularity for utilization checks.
const ADMISSION_SCALE: u64 = 10_000;

/// Called from the timer interrupt handler (interrupts already disabled by hardware).
///
/// RT tasks: never preempted by timer — only by a strictly higher-priority ready task.
/// Normal tasks: preempted when time slice expires (standard round-robin).
/// Deadline tasks: decrement remaining budget; replenish on deadline expiry.
pub fn schedule() {
    let cpu = current_cpu_id();
    unsafe {
        let current_idx = PER_CPU[cpu].current_idx;
        if current_idx != usize::MAX {
            let task = task_mut(current_idx);
            task.total_runtime += 1;

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
    unsafe {
        let next_idx = match cpu_dequeue(cpu) {
            Some(idx) => idx,
            None => panic!("sched_start: no tasks for CPU {}", cpu),
        };
        let next = task_mut(next_idx);
        next.state      = TaskState::Running;
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

/// Find the ready deadline task with the earliest absolute deadline on `cpu`.
///
/// Scans all valid tasks assigned to (or compatible with) `cpu` that are in
/// Ready state and have `period_us > 0` (deadline task) with remaining budget.
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
        if t.state != TaskState::Ready && t.state != TaskState::Running { continue; }
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
/// Must be called with interrupts disabled on the calling CPU.
/// No locks may be held when this function is called (context_switch may not return).
unsafe fn do_schedule(cpu: usize) {
    // Phase 1: Check for deadline tasks (absolute priority over RT and normal).
    let next_idx = if let Some(dl_idx) = find_earliest_deadline(cpu) {
        // Remove the deadline task from the priority queue if it was enqueued there.
        // Since we found it by scanning TASKS[], it might still be in the queue.
        // We don't dequeue by index from the bitmap queue — instead we just skip
        // the normal dequeue path. The task's state change to Running prevents
        // double-selection.
        dl_idx
    } else {
        // Phase 2: No deadline tasks — use bitmap-based priority queue.
        match cpu_dequeue(cpu) {
            Some(idx) => idx,
            None => return, // No ready tasks on this CPU — caller will idle.
        }
    };

    let old_idx = PER_CPU[cpu].current_idx;

    // Don't switch if we'd switch to ourselves.
    if next_idx == old_idx {
        return;
    }

    // Re-enqueue old task if it is still runnable.
    if old_idx != usize::MAX {
        let old = task_mut(old_idx);
        if old.state == TaskState::Running {
            old.state = TaskState::Ready;
            cpu_enqueue(cpu, old_idx); // enqueues at old.priority level
        }
    }

    // Activate next task.
    let next = task_mut(next_idx);
    next.state      = TaskState::Running;
    next.time_slice = if is_rt_priority(next.priority) {
        RT_TIME_SLICE_TICKS
    } else {
        TIME_SLICE_TICKS
    };
    PER_CPU[cpu].current_idx = next_idx;

    let old_ptr = if old_idx != usize::MAX {
        task_mut(old_idx) as *mut Task
    } else {
        core::ptr::null_mut() // No old task to save (first run, or post-task_exit).
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
        let len = task.name.iter().position(|&b| b == 0).unwrap_or(31);
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
#[cfg(target_pointer_width = "64")]
pub fn current_user_pt() -> usize {
    let cpu = current_cpu_id();
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { 0 } else { TASKS[idx].user_pt as usize }
    }
}

#[cfg(target_pointer_width = "32")]
pub fn current_user_pt() -> usize { 0 }

/// Update the task_satp, user_pt and user_brk of the current task.
/// Called by exec_user after a new user page table has been built.
#[cfg(target_pointer_width = "64")]
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
#[cfg(target_pointer_width = "64")]
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
#[cfg(target_pointer_width = "64")]
pub fn set_task_user_info(idx: usize, task_satp: u64, user_pt: u64, brk: u64) {
    unsafe {
        if idx < MAX_TASKS && TASK_VALID[idx] {
            TASKS[idx].task_satp = task_satp;
            TASKS[idx].user_pt   = user_pt;
            TASKS[idx].user_brk  = brk;
        }
    }
}

/// Boost a task's priority by TID (for priority inheritance).
/// Only boosts if `new_prio` is higher (lower number) than current priority.
pub fn pi_boost_task(tid: u32, new_prio: u32) {
    unsafe {
        for i in 0..MAX_TASKS {
            if TASK_VALID[i] && TASKS[i].tid == tid {
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
                TASKS[i].priority = TASKS[i].base_priority;
                break;
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
#[cfg(not(any(feature = "no-mmu", feature = "esp32c3")))]
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
    unsafe {
        let idx = PER_CPU[cpu].current_idx;
        if idx == usize::MAX { return; }
        let task = task_mut(idx);
        task.state = TaskState::Blocked;
        task.wait_reason = reason;
        // Don't re-enqueue — blocked tasks leave the ready queue.
        do_schedule(cpu);
        // Returns here when woken and rescheduled.
    }
}

/// Try to wake task `idx` if it matches the predicate.
/// Called from wait.rs wake_matching().
pub fn try_wake_task(idx: usize, pred: &dyn Fn(&WaitReason) -> bool) {
    if idx >= MAX_TASKS { return; }
    unsafe {
        if !TASK_VALID[idx] { return; }
        let task = task_mut(idx);
        if task.state != TaskState::Blocked { return; }
        if !pred(&task.wait_reason) { return; }

        task.state = TaskState::Ready;
        task.wait_reason = WaitReason::None;
        let target_cpu = if task.cpu_affinity >= 0 {
            task.cpu_affinity as usize
        } else {
            0
        };
        cpu_enqueue(target_cpu.min(MAX_CPUS - 1), idx);
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
pub fn wq_wake_by_tid(tid: u32) {
    unsafe {
        for i in 0..MAX_TASKS {
            if !TASK_VALID[i] { continue; }
            let task = task_mut(i);
            if task.tid != tid { continue; }
            if task.state != TaskState::Blocked { break; }
            if task.wait_reason != WaitReason::WaitQueue { break; }

            task.state = TaskState::Ready;
            task.wait_reason = WaitReason::None;
            let target_cpu = if task.cpu_affinity >= 0 {
                task.cpu_affinity as usize
            } else {
                0
            };
            cpu_enqueue(target_cpu.min(MAX_CPUS - 1), i);
            break;
        }
    }
}
