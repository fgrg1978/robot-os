//! Cap<Pwm> typed wrappers — RFC-0003 W5 batch 5.3.
//!
//! `Cap<Pwm>` identifies one PWM channel. The `resource_id` is
//! the channel number directly (0..[`PWM_MAX_CHANNELS`]).

use crate::cap::{Cap, CapError, CapPerms, CapTable};
use robot_os_drivers::pwm::{
    pwm_disable, pwm_enable, pwm_set_duty, pwm_set_duty_pct, pwm_set_period,
    PWM_MAX_CHANNELS,
};

/// Errors returned by the typed `pwm_*_cap` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PwmCapError {
    Cap(CapError),
    BadChannel,
    DriverFault,
}

impl From<CapError> for PwmCapError {
    fn from(e: CapError) -> Self {
        Self::Cap(e)
    }
}

/// Topology-loader entry: grant `tid` a `Cap<Pwm>` for `channel`.
pub fn pwm_grant_cap(
    tid: u32,
    channel: u32,
    perms: CapPerms,
) -> Option<Cap<crate::cap::targets::Pwm>> {
    if (channel as usize) >= PWM_MAX_CHANNELS {
        return None;
    }
    crate::cap_store::grant::<crate::cap::targets::Pwm>(tid, perms, channel)
}

fn resolve_channel(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Pwm>,
    perms: CapPerms,
) -> Result<u32, PwmCapError> {
    let ch = table.get(cap, perms)?;
    if (ch as usize) >= PWM_MAX_CHANNELS {
        return Err(PwmCapError::BadChannel);
    }
    Ok(ch)
}

fn rc_to_result(rc: i32) -> Result<(), PwmCapError> {
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmCapError::DriverFault)
    }
}

/// Typed `pwm_enable`: requires `WRITE`.
pub fn pwm_enable_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Pwm>,
) -> Result<(), PwmCapError> {
    let ch = resolve_channel(table, cap, CapPerms::WRITE)?;
    rc_to_result(pwm_enable(ch))
}

/// Typed `pwm_disable`: requires `WRITE`.
pub fn pwm_disable_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Pwm>,
) -> Result<(), PwmCapError> {
    let ch = resolve_channel(table, cap, CapPerms::WRITE)?;
    rc_to_result(pwm_disable(ch))
}

/// Typed `pwm_set_period`: requires `WRITE`. `period_ns` is in
/// nanoseconds.
pub fn pwm_set_period_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Pwm>,
    period_ns: u32,
) -> Result<(), PwmCapError> {
    let ch = resolve_channel(table, cap, CapPerms::WRITE)?;
    rc_to_result(pwm_set_period(ch, period_ns))
}

/// Typed `pwm_set_duty`: requires `WRITE`. `duty_ns` is the on-
/// time in nanoseconds.
pub fn pwm_set_duty_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Pwm>,
    duty_ns: u32,
) -> Result<(), PwmCapError> {
    let ch = resolve_channel(table, cap, CapPerms::WRITE)?;
    rc_to_result(pwm_set_duty(ch, duty_ns))
}

/// Typed `pwm_set_duty_pct`: requires `WRITE`. `pct` is the
/// duty cycle in integer percent (0..=100).
pub fn pwm_set_duty_pct_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Pwm>,
    pct: u32,
) -> Result<(), PwmCapError> {
    let ch = resolve_channel(table, cap, CapPerms::WRITE)?;
    rc_to_result(pwm_set_duty_pct(ch, pct))
}
