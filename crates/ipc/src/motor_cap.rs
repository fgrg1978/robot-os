//! Cap<Motor> typed wrappers — RFC-0003 W5 batch 5.4.
//!
//! **Granularity decision (2026-08-24, user decision, migration
//! phase P1).** `Cap<Motor>` is scoped **per physical motor**:
//! `motor_grant_cap(tid, motor_id, perms)` mints a cap whose `resource`
//! IS `motor_id` (0 = left wheel, 1 = right wheel on the current
//! two-wheel drivetrain). This used to hard-code `resource = 0` for
//! every grant, on the theory that "there is one motor controller per
//! robot" — true of the *PID loop* (`robot_os_drivers::motor_pid`,
//! which has no per-wheel API at all: `motor_pid_set_target` takes both
//! `speed_l`/`speed_r` in one call), but wrong for the *authority* a cap
//! should represent: the legacy path already grants and checks
//! `Motor(0)`/`Motor(1)` separately (`kernel/src/main.rs` autorun seed,
//! `crates/syscall/src/handlers.rs::sys_motor_enable/speed`), and
//! `crates/ipc/src/io_ring.rs`'s `OP_MOTOR_SPEED` deliberately requires
//! write on **both** ids before driving the pair — see
//! `motor_speed_requires_write_on_both_wheels` there. Collapsing the
//! typed side to one shared resource id would have silently *widened*
//! authority relative to the legacy path it's meant to replace: a task
//! holding a cap minted for "the left wheel" would functionally control
//! both. Per-motor granularity preserves wheel-level isolation and
//! keeps the migration authority-preserving, not authority-expanding.
//!
//! Every operation below that actuates the shared PID loop (`WRITE`:
//! `set_target`, `tick`, `enable`, `set_gains`, `reset`) therefore
//! requires WRITE on **both** `Motor(0)` and `Motor(1)` — see
//! [`require_pair_write`] — mirroring `OP_MOTOR_SPEED`'s rule exactly.
//! The one READ-only query (`motor_enabled_cap`, `SYS_MOTOR_ENABLED_TYPED`
//! 553) is not an actuation and is deliberately left single-cap: it
//! reports shared state, and a task's own single-wheel READ authority is
//! enough to observe it — least-privilege, not a safety property, so no
//! pairing rule applies there.

use crate::cap::{Cap, CapError, CapKind, CapPerms, CapTable};
use robot_os_drivers::motor_pid::{
    motor_pid_enable, motor_pid_enabled, motor_pid_reset, motor_pid_set_gains,
    motor_pid_set_target, motor_pid_tick,
};

/// Errors returned by the typed `motor_*_cap` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotorCapError {
    Cap(CapError),
}

impl From<CapError> for MotorCapError {
    fn from(e: CapError) -> Self {
        Self::Cap(e)
    }
}

/// Topology-loader / autorun-seed entry: grant `tid` a `Cap<Motor>` scoped
/// to one physical motor. `motor_id` becomes the slot's `resource` — 0 and
/// 1 are the only ids the current two-wheel drivetrain (and
/// [`require_pair_write`]) recognises; other ids mint successfully (the
/// cap system has no opinion on hardware topology) but can never satisfy a
/// pair-wide check.
pub fn motor_grant_cap(
    tid: u32,
    motor_id: u32,
    perms: CapPerms,
) -> Option<Cap<crate::cap::targets::Motor>> {
    crate::cap_store::grant::<crate::cap::targets::Motor>(tid, perms, motor_id)
}

/// Shared gate for every pair-wide (WRITE) motor operation.
///
/// Validates `cap` itself via `table.get` — forgery/stale/wrong-kind/
/// missing-perms/degraded-containment, exactly like every other typed
/// wrapper — and then requires that the SAME table also holds WRITE on the
/// complementary wheel (`0`↔`1`), via
/// [`CapTable::holds_kind_resource_with`]. Both checks run under the one
/// `table` borrow the caller already holds (`cap_store::with_table`), so
/// there is no two-lock/two-scan cost the legacy `io_ring::ring_cap_ok`
/// path pays when it checks `Motor(0)` and `Motor(1)` as two separate
/// `handle_owned_by` calls.
///
/// A `resource` outside `{0, 1}` — which can only happen if something minted
/// a cap with a motor id the current drivetrain does not have — always fails
/// closed: there is no "other wheel" to require, so the pair can never be
/// satisfied. This is deliberately NOT "any second Motor cap will do": see
/// `holds_kind_resource_with`'s resource-specific filter.
fn require_pair_write(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Motor>,
) -> Result<(), MotorCapError> {
    let resource = table.get(cap, CapPerms::WRITE)?;
    let other = match resource {
        0 => 1,
        1 => 0,
        _ => return Err(MotorCapError::Cap(CapError::MissingPerms)),
    };
    if !table.holds_kind_resource_with(CapKind::Motor, other, CapPerms::WRITE) {
        return Err(MotorCapError::Cap(CapError::MissingPerms));
    }
    Ok(())
}

/// Typed `motor_pid_set_target`: pair-wide WRITE (see module doc).
pub fn motor_set_target_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Motor>,
    speed_l: i16,
    speed_r: i16,
) -> Result<(), MotorCapError> {
    require_pair_write(table, cap)?;
    motor_pid_set_target(speed_l, speed_r);
    Ok(())
}

/// Typed `motor_pid_tick`: pair-wide WRITE (the PID loop updates its
/// integrator for both wheels at once). Returns `(pwm_l, pwm_r)`.
pub fn motor_tick_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Motor>,
    ticks_l: i64,
    ticks_r: i64,
    now: u64,
) -> Result<(i32, i32), MotorCapError> {
    require_pair_write(table, cap)?;
    Ok(motor_pid_tick(ticks_l, ticks_r, now))
}

/// Typed `motor_pid_enable`: pair-wide WRITE. `en = false` disables,
/// `true` enables.
pub fn motor_enable_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Motor>,
    en: bool,
) -> Result<(), MotorCapError> {
    require_pair_write(table, cap)?;
    motor_pid_enable(en);
    Ok(())
}

/// Typed `motor_pid_enabled`: single-cap READ (not an actuation — see
/// module doc for why this one function does not pair-check).
pub fn motor_enabled_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Motor>,
) -> Result<bool, MotorCapError> {
    let _ = table.get(cap, CapPerms::READ)?;
    Ok(motor_pid_enabled())
}

/// Typed `motor_pid_set_gains`: pair-wide WRITE.
pub fn motor_set_gains_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Motor>,
    kp: i32,
    ki: i32,
    kd: i32,
) -> Result<(), MotorCapError> {
    require_pair_write(table, cap)?;
    motor_pid_set_gains(kp, ki, kd);
    Ok(())
}

/// Typed `motor_pid_reset`: pair-wide WRITE. Clears integrator + previous
/// error.
pub fn motor_reset_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Motor>,
) -> Result<(), MotorCapError> {
    require_pair_write(table, cap)?;
    motor_pid_reset();
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────
//
// `robot_os_drivers::motor_pid` is real hardware-simulation state (RV64-only
// via its `robot_os_sync::SpinLock`/`robot_os_arch` chain in the full crate
// graph), so these tests only run when this file is pulled in — same trick
// as `cap.rs` — by a host test crate that supplies host stand-ins for
// `robot_os_sync`/`robot_os_sched`/`robot_os_drivers`. See
// `crates/topology-tests` (RFC-0003 migration phase P1).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::targets::{Gpio, Motor};

    #[test]
    fn pair_write_requires_both_wheels() {
        let mut t = CapTable::empty();
        let left: Cap<Motor> = motor_grant_cap_into(&mut t, 0, CapPerms::RW);
        // Only the left wheel is held — every pair-wide op must deny.
        assert_eq!(
            motor_set_target_cap(&t, left, 10, 10),
            Err(MotorCapError::Cap(CapError::MissingPerms))
        );
        assert_eq!(
            motor_enable_cap(&t, left, true),
            Err(MotorCapError::Cap(CapError::MissingPerms))
        );

        // Grant the right wheel too — now the pair is complete.
        let _right: Cap<Motor> = motor_grant_cap_into(&mut t, 1, CapPerms::RW);
        assert_eq!(motor_set_target_cap(&t, left, 10, 10), Ok(()));
        assert_eq!(motor_enable_cap(&t, left, true), Ok(()));
    }

    #[test]
    fn pair_write_denies_read_only_grants() {
        let mut t = CapTable::empty();
        let left: Cap<Motor> = motor_grant_cap_into(&mut t, 0, CapPerms::READ);
        let _right: Cap<Motor> = motor_grant_cap_into(&mut t, 1, CapPerms::READ);
        // Both wheels present, but neither carries WRITE.
        assert_eq!(
            motor_set_target_cap(&t, left, 0, 0),
            Err(MotorCapError::Cap(CapError::MissingPerms))
        );
    }

    #[test]
    fn out_of_range_motor_id_can_never_satisfy_the_pair() {
        let mut t = CapTable::empty();
        // A cap minted for a motor id the drivetrain does not have.
        let odd: Cap<Motor> = motor_grant_cap_into(&mut t, 7, CapPerms::RW);
        assert_eq!(
            motor_set_target_cap(&t, odd, 0, 0),
            Err(MotorCapError::Cap(CapError::MissingPerms))
        );
    }

    #[test]
    fn enabled_query_is_single_cap_not_pair_wide() {
        let mut t = CapTable::empty();
        // Only the left wheel granted — READ query still succeeds.
        let left: Cap<Motor> = motor_grant_cap_into(&mut t, 0, CapPerms::READ);
        assert!(motor_enabled_cap(&t, left).is_ok());
    }

    #[test]
    fn wrong_kind_still_rejected_before_pairing_logic() {
        let mut t = CapTable::empty();
        let gpio: Cap<Gpio> = t.grant(CapPerms::RW, 0).unwrap();
        let forged: Cap<Motor> = Cap::from_raw(gpio.raw());
        assert_eq!(
            motor_set_target_cap(&t, forged, 0, 0),
            Err(MotorCapError::Cap(CapError::WrongKind))
        );
    }

    /// Test-only helper: grant directly into a `CapTable` (these tests
    /// exercise the table-level functions, not `cap_store`/TIDs).
    fn motor_grant_cap_into(
        table: &mut CapTable,
        motor_id: u32,
        perms: CapPerms,
    ) -> Cap<Motor> {
        table.grant(perms, motor_id).unwrap()
    }
}
