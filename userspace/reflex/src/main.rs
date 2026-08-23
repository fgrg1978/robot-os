//! Reflex Daemon -local obstacle avoidance without brain connection.
//!
//! Runs as a user-mode process alongside brain_client. Continuously reads
//! the rangefinder and IMU, and overrides motor commands when danger is
//! detected. This is a safety layer that works even if the brain server
//! is unreachable or the TCP link is down.
//!
//! Priority: reflex overrides brain commands when triggered.
//! Implementation: reads sensors at high rate, publishes motor commands
//! only when an override is needed (obstacle or tilt detected).
//!
//! Behaviors (Brooks subsumption style, highest priority first):
//!   1. E-STOP:  extreme tilt (fall) → motors off
//!   2. BACKUP:  obstacle < CRITICAL_MM → reverse briefly
//!   3. TURN:    obstacle < WARNING_MM → turn away
//!   4. PASS:    no danger → do nothing (brain_client controls)

#![no_std]
#![no_main]

use robot_os_libsys as sys;

// ── Constants ────────────────────────────────────────────────────────────────

// Obstacle thresholds (mm)
const OBSTACLE_CRITICAL_MM: u16 = 150;   // immediate reverse
const OBSTACLE_WARNING_MM: u16 = 400;    // turn away
const OBSTACLE_CLEAR_MM: u16 = 600;      // resume normal

// Tilt threshold (milli-g, ~45 degrees)
const TILT_ESTOP_MG: i32 = 700;

// Motor speeds
//
// There are no speed constants here any more, and that is a kernel-ABI
// limitation, not an oversight. This file used to carry
// `BACKUP_SPEED = 30` / `TURN_SPEED = 40` and reverse by passing the
// negation of them to `sys::motor_speed`, on the strength of a libsys doc
// that read "signed: positive = forward, negative = reverse".
//
// That doc was wrong. `sys_motor_speed` (crates/syscall/src/handlers.rs:753)
// takes an UNSIGNED percentage, hard-codes `MotorDir::Forward`, and
// `motor_set` (crates/robot/src/motor.rs:91) clamps it with
// `speed_pct.min(100)`. A sign-extended -30 arrives as 0xFFFF...E2 and
// clamps to 100 — so "reverse away from the obstacle" drove BOTH MOTORS
// FULL SPEED FORWARD INTO IT. On QEMU that was a log line; on the robot it
// is a collision, and it is the reason this daemon is here at all.
//
// The untyped motor ABI cannot express a (direction, speed) pair:
// `sys::motor_speed` sets speed and forces Forward;
// `sys::motor_set_direction` sets direction and forces a kernel-fixed 50%.
// The behaviours below therefore run at 50% rather than the 30/40 they
// used to ask for. Restoring the chosen speeds needs a kernel-side syscall
// carrying both — see the ABI audit report.

// Timing
const REFLEX_PERIOD_MS: u64 = 25;       // 40 Hz -faster than brain's 20 Hz
const BACKUP_DURATION_MS: u64 = 500;     // reverse for 500ms
const TURN_DURATION_MS: u64 = 400;       // turn for 400ms
const ESTOP_HOLD_MS: u64 = 2000;         // hold e-stop for 2s before re-checking

// Sensor types — re-exported from libsys
use sys::{SENSOR_TYPE_IMU, SENSOR_TYPE_RANGE};

// Motor IDs
const MOTOR_LEFT: u64 = 0;
const MOTOR_RIGHT: u64 = 1;

// ── Sensor reading ──────────────────────────────────────────────────────────

struct ReflexSensors {
    range_front: u16,
    range_right: u16,
    accel_x_mg: i32,
    accel_y_mg: i32,
    accel_z_mg: i32,
}

impl ReflexSensors {
    fn new() -> Self {
        Self {
            range_front: u16::MAX,  // assume clear until first read
            range_right: u16::MAX,
            accel_x_mg: 0,
            accel_y_mg: 0,
            accel_z_mg: 1000,       // assume upright (1g on Z)
        }
    }

    fn read(&mut self) {
        // Rangefinder: 4 bytes
        let mut range_buf = [0u8; 4];
        if sys::sensor_read(SENSOR_TYPE_RANGE, &mut range_buf) >= 4 {
            self.range_front = u16::from_le_bytes([range_buf[0], range_buf[1]]);
            self.range_right = u16::from_le_bytes([range_buf[2], range_buf[3]]);
        }

        // IMU: 24 bytes (only need accel for tilt detection)
        let mut imu_buf = [0u8; 24];
        if sys::sensor_read(SENSOR_TYPE_IMU, &mut imu_buf) >= 12 {
            self.accel_x_mg = i32::from_le_bytes([
                imu_buf[0], imu_buf[1], imu_buf[2], imu_buf[3],
            ]);
            self.accel_y_mg = i32::from_le_bytes([
                imu_buf[4], imu_buf[5], imu_buf[6], imu_buf[7],
            ]);
            self.accel_z_mg = i32::from_le_bytes([
                imu_buf[8], imu_buf[9], imu_buf[10], imu_buf[11],
            ]);
        }
    }

    fn is_tilted(&self) -> bool {
        // Tilt detected when lateral acceleration exceeds threshold
        // (robot is falling or tipped)
        abs_i32(self.accel_x_mg) > TILT_ESTOP_MG
            || abs_i32(self.accel_y_mg) > TILT_ESTOP_MG
    }

    fn obstacle_front(&self) -> bool {
        self.range_front > 0 && self.range_front < OBSTACLE_WARNING_MM
    }

    fn obstacle_critical(&self) -> bool {
        self.range_front > 0 && self.range_front < OBSTACLE_CRITICAL_MM
    }

    fn front_clear(&self) -> bool {
        self.range_front == 0 || self.range_front >= OBSTACLE_CLEAR_MM
    }
}

fn abs_i32(v: i32) -> i32 {
    if v < 0 { -v } else { v }
}

// ── Reflex behaviors ────────────────────────────────────────────────────────

/// Stop both motors immediately.
fn motor_stop() {
    sys::motor_speed(MOTOR_LEFT, 0);
    sys::motor_speed(MOTOR_RIGHT, 0);
}

/// Reverse both motors briefly.
///
/// Reverse is only reachable through `motor_set_direction`, which is the
/// kernel's `SYS_MOTOR_ENABLE` — direction plus a fixed 50% speed. See the
/// note on the removed speed constants above.
fn motor_backup() {
    sys::motor_set_direction(MOTOR_LEFT, sys::MOTOR_DIR_BACKWARD);
    sys::motor_set_direction(MOTOR_RIGHT, sys::MOTOR_DIR_BACKWARD);
    sys::sleep(BACKUP_DURATION_MS);
    motor_stop();
}

/// Turn away from obstacle (spin in place: left reverses, right drives).
fn motor_turn_away() {
    sys::motor_set_direction(MOTOR_LEFT, sys::MOTOR_DIR_BACKWARD);
    sys::motor_set_direction(MOTOR_RIGHT, sys::MOTOR_DIR_FORWARD);
    sys::sleep(TURN_DURATION_MS);
    motor_stop();
}

// ── Main loop ───────────────────────────────────────────────────────────────

fn run() {
    sys::println(b"[reflex] Starting reflex daemon (obstacle avoidance)");

    let mut sensors = ReflexSensors::new();
    let mut overriding = false;

    loop {
        sensors.read();

        // Priority 1: E-STOP on tilt (fall detection)
        if sensors.is_tilted() {
            if !overriding {
                sys::print(b"[reflex] TILT DETECTED -E-STOP\n");
            }
            motor_stop();
            overriding = true;
            sys::sleep(ESTOP_HOLD_MS);
            continue;
        }

        // Priority 2: Critical obstacle -reverse
        if sensors.obstacle_critical() {
            if !overriding {
                sys::print(b"[reflex] CRITICAL OBSTACLE -BACKUP\n");
            }
            overriding = true;
            motor_backup();
            continue;
        }

        // Priority 3: Warning obstacle -turn away
        if sensors.obstacle_front() {
            if !overriding {
                sys::print(b"[reflex] OBSTACLE WARNING -TURNING\n");
            }
            overriding = true;
            motor_turn_away();
            continue;
        }

        // Priority 4: All clear -release control
        if overriding && sensors.front_clear() {
            sys::print(b"[reflex] Clear -releasing control\n");
            overriding = false;
            // Don't set motor speed -let brain_client resume control
        }

        sys::sleep(REFLEX_PERIOD_MS);
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    run();
    sys::exit(0);
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::print(b"[reflex] PANIC -stopping motors\n");
    motor_stop();
    sys::exit(1);
}
