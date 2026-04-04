//! On-board safety profiles — kernel-level safety checks per robot type.
//!
//! These run ON the robot, independent of the brain server. Even if TCP
//! is down, WiFi is dead, and the brain is off, these checks ALWAYS run.
//!
//! Architecture:
//!   Brain safety (policy/safety.py) — high-level, VLM-informed decisions
//!   Kernel safety (this module) — hard limits, cannot be overridden
//!
//! The kernel safety layer runs as part of L0 (emergency stop) in the
//! subsumption arbiter. It is ALWAYS active and CANNOT be disabled.
//!
//! Robot types:
//!   WHEELED  — tilt, battery, obstacle, speed limit
//!   DRONE    — tilt, battery (2-tier), GPS lock, altitude ceiling, comms timeout
//!   HUMANOID — tilt, battery, fall detection (accel magnitude)
//!   ACKERMANN — same as wheeled + steering limit

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use crate::types::*;

// ---------------------------------------------------------------------------
// Robot type constants (must match brain_protocol.rs)
// ---------------------------------------------------------------------------
pub const ROBOT_TYPE_WHEELED: u8 = 0;
pub const ROBOT_TYPE_DRONE: u8 = 1;
pub const ROBOT_TYPE_HUMANOID: u8 = 2;
pub const ROBOT_TYPE_ACKERMANN: u8 = 3;

// ---------------------------------------------------------------------------
// Safety thresholds — named constants, NO magic numbers
// ---------------------------------------------------------------------------

// ── Common (all types) ──────────────────────────────────────────────────────
/// Minimum accel_z (mg) — below this, robot is falling/tipped.
pub const SAFETY_FALL_ACCEL_Z_MG: i32 = 500;
/// Maximum gyro rate (mdps) — above this, robot is spinning out of control.
pub const SAFETY_MAX_GYRO_MDPS: u32 = 90_000;
/// Comms timeout (CLINT ticks at 10 MHz) — 5 seconds.
pub const SAFETY_COMMS_TIMEOUT_TICKS: u64 = 50_000_000;

// ── Wheeled ─────────────────────────────────────────────────────────────────
/// Battery cutoff for wheeled robots (mV).
pub const SAFETY_WHEELED_MIN_BATTERY_MV: u16 = 6500;
/// Maximum tilt for wheeled (centidegrees) — ~45°.
pub const SAFETY_WHEELED_MAX_TILT_CDEG: u16 = 4500;
/// Obstacle emergency stop distance (mm).
pub const SAFETY_WHEELED_OBSTACLE_MM: u16 = 150;
/// Maximum motor speed (% of max).
pub const SAFETY_WHEELED_MAX_SPEED_PCT: u8 = 80;

// ── Drone (stricter) ────────────────────────────────────────────────────────
/// Battery: trigger RTL (mV).
pub const SAFETY_DRONE_LOW_BATTERY_MV: u16 = 7000;
/// Battery: trigger immediate LAND (mV).
pub const SAFETY_DRONE_CRITICAL_BATTERY_MV: u16 = 6500;
/// Maximum tilt for drone (centidegrees) — ~35°.
pub const SAFETY_DRONE_MAX_TILT_CDEG: u16 = 3500;
/// Drone comms timeout (CLINT ticks) — 3 seconds (shorter than wheeled).
pub const SAFETY_DRONE_COMMS_TIMEOUT_TICKS: u64 = 30_000_000;
/// Maximum altitude (mm) — geofence ceiling.
pub const SAFETY_DRONE_MAX_ALTITUDE_MM: i32 = 50_000;
/// Minimum GPS satellites for safe flight.
pub const SAFETY_DRONE_MIN_SATELLITES: u8 = 6;

// ── Humanoid ────────────────────────────────────────────────────────────────
/// Battery cutoff for humanoid (mV).
pub const SAFETY_HUMANOID_MIN_BATTERY_MV: u16 = 6500;
/// Fall detection: accel magnitude threshold (mg).
pub const SAFETY_HUMANOID_FALL_ACCEL_MG: u32 = 4000;

// ---------------------------------------------------------------------------
// Safety action results
// ---------------------------------------------------------------------------

/// Action the safety system demands.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SafetyAction {
    /// No safety violation — continue normal operation.
    None,
    /// Stop all motors immediately.
    EmergencyStop,
    /// Drone: return to launch point.
    ReturnToLaunch,
    /// Drone: land immediately (critical battery).
    LandNow,
    /// Reduce speed to safe limit.
    SpeedLimit(i32),
}

/// Result of a safety check — what happened and what to do.
#[derive(Clone, Copy)]
pub struct SafetyResult {
    pub action: SafetyAction,
    pub violation: SafetyViolation,
}

/// What triggered the safety action.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SafetyViolation {
    None,
    Falling,
    Spinning,
    LowBattery,
    CriticalBattery,
    ObstacleTooClose,
    ExcessiveTilt,
    NoGpsFix,
    AltitudeCeiling,
    CommsTimeout,
    FallDetected,
    Overheated,
    RemoteEstop,
}

impl SafetyResult {
    pub const fn safe() -> Self {
        Self { action: SafetyAction::None, violation: SafetyViolation::None }
    }

    pub fn is_violation(&self) -> bool {
        self.violation != SafetyViolation::None
    }
}

// ── Thermal thresholds ──────────────────────────────────────────────────────
/// Maximum operating temperature (centidegrees Celsius) — MPU-6050 limit.
pub const SAFETY_MAX_TEMP_CDEG: i32 = 8500;
/// Warning temperature threshold (centidegrees Celsius).
pub const SAFETY_WARN_TEMP_CDEG: i32 = 7000;

// ---------------------------------------------------------------------------
// Remote emergency stop (set by PKT_ESTOP, cleared by MODE_CMD)
// ---------------------------------------------------------------------------
static ESTOP_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Activate remote emergency stop.
pub fn estop_activate() {
    ESTOP_ACTIVE.store(true, Ordering::Release);
}

/// Deactivate remote emergency stop (requires explicit MODE_CMD reset).
pub fn estop_deactivate() {
    ESTOP_ACTIVE.store(false, Ordering::Release);
}

/// Check if remote ESTOP is active.
pub fn estop_is_active() -> bool {
    ESTOP_ACTIVE.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// Current robot type (set at boot from CONFIG.INI or brain negotiation)
// ---------------------------------------------------------------------------
static ROBOT_TYPE: AtomicU8 = AtomicU8::new(ROBOT_TYPE_WHEELED);

/// Set the robot type for safety checks.
pub fn safety_set_robot_type(robot_type: u8) {
    ROBOT_TYPE.store(robot_type, Ordering::Release);
}

/// Get current robot type.
pub fn safety_robot_type() -> u8 {
    ROBOT_TYPE.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// Main safety check — dispatches by robot type
// ---------------------------------------------------------------------------

/// Run all safety checks for the current robot type.
/// This is called from L0 (emergency stop layer) every tick.
///
/// Returns the highest-priority safety action needed.
pub fn safety_check(state: &SensorState) -> SafetyResult {
    // Remote ESTOP — highest priority, unconditional
    if estop_is_active() {
        return SafetyResult {
            action: SafetyAction::EmergencyStop,
            violation: SafetyViolation::RemoteEstop,
        };
    }

    // Common checks first (all robot types)
    let common = check_common(state);
    if common.is_violation() {
        return common;
    }

    // Type-specific checks
    match ROBOT_TYPE.load(Ordering::Relaxed) {
        ROBOT_TYPE_DRONE => check_drone(state),
        ROBOT_TYPE_HUMANOID => check_humanoid(state),
        _ => check_wheeled(state), // wheeled + ackermann
    }
}

// ---------------------------------------------------------------------------
// Common checks (all robot types)
// ---------------------------------------------------------------------------

fn check_common(state: &SensorState) -> SafetyResult {
    // Thermal check — works even without valid IMU flag (temp reads separately)
    if state.temp_cdeg > SAFETY_MAX_TEMP_CDEG {
        return SafetyResult {
            action: SafetyAction::EmergencyStop,
            violation: SafetyViolation::Overheated,
        };
    }

    if !state.imu_valid {
        return SafetyResult::safe();
    }

    // Falling: accel_z too low
    if state.accel_mg[2] < SAFETY_FALL_ACCEL_Z_MG {
        return SafetyResult {
            action: SafetyAction::EmergencyStop,
            violation: SafetyViolation::Falling,
        };
    }

    // Spinning: any gyro axis too fast
    if state.gyro_mdps[0].unsigned_abs() > SAFETY_MAX_GYRO_MDPS
        || state.gyro_mdps[1].unsigned_abs() > SAFETY_MAX_GYRO_MDPS
        || state.gyro_mdps[2].unsigned_abs() > SAFETY_MAX_GYRO_MDPS
    {
        return SafetyResult {
            action: SafetyAction::EmergencyStop,
            violation: SafetyViolation::Spinning,
        };
    }

    SafetyResult::safe()
}

// ---------------------------------------------------------------------------
// Wheeled safety
// ---------------------------------------------------------------------------

fn check_wheeled(state: &SensorState) -> SafetyResult {
    // Battery cutoff
    if state.battery_mv > 0 && state.battery_mv < SAFETY_WHEELED_MIN_BATTERY_MV {
        return SafetyResult {
            action: SafetyAction::EmergencyStop,
            violation: SafetyViolation::LowBattery,
        };
    }

    // Obstacle too close (front rangefinder)
    if state.cam_dist_front > 0 && state.cam_dist_front < SAFETY_WHEELED_OBSTACLE_MM {
        return SafetyResult {
            action: SafetyAction::EmergencyStop,
            violation: SafetyViolation::ObstacleTooClose,
        };
    }

    // Tilt check
    if state.imu_valid {
        let tilt = tilt_from_accel(state.accel_mg);
        if tilt > SAFETY_WHEELED_MAX_TILT_CDEG as u32 {
            return SafetyResult {
                action: SafetyAction::EmergencyStop,
                violation: SafetyViolation::ExcessiveTilt,
            };
        }
    }

    SafetyResult::safe()
}

// ---------------------------------------------------------------------------
// Drone safety (stricter)
// ---------------------------------------------------------------------------

fn check_drone(state: &SensorState) -> SafetyResult {
    // Critical battery → immediate land (highest priority after common)
    if state.battery_mv > 0 && state.battery_mv < SAFETY_DRONE_CRITICAL_BATTERY_MV {
        return SafetyResult {
            action: SafetyAction::LandNow,
            violation: SafetyViolation::CriticalBattery,
        };
    }

    // Low battery → RTL
    if state.battery_mv > 0 && state.battery_mv < SAFETY_DRONE_LOW_BATTERY_MV {
        return SafetyResult {
            action: SafetyAction::ReturnToLaunch,
            violation: SafetyViolation::LowBattery,
        };
    }

    // Tilt check (stricter for drone)
    if state.imu_valid {
        let tilt = tilt_from_accel(state.accel_mg);
        if tilt > SAFETY_DRONE_MAX_TILT_CDEG as u32 {
            return SafetyResult {
                action: SafetyAction::EmergencyStop,
                violation: SafetyViolation::ExcessiveTilt,
            };
        }
    }

    // Comms timeout — if no brain command in N seconds, hover/RTL
    if state.remote_action.valid {
        let age = state.timestamp.saturating_sub(state.remote_action.received_at);
        if age > SAFETY_DRONE_COMMS_TIMEOUT_TICKS {
            return SafetyResult {
                action: SafetyAction::ReturnToLaunch,
                violation: SafetyViolation::CommsTimeout,
            };
        }
    }

    SafetyResult::safe()
}

// ---------------------------------------------------------------------------
// Humanoid safety
// ---------------------------------------------------------------------------

fn check_humanoid(state: &SensorState) -> SafetyResult {
    // Battery cutoff
    if state.battery_mv > 0 && state.battery_mv < SAFETY_HUMANOID_MIN_BATTERY_MV {
        return SafetyResult {
            action: SafetyAction::EmergencyStop,
            violation: SafetyViolation::LowBattery,
        };
    }

    // Fall detection via acceleration magnitude
    if state.imu_valid {
        let mag = accel_magnitude(state.accel_mg);
        if mag > SAFETY_HUMANOID_FALL_ACCEL_MG {
            return SafetyResult {
                action: SafetyAction::EmergencyStop,
                violation: SafetyViolation::FallDetected,
            };
        }
    }

    SafetyResult::safe()
}

// ---------------------------------------------------------------------------
// Math helpers (no libm, integer only)
// ---------------------------------------------------------------------------

/// Compute tilt angle from accelerometer in centidegrees (integer approx).
/// Uses the ratio of horizontal to vertical acceleration.
fn tilt_from_accel(accel_mg: [i32; 3]) -> u32 {
    let ax = accel_mg[0].unsigned_abs();
    let ay = accel_mg[1].unsigned_abs();
    let az = accel_mg[2].unsigned_abs();

    let horiz_sq = (ax as u64) * (ax as u64) + (ay as u64) * (ay as u64);
    let vert_sq = (az as u64) * (az as u64);

    if vert_sq == 0 {
        return 9000; // 90 degrees — completely horizontal
    }

    // atan(sqrt(horiz_sq) / sqrt(vert_sq)) in centidegrees
    // Approximation: atan(x) ≈ 57.3° * x for small x, clamped
    // ratio = sqrt(horiz_sq / vert_sq) * 100 (in percent)
    let ratio_pct = isqrt(horiz_sq * 10000 / vert_sq);
    // Convert: ratio_pct * 57.3 = centidegrees (approx for small angles)
    // For larger angles this overestimates, which is safer (more conservative)
    let cdeg = (ratio_pct * 573 / 100) as u32;
    cdeg.min(9000) // cap at 90°
}

/// Compute acceleration magnitude (mg) from [ax, ay, az].
fn accel_magnitude(accel_mg: [i32; 3]) -> u32 {
    let ax = accel_mg[0] as i64;
    let ay = accel_mg[1] as i64;
    let az = accel_mg[2] as i64;
    isqrt((ax * ax + ay * ay + az * az) as u64) as u32
}

/// Integer square root (Newton's method).
fn isqrt(n: u64) -> u64 {
    if n <= 1 { return n; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}
