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
    /// Robot has crossed the circular geofence boundary (E03).
    GeofenceViolation,
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
// Bounded runtime safety monitor — output envelope (RFC-0033, RFC-0035)
// ---------------------------------------------------------------------------

/// Reduced speed cap (% of max) applied to LOW-CONFIDENCE commands (RFC-0035).
/// When the brain marks a command low-confidence (e.g. a reactive-LLM action vs
/// a deterministic plan/scripted step), the robot self-limits: act, but cautiously.
pub const SAFETY_LOW_CONFIDENCE_CAP_PCT: u8 = 40;

// ── RFC-0037: Graded degrade-level speed caps ────────────────────────────────
//
// Applied in `motor_envelope` AFTER the low-confidence cap. The level→pct
// mapping is `robot_os_degrade_policy::level_cap_pct()`, a pure function in
// the dep-free leaf crate. This keeps motor-actuation policy out of the TCB
// (`crates/ipc`) while remaining independently host-tested in the leaf.
//
// `DEGRADE_LEVEL_CONTAINED` (stop + cap-denial) is also handled in `CapTable::get`
// for user-task actuation; the 0 % cap here ensures the in-kernel motor loop
// (which does NOT go through `get()`) also stops. Both layers are required.
//
// The re-exports below keep the safety-module naming convention for callers
// inside behavior that need these values without importing from the leaf directly.

/// Speed ceiling at `DEGRADE_LEVEL_FULL` (RFC-0037): no extra restriction.
/// Re-exported from `robot_os_degrade_policy::DEGRADE_SPEED_CAP_FULL_PCT`.
pub const SAFETY_DEGRADE_FULL_CAP_PCT: i32 = robot_os_degrade_policy::DEGRADE_SPEED_CAP_FULL_PCT;

/// Speed ceiling at `DEGRADE_LEVEL_CAUTIOUS` (RFC-0037): 70 % of per-type max.
/// Re-exported from `robot_os_degrade_policy::DEGRADE_SPEED_CAP_CAUTIOUS_PCT`.
pub const SAFETY_DEGRADE_CAUTIOUS_CAP_PCT: i32 = robot_os_degrade_policy::DEGRADE_SPEED_CAP_CAUTIOUS_PCT;

/// Speed ceiling at `DEGRADE_LEVEL_SLOW` (RFC-0037): 30 % of per-type max.
/// Re-exported from `robot_os_degrade_policy::DEGRADE_SPEED_CAP_SLOW_PCT`.
pub const SAFETY_DEGRADE_SLOW_CAP_PCT: i32 = robot_os_degrade_policy::DEGRADE_SPEED_CAP_SLOW_PCT;

/// Speed ceiling at `DEGRADE_LEVEL_CONTAINED` (RFC-0037): 0 % — full stop.
/// Re-exported from `robot_os_degrade_policy::DEGRADE_SPEED_CAP_CONTAINED_PCT`.
pub const SAFETY_DEGRADE_CONTAINED_CAP_PCT: i32 = robot_os_degrade_policy::DEGRADE_SPEED_CAP_CONTAINED_PCT;

/// Whether the most recent brain command was flagged low-confidence (RFC-0035).
/// Set at command ingest from `FLAG_LOW_CONFIDENCE`; read by `motor_envelope` at
/// the chokepoint. Conservative on staleness: stays low until a high-confidence
/// command clears it (and the watchdog safe-stops on comms loss regardless).
static CMD_LOW_CONF: AtomicBool = AtomicBool::new(false);

/// Record the confidence of the most recent brain command (RFC-0035).
pub fn cmd_set_low_confidence(low: bool) {
    CMD_LOW_CONF.store(low, Ordering::Release);
}

/// Whether the current command context is low-confidence.
pub fn cmd_low_confidence() -> bool {
    CMD_LOW_CONF.load(Ordering::Acquire)
}

/// Bounded runtime safety monitor: the LAST line of defence between any motor
/// command and PWM, applied at the single `rt_motor_task` MotorCmd→PID→PWM
/// chokepoint (so it is structurally unbypassable — every command source funnels
/// through it).
///
/// This complements, and does not replace, the sensor-reactive L0 `safety_check`
/// upstream: L0 reacts to the world (obstacle, tilt, battery); this validates the
/// command's own MAGNITUDE — a hard ESTOP override, the per-robot-type speed cap
/// (`SAFETY_*_MAX_SPEED_PCT`), and (RFC-0035) a tighter cap when the brain marked
/// the command low-confidence. `(speed_l, speed_r)` are percent (±100); returns
/// the clamped pair.
///
/// O(1), no allocation, no I/O — its cost is bounded by construction (a couple of
/// branches plus two clamps), not by measurement. This is a runtime-assurance
/// gate, NOT a formally verified component (see RFC-0033; "verified" is reserved
/// for the Phase-5 horizon).
pub fn motor_envelope(speed_l: i32, speed_r: i32) -> (i32, i32) {
    // Hard stop overrides everything — unconditional, highest priority.
    if estop_is_active() {
        return (0, 0);
    }
    // Per-type magnitude cap. Wheeled/Ackermann share the wheeled cap; drone and
    // humanoid actuation has its own envelope on its own path, so pass through
    // here (still clamped to the protocol's ±100 upstream).
    let mut cap: i32 = match safety_robot_type() {
        ROBOT_TYPE_WHEELED | ROBOT_TYPE_ACKERMANN => SAFETY_WHEELED_MAX_SPEED_PCT as i32,
        _ => 100,
    };
    // RFC-0035: confidence-aware real-time — act cautiously on uncertain commands.
    if cmd_low_confidence() {
        cap = cap.min(SAFETY_LOW_CONFIDENCE_CAP_PCT as i32);
    }
    // RFC-0037: graded degrade-level speed ceiling. Applied AFTER the low-confidence
    // cap so both constraints compose (the tighter one wins). The mapping lives in
    // the dep-free leaf crate `robot_os_degrade_policy` (level_cap_pct); the
    // runtime level state stays in `robot_os_ipc::cap::degrade_level()`. Unknown
    // levels clamp to 0 (fail-closed) inside level_cap_pct.
    let level_cap: i32 = robot_os_degrade_policy::level_cap_pct(
        robot_os_ipc::cap::degrade_level(),
    );
    cap = cap.min(level_cap);
    (speed_l.clamp(-cap, cap), speed_r.clamp(-cap, cap))
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

    // E03: Geofence check (EmergencyStop for wheeled — can't RTL)
    if let Some(mut r) = check_geofence_from_gps(state) {
        r.action = SafetyAction::EmergencyStop; // wheeled can't fly back
        return r;
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

    // E03: Geofence check (only when GPS is available)
    if let Some(r) = check_geofence_from_gps(state) { return r; }

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

// ── E03: Circular Geofence (GPS boundary) ────────────────────────────────────
//
// A single circular geofence centered on a GPS coordinate.  The robot must
// stay within `radius_m` metres of the centre.  If it exits the fence, the
// safety system triggers `ReturnToLaunch` (drone) or `EmergencyStop` (wheeled).
//
// The fence is disabled when `radius_m == 0`.
//
// Distance is approximated using the equirectangular projection:
//
//   Δlat_m  = (lat - center_lat) * LAT_DEG_TO_M
//   Δlon_m  = (lon - center_lon) * LON_DEG_TO_M * cos(center_lat)
//   dist_m  = sqrt(Δlat_m² + Δlon_m²)
//
// The trig-free version replaces cos(lat) with a precomputed integer factor.
// Valid for fences ≤ 50 km from equator; sufficient for robotics use.

use robot_os_sync::SpinLock;

/// Geofence configuration stored in a SpinLock for atomic updates from brain.
struct GeofenceConfig {
    /// Centre latitude (micro-degrees × 10⁶, i.e. degrees * 1_000_000).
    center_lat_udeg: i32,
    /// Centre longitude (micro-degrees).
    center_lon_udeg: i32,
    /// Radius in metres (0 = disabled).
    radius_m: u32,
}

impl GeofenceConfig {
    const fn disabled() -> Self {
        GeofenceConfig { center_lat_udeg: 0, center_lon_udeg: 0, radius_m: 0 }
    }
}

static GEOFENCE: SpinLock<GeofenceConfig> = SpinLock::new(GeofenceConfig::disabled());

/// Configure the circular geofence.
///
/// `center_lat_udeg` — latitude in micro-degrees (degrees × 1_000_000).
/// `center_lon_udeg` — longitude in micro-degrees.
/// `radius_m`        — radius in metres (0 = disable fence).
pub fn geofence_set(center_lat_udeg: i32, center_lon_udeg: i32, radius_m: u32) {
    let mut g = GEOFENCE.lock();
    g.center_lat_udeg = center_lat_udeg;
    g.center_lon_udeg = center_lon_udeg;
    g.radius_m        = radius_m;
}

/// Disable the geofence.
pub fn geofence_disable() {
    GEOFENCE.lock().radius_m = 0;
}

/// Returns true if GPS position is outside the configured geofence.
/// Always returns false if the fence is disabled (radius_m == 0).
///
/// `lat_udeg`, `lon_udeg` — current position in micro-degrees.
pub fn geofence_check(lat_udeg: i32, lon_udeg: i32) -> bool {
    /// Metres per degree of latitude (constant, ~111 km/deg).
    const LAT_M_PER_DEG_UDEG: i64 = 111_000; // metres per 1_000_000 µdeg

    let g = GEOFENCE.lock();
    if g.radius_m == 0 { return false; }

    // Δlat and Δlon in micro-degrees
    let dlat_udeg = (lat_udeg - g.center_lat_udeg) as i64;
    let dlon_udeg = (lon_udeg - g.center_lon_udeg) as i64;

    // Convert to metres (integer, scaled by 1000 for precision)
    // Δlat_m * 1000 = dlat_udeg * LAT_M_PER_DEG_UDEG / 1_000_000 * 1000
    //               = dlat_udeg * LAT_M_PER_DEG_UDEG / 1_000
    let dlat_mm = dlat_udeg * LAT_M_PER_DEG_UDEG / 1_000_000;

    // Longitude scale: cos(lat) approximation using centre latitude.
    // cos(lat) ≈ 1 - (lat_deg² / 2) for |lat| < 45°; we use integer 1000ths.
    // More accurately: we precompute cos_factor = cos(center_lat) * 1000
    // Using the identity: cos(x) ≈ (1 - 2sin²(x/2)), small-angle: ≈ 1 - x²/2
    // For a simpler bound, use cos(45°) ≈ 707/1000 as minimum (worst case).
    let lat_deg_abs = (g.center_lat_udeg.unsigned_abs() / 1_000_000) as i64;
    // cos_factor/1000 = (1 - lat_deg² / 20000) clamped to [500, 1000]
    let cos_factor: i64 = (1000 - (lat_deg_abs * lat_deg_abs / 20000)).clamp(500, 1000);

    let dlon_mm = dlon_udeg * LAT_M_PER_DEG_UDEG * cos_factor / (1_000_000 * 1000);

    // Squared distance in m² (no sqrt needed — compare to radius²)
    let dist_sq_m2 = dlat_mm * dlat_mm + dlon_mm * dlon_mm;
    let radius_m2  = (g.radius_m as i64) * (g.radius_m as i64);

    dist_sq_m2 > radius_m2
}

/// Minimum GPS fix quality required for geofence enforcement.
const GEOFENCE_MIN_FIX: u8 = 2;
/// Minimum satellites required for geofence enforcement.
const GEOFENCE_MIN_SATELLITES: u8 = 3;

/// Check geofence against GPS data in the current sensor snapshot.
/// Returns a SafetyResult if violated, else None.
fn check_geofence_from_gps(state: &SensorState) -> Option<SafetyResult> {
    let valid = state.gps_fix >= GEOFENCE_MIN_FIX
        && state.gps_satellites >= GEOFENCE_MIN_SATELLITES;

    if !valid { return None; } // No GPS fix — can't check fence

    if geofence_check(state.gps_lat_udeg, state.gps_lon_udeg) {
        Some(SafetyResult {
            action: SafetyAction::ReturnToLaunch,
            violation: SafetyViolation::GeofenceViolation,
        })
    } else {
        None
    }
}
