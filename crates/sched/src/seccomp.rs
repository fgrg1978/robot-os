//! Syscall filter profiles — predefined whitelists per task role (AQ11).
//!
//! Profiles restrict which syscalls a task can use. Once activated via
//! SYS_SECCOMP, the filter cannot be removed (one-way escalation).
//!
//! Profiles:
//!   UNRESTRICTED — all syscalls allowed (kernel tasks, default)
//!   SENSOR       — sensor reads, GPIO input, ADC, and I2C *read and write*.
//!                  No net, no fs, no PWM/motor syscalls.
//!   MOTOR        — PWM, motor control, GPIO output. No net, no fs.
//!   NET          — sockets, DNS. No GPIO, no motors, no sensors.
//!   MINIMAL      — only exit, yield, sleep, write(stdout), brk.
//!
//! WHAT `SENSOR` DOES NOT GUARANTEE (corrected claim — read this before
//! trusting the profile with anything):
//!
//! This header used to say SENSOR meant "No motors". It does not, and the
//! allowlist is the honest half of the pair. SENSOR grants `SYS_I2C_WRITE`,
//! whose signature is `sys_i2c_write(bus, addr, ptr, len)` — the target
//! device address is a caller-supplied argument, so the grant is *bus-wide*.
//! On a board with an I2C-attached motor controller (PCA9685, an I2C ESC),
//! a SENSOR-profiled task can drive the motors through it. Seccomp filters
//! syscall *numbers*; it cannot see arguments, so no arrangement of this
//! allowlist can express "I2C, but not that address".
//!
//! The allowlist is nonetheless correct as written: essentially every I2C
//! sensor (MPU-6050, BME280, VL53L0X…) requires register-select and init
//! writes before it can be read at all. Removing `SYS_I2C_WRITE` would not
//! tighten the profile, it would make it non-functional — so the header was
//! the thing that had to change, not the profile.
//!
//! What actually scopes I2C to a device today is the separate capability
//! check inside `sys_i2c_write` — bus/addr packed identically to the typed
//! `Cap<I2c>` resource in `crates/ipc/src/i2c_cap.rs` (`bus << 8 | addr`).
//! Address-scoped caps are the real fix; this profile is a coarse second layer, not the
//! boundary between a sensor task and the motors.

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
///
/// Returns `None` for an id that names no profile. The `Option` is the
/// whole point: `PROFILE_UNRESTRICTED` legitimately maps to a *disabled*
/// filter, so a bare `SyscallFilter` return value cannot distinguish
/// "the caller asked for no sandbox" from "the caller asked for a
/// sandbox that does not exist". The previous `_ => disabled()` arm
/// collapsed the two, so a typo'd profile id silently produced an
/// unrestricted task while every caller was told it had succeeded.
/// Callers MUST decide explicitly what an unknown id means for them.
pub fn profile_to_filter(profile_id: u64) -> Option<SyscallFilter> {
    Some(match profile_id {
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
        _ => return None, // unknown id — NOT "unrestricted"; see the doc above
    })
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

/// Error: the task already has a filter installed (one-way, no downgrade).
pub const SECCOMP_E_ALREADY: i64 = -1;
/// Error: `profile_id` names no profile. Nothing was installed.
pub const SECCOMP_E_BADPROFILE: i64 = -2;

/// Activate a security profile on the current task (one-way).
///
/// Returns 0 on success, [`SECCOMP_E_ALREADY`] if a filter is already
/// installed, [`SECCOMP_E_BADPROFILE`] for an unknown profile id.
///
/// The unknown-id check happens BEFORE anything is installed. It used to
/// fall through to a disabled (= unrestricted) filter and still return 0:
/// a task that meant to sandbox itself but passed a typo'd id was told it
/// had succeeded and kept full syscall authority. A security mechanism
/// that reports success while installing nothing is worse than none at
/// all, because the caller stops looking.
///
/// Note for callers: `activate_profile(PROFILE_UNRESTRICTED)` is a
/// deliberate no-op that returns 0 — it installs a disabled filter, so
/// `enabled` stays false and the one-way gate above is NOT burned. That
/// is by design (it names "no sandbox"), but it means a 0 return does not
/// on its own prove the task is now confined.
pub fn activate_profile(profile_id: u64) -> i64 {
    // Reject the unknown id first — never touch the task's filter on a
    // request we could not honour.
    let filter = match profile_to_filter(profile_id) {
        Some(f) => f,
        None => return SECCOMP_E_BADPROFILE,
    };
    let current = crate::scheduler::current_syscall_filter();
    if current.enabled {
        return SECCOMP_E_ALREADY; // already filtered — one-way, can't change
    }
    crate::scheduler::set_current_syscall_filter(filter);
    0
}
