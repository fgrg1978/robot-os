/// Task definitions for the Robot OS scheduler.
///
/// Ported from kernel/include/sched.h

pub use robot_os_limits::MAX_TASKS;

pub const NUM_PRIORITIES: usize = 32;
pub const DEFAULT_PRIORITY: u32 = 16;
pub const IDLE_PRIORITY: u32 = 31;

/// Size of `Task::name`, in bytes. Names are null-terminated ASCII, so the
/// longest storable name is `TASK_NAME_CAPACITY - 1` characters.
pub const TASK_NAME_CAPACITY: usize = 32;

/// High priority for real-time motor control (PID loop).
/// Must run promptly to maintain control loop timing.
pub const RT_MOTOR_PRIORITY: u32 = 8;

/// Priority for the network polling task.
/// Slightly above default so incoming packets are processed promptly.
pub const NET_POLL_PRIORITY: u32 = 12;

/// Priority for the behavior engine (sensor→decision→motor).
/// Above default since it drives the robot's actions.
pub const BEHAVIOR_PRIORITY: u32 = 14;

/// Priority for sensor fusion / AHRS task (~100 Hz).
/// Matches behavior priority — both are critical for control.
pub const SENSOR_AHRS_PRIORITY: u32 = 14;

/// Priority for the flight controller (PID→mixer→ESC).
/// Same as rt-motor — real-time critical.
pub const FLIGHT_CTRL_PRIORITY: u32 = 8;

/// Priority for the system watchdog task.
/// Runs at default — periodic health checks, not latency-critical.
pub const WATCHDOG_PRIORITY: u32 = 20;
/// Priority threshold separating real-time from normal tasks.
/// Priorities 0..RT_PRIORITY_THRESHOLD are hard real-time (not preempted by timer).
/// Priorities RT_PRIORITY_THRESHOLD..31 are normal (time-sliced round-robin).
pub const RT_PRIORITY_THRESHOLD: u32 = 12;

/// Time slice for RT tasks: 0 means "run until yield or preemption by
/// a higher-priority RT task".  RT tasks are never preempted by the timer.
pub const RT_TIME_SLICE_TICKS: u32 = 0;

/// Each timer tick is 10ms; one tick per time slice (normal tasks only).
pub const TIME_SLICE_TICKS: u32 = 1;

/// Returns true if the given priority is in the hard real-time range.
#[inline]
pub const fn is_rt_priority(prio: u32) -> bool {
    prio < RT_PRIORITY_THRESHOLD
}

/// Stack size for kernel tasks.
/// Sourced from Kconfig `KERNEL_STACK_SIZE_KB` (see Kconfig.limits) — with
/// guard page (4 KiB unmapped at bottom), usable size is this minus 4 KiB.
/// Rust kernel tasks with nested calls (PID, flight controller, kprintln
/// formatting) need substantial stack space.
pub const STACK_SIZE: usize = robot_os_limits::KERNEL_STACK_SIZE_BYTES;

// ---- Register-width type for context ----

pub type CtxReg = u64;

// ---- Task context (callee-saved registers for context switch) ----

/// Saved CPU state during a context switch.
///
/// **MUST be the first field of `Task`** (at offset 0) because
/// `context_switch.S` accesses these fields directly from the task pointer.
///
/// Offsets (8 bytes each): ra=0, sp=8, ..., pc=112, tp=120 (128 bytes total)
#[repr(C)]
#[derive(Default)]
pub struct TaskContext {
    pub ra:  CtxReg,
    pub sp:  CtxReg,
    pub s0:  CtxReg,
    pub s1:  CtxReg,
    pub s2:  CtxReg,
    pub s3:  CtxReg,
    pub s4:  CtxReg,
    pub s5:  CtxReg,
    pub s6:  CtxReg,
    pub s7:  CtxReg,
    pub s8:  CtxReg,
    pub s9:  CtxReg,
    pub s10: CtxReg,
    pub s11: CtxReg,
    pub pc:  CtxReg,
    pub tp:  CtxReg,  // preserved across context switches so current_cpu_id() stays correct
}

// Compile-time check: TaskContext is 16 fields × register_width (ra,sp,s0-s11,pc,tp)
const _: () = assert!(core::mem::size_of::<TaskContext>() == 128);

// ---- Task state ----

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Ready   = 0,
    Running = 1,
    Blocked = 2,
    Zombie  = 3,
    Invalid = 4,
}

// ---- K-C19: state + wake stamp, one atomic word ----

/// The scheduling state word: [`TaskState`] and the K-C9 wake stamp packed
/// into one `AtomicU32`, so the two are read and written under the same
/// exclusion — a CAS — instead of as two independently-ordered cells.
///
/// # WHY one word (K-C19)
///
/// With `state: TaskState` and `wake_pending: AtomicBool` as separate cells
/// there were two windows, one per direction, and both were measured hangs:
///
///  * **Blocker side.** `block_current()` consumed the stamp and *then*
///    marked `Blocked`. A waker reading `Running` between those two
///    operations stamped a wake the blocker had already passed by; nobody
///    ever consumed it. Permanent sleep with the wake emitted and counted —
///    the K-C19 signature: a fast-IPC slot `Replied`, addressed to exactly
///    the client sleeping on it, ~1 in 3 runs.
///  * **Waker side.** The double-check handshake (re-check the stamp after
///    `Blocked`, re-read state after stamping) was tried and REVERTED: when
///    the waker wins the stamp `swap` and enqueues while the blocker, having
///    lost that `swap`, continues into `do_schedule`, the task is both
///    "current" and queued — observed as a boot-time freeze. Hanging the
///    board is worse than losing a wake.
///
/// With one word both transitions become *conditional* and *atomic*:
///
///  * Committing `Blocked` is a CAS that requires the stamp bit to be clear.
///    A stamp that lands first makes the CAS fail; the retry consumes it and
///    the block is skipped. It is structurally impossible to commit to
///    `Blocked` past a pending wake.
///  * Stamping is a CAS that requires `state != Blocked`. A commit that
///    lands first makes the CAS fail; the retry observes `Blocked` and
///    dispatches. It is structurally impossible to stamp a task that has
///    already committed.
///
/// Each side's CAS only fails because the other side's succeeded, so the
/// retry loops are bounded in practice by one extra iteration.
///
/// The invariant documented in `wait.rs` — "`Blocked` with the stamp set
/// does not occur" — is enforced by construction here, not by protocol
/// discipline at the call sites.
///
/// These are free functions over `&AtomicU32` (no statics, no CSRs) so the
/// host suite (`crates/sched-wake-tests`) exercises the *real* transition
/// code, not a hand-written replica of it.
pub mod sched_word {
    use core::sync::atomic::{AtomicU32, Ordering};
    use super::TaskState;

    /// Bits 0..=2: the `TaskState` discriminant (0..=4).
    pub const STATE_MASK: u32 = 0b0111;
    /// Bit 3: the K-C9 wake stamp ("a wake arrived before you blocked").
    pub const WAKE_STAMP: u32 = 0b1000;

    #[inline]
    pub const fn pack(s: TaskState) -> u32 { s as u32 }

    /// Decode the state field. Never panics: an out-of-range discriminant
    /// (impossible unless the word is corrupted) decodes to `Invalid`, which
    /// every consumer already treats as "not schedulable".
    #[inline]
    pub const fn state_of(w: u32) -> TaskState {
        match w & STATE_MASK {
            0 => TaskState::Ready,
            1 => TaskState::Running,
            2 => TaskState::Blocked,
            3 => TaskState::Zombie,
            _ => TaskState::Invalid,
        }
    }

    /// What a wake attempt did. The scheduler maps these onto
    /// `wait::WakeAction` bookkeeping; `Skip` has no equivalent here because
    /// addressee selection happens before the word is touched.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum WakeTransition {
        /// `Blocked` + matching reason: transitioned to `Ready` — the caller
        /// must now clear `wait_reason` and enqueue.
        Dispatched,
        /// Not `Blocked`: the stamp bit is now set; its imminent
        /// `block_current()` will consume it. (Only when stamping was
        /// requested.)
        Stamped,
        /// `Blocked` on a non-matching reason. Untouched — deliberately no
        /// stamp; see `wait::wake_action`'s Mismatch rationale.
        Mismatch,
        /// Not `Blocked` and stamping was not requested (broadcast wakes).
        /// Untouched.
        NotBlocked,
    }

    /// Blocker side. Called by `block_current()` **after** `wait_reason` is
    /// written; the Release CAS publishes that write to whoever sees
    /// `Blocked` (Acquire) — this pairing replaces the old K-C17 fences.
    ///
    /// Returns `true` if the task committed to `Blocked` (caller proceeds to
    /// `do_schedule`), `false` if a pending wake was consumed instead (caller
    /// must skip blocking — the condition it was about to sleep on is already
    /// satisfied, and every caller re-checks its condition in a loop).
    ///
    /// Defensive arm: if the state is somehow not `Running` (a concurrent
    /// remote transition — no such path exists today), it refuses to
    /// overwrite and lets the caller fall through to `do_schedule`, which
    /// already handles every non-Running current state. The old plain store
    /// would have clobbered e.g. `Zombie` with `Blocked` and leaked the slot.
    #[inline]
    pub fn commit_blocked_or_consume_wake(w: &AtomicU32) -> bool {
        loop {
            let cur = w.load(Ordering::Relaxed);
            if cur & WAKE_STAMP != 0 {
                // Consume AND normalize in one CAS (K-C24). The caller is
                // the current task, so `Running` is the truth; leaving a
                // stale `Blocked` behind on the skip path would let a waker
                // dispatch-and-enqueue a task that is executing — the
                // Ready-in-no-queue starvation this protocol exists to make
                // impossible. Reachable with `Blocked` here: an unswitched
                // block (do_schedule found nothing, we kept running) loops
                // back into a fresh block attempt while the word still says
                // `Blocked`, and a K-C24 stamp may have landed on it.
                // Acquire pairs with the waker's Release stamp: everything
                // the waker published before waking is visible on return.
                if w.compare_exchange_weak(
                    cur, pack(TaskState::Running),
                    Ordering::Acquire, Ordering::Relaxed,
                ).is_ok() {
                    return false;
                }
                continue;
            }
            match state_of(cur) {
                TaskState::Running => {
                    if w.compare_exchange_weak(
                        cur, pack(TaskState::Blocked),
                        Ordering::Release, Ordering::Relaxed,
                    ).is_ok() {
                        return true;
                    }
                    // CAS failed ⇒ a waker just stamped us ⇒ next iteration
                    // consumes the stamp and skips the block.
                }
                // Already `Blocked` (unswitched block looping back in) with
                // no stamp: the commit stands as-is — proceed to
                // do_schedule and keep trying to yield the hart.
                TaskState::Blocked => return true,
                // Defensive (no such remote transition exists today): don't
                // clobber Zombie/Ready — fall through to do_schedule, which
                // handles every non-Running current.
                _ => return true,
            }
        }
    }

    /// Waker side — the one transition every wake path goes through.
    ///
    /// `reason_matches` is evaluated only under an observed-`Blocked`
    /// snapshot; the Acquire load guarantees the `wait_reason` it reads is
    /// the one the blocker published before its Release commit (K-C17).
    /// `stamp_if_unblocked` distinguishes the TID-directed wakes (which may
    /// stamp, K-C9/K-C10) from the broadcast sweeps (which must not — a
    /// sweep cannot tell its addressee from any other task about to sleep;
    /// see `wait.rs`).
    ///
    /// `saved` is the K-C24 gate: `Blocked` alone does NOT mean parked. When
    /// `block_current`'s `do_schedule` finds nothing to run it RETURNS, and
    /// the task keeps executing its caller's retry loop with `state ==
    /// Blocked` and `context_saving == true`. Dispatching it then enqueues a
    /// RUNNING task: its own hart can dequeue it as `next == old` (entry
    /// consumed, no switch), and the next tick's re-enqueue arm only saves
    /// `Running` currents — the task is left `Ready` in no queue, forever.
    /// Measured: the phase-A server (`autorun`) starved exactly this way,
    /// and the 2026-08-22 "Replied + client asleep" residue is the same
    /// mechanism hitting a client. So: `Blocked` + matching reason + NOT
    /// saved ⇒ **stamp, never dispatch** — the target is still running, its
    /// next commit attempt consumes the stamp. If it instead gets parked
    /// with the stamp set (the stamp landed after `do_schedule`'s
    /// switch-away sweep checked), the dispatch CAS below WOULD sweep stamp
    /// and state together on the next wake — but the one-shot wakes have no
    /// next wake, which is why [`reap_orphaned_stamp`] (K-C25) exists: the
    /// timer tick reaps `Blocked + stamp + saved` back to `Ready`. Callers
    /// derive
    /// `saved` from `!context_saving` (Acquire); the flag's store precedes
    /// the Release commit in program order, so any waker that sees `Blocked`
    /// sees the true flag. Under `rvv` (which never maintains the flag)
    /// callers pass `true`, keeping that build's documented pre-existing gap
    /// unchanged rather than silently different.
    ///
    /// On `Dispatched` the caller owns the task's dispatch: it must clear
    /// `wait_reason` and enqueue. Exactly one waker can win the CAS, so the
    /// ownership is exclusive.
    #[inline]
    pub fn wake_transition(
        w: &AtomicU32,
        mut reason_matches: impl FnMut() -> bool,
        stamp_if_unblocked: bool,
        saved: bool,
    ) -> WakeTransition {
        loop {
            let cur = w.load(Ordering::Acquire);
            if state_of(cur) == TaskState::Blocked {
                if !reason_matches() {
                    return WakeTransition::Mismatch;
                }
                if !saved {
                    // K-C24: committed but still running (unswitched block).
                    // Stamp — even for broadcast sweeps: having matched the
                    // reason under Blocked, this IS the addressee. CAS, not
                    // fetch_or, so a concurrent commit-consume retries
                    // against us and the pairing stays exact.
                    if w.compare_exchange_weak(
                        cur, cur | WAKE_STAMP,
                        Ordering::Release, Ordering::Relaxed,
                    ).is_ok() {
                        return WakeTransition::Stamped;
                    }
                    continue;
                }
                // AcqRel: Acquire so the dispatch bookkeeping after the win
                // sees the blocker's writes; Release so the Ready transition
                // is ordered before the enqueue that publishes it.
                if w.compare_exchange_weak(
                    cur, pack(TaskState::Ready),
                    Ordering::AcqRel, Ordering::Relaxed,
                ).is_ok() {
                    return WakeTransition::Dispatched;
                }
                // Lost to a competing waker (state moved on) — retry.
            } else if stamp_if_unblocked {
                // Idempotent by re-CAS: if the stamp is already set this
                // rewrites the same value. Release pairs with the blocker's
                // Acquire consume.
                if w.compare_exchange_weak(
                    cur, cur | WAKE_STAMP,
                    Ordering::Release, Ordering::Relaxed,
                ).is_ok() {
                    return WakeTransition::Stamped;
                }
                // CAS failed ⇒ the blocker just committed `Blocked` ⇒ the
                // retry observes it and dispatches. This is the exact
                // closure of K-C19's waker-side half.
            } else {
                return WakeTransition::NotBlocked;
            }
        }
    }

    /// Reaper side (K-C25) — recover a *parked* task whose wake was delivered
    /// as a K-C24 stamp and then orphaned.
    ///
    /// `wake_transition`'s `!saved` arm stamps a `Blocked` task that is still
    /// executing (unswitched block). Its doc argued the stamp is consumed
    /// either by the target's next commit, by `do_schedule`'s switch-away
    /// sweep, or by "the next wake's dispatch CAS, which sweeps stamp and
    /// state together". That last leg silently assumed a next wake exists.
    /// For the one-shot wakes (`fast_ipc_reply`'s client wake, the FAST_CALL
    /// doorbell once every client is asleep, RPC replies, lease returns)
    /// there is none: a stamp that lands in the window between the sweep's
    /// check and context_switch.S clearing `context_saving` parks the task as
    /// `Blocked + WAKE_STAMP` with the context fully saved — a state no
    /// commit will consume (not running), no sweep will convert (not
    /// switching), and no wake will dispatch (none is coming). Measured on
    /// QEMU `-icount`: phase A of `ipctest` wedges with the reply deposited
    /// and the client in exactly this state (2026-08-24).
    ///
    /// This transition is the missing consumer: `Blocked + WAKE_STAMP` →
    /// `Ready` (stamp cleared) in one CAS. The caller must verify TWO things
    /// first, and the CAS is only sound with both:
    ///
    ///  * `context_saving == false` — while the flag is true the target may
    ///    still be executing its retry loop, and its own
    ///    `commit_blocked_or_consume_wake` or the switch-away sweep still
    ///    owns the stamp.
    ///  * the task is **current on no hart** — the word has no generation,
    ///    so `Blocked+STAMP` can recur as the same bit pattern across a full
    ///    consume→run→re-block→re-stamp cycle, and `context_saving` sampled
    ///    in that window reads false (ABA, measured 2026-08-24: the first
    ///    reaper version enqueued the still-executing phase-A server, which
    ///    then parked `Ready` in no queue). Non-currency is what a recycled
    ///    pattern cannot fake: a parked `Blocked` task only becomes current
    ///    again by first leaving `Blocked`, which fails this CAS.
    ///
    /// With both checks a lost race here just means someone else delivered
    /// the wake, which the `false` return reports.
    ///
    /// Returns `true` if this call performed the recovery (the caller now
    /// owns the dispatch: clear `wait_reason`, enqueue — same contract as
    /// `WakeTransition::Dispatched`).
    #[inline]
    pub fn reap_orphaned_stamp(w: &AtomicU32) -> bool {
        loop {
            let cur = w.load(Ordering::Acquire);
            if cur & WAKE_STAMP == 0 || state_of(cur) != TaskState::Blocked {
                return false;
            }
            // AcqRel for the same reasons as the dispatch CAS in
            // `wake_transition`: Acquire so the dispatch bookkeeping sees the
            // blocker's writes, Release so Ready is ordered before the
            // enqueue that publishes it.
            if w.compare_exchange_weak(
                cur, pack(TaskState::Ready),
                Ordering::AcqRel, Ordering::Relaxed,
            ).is_ok() {
                return true;
            }
        }
    }

    /// Scheduler-side state change (Running/Ready/Zombie transitions made by
    /// the owning hart or under a queue lock). Preserves the stamp bit — a
    /// wake stamped while a task is Ready or Running must survive until its
    /// next `block_current()` consumes it, exactly as the separate
    /// `wake_pending` cell used to.
    #[inline]
    pub fn set_state(w: &AtomicU32, s: TaskState) {
        let mut cur = w.load(Ordering::Relaxed);
        loop {
            match w.compare_exchange_weak(
                cur, (cur & WAKE_STAMP) | pack(s),
                Ordering::Relaxed, Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }
}

// ---- Wait reason (AQ0: IO-wait scheduler) ----

/// Why a task is blocked. Used by wake functions to selectively unblock.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitReason {
    /// Not waiting (task is Ready/Running).
    None,
    /// Waiting for a specific IRQ from the PLIC.
    Irq(u32),
    /// Waiting for data on a channel handle.
    Channel(u32),
    /// Waiting for data on a ring buffer.
    Ring(u32),
    /// Waiting until a timestamp (CLINT ticks).
    Timer(u64),
    /// Waiting on an event port (any bound source).
    Port(u32),
    /// Waiting on a WaitQueue/Completion (woken by TID).
    WaitQueue,
    /// Waiting for an RPC reply (woken by IPC_REPLY with matching caller TID).
    Rpc(u32),
    /// Waiting for a fast IPC call to arrive (server side).
    /// u32 = this server's own TID (for targeted wake).
    FastIpcServer(u32),
    /// Waiting for a fast IPC reply from the server (client side).
    /// u64 = the generation-tagged exchange HANDLE (57-bit generation +
    /// 6-bit slot index, bit 63 clear — the encoding lives in
    /// `crates/ipc/src/fast_ipc.rs`), NOT a bare slot index. Carrying the
    /// handle is the client-side half of the slot-ABA closure: a wake for
    /// the exchange that died in a seat can never match the client of the
    /// exchange that re-let it.
    FastIpcClient(u64),
}

// ---- Deadline scheduling params (AQ7) ----

/// Deadline task parameters (Earliest Deadline First).
/// period=0 means this task is not a deadline task (uses round-robin).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DeadlineParams {
    /// Period in microseconds (how often the task must run).
    pub period_us: u64,
    /// Maximum runtime per period in microseconds.
    pub runtime_us: u64,
    /// Absolute deadline of current period (CLINT ticks).
    pub abs_deadline: u64,
    /// Remaining runtime in current period (CLINT ticks).
    pub remaining: u64,
}

// ---- Syscall filter (AQ11) ----

/// Maximum number of allowed syscalls per process.
pub const SYSCALL_FILTER_MAX: usize = 32;

/// Per-task syscall whitelist. If `enabled`, only listed syscalls are allowed.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SyscallFilter {
    pub enabled: bool,
    pub allowed: [u16; SYSCALL_FILTER_MAX],
    pub count: u8,
}

impl SyscallFilter {
    pub const fn disabled() -> Self {
        Self { enabled: false, allowed: [0; SYSCALL_FILTER_MAX], count: 0 }
    }

    pub fn is_allowed(&self, syscall_num: u16) -> bool {
        if !self.enabled { return true; }
        let n = self.count as usize;
        for i in 0..n {
            if self.allowed[i] == syscall_num { return true; }
        }
        false
    }

    pub fn allow(&mut self, syscall_num: u16) {
        if (self.count as usize) < SYSCALL_FILTER_MAX {
            self.allowed[self.count as usize] = syscall_num;
            self.count += 1;
        }
    }
}

// ---- Task Control Block (TCB) ----

/// Task Control Block.
///
/// `#[repr(C, align(64))]` ensures deterministic field layout and
/// cache-line alignment for performance.
///
/// `context` MUST remain at offset 0.
#[repr(C, align(64))]
pub struct Task {
    // == offset 0: context (MUST be first for context_switch.S) ==
    pub context:    TaskContext,  // 128 bytes

    // == task metadata (offset 128) ==
    pub tid:        u32,
    /// K-C19: [`TaskState`] + wake stamp, packed — see [`sched_word`].
    /// Same size and alignment as the plain `TaskState` field it replaced
    /// (`repr(u32)` enum → `AtomicU32`), so the `repr(C)` layout is
    /// unchanged. Zero-initialized = `Ready` + no stamp, which is the same
    /// default the two separate fields had.
    pub state_word: core::sync::atomic::AtomicU32,
    pub priority:      u32,
    pub base_priority: u32,      // original priority (for PI restore)
    pub time_slice:    u32,      // remaining ticks for current slice
    pub cpu_affinity:  i8,       // -1 = any CPU, 0..3 = pinned to that hart
    pub _pad:          [u8; 3],  // explicit padding for repr(C) alignment

    // == name ==
    pub name: [u8; TASK_NAME_CAPACITY],  // null-terminated ASCII

    // == stack info ==
    pub stack_idx:  usize,       // index into TASK_STACKS[]

    // == entry point ==
    pub entry_fn:  usize,        // fn ptr cast to usize
    pub entry_arg: usize,        // argument (raw pointer or integer)

    // == statistics ==
    pub total_runtime: u64,      // total timer ticks consumed

    // == IO-wait reason (AQ0) ==
    pub wait_reason: WaitReason,

    // == Deadline scheduling (AQ7) ==
    pub deadline: DeadlineParams,

    // == Syscall filter (AQ11) ==
    pub syscall_filter: SyscallFilter,

    // == user-space state (Phase 7 — requires MMU) ==
    pub task_satp: u64,
    pub user_pt:   u64,
    pub user_brk:  u64,

    // == PHANES Phase 1 W4-int — multi-policy scheduler metadata ==
    //
    // Appended at the end of the struct so the auto-injected
    // TASK_SATP_OFFSET used by context_switch.S stays stable.
    //
    // Stored as raw bytes (no `SchedClass` type import here) to keep
    // `task.rs` free of dependencies on the policy module hierarchy.
    // Helpers below convert to/from the typed enum.
    /// RFC-0004 scheduler-class discriminant. Defaults to
    /// `SchedClass::BestEffort` (= 3) so existing `task_create` calls
    /// continue to behave like CFS-style fair-share.
    pub sched_class_raw: u8,
    /// Explicit padding for repr(C) alignment.
    pub _sched_pad: [u8; 3],
    /// Per-task round-robin / CBS quantum in microseconds. `0` ⇒ use
    /// the policy's default quantum.
    pub sched_time_slice_us: u32,
    /// Absolute deadline in monotonic microseconds. `0` ⇒ "no
    /// deadline" (sentinel; real deadlines after boot are never 0).
    /// Read by the EDF + CBS policy.
    pub sched_deadline_us: u64,

    // == SMP context-switch safety (task-lifecycle race fix) ==
    //
    // `true` while this task's `context` field is mid-transition: its
    // state was just changed away from Running (blocked, or preempted
    // and re-enqueued) but `context_switch.S` has not yet finished
    // saving its registers. A waker on another hart must not dispatch
    // this task while `true` — cleared by the `fence rw, w` + `sb zero`
    // tail of the save path in `context_switch.S` (K-C23: the clear must
    // not be a Rust call, because a call runs on the OLD task's stack
    // after the store has published that stack as reusable); see the
    // spin-gate in `do_schedule()`.
    //
    // The static `TASKS` array is zero-initialized via
    // `core::mem::zeroed()` (see scheduler.rs), so `false` MUST be the
    // safe/default value — a freshly created task has never been
    // "saved" by context_switch.S and must still be dispatchable.
    pub context_saving: core::sync::atomic::AtomicBool,

    // == K-C9 note: the wake stamp lives in `state_word` now ==
    //
    // The lost-wakeup stamp ("a wake arrived between becoming wake-able and
    // committing to Blocked") used to be a separate `wake_pending:
    // AtomicBool` here. K-C19 showed that two independently-ordered cells
    // cannot close the race in either direction — the stamp and the state
    // must move under one CAS. It is now bit 3 of `state_word`; see
    // [`sched_word`] for the full protocol and the history.

    // == K-A15: fork() child hand-off, per-task instead of one global slot ==
    //
    // `sys_fork_impl()` (parent) writes where the newly created child should
    // resume in userspace (entry PC + user SP + child's own SATP);
    // `fork_child_entry()` (child, on whatever hart it gets dispatched to)
    // reads it back and SRETs there. This used to be a single global
    // `Option<ForkChildCtx>` slot: two concurrent forks (different parent
    // tasks, different harts) could overwrite each other's context before
    // either child read it back — a child could SRET with a *different*
    // process's SATP. Storing it on the CHILD's own Task struct (indexed by
    // its own, exclusively-owned pool slot) removes any cross-fork
    // ambiguity: only the one parent that created this child ever writes
    // these fields, and only this child ever reads them.
    //
    // `fork_ctx_ready` is the publish flag: the parent writes
    // entry/user_sp/satp first, then stores `true` here (Release); the
    // child polls it (Acquire) and only reads the payload fields once it
    // observes `true` — same publish protocol as `context_saving`. This
    // closes the residual window where the child is dispatched (e.g. on an
    // idle hart) before the parent — still running the tail of
    // sys_fork_impl() — has finished writing: instead of finding `None` and
    // silently exiting (the old bug), the child yields and retries a
    // bounded number of times.
    pub fork_ctx_ready:  core::sync::atomic::AtomicBool,
    pub fork_entry:      u64,
    pub fork_user_sp:    u64,
    pub fork_satp:       u64,
    /// The parent's complete user register file at the moment of its `ecall`
    /// (`x0..x31`, in trap-frame order).
    ///
    /// **WHY the whole file and not just entry/sp (K-C11).** The child used to
    /// enter user mode through `sret_to_user`, which writes `sepc`, `sstatus`,
    /// `sscratch`, `satp` and `sp`, and zeroes `a0..a7`. Nothing restored `ra`,
    /// `gp`, `tp`, `t0..t6` or `s0..s11`, so the child resumed *the parent's
    /// own code* holding whatever the kernel task that dispatched it had left
    /// in those registers. Two consequences, both observed from ring 3:
    ///
    ///  * **Correctness.** `fork()` returns into the middle of a function, and
    ///    the compiler keeps live values in callee-saved registers across the
    ///    `ecall`. A loop counter, a base address, a captured TID — all garbage
    ///    in the child. Measured: with the child index held in an `s` register,
    ///    every one of eight children believed it was child 0.
    ///  * **Disclosure.** The garbage is *kernel* state. `ra` was observed
    ///    arriving in user mode holding `0x8020198a`, a kernel text address —
    ///    a layout oracle handed to an unprivileged task on every fork.
    ///
    /// Costs 256 bytes per task slot (16 KiB at `MAX_TASKS` = 64), in BSS.
    ///
    /// Only the fork path uses this. A fresh `exec` still goes through
    /// `sret_to_user` and still zeroes the argument registers, which is both
    /// correct (the ELF entry ABI defines nothing) and what keeps that path
    /// from leaking the same kernel state.
    pub fork_regs:       [u64; 32],

    // == K-C21: exec() hand-off, per-task instead of one global slot ==
    //
    // `exec_user()` writes where the CURRENT task's fresh address space
    // starts (entry PC, user SP, initial SSTATUS, new SATP); the same task
    // consumes it moments later — at the tail of its own ecall arm (ring-3
    // callers) or straight after `exec_user` returns (kernel tasks: shell,
    // autorun). This used to be one global `SpinLock<Option<ExecContext>>`
    // drained at the end of EVERY U-mode ecall on EVERY hart: any other hart
    // finishing any syscall inside the window stole the context and SRET'd
    // into an address space that was never its own, while the real exec'er —
    // whose `task_satp` had already been switched — resumed its old sepc
    // under the new page table. Two concurrent SYS_EXECs also overwrote the
    // slot and leaked the first page table. Same shared-slot class K-A15
    // removed for fork; exec had been left behind.
    //
    // Unlike the fork trio there is no identity check: fork's writer
    // (parent) and reader (child) are different tasks on possibly different
    // harts, with a slot-reuse window in between — here writer and reader
    // are the SAME task inside the SAME syscall/trap, so the slot cannot be
    // reused or read by anyone else mid-flight. `exec_ctx_ready` still
    // publishes with Release/Acquire like `fork_ctx_ready` (cheap, and it
    // keeps the protocol uniform), and `task_create` clears it on slot
    // reuse. Zero-init (`false`) MUST stay the safe default: a fresh task
    // has no pending exec.
    pub exec_ctx_ready:  core::sync::atomic::AtomicBool,
    pub exec_entry:      u64,
    pub exec_user_sp:    u64,
    pub exec_sstatus:    u64,
    pub exec_satp:       u64,
    /// K-C22: the address space this task is abandoning (its pre-exec
    /// `user_pt`; 0 when a kernel task execs for the first time). Carried
    /// through the hand-off because only the CONSUMER may destroy it — after
    /// the hart's satp points at the new page table, never before. See
    /// `process::take_current_task_exec_ctx` for the full safety argument.
    pub exec_old_pt:     u64,

    // == priority-inheritance donation count ==
    //
    // Number of LEASE donations currently active on this task. Incremented by
    // every `boost_ready_task`, decremented by the matching
    // `restore_ready_task` — that pair is strictly balanced (one boost, one
    // restore, per `lease_wait_return` donor). `priority` returns to
    // `base_priority` only when this reaches 0.
    //
    // The PiMutex path (`pi_boost_task`/`pi_restore_task`) deliberately does
    // NOT count: its boost calls are level-triggered (re-asserted every
    // REASSERT_SPINS and once per extra waiter) against a single restore per
    // unlock, so counting them would drift the counter upward without bound
    // and pin the task boosted forever. `pi_restore_task` instead consults
    // this counter read-only and skips its hard reset while lease donors are
    // active.
    //
    // Without it, restores clobber each other. Two lessors donating to the same
    // lessee (`ipc/lease.rs`) each remember the priority they *observed* before
    // boosting, and nothing forces them to restore in LIFO order — each blocks
    // on its own lease id, and leases can be returned in any order. If the
    // outer donor restores first it drops the lessee below the inner donor's
    // level (reopening the very inversion that donation paid to avoid), and the
    // inner donor then restores to a stale value, leaving the task boosted
    // forever. Counting makes restore order irrelevant.
    //
    // The trade is deliberate: a task stays at the *highest* donation until the
    // last donor leaves, so it can be over-boosted briefly. That is bounded and
    // harmless; under-boosting reopens priority inversion and a leaked boost is
    // permanent. Fail towards over-boosting.
    //
    // Atomic because `boost_ready_task`/`restore_ready_task` are a documented
    // exception to this crate's POOL_LOCK discipline (see the module header):
    // they touch `TASKS[]` without holding it. A plain `u32` read-modify-write
    // would let two harts donating to the same task concurrently lose an
    // increment, and an undercounted task restores early — reintroducing the
    // exact clobber this field exists to prevent. `fetch_add`/`fetch_sub` make
    // the count itself race-free; the `priority` write next to it is still
    // unsynchronised, which is pre-existing and why the whole path stays gated
    // behind LEASE_PRIORITY_INHERITANCE (default n).
    //
    // Declared last: everything above `task_satp` is layout-frozen against
    // `TASK_SATP_OFFSET` in context_switch.S (see the assert at the bottom of
    // this file), so new fields go at the end. `AtomicU32` matches `u32` in
    // size and alignment, and zero-init via `mem::zeroed()` is valid for it.
    pub donation_count: core::sync::atomic::AtomicU32,

    /// K-C12 · `true` while this task occupies **exactly one** slot in
    /// **exactly one** per-CPU ready queue.
    ///
    /// **WHY.** `PrioQueue` is a fixed ring of `MAX_TASKS` entries and
    /// `cpu_enqueue` used to guard it with a `debug_assert!`, which release
    /// builds compile away: a full queue silently overwrote a live entry and
    /// pushed `count` past the ring, so ready tasks vanished with no error and
    /// `count` stopped describing the buffer. Under `panic = "abort"` the
    /// alternative — asserting for real — is a board reset, which on a robot
    /// is a physical-safety event, so neither "corrupt" nor "panic" is an
    /// acceptable answer.
    ///
    /// This flag removes the question instead of answering it. There are only
    /// `MAX_TASKS` task slots, so as long as no task is queued twice, the
    /// ready queues of one CPU can hold at most `MAX_TASKS` entries in total
    /// and a single-priority ring of that size **cannot** overflow. The flag
    /// is what enforces "no task queued twice" in O(1): `cpu_enqueue` *claims*
    /// it with an atomic swap and refuses when it was already set. A claim and
    /// not a test-then-set, because `CPU_LOCKS` only serializes enqueues onto
    /// one CPU — two harts enqueueing the same task onto two different CPUs
    /// hold different locks, so the swap is the only thing arbitrating between
    /// them.
    ///
    /// Refusing a duplicate never loses a wakeup, and that is the whole
    /// safety argument: the flag is only set while an entry for this task is
    /// actually sitting in a queue, so a refused enqueue is refused *because
    /// the task is already dispatchable*. Cleared by `cpu_dequeue` (the only
    /// pop), by `cpu_remove` (the priority re-bucketing path) and at slot
    /// allocation, since pool slots are recycled and a stale `true` would
    /// make the new occupant permanently un-enqueueable — i.e. exactly the
    /// silent starvation this field exists to prevent.
    ///
    /// Zero-init via `mem::zeroed()` gives `false`, which is correct: a fresh
    /// task is in no queue. Declared after `donation_count` for the same
    /// layout-freeze reason documented there.
    pub queued: core::sync::atomic::AtomicBool,
}

impl Task {
    /// Current [`TaskState`] (Relaxed). For scans and same-hart decisions.
    /// Wakers must not use this — they go through
    /// [`sched_word::wake_transition`], which couples the read to the CAS.
    #[inline]
    pub fn state(&self) -> TaskState {
        sched_word::state_of(self.state_word.load(core::sync::atomic::Ordering::Relaxed))
    }

    /// Current [`TaskState`] with Acquire: seeing `Blocked` here guarantees
    /// the `wait_reason` that task published is visible (pairs with the
    /// Release commit in [`sched_word::commit_blocked_or_consume_wake`]).
    /// K-C17's fence pairing, expressed as a load ordering.
    #[inline]
    pub fn state_acquire(&self) -> TaskState {
        sched_word::state_of(self.state_word.load(core::sync::atomic::Ordering::Acquire))
    }

    /// Scheduler-side state change; preserves the wake stamp. See
    /// [`sched_word::set_state`].
    #[inline]
    pub fn set_state(&self, s: TaskState) {
        sched_word::set_state(&self.state_word, s);
    }
}

// ---- K-C12: placement policy, isolated as pure logic ----

/// One CPU's load as seen by the placement policy.
///
/// Both counts are over the tasks *resident* on that hart — everything whose
/// home is that CPU, whatever state it is in right now — not over its ready
/// queue. That distinction is the fix; see [`pick_cpu_by_load`].
///
/// Approximate by design (sampled without the per-CPU queue locks); a stale
/// sample costs at worst one suboptimal placement, never correctness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuLoad {
    /// Outranking residents that are in the **hard real-time band**
    /// ([`is_rt_priority`]). Ranked ahead of everything else because this
    /// scheduler never lets the timer preempt them (`RT_TIME_SLICE_TICKS` is
    /// 0 and `schedule()` skips them) — they run until they block, by design,
    /// and on a hart dedicated to them (`rt-motor` and `flight-ctrl` are both
    /// pinned to hart 0 "to avoid jitter") they are the whole point of that
    /// hart. Best-effort work does not belong there.
    pub rt_blocking: u32,
    /// All resident tasks on this CPU that **outrank** the task being placed
    /// (strictly lower priority number), real-time band included. Dispatch is
    /// strict priority with no aging, so even a time-sliced higher-priority
    /// task that never blocks starves the newcomer — time slicing only
    /// rotates *within* one priority level.
    pub blocking: u32,
    /// Total resident tasks on this CPU, at any priority.
    pub total: u32,
}

/// Pick the CPU a task of a given priority should be placed on.
///
/// # K-C12: why "fewest queued tasks" was the wrong question
///
/// The previous policy (`find_least_loaded_cpu`) minimised the number of
/// *queued* tasks. That metric is blind in the two ways that matter:
///
///  * It counts only what is queued **right now**. A task that is `Running`,
///    or a periodic real-time task that happens to be `Blocked` between
///    activations, is in no ready queue — so the harts most hostile to
///    low-priority work sample as the emptiest ones.
///  * It ignores **priority** entirely, while dispatch
///    (`cpu_dequeue` → `ready_bitmap.trailing_zeros()`) is strict priority
///    with no aging. A task placed behind a higher-priority task that never
///    stops being runnable is not "slower": it never runs at all.
///
/// Measured, not argued (ring-3 probe `userspace/ipctest`, `-smp 4`): of 104
/// `fork()`s, 98 children were placed on a hart with no higher-priority work
/// and **all 98 ran**; 3 landed on hart 0 (`rt-motor` + `flight-ctrl`, both
/// priority 8) and 2 on hart 1 (`imu` priority 8), and **all 5 never executed
/// a single instruction** — no fault, no log line, `fork()` having already
/// returned a positive TID to the parent. That is K-C12, and the correlation
/// was 5 of 5.
///
/// So the question is not "which hart is emptiest" but "which hart will
/// actually dispatch this task": order by `(rt_blocking, blocking, total)`.
/// Ties resolve to the lowest index so the choice is deterministic and
/// reproducible in tests.
///
/// **Why `rt_blocking` is a separate key, and not just part of `blocking`.**
/// The first version of this ranked on `(blocking, total)`. It worked until
/// the unpinned mid-priority tasks (`shell` p13, `behavior`/`odom`/
/// `sensor-ahrs` p14) had migrated around and every hart carried two
/// outranking residents — at which point `total` decided, and hart 0 won it,
/// because all the forked children had gone elsewhere and it looked lightly
/// loaded. Measured, from the probe: `online=4 best_for_p16=0 cpu0.blk=2
/// cpu2.blk=2`, with the children that landed there sitting `Ready` and
/// un-dispatched for the rest of the run. Two blockers at priority 8 on a
/// hart dedicated to real-time control are not the same hazard as two at
/// priority 13/14 that sleep between activations, and a count that cannot
/// tell them apart hands best-effort work to the one hart guaranteed never
/// to run it.
///
/// **Residual, deliberately not fixed here.** This is placement, not a
/// starvation guarantee. If *every* hart is saturated by higher-priority
/// work, a low-priority task still starves — that is inherent to strict
/// priority without aging, and adding aging changes the dispatch guarantees
/// the RT and PiMutex scenarios depend on. It belongs in its own pass.
///
/// **Known consequence, measured.** On this kernel's default boot layout
/// exactly one hart (2) has no real-time residents, so unpinned best-effort
/// work concentrates there: all 105 `fork()` children of a `-smp 4` ipctest
/// run landed on hart 2. Liveness is worth that, but note what it does to
/// cross-hart coverage. `ci_check.sh` says in its own comment that `-smp 4`
/// is not decorative for the `userspace: IPC` scenario, because phase A
/// exists to open the window between `wake_fast_ipc_server()` and
/// `task_block()`. That window is still opened today only because the phase-A
/// *server* is the `autorun` parent, pinned to hart 3 at priority 10, while
/// its clients sit on hart 2 — measured, `tid=17 tp=3` against `tid=12x
/// tp=2`. Two peers that are both unpinned best-effort tasks would now share
/// a hart, and a scenario that needs them apart must pin them rather than
/// rely on placement scattering them.
///
/// Returns 0 for an empty slice (there is always at least hart 0).
pub fn pick_cpu_by_load(loads: &[CpuLoad]) -> usize {
    let mut best = 0usize;
    let mut best_load = match loads.first() {
        Some(l) => *l,
        None => return 0,
    };
    for (i, l) in loads.iter().enumerate().skip(1) {
        if (l.rt_blocking, l.blocking, l.total)
            < (best_load.rt_blocking, best_load.blocking, best_load.total)
        {
            best = i;
            best_load = *l;
        }
    }
    best
}

// ---- K-C12: ready-queue admission policy, isolated as pure logic ----

/// What `cpu_enqueue` must do with one enqueue request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Append the task to the ring and mark it queued.
    Append,
    /// The task is already in a ready queue. Do nothing: an entry for it
    /// already exists, so nothing is lost and the ring stays consistent.
    AlreadyQueued,
    /// The ring is full. Do **not** write — the old code overwrote a live
    /// entry here — and report the refusal to the caller.
    ///
    /// Unreachable while `Task::queued` holds (`MAX_TASKS` distinct tasks
    /// cannot fill an `MAX_TASKS`-entry ring and still have one left over),
    /// which is exactly why it may log loudly instead of trying to recover.
    Full,
}

/// The `cpu_enqueue` admission decision, free of statics and assembly so the
/// host suite can exercise it (`crates/sched-wake-tests`).
///
/// `capacity` is the ring size (`MAX_TASKS`); `count` is its current
/// occupancy.
#[inline]
pub fn enqueue_decision(already_queued: bool, count: usize, capacity: usize) -> EnqueueOutcome {
    if already_queued {
        EnqueueOutcome::AlreadyQueued
    } else if count >= capacity {
        EnqueueOutcome::Full
    } else {
        EnqueueOutcome::Append
    }
}

// ---- W4-int helpers around the new fields ----

/// Default scheduler class for the legacy `task_create()` path. Maps
/// to `SchedClass::BestEffort` so existing tasks keep their old
/// behaviour even after the multi-policy machinery is wired in.
pub const DEFAULT_SCHED_CLASS_RAW: u8 = 3; // SchedClass::BestEffort

/// Sentinel value for "no deadline".
pub const NO_DEADLINE: u64 = 0;

// TaskContext at offset 0; tid follows immediately after
const _: () = assert!(core::mem::offset_of!(Task, tid) == 128);

// task_satp offset MUST match TASK_SATP_OFFSET in context_switch.S.
// If this assert fails, update .equ TASK_SATP_OFFSET in context_switch.S.
pub const TASK_SATP_OFFSET: usize = core::mem::offset_of!(Task, task_satp);
// Verify the offset matches what context_switch.S expects.
// If fields are added/removed above task_satp, update TASK_SATP_OFFSET in the .S file.
const _: () = assert!(TASK_SATP_OFFSET == 336, "Update TASK_SATP_OFFSET in context_switch.S to 336");
