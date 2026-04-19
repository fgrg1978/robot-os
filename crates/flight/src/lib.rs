#![no_std]

//! Flight controller — mixer, PID, modes (Phases J + K) + drone phases D01-D06.
//!
//! Provides the core flight control components for multirotor drones:
//! - **Mixer**: converts roll/pitch/yaw/throttle commands into per-motor throttle
//! - **Flight PID**: cascaded angle → rate PID for 3 axes + altitude hold
//! - **Flight modes**: Disarmed, Manual, Stabilize, AltHold, PosHold, Auto, RTL, Land
//! - **Failsafe**: automatic mode transitions on link/sensor loss
//!
//! All arithmetic is integer (no `f32`).  PID gains are stored × 1000.
//!
//! # Channels
//!
//! - `CH_FLIGHT_TARGET` — desired attitude/throttle (from RC or server)
//! - `CH_RC_INPUT`      — raw RC receiver channels

// D01-D06: drone-critical modules
pub mod ekf;
pub mod sitl;
pub mod path3d;
pub mod terrain;
pub mod slam;

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use robot_os_channel::Channel;

// ── Constants ───────────────────────────────────────────────────────────────

/// Maximum number of motors supported.
pub const MAX_MOTORS: usize = 8;

/// Throttle range: 0 = off, 1000 = full power.
pub const THROTTLE_MAX: u16 = 1000;

/// Minimum throttle when armed (keeps propellers spinning for response).
pub const THROTTLE_IDLE: u16 = 50;

// ── Channels ────────────────────────────────────────────────────────────────

/// Channel for flight target (from RC input, autopilot, or server).
pub static CH_FLIGHT_TARGET: Channel<FlightTarget> = Channel::new(FlightTarget::new());

/// Channel for RC receiver input.
pub static CH_RC_INPUT: Channel<RcInput> = Channel::new(RcInput::new());

// ── Types ───────────────────────────────────────────────────────────────────

/// Desired flight state (from pilot or autopilot).
#[derive(Clone, Copy)]
pub struct FlightTarget {
    /// Target roll angle in centi-degrees (-18000..+18000).
    pub roll_cdeg: i32,
    /// Target pitch angle in centi-degrees (-9000..+9000).
    pub pitch_cdeg: i32,
    /// Target yaw rate in milli-degrees/sec.
    pub yaw_rate_mdps: i32,
    /// Throttle 0-1000 (0.0%-100.0%).
    pub throttle: u16,
    /// Target altitude in mm (for AltHold/PosHold modes).
    pub alt_mm: i32,
}

impl FlightTarget {
    pub const fn new() -> Self {
        FlightTarget {
            roll_cdeg: 0,
            pitch_cdeg: 0,
            yaw_rate_mdps: 0,
            throttle: 0,
            alt_mm: 0,
        }
    }
}

/// RC receiver input (SBUS / PPM).
#[derive(Clone, Copy)]
pub struct RcInput {
    /// Channel values in microseconds (typically 1000-2000, center 1500).
    pub channels: [u16; 16],
    /// RSSI (0-100%).
    pub rssi: u8,
    /// True if receiver has lost signal.
    pub failsafe: bool,
}

impl RcInput {
    pub const fn new() -> Self {
        RcInput {
            channels: [1500; 16],
            rssi: 0,
            failsafe: true,
        }
    }

    /// Map a channel (1000-2000us) to a signed value (-500..+500).
    pub fn channel_signed(&self, ch: usize) -> i32 {
        if ch >= 16 { return 0; }
        self.channels[ch] as i32 - 1500
    }

    /// Map a channel (1000-2000us) to 0-1000 range.
    pub fn channel_unsigned(&self, ch: usize) -> u16 {
        if ch >= 16 { return 0; }
        let v = self.channels[ch];
        if v <= 1000 { 0 }
        else if v >= 2000 { 1000 }
        else { v - 1000 }
    }
}

/// Mixer output per motor.
#[derive(Clone, Copy)]
pub struct MixerOutput {
    /// Throttle per motor (0-1000).
    pub motors: [u16; MAX_MOTORS],
    /// Number of active motors.
    pub count: u8,
}

impl MixerOutput {
    pub const fn new() -> Self {
        MixerOutput { motors: [0; MAX_MOTORS], count: 0 }
    }
}

/// Frame type (multirotor geometry).
#[derive(Clone, Copy, PartialEq)]
pub enum FrameType {
    QuadX,
    QuadPlus,
    Hex,
    Octo,
    // D04: extended configurations
    /// Tricopter: 2 front motors + 1 rear with servo yaw.
    Tri,
    /// Y6: 6-motor Y layout (3 arms, co-axial pairs — top CW, bottom CCW).
    Y6,
    /// Hex-X (60° arm offsets, alternating CW/CCW).
    HexX,
    /// Co-axial quad (4 arms, 2 motors each: top CW, bottom CCW).
    Coax,
}

/// Flight mode.
#[derive(Clone, Copy, PartialEq)]
pub enum FlightMode {
    /// Motors off, cannot fly.
    Disarmed,
    /// RC direct to mixer (only rate PID).
    Manual,
    /// RC = target angle, angle + rate PID.
    Stabilize,
    /// Stabilize + altitude PID.
    AltHold,
    /// AltHold + GPS position PID.
    PosHold,
    /// Follow waypoints from server.
    Auto,
    /// Return To Launch (failsafe).
    RTL,
    /// Controlled descent.
    Land,
}

impl FlightMode {
    pub fn name(&self) -> &'static str {
        match self {
            FlightMode::Disarmed  => "Disarmed",
            FlightMode::Manual    => "Manual",
            FlightMode::Stabilize => "Stabilize",
            FlightMode::AltHold   => "AltHold",
            FlightMode::PosHold   => "PosHold",
            FlightMode::Auto      => "Auto",
            FlightMode::RTL       => "RTL",
            FlightMode::Land      => "Land",
        }
    }

    pub fn from_str(s: &[u8]) -> Option<FlightMode> {
        match s {
            b"disarmed"  | b"off"  => Some(FlightMode::Disarmed),
            b"manual"              => Some(FlightMode::Manual),
            b"stabilize" | b"stab" => Some(FlightMode::Stabilize),
            b"althold"   | b"alt"  => Some(FlightMode::AltHold),
            b"poshold"   | b"pos"  => Some(FlightMode::PosHold),
            b"auto"                => Some(FlightMode::Auto),
            b"rtl"                 => Some(FlightMode::RTL),
            b"land"                => Some(FlightMode::Land),
            _ => None,
        }
    }
}

// ── Global flight state ─────────────────────────────────────────────────────

static ARMED: AtomicBool = AtomicBool::new(false);
static FLIGHT_MODE: AtomicU8 = AtomicU8::new(0); // 0 = Disarmed

/// Check if motors are armed.
pub fn is_armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

/// Arm the motors (enable flight).
pub fn flight_arm() -> bool {
    if flight_mode() != FlightMode::Disarmed {
        // Already in a flying mode — just set armed flag.
        ARMED.store(true, Ordering::Release);
        return true;
    }
    // Transition from Disarmed → Stabilize by default.
    ARMED.store(true, Ordering::Release);
    set_flight_mode(FlightMode::Stabilize);
    robot_os_drivers::kprintln!("[FLIGHT] ARMED — Stabilize mode");
    true
}

/// Disarm the motors (stop flight).
pub fn flight_disarm() {
    ARMED.store(false, Ordering::Release);
    set_flight_mode(FlightMode::Disarmed);
    robot_os_drivers::kprintln!("[FLIGHT] DISARMED");
}

/// Get current flight mode.
pub fn flight_mode() -> FlightMode {
    match FLIGHT_MODE.load(Ordering::Acquire) {
        1 => FlightMode::Manual,
        2 => FlightMode::Stabilize,
        3 => FlightMode::AltHold,
        4 => FlightMode::PosHold,
        5 => FlightMode::Auto,
        6 => FlightMode::RTL,
        7 => FlightMode::Land,
        _ => FlightMode::Disarmed,
    }
}

/// Set flight mode.
pub fn set_flight_mode(mode: FlightMode) {
    let val = match mode {
        FlightMode::Disarmed  => 0,
        FlightMode::Manual    => 1,
        FlightMode::Stabilize => 2,
        FlightMode::AltHold   => 3,
        FlightMode::PosHold   => 4,
        FlightMode::Auto      => 5,
        FlightMode::RTL       => 6,
        FlightMode::Land      => 7,
    };
    FLIGHT_MODE.store(val, Ordering::Release);
}

// ── Mixer ───────────────────────────────────────────────────────────────────

/// Compute per-motor throttle from control inputs.
///
/// - `frame`: multirotor geometry
/// - `throttle`: base throttle (0-1000)
/// - `roll`: roll correction (-1000..+1000)
/// - `pitch`: pitch correction (-1000..+1000)
/// - `yaw`: yaw correction (-1000..+1000)
///
/// Returns per-motor throttle values (0-1000).
pub fn mixer_compute(
    frame: FrameType,
    throttle: i32,
    roll: i32,
    pitch: i32,
    yaw: i32,
) -> MixerOutput {
    let mut out = MixerOutput::new();

    match frame {
        FrameType::QuadX => {
            out.count = 4;
            // QuadX layout (looking down, front is up):
            //   M1(CW)  M2(CCW)     front-right / front-left
            //   M3(CCW) M4(CW)      rear-left   / rear-right
            //
            // Motor 1 (front-right): +T -R +P -Y
            // Motor 2 (front-left):  +T +R +P +Y
            // Motor 3 (rear-left):   +T +R -P -Y
            // Motor 4 (rear-right):  +T -R -P +Y
            let m1 = throttle - roll + pitch - yaw;
            let m2 = throttle + roll + pitch + yaw;
            let m3 = throttle + roll - pitch - yaw;
            let m4 = throttle - roll - pitch + yaw;

            out.motors[0] = clamp_throttle(m1);
            out.motors[1] = clamp_throttle(m2);
            out.motors[2] = clamp_throttle(m3);
            out.motors[3] = clamp_throttle(m4);
        }
        FrameType::QuadPlus => {
            out.count = 4;
            // Quad+ layout:
            //       M1(CW)        front
            //   M2(CCW)  M3(CCW)  left / right
            //       M4(CW)        rear
            let m1 = throttle + pitch - yaw;
            let m2 = throttle + roll  + yaw;
            let m3 = throttle - roll  + yaw;
            let m4 = throttle - pitch - yaw;

            out.motors[0] = clamp_throttle(m1);
            out.motors[1] = clamp_throttle(m2);
            out.motors[2] = clamp_throttle(m3);
            out.motors[3] = clamp_throttle(m4);
        }
        FrameType::Hex => {
            out.count = 6;
            // Hex layout (simplified — equal 60° spacing):
            let m1 = throttle + pitch - yaw;
            let m2 = throttle + roll / 2 + pitch / 2 + yaw;
            let m3 = throttle + roll / 2 - pitch / 2 - yaw;
            let m4 = throttle - pitch + yaw;
            let m5 = throttle - roll / 2 - pitch / 2 - yaw;
            let m6 = throttle - roll / 2 + pitch / 2 + yaw;

            out.motors[0] = clamp_throttle(m1);
            out.motors[1] = clamp_throttle(m2);
            out.motors[2] = clamp_throttle(m3);
            out.motors[3] = clamp_throttle(m4);
            out.motors[4] = clamp_throttle(m5);
            out.motors[5] = clamp_throttle(m6);
        }
        FrameType::Octo => {
            out.count = 8;
            // Octo (simplified — 8 equal spacing):
            for i in 0..8 {
                out.motors[i] = clamp_throttle(throttle);
            }
            // Apply roll/pitch/yaw with 45° mixing ratios.
            // sin(0)=0, sin(45)=707/1000, sin(90)=1000/1000
            let s45: i32 = 707; // sin(45°) × 1000
            let corrections: [(i32, i32, i32); 8] = [
                ( 0,     1000,  -1), // front       : +P -Y
                ( s45,   s45,    1), // front-left  : +R +P +Y
                ( 1000,  0,     -1), // left        : +R -Y
                ( s45,  -s45,    1), // rear-left   : +R -P +Y
                ( 0,    -1000,  -1), // rear        : -P -Y
                (-s45,  -s45,    1), // rear-right  : -R -P +Y
                (-1000,  0,     -1), // right       : -R -Y
                (-s45,   s45,    1), // front-right : -R +P +Y
            ];
            for (i, &(cr, cp, cy)) in corrections.iter().enumerate() {
                let m = throttle + roll * cr / 1000 + pitch * cp / 1000 + yaw * cy;
                out.motors[i] = clamp_throttle(m);
            }
        }
        // D04: Tricopter (2 front + 1 rear; yaw via rear servo — approximated here
        // as yaw authority split across front motors with opposite signs).
        FrameType::Tri => {
            out.count = 3;
            // Motor 1 (front-right, CW): +T -R +P
            // Motor 2 (front-left, CCW): +T +R +P
            // Motor 3 (rear, CW/servo): +T -P ; yaw via tilt servo (not modeled)
            out.motors[0] = clamp_throttle(throttle - roll + pitch - yaw / 2);
            out.motors[1] = clamp_throttle(throttle + roll + pitch + yaw / 2);
            out.motors[2] = clamp_throttle(throttle - pitch);
        }

        // D04: Y6 — 3-arm Y, co-axial pairs.
        // Arms at 0°(front), 120°(rear-left), 240°(rear-right).
        // Top motors (CW): M1, M3, M5.  Bottom motors (CCW): M2, M4, M6.
        // sin(120°)=866/1000, cos(120°)=-500/1000.
        FrameType::Y6 => {
            out.count = 6;
            const S120: i32 = 866;
            const C120: i32 = -500;
            // Arm force contributions (×1000 normalized):
            // Front arm:     roll_factor=0,     pitch_factor=1000
            // Rear-left arm: roll_factor=-S120, pitch_factor=C120
            // Rear-right arm:roll_factor=+S120, pitch_factor=C120
            let t = throttle / 2; // split evenly across top+bottom per arm
            // Front top/bottom:
            out.motors[0] = clamp_throttle(t + pitch - yaw); // front top
            out.motors[1] = clamp_throttle(t + pitch + yaw); // front bottom
            // Rear-left top/bottom:
            out.motors[2] = clamp_throttle(t - roll * S120/1000 + pitch * C120/1000 - yaw);
            out.motors[3] = clamp_throttle(t - roll * S120/1000 + pitch * C120/1000 + yaw);
            // Rear-right top/bottom:
            out.motors[4] = clamp_throttle(t + roll * S120/1000 + pitch * C120/1000 - yaw);
            out.motors[5] = clamp_throttle(t + roll * S120/1000 + pitch * C120/1000 + yaw);
        }

        // D04: HexX — 6 motors at 30/90/150/210/270/330° (hex-X layout).
        // Motors alternate CW/CCW starting with CW at 30°.
        // sin/cos values for 30° multiples (×1000):
        // 30°: s=500 c=866; 90°: s=1000 c=0; 150°: s=500 c=-866
        FrameType::HexX => {
            out.count = 6;
            // Arm angles (cdeg) for hex-X: 30, 90, 150, 210, 270, 330
            // Roll contribution: sin(angle), Pitch: cos(angle), Yaw: alternating ±1
            const ARMS: [(i32, i32, i32); 6] = [
                ( 500,  866, -1), //  30°: right-front   CW
                ( 1000, 0,    1), //  90°: right          CCW
                ( 500, -866, -1), // 150°: right-rear    CW
                (-500, -866,  1), // 210°: left-rear     CCW
                (-1000, 0,   -1), // 270°: left           CW
                (-500,  866,  1), // 330°: left-front    CCW
            ];
            for (i, &(sr, cp, cy)) in ARMS.iter().enumerate() {
                let m = throttle
                    + roll  * sr / 1000
                    + pitch * cp / 1000
                    + yaw   * cy;
                out.motors[i] = clamp_throttle(m);
            }
        }

        // D04: Co-axial quad (X layout, each arm has top CW + bottom CCW).
        // 4 arms (45°/135°/225°/315°), 2 motors per arm = 8 motors total.
        FrameType::Coax => {
            out.count = 8;
            let s45: i32 = 707;
            // Arms: FR, FL, RL, RR
            let arms: [(i32, i32); 4] = [
                (-s45,  s45), // front-right: -R +P
                ( s45,  s45), // front-left:  +R +P
                ( s45, -s45), // rear-left:   +R -P
                (-s45, -s45), // rear-right:  -R -P
            ];
            for (i, &(ar, ap)) in arms.iter().enumerate() {
                let t = throttle / 2;
                let m_top = t + roll * ar / 1000 + pitch * ap / 1000 - yaw;
                let m_bot = t + roll * ar / 1000 + pitch * ap / 1000 + yaw;
                out.motors[i * 2]     = clamp_throttle(m_top);
                out.motors[i * 2 + 1] = clamp_throttle(m_bot);
            }
        }
    }

    out
}

fn clamp_throttle(v: i32) -> u16 {
    if v <= 0 { 0 }
    else if v >= THROTTLE_MAX as i32 { THROTTLE_MAX }
    else { v as u16 }
}

// ── PID controller ──────────────────────────────────────────────────────────

/// Integer PID controller.  Gains are stored × 1000.
#[derive(Clone, Copy)]
pub struct Pid {
    /// Proportional gain × 1000.
    pub kp: i32,
    /// Integral gain × 1000.
    pub ki: i32,
    /// Derivative gain × 1000.
    pub kd: i32,
    /// Accumulated integral.
    integral: i32,
    /// Previous error (for derivative).
    prev_error: i32,
    /// Output clamp (min).
    pub out_min: i32,
    /// Output clamp (max).
    pub out_max: i32,
    /// Integral windup limit.
    pub i_max: i32,
}

impl Pid {
    pub const fn new(kp: i32, ki: i32, kd: i32, out_min: i32, out_max: i32) -> Self {
        Pid {
            kp, ki, kd,
            integral: 0,
            prev_error: 0,
            out_min, out_max,
            i_max: out_max * 1000, // default windup limit
        }
    }

    /// Run one PID update.  Returns control output.
    ///
    /// - `error`: setpoint - measurement
    /// - `dt_us`: time delta in microseconds
    pub fn update(&mut self, error: i32, dt_us: u32) -> i32 {
        if dt_us == 0 { return 0; }

        // P term.
        let p = (self.kp as i64 * error as i64 / 1000) as i32;

        // I term: integral += error * dt_us / 1_000_000.
        // Scale: integral is in error·seconds × 1000.
        self.integral += (error as i64 * dt_us as i64 / 1_000_000) as i32;
        // Anti-windup clamp.
        if self.integral > self.i_max { self.integral = self.i_max; }
        if self.integral < -self.i_max { self.integral = -self.i_max; }
        let i = (self.ki as i64 * self.integral as i64 / 1000) as i32;

        // D term: derivative = (error - prev_error) / dt.
        // d_error per second = (error - prev) * 1_000_000 / dt_us.
        let d_error = ((error - self.prev_error) as i64 * 1_000_000 / dt_us as i64) as i32;
        let d = (self.kd as i64 * d_error as i64 / 1000) as i32;
        self.prev_error = error;

        // Total output, clamped.
        let out = p + i + d;
        if out > self.out_max { self.out_max }
        else if out < self.out_min { self.out_min }
        else { out }
    }

    /// Reset integral and derivative state.
    pub fn reset(&mut self) {
        self.integral = 0;
        self.prev_error = 0;
    }
}

// ── Cascaded flight PID ─────────────────────────────────────────────────────

/// Cascaded PID for flight control: outer loop (angle) → inner loop (rate).
pub struct FlightPid {
    /// Angle PID: roll, pitch, yaw (outer loop, ~250 Hz).
    pub angle_pid: [Pid; 3],
    /// Rate PID: roll, pitch, yaw (inner loop, ~1000 Hz).
    pub rate_pid: [Pid; 3],
    /// Altitude hold PID (~50 Hz).
    pub alt_pid: Pid,
}

impl FlightPid {
    /// Create with default tuning values.
    pub const fn new() -> Self {
        // Angle PID: moderate gains, output is target rate in mdps.
        let angle = Pid::new(4500, 500, 0, -30000, 30000); // ±30000 mdps max rate
        // Rate PID: faster response, output is mixer correction (0-500).
        let rate = Pid::new(1200, 300, 100, -500, 500);
        // Alt PID: slow, output is throttle offset.
        let alt = Pid::new(2000, 200, 500, -300, 300);

        FlightPid {
            angle_pid: [angle, angle, angle],
            rate_pid: [rate, rate, rate],
            alt_pid: alt,
        }
    }

    /// Run cascaded PID for one axis.
    ///
    /// - `angle_error`: target_angle - measured_angle (centi-degrees)
    /// - `gyro_rate`: measured angular rate (milli-degrees/sec)
    /// - `axis`: 0=roll, 1=pitch, 2=yaw
    /// - `dt_us`: time delta in microseconds
    ///
    /// Returns mixer correction value (-500..+500).
    pub fn update_axis(&mut self, angle_error: i32, gyro_rate: i32, axis: usize, dt_us: u32) -> i32 {
        if axis >= 3 { return 0; }

        // Outer loop: angle error → target rate.
        let target_rate = self.angle_pid[axis].update(angle_error, dt_us);

        // Inner loop: rate error → control output.
        let rate_error = target_rate - gyro_rate;
        self.rate_pid[axis].update(rate_error, dt_us)
    }

    /// Run altitude hold PID.
    ///
    /// - `alt_error`: target_alt - measured_alt (millimetres)
    /// - `dt_us`: time delta
    ///
    /// Returns throttle offset (-300..+300).
    pub fn update_alt(&mut self, alt_error: i32, dt_us: u32) -> i32 {
        self.alt_pid.update(alt_error, dt_us)
    }

    /// Reset all PID state.
    pub fn reset(&mut self) {
        for pid in &mut self.angle_pid { pid.reset(); }
        for pid in &mut self.rate_pid { pid.reset(); }
        self.alt_pid.reset();
    }
}

// ── Failsafe ────────────────────────────────────────────────────────────────

/// Failsafe priority chain result.
#[derive(Clone, Copy, PartialEq)]
pub enum FailsafeAction {
    /// No failsafe active — continue normal operation.
    None,
    /// Switch to position hold (server link lost).
    PosHold,
    /// Return to launch (RC link lost).
    RTL,
    /// Level and descend (attitude estimation failure).
    Land,
    /// Immediate motor shutoff (HW watchdog / critical failure).
    Disarm,
}

/// Check failsafe conditions.
///
/// - `attitude_age_us`: age of last attitude estimate in microseconds
/// - `rc_age_us`: age of last RC input in microseconds
/// - `server_age_us`: age of last server command in microseconds
pub fn check_failsafe(attitude_age_us: u64, rc_age_us: u64, server_age_us: u64) -> FailsafeAction {
    // Priority 1: attitude estimation failure (>50 ms old).
    if attitude_age_us > 50_000 {
        return FailsafeAction::Land;
    }

    // Priority 2: RC link loss (>1 second).
    if rc_age_us > 1_000_000 {
        return FailsafeAction::RTL;
    }

    // Priority 3: server link loss (>3 seconds) — switch to PosHold.
    if server_age_us > 3_000_000 {
        return FailsafeAction::PosHold;
    }

    FailsafeAction::None
}

// ── RC mapping ──────────────────────────────────────────────────────────────

/// Standard RC channel mapping.
pub const RC_CH_ROLL:     usize = 0;
pub const RC_CH_PITCH:    usize = 1;
pub const RC_CH_THROTTLE: usize = 2;
pub const RC_CH_YAW:      usize = 3;
pub const RC_CH_MODE:     usize = 4;
pub const RC_CH_AUX1:     usize = 5;

/// Convert RC input to flight target (Stabilize mode).
///
/// Roll/pitch: map ±500us to ±4500 cdeg (±45°).
/// Yaw: map ±500us to ±30000 mdps (±30°/s).
/// Throttle: map 0-1000 directly.
pub fn rc_to_target(rc: &RcInput) -> FlightTarget {
    FlightTarget {
        roll_cdeg:     rc.channel_signed(RC_CH_ROLL) * 9,     // ±500 → ±4500 cdeg
        pitch_cdeg:    rc.channel_signed(RC_CH_PITCH) * 9,    // ±500 → ±4500 cdeg
        yaw_rate_mdps: rc.channel_signed(RC_CH_YAW) * 60,     // ±500 → ±30000 mdps
        throttle:      rc.channel_unsigned(RC_CH_THROTTLE),
        alt_mm:        0,
    }
}

// ── Info ─────────────────────────────────────────────────────────────────────

/// Print flight controller status.
pub fn flight_info() {
    let mode = flight_mode();
    let armed = is_armed();
    robot_os_drivers::kprintln!("[FLIGHT] Mode: {} | Armed: {}", mode.name(), armed);

    let target_snap = CH_FLIGHT_TARGET.read();
    if target_snap.seq > 0 {
        let t = target_snap.val;
        robot_os_drivers::kprintln!("[FLIGHT] Target: roll={} pitch={} yaw_rate={} thr={}",
            t.roll_cdeg, t.pitch_cdeg, t.yaw_rate_mdps, t.throttle);
    }

    let rc_snap = CH_RC_INPUT.read();
    if rc_snap.seq > 0 {
        let rc = rc_snap.val;
        robot_os_drivers::kprintln!("[FLIGHT] RC: ch1={} ch2={} ch3={} ch4={} rssi={} fs={}",
            rc.channels[0], rc.channels[1], rc.channels[2], rc.channels[3],
            rc.rssi, rc.failsafe);
    } else {
        robot_os_drivers::kprintln!("[FLIGHT] RC: no data");
    }
}
