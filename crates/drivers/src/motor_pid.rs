//! PID velocity controller for wheeled robots — 4WD differential drive.
//!
//! The robot has 4 motors wired as 2 channels (left pair, right pair).
//! Each channel has an encoder. The PID loop reads encoder ticks, computes
//! the velocity error against the desired speed (ticks/s), and adjusts PWM
//! output accordingly.
//!
//! This module lives in the drivers crate; the kernel's RT motor task glues
//! it with encoder reads (from robot crate) and motor writes.
//!
//! # Usage
//!
//! 1. Call `motor_pid_init()` once at startup.
//! 2. From the deliberative layer, call `motor_pid_set_target(speed_l, speed_r)`
//!    where speeds are in encoder ticks per second.
//! 3. From the RT motor task, call `motor_pid_tick(ticks_l, ticks_r)` every
//!    `PID_DT_MS` ms. It returns `(pwm_l, pwm_r)` to apply to the motors.
//! 4. Use `motor_pid_enable(false)` to bypass PID for direct PWM control.

use core::sync::atomic::{AtomicBool, AtomicI16, Ordering};
use robot_os_sync::SpinLock;

// ── PID Tuning Constants ─────────────────────────────────────────────────────

/// Default proportional gain (integer, converted to Q16.16 internally).
pub const DEFAULT_KP: i32 = 1;

/// Default integral gain.
pub const DEFAULT_KI: i32 = 0;

/// Default derivative gain.
pub const DEFAULT_KD: i32 = 0;

/// Minimum PID output (maps to PWM duty -100%).
pub const PID_OUTPUT_MIN: i32 = -100;

/// Maximum PID output (maps to PWM duty +100%).
pub const PID_OUTPUT_MAX: i32 = 100;

/// PID loop period in milliseconds (expected call rate of `motor_pid_tick`).
pub const PID_DT_MS: u32 = 10;

/// Anti-windup: maximum absolute value for the integral accumulator (ticks·ms).
/// Prevents integral windup during sustained error.
pub const INTEGRAL_WINDUP_LIMIT: i64 = 10_000;

/// Number of motor channels (left pair + right pair).
pub const NUM_CHANNELS: usize = 2;

/// Index for left motor channel.
pub const CH_LEFT: usize = 0;

/// Index for right motor channel.
pub const CH_RIGHT: usize = 1;

// ── PID Controller ───────────────────────────────────────────────────────────

/// Fixed-point scale factor (Q16.16): 1.0 = 65536.
const FIXED_SCALE: i64 = 65_536;

/// PID controller for a single motor channel.
///
/// Uses fixed-point Q16.16 arithmetic internally for `no_std` compatibility.
/// All gains are stored in Q16.16; inputs/outputs are integer ticks and PWM%.
#[derive(Clone, Copy)]
pub struct PidController {
    /// Proportional gain (Q16.16).
    kp: i64,
    /// Integral gain (Q16.16).
    ki: i64,
    /// Derivative gain (Q16.16).
    kd: i64,
    /// Accumulated integral term (Q16.16).
    integral: i64,
    /// Previous error for derivative calculation.
    prev_error: i64,
    /// Minimum output value.
    output_min: i32,
    /// Maximum output value.
    output_max: i32,
}

impl PidController {
    /// Create a new PID controller with the given gains.
    ///
    /// Gains are integer values that get converted to Q16.16 internally.
    pub const fn new(kp: i32, ki: i32, kd: i32) -> Self {
        PidController {
            kp: kp as i64 * FIXED_SCALE,
            ki: ki as i64 * FIXED_SCALE,
            kd: kd as i64 * FIXED_SCALE,
            integral: 0,
            prev_error: 0,
            output_min: PID_OUTPUT_MIN,
            output_max: PID_OUTPUT_MAX,
        }
    }

    /// Compute PID output given setpoint and measurement.
    ///
    /// - `setpoint`: desired speed in ticks/s.
    /// - `measurement`: actual speed in ticks/s.
    /// - `dt_ms`: time delta in milliseconds (must be > 0).
    ///
    /// Returns clamped PWM output in `[output_min, output_max]`.
    pub fn update(&mut self, setpoint: i32, measurement: i32, dt_ms: u32) -> i32 {
        if dt_ms == 0 {
            return 0;
        }

        let error = (setpoint - measurement) as i64;

        // Proportional term: P = Kp * error
        let p = self.kp * error / FIXED_SCALE;

        // Integral term: I += Ki * error * dt
        // dt in seconds = dt_ms / 1000, so: Ki * error * dt_ms / 1000
        self.integral += self.ki * error * dt_ms as i64 / (FIXED_SCALE * 1000);
        // Anti-windup clamping
        let windup_limit = INTEGRAL_WINDUP_LIMIT * FIXED_SCALE;
        self.integral = self.integral.clamp(-windup_limit, windup_limit);
        let i = self.integral / FIXED_SCALE;

        // Derivative term: D = Kd * d(error)/dt
        // d(error)/dt = (error - prev_error) / dt_seconds
        let de = error - self.prev_error;
        let d = self.kd * de * 1000 / (FIXED_SCALE * dt_ms as i64);
        let d = d / FIXED_SCALE;

        self.prev_error = error;

        // Sum and clamp
        let output = p + i + d;
        output.clamp(self.output_min as i64, self.output_max as i64) as i32
    }

    /// Reset integral and derivative state.
    pub fn reset(&mut self) {
        self.integral = 0;
        self.prev_error = 0;
    }
}

// ── Global State (atomic, shared between RT task and deliberative layer) ─────

/// Target speed for left channel (ticks/s), set by deliberative layer.
static TARGET_SPEED_L: AtomicI16 = AtomicI16::new(0);

/// Target speed for right channel (ticks/s), set by deliberative layer.
static TARGET_SPEED_R: AtomicI16 = AtomicI16::new(0);

/// Whether the PID velocity controller is enabled.
/// When disabled, motor commands pass through directly (open-loop).
static PID_ENABLED: AtomicBool = AtomicBool::new(false);

/// The two PID controllers (left, right), protected by a spinlock.
/// Only the RT motor task calls `update()`; tuning can happen from shell/config.
static PID_CONTROLLERS: SpinLock<[PidController; NUM_CHANNELS]> = SpinLock::new([
    PidController::new(DEFAULT_KP, DEFAULT_KI, DEFAULT_KD),
    PidController::new(DEFAULT_KP, DEFAULT_KI, DEFAULT_KD),
]);

/// Internal state: previous encoder ticks and timestamp for velocity calculation.
/// Protected by spinlock since only the RT task mutates it, but keeps it grouped.
struct TickState {
    prev_ticks_l: i64,
    prev_ticks_r: i64,
    /// Previous CLINT timestamp (ticks). Zero means "first call, no baseline yet."
    prev_time: u64,
}

impl TickState {
    const fn new() -> Self {
        TickState {
            prev_ticks_l: 0,
            prev_ticks_r: 0,
            prev_time: 0,
        }
    }
}

static TICK_STATE: SpinLock<TickState> = SpinLock::new(TickState::new());

// ── Public API ───────────────────────────────────────────────────────────────

/// Initialize both PID controllers and enable closed-loop control.
///
/// Call once during system startup, before the RT motor task begins.
pub fn motor_pid_init() {
    {
        let mut pids = PID_CONTROLLERS.lock();
        pids[CH_LEFT] = PidController::new(DEFAULT_KP, DEFAULT_KI, DEFAULT_KD);
        pids[CH_RIGHT] = PidController::new(DEFAULT_KP, DEFAULT_KI, DEFAULT_KD);
    }

    TARGET_SPEED_L.store(0, Ordering::Relaxed);
    TARGET_SPEED_R.store(0, Ordering::Relaxed);

    {
        let mut ts = TICK_STATE.lock();
        *ts = TickState::new();
    }

    PID_ENABLED.store(true, Ordering::Release);

    crate::kprintln!(
        "[MOTOR-PID] Initialized: Kp={} Ki={} Kd={} dt={}ms windup_limit={}",
        DEFAULT_KP, DEFAULT_KI, DEFAULT_KD, PID_DT_MS, INTEGRAL_WINDUP_LIMIT
    );
}

/// Set the target speed for both channels (ticks per second).
///
/// Positive = forward, negative = reverse.
/// Called from the deliberative layer (behavior task / brain protocol).
pub fn motor_pid_set_target(speed_l: i16, speed_r: i16) {
    TARGET_SPEED_L.store(speed_l, Ordering::Relaxed);
    TARGET_SPEED_R.store(speed_r, Ordering::Relaxed);
}

/// Execute one PID control tick.
///
/// - `ticks_l`, `ticks_r`: current cumulative encoder readings.
/// - `now`: current CLINT timestamp (from `clint::get_time()`).
///
/// Returns `(pwm_left, pwm_right)` — signed PWM output to apply to motors.
/// Positive = forward, negative = backward. Magnitude is duty cycle 0-100%.
///
/// Must be called from the RT motor task at approximately `PID_DT_MS` intervals.
pub fn motor_pid_tick(ticks_l: i64, ticks_r: i64, now: u64) -> (i32, i32) {
    if !motor_pid_enabled() {
        return (0, 0);
    }

    let mut ts = TICK_STATE.lock();

    // On first call, record baseline — no PID output yet.
    if ts.prev_time == 0 {
        ts.prev_ticks_l = ticks_l;
        ts.prev_ticks_r = ticks_r;
        ts.prev_time = now;
        return (0, 0);
    }

    // Compute elapsed time in milliseconds.
    let elapsed_clint = now.wrapping_sub(ts.prev_time);
    let timer_freq = crate::clint::TIMER_FREQ;
    let dt_ms = if timer_freq > 0 {
        (elapsed_clint * 1000 / timer_freq) as u32
    } else {
        PID_DT_MS // fallback
    };

    // Avoid computation on zero-length interval.
    if dt_ms == 0 {
        return (0, 0);
    }

    // Compute actual velocity (ticks per second).
    let delta_l = ticks_l - ts.prev_ticks_l;
    let delta_r = ticks_r - ts.prev_ticks_r;
    let vel_l = (delta_l * 1000 / dt_ms as i64) as i32;
    let vel_r = (delta_r * 1000 / dt_ms as i64) as i32;

    // Save current state for next iteration.
    ts.prev_ticks_l = ticks_l;
    ts.prev_ticks_r = ticks_r;
    ts.prev_time = now;

    // Drop the tick state lock before acquiring PID lock (avoid nested lock order issues).
    drop(ts);

    // Read targets.
    let target_l = TARGET_SPEED_L.load(Ordering::Relaxed) as i32;
    let target_r = TARGET_SPEED_R.load(Ordering::Relaxed) as i32;

    // Run PID controllers.
    let mut pids = PID_CONTROLLERS.lock();
    let pwm_l = pids[CH_LEFT].update(target_l, vel_l, dt_ms);
    let pwm_r = pids[CH_RIGHT].update(target_r, vel_r, dt_ms);

    (pwm_l, pwm_r)
}

/// Check whether PID velocity control is enabled.
pub fn motor_pid_enabled() -> bool {
    PID_ENABLED.load(Ordering::Acquire)
}

/// Enable or disable PID velocity control.
///
/// When disabled, the RT motor task should apply motor commands directly
/// (open-loop). When enabled, `motor_pid_tick()` handles closed-loop control.
pub fn motor_pid_enable(en: bool) {
    if en && !motor_pid_enabled() {
        // Reset controllers when re-enabling to avoid integral windup carryover.
        let mut pids = PID_CONTROLLERS.lock();
        pids[CH_LEFT].reset();
        pids[CH_RIGHT].reset();
        let mut ts = TICK_STATE.lock();
        ts.prev_time = 0;
    }
    PID_ENABLED.store(en, Ordering::Release);
    crate::kprintln!("[MOTOR-PID] PID {}", if en { "ENABLED" } else { "DISABLED" });
}

/// Update PID gains at runtime (e.g., from shell or config).
pub fn motor_pid_set_gains(kp: i32, ki: i32, kd: i32) {
    let mut pids = PID_CONTROLLERS.lock();
    for pid in pids.iter_mut() {
        *pid = PidController::new(kp, ki, kd);
    }
    crate::kprintln!("[MOTOR-PID] Gains updated: Kp={} Ki={} Kd={}", kp, ki, kd);
}

/// Reset both PID controllers (zero integral and derivative state).
pub fn motor_pid_reset() {
    let mut pids = PID_CONTROLLERS.lock();
    pids[CH_LEFT].reset();
    pids[CH_RIGHT].reset();
    TARGET_SPEED_L.store(0, Ordering::Relaxed);
    TARGET_SPEED_R.store(0, Ordering::Relaxed);
    let mut ts = TICK_STATE.lock();
    *ts = TickState::new();
}
