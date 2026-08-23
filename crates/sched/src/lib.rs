#![no_std]

// PHANES Phase 1 W4 — multi-policy scheduler scaffolding.
// New code lives alongside the existing scheduler; integration into
// the live dispatch path is a separate wave.
pub mod aps_state;
pub mod class;
pub mod partitions;
pub mod policies;
// RFC-0002 runtime layer for the scheduler subsystem. Phase 1: typed
// wrapper over the existing legacy/APS toggle; reserved enum slots
// for the per-policy standalone backends that land in Phase 2+.
pub mod runtime;

pub mod task;
pub mod scheduler;
pub mod smp;
pub mod wait;
pub mod seccomp;
pub mod driver;
pub mod process;

pub use scheduler::{
    init, start, schedule, task_create, task_create_affinity, try_task_create_affinity,
    task_yield, task_exit, set_task_exit_hook,
    task_create_with_class, task_set_class, idx_for_tid, tid_for_idx,
    aps_dispatch_enabled, use_aps_dispatch,
    current_task_name, current_task_tid, current_task_stack_top,
    current_user_pt,
    stack_canary_check, MAX_CPUS, STACK_CANARY,
    pi_boost_task, pi_restore_task, boost_ready_task, restore_ready_task, task_priority,
    task_census, wake_counters, blocked_fastipc_ids, ready_unqueued_ids,
    reap_stamped_sleepers, current_snapshot,
    alloc_asid,
    wq_block_current, wq_wake_by_tid,
    current_syscall_filter, set_current_syscall_filter,
    set_task_syscall_filter, task_create_filtered,
    task_set_deadline, deadline_admission_check,
    nearest_timer_deadline,
    preempt_disable, preempt_enable, preempt_disabled,
    rebalance_from_offline_cpus,
};

#[cfg(not(feature = "no-mmu"))]
pub use scheduler::setup_stack_guard_pages;

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
    // `driver_heartbeat` (no timestamp) was removed — see driver.rs for why.
    driver_start, driver_heartbeat_with_time,
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
    wake_fast_ipc_server, wake_fast_ipc_client,
};

pub use process::{
    exec_user, take_current_task_exec_ctx, sret_to_user, ExecHandoff,
    copy_from_user, copy_to_user, copy_cstr_from_user, sys_brk_impl,
    mmio_map_user, shm_map_user,
};
