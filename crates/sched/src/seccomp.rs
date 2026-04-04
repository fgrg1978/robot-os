//! Syscall filter profiles — predefined whitelists per task role (AQ11).
//!
//! Profiles restrict which syscalls a task can use. Once activated via
//! SYS_SECCOMP, the filter cannot be removed (one-way escalation).
//!
//! Profiles:
//!   UNRESTRICTED — all syscalls allowed (kernel tasks, default)
//!   SENSOR       — read sensors, I2C, GPIO input. No motors, no net, no fs.
//!   MOTOR        — PWM, motor control, GPIO output. No net, no fs.
//!   NET          — sockets, DNS. No GPIO, no motors, no sensors.
//!   MINIMAL      — only exit, yield, sleep, write(stdout), brk.

use crate::task::SyscallFilter;

// Re-import syscall numbers for building profiles.
// These must match robot_os_syscall::numbers exactly.
const SYS_EXIT: u16 = 3;
const SYS_GETPID: u16 = 10;
const SYS_YIELD: u16 = 11;
const SYS_SLEEP: u16 = 15;
const SYS_WRITE: u16 = 23;
const SYS_BRK: u16 = 400;
const SYS_PUTCHAR: u16 = 1;

// Sensor-related
const SYS_SENSOR_READ: u16 = 332;
const SYS_GPIO_READ: u16 = 200;
const SYS_I2C_READ: u16 = 220;
const SYS_I2C_WRITE: u16 = 221;
const SYS_ADC_READ: u16 = 410;

// Motor-related
const SYS_GPIO_WRITE: u16 = 201;
const SYS_GPIO_MODE: u16 = 202;
const SYS_PWM_ENABLE: u16 = 210;
const SYS_PWM_DISABLE: u16 = 211;
const SYS_PWM_SET_FREQ: u16 = 212;
const SYS_PWM_SET_DUTY: u16 = 213;
const SYS_MOTOR_CREATE: u16 = 230;
const SYS_MOTOR_ENABLE: u16 = 231;
const SYS_MOTOR_SPEED: u16 = 232;

// Network-related
const SYS_SOCKET: u16 = 370;
const SYS_BIND: u16 = 371;
const SYS_LISTEN: u16 = 372;
const SYS_ACCEPT: u16 = 373;
const SYS_CONNECT: u16 = 374;
const SYS_SEND: u16 = 375;
const SYS_RECV: u16 = 376;

// Seccomp itself (must be allowed to activate)
const SYS_SECCOMP: u16 = 430;

/// Profile IDs (passed via SYS_SECCOMP a0 argument).
pub const PROFILE_UNRESTRICTED: u64 = 0;
pub const PROFILE_SENSOR: u64 = 1;
pub const PROFILE_MOTOR: u64 = 2;
pub const PROFILE_NET: u64 = 3;
pub const PROFILE_MINIMAL: u64 = 4;

/// Common syscalls allowed in ALL restricted profiles.
const COMMON: &[u16] = &[
    SYS_EXIT, SYS_GETPID, SYS_YIELD, SYS_SLEEP,
    SYS_WRITE, SYS_PUTCHAR, SYS_BRK, SYS_SECCOMP,
];

/// Build a SyscallFilter from a profile ID.
pub fn profile_to_filter(profile_id: u64) -> SyscallFilter {
    match profile_id {
        PROFILE_UNRESTRICTED => SyscallFilter::disabled(),
        PROFILE_SENSOR => build_filter(&[
            SYS_SENSOR_READ, SYS_GPIO_READ, SYS_I2C_READ,
            SYS_I2C_WRITE, SYS_ADC_READ,
        ]),
        PROFILE_MOTOR => build_filter(&[
            SYS_GPIO_WRITE, SYS_GPIO_MODE,
            SYS_PWM_ENABLE, SYS_PWM_DISABLE, SYS_PWM_SET_FREQ, SYS_PWM_SET_DUTY,
            SYS_MOTOR_CREATE, SYS_MOTOR_ENABLE, SYS_MOTOR_SPEED,
        ]),
        PROFILE_NET => build_filter(&[
            SYS_SOCKET, SYS_BIND, SYS_LISTEN, SYS_ACCEPT,
            SYS_CONNECT, SYS_SEND, SYS_RECV,
        ]),
        PROFILE_MINIMAL => build_filter(&[]),
        _ => SyscallFilter::disabled(), // unknown → unrestricted (safe default)
    }
}

/// Build a filter from common + extra syscalls.
fn build_filter(extra: &[u16]) -> SyscallFilter {
    let mut filter = SyscallFilter::disabled();
    filter.enabled = true;
    for &s in COMMON {
        filter.allow(s);
    }
    for &s in extra {
        filter.allow(s);
    }
    filter
}

/// Activate a security profile on the current task (one-way).
/// Returns 0 on success, -1 if already filtered (can't downgrade).
pub fn activate_profile(profile_id: u64) -> i64 {
    let current = crate::scheduler::current_syscall_filter();
    if current.enabled {
        return -1; // already filtered — one-way, can't change
    }
    let filter = profile_to_filter(profile_id);
    crate::scheduler::set_current_syscall_filter(filter);
    0
}
