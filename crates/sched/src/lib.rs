#![no_std]

pub mod task;
pub mod scheduler;
pub mod smp;
pub mod wait;
pub mod seccomp;
pub mod driver;
#[cfg(target_pointer_width = "64")]
pub mod process;

pub use scheduler::{
    init, start, schedule, task_create, task_create_affinity, task_yield, task_exit,
    current_task_name, current_task_tid,
    current_user_pt,
    stack_canary_check, MAX_CPUS, STACK_CANARY,
    pi_boost_task, pi_restore_task,
    alloc_asid,
    wq_block_current, wq_wake_by_tid,
    current_syscall_filter, set_current_syscall_filter,
    set_task_syscall_filter, task_create_filtered,
    task_set_deadline, deadline_admission_check,
};

#[cfg(not(any(feature = "no-mmu", feature = "esp32c3")))]
pub use scheduler::setup_stack_guard_pages;

#[cfg(target_pointer_width = "64")]
pub use scheduler::{
    set_current_user_info, set_task_user_info, update_user_brk,
};

pub use task::{
    DEFAULT_PRIORITY, IDLE_PRIORITY, STACK_SIZE, MAX_TASKS,
    RT_MOTOR_PRIORITY, NET_POLL_PRIORITY, BEHAVIOR_PRIORITY,
    SENSOR_AHRS_PRIORITY, FLIGHT_CTRL_PRIORITY, WATCHDOG_PRIORITY,
    RT_PRIORITY_THRESHOLD, RT_TIME_SLICE_TICKS,
    WaitReason, DeadlineParams, SyscallFilter, SYSCALL_FILTER_MAX,
};

pub use driver::{
    driver_register, driver_set_mmio, driver_set_irq,
    driver_start, driver_heartbeat, driver_heartbeat_with_time,
    driver_on_crash, driver_on_crash_with_time, driver_check_health,
    driver_info, driver_count,
    driver_add_spawn_descriptor, driver_spawn_count, driver_spawn_descriptor,
    driver_spawn_register, driver_get_restart_list,
    DriverState, DriverEntry, DriverDescriptor, MmioRegion,
};

pub use wait::{
    task_block,
    wake_by_irq, wake_by_channel, wake_by_ring, wake_by_port,
    wake_expired_timers, wake_by_rpc,
};

#[cfg(target_pointer_width = "64")]
pub use process::{
    exec_user, take_pending_exec, sret_to_user, ExecContext,
    copy_from_user, copy_to_user, copy_cstr_from_user, sys_brk_impl,
    set_ecall_context, mmio_map_user,
};

// ── RV32 stubs for process functions ────────────────────────────────────────
// On RV32 (ESP32-C3), there is no MMU — copy_from/to_user do identity copies,
// and mmap/brk/exec functions are no-ops or return errors.

#[cfg(target_pointer_width = "32")]
pub struct ExecContext {
    pub satp: u64,
    pub entry: u64,
    pub user_sp: u64,
    pub sstatus: u64,
    pub user_pt: u64,
    pub brk: u64,
}

#[cfg(target_pointer_width = "32")]
pub fn copy_from_user(kernel_dst: *mut u8, user_src: usize, len: usize) -> bool {
    unsafe { core::ptr::copy_nonoverlapping(user_src as *const u8, kernel_dst, len); }
    true
}

#[cfg(target_pointer_width = "32")]
pub fn copy_to_user(user_dst: usize, kernel_src: *const u8, len: usize) -> bool {
    unsafe { core::ptr::copy_nonoverlapping(kernel_src, user_dst as *mut u8, len); }
    true
}

#[cfg(target_pointer_width = "32")]
pub fn copy_cstr_from_user(buf: &mut [u8], user_ptr: usize) -> Option<usize> {
    let src = user_ptr as *const u8;
    for i in 0..buf.len() {
        let c = unsafe { core::ptr::read_volatile(src.add(i)) };
        buf[i] = c;
        if c == 0 { return Some(i); }
    }
    None
}

#[cfg(target_pointer_width = "32")]
pub fn update_user_brk(_addr: u64) -> u64 { 0 }

#[cfg(target_pointer_width = "32")]
pub fn sys_brk_impl(_addr: u64) -> i64 { -1 }

#[cfg(target_pointer_width = "32")]
pub fn set_ecall_context(_sepc: impl Into<u64>, _user_sp: impl Into<u64>) {}

#[cfg(target_pointer_width = "32")]
pub fn take_pending_exec() -> Option<ExecContext> { None }

#[cfg(target_pointer_width = "32")]
pub fn sret_to_user(_ctx: &ExecContext) {}

#[cfg(target_pointer_width = "32")]
pub fn exec_user(_path: &[u8]) -> Result<ExecContext, ()> { Err(()) }
