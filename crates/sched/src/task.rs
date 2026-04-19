/// Task definitions for the Robot OS scheduler.
///
/// Ported from kernel/include/sched.h

#[cfg(not(feature = "esp32c3"))]
pub const MAX_TASKS: usize = 64;
#[cfg(feature = "esp32c3")]
pub const MAX_TASKS: usize = 8;

pub const NUM_PRIORITIES: usize = 32;
pub const DEFAULT_PRIORITY: u32 = 16;
pub const IDLE_PRIORITY: u32 = 31;

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
/// 16 KiB total — with guard page (4 KiB unmapped at bottom), 12 KiB usable.
/// Rust kernel tasks with nested calls (PID, flight controller, kprintln
/// formatting) need substantial stack space.
#[cfg(not(feature = "esp32c3"))]
pub const STACK_SIZE: usize = 16 * 1024;
#[cfg(feature = "esp32c3")]
pub const STACK_SIZE: usize = 2 * 1024;

// ---- Register-width type for context (u64 on RV64, u32 on RV32) ----

#[cfg(target_pointer_width = "64")]
pub type CtxReg = u64;
#[cfg(target_pointer_width = "32")]
pub type CtxReg = u32;

// ---- Task context (callee-saved registers for context switch) ----

/// Saved CPU state during a context switch.
///
/// **MUST be the first field of `Task`** (at offset 0) because
/// `context_switch.S` accesses these fields directly from the task pointer.
///
/// RV64 offsets (8 bytes each): ra=0, sp=8, ..., pc=112 (120 bytes)
/// RV32 offsets (4 bytes each): ra=0, sp=4, ..., pc=56  (60 bytes)
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
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<TaskContext>() == 128);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::size_of::<TaskContext>() == 64);

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
    /// u32 = slot_idx used to collect the reply.
    FastIpcClient(u32),
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
    pub context:    TaskContext,  // 120 bytes (RV64) / 60 bytes (RV32)

    // == task metadata (offset 120 on RV64, 60 on RV32) ==
    pub tid:        u32,
    pub state:      TaskState,
    pub priority:      u32,
    pub base_priority: u32,      // original priority (for PI restore)
    pub time_slice:    u32,      // remaining ticks for current slice
    pub cpu_affinity:  i8,       // -1 = any CPU, 0..3 = pinned to that hart
    pub _pad:          [u8; 3],  // explicit padding for repr(C) alignment

    // == name ==
    pub name: [u8; 32],          // null-terminated ASCII

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

    // == user-space state (Phase 7, RV64 only — requires MMU) ==
    #[cfg(target_pointer_width = "64")]
    pub task_satp: u64,
    #[cfg(target_pointer_width = "64")]
    pub user_pt:   u64,
    #[cfg(target_pointer_width = "64")]
    pub user_brk:  u64,
}

// TaskContext at offset 0; tid follows immediately after
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::offset_of!(Task, tid) == 128);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::offset_of!(Task, tid) == 64);

// task_satp offset MUST match TASK_SATP_OFFSET in context_switch.S.
// If this assert fails, update .equ TASK_SATP_OFFSET in context_switch.S.
#[cfg(target_pointer_width = "64")]
pub const TASK_SATP_OFFSET: usize = core::mem::offset_of!(Task, task_satp);
// Verify the offset matches what context_switch.S expects.
// If fields are added/removed above task_satp, update TASK_SATP_OFFSET in both .S files.
#[cfg(target_pointer_width = "64")]
#[cfg(target_pointer_width = "64")]
const _: () = assert!(TASK_SATP_OFFSET == 336, "Update TASK_SATP_OFFSET in context_switch.S to 336");
