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
/// Each timer tick is 10ms; one tick per time slice.
pub const TIME_SLICE_TICKS: u32 = 1;

/// Stack size for kernel tasks.
#[cfg(not(feature = "esp32c3"))]
pub const STACK_SIZE: usize = 8 * 1024;
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
}

// Compile-time check: TaskContext is 15 fields × register_width
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<TaskContext>() == 120);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::size_of::<TaskContext>() == 60);

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
    pub priority:   u32,
    pub time_slice: u32,         // remaining ticks for current slice

    // == name ==
    pub name: [u8; 32],          // null-terminated ASCII

    // == stack info ==
    pub stack_idx:  usize,       // index into TASK_STACKS[]

    // == entry point ==
    pub entry_fn:  usize,        // fn ptr cast to usize
    pub entry_arg: usize,        // argument (raw pointer or integer)

    // == statistics ==
    pub total_runtime: u64,      // total timer ticks consumed

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
const _: () = assert!(core::mem::offset_of!(Task, tid) == 120);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::offset_of!(Task, tid) == 60);

// task_satp offset must match TASK_SATP_OFFSET in context_switch.S (200)
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::offset_of!(Task, task_satp) == 200);
