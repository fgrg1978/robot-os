//! E04: Payload Abstraction — spray pump, gripper servo, camera trigger.
//!
//! The Brain Server sends `PKT_PAYLOAD` commands that route here.
//! All hardware assignments are named constants — no magic numbers.
//!
//! Hardware assignments (VF2 GPIO / PWM):
//!   SPRAY        → GPIO pin 20   (MOSFET drives 12 V pump)
//!   CAM_TRIGGER  → GPIO pin 21   (3.3 V pulse → external camera shutter)
//!   GRIPPER      → PWM channel 4 (hobby servo, 50 Hz)
//!
//! On QEMU the GPIO/PWM drivers use an in-memory simulation, so the same
//! code path runs in simulation and on real hardware.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use robot_os_drivers::clint::get_time;
use robot_os_drivers::gpio::{GpioDir, gpio_set_direction, gpio_write};
use robot_os_drivers::pwm::{pwm_enable, pwm_set_period, pwm_set_duty};

use crate::brain_protocol::{
    PayloadCmd,
    PAYLOAD_TYPE_SPRAY, PAYLOAD_TYPE_GRIPPER, PAYLOAD_TYPE_CAM_TRIGGER,
    PAYLOAD_OFF,
};

// ── Hardware pin / channel assignments ───────────────────────────────────────

/// GPIO pin driving the spray pump MOSFET gate.
pub const PAYLOAD_GPIO_SPRAY: u32 = 20;
/// GPIO pin driving the external camera shutter trigger input.
pub const PAYLOAD_GPIO_CAM_TRIGGER: u32 = 21;
/// PWM channel connected to the gripper servo signal wire.
///
/// Currently non-functional on real hardware: `pwm_set_duty` is
/// unimplemented (see `drivers::pwm::pwm_set_duty`) and this channel index
/// (4) is out of range on the real 4-channel JH7110 PWM8 instance (valid:
/// 0-3) even if it were implemented. A working gripper needs its own
/// high-resolution timer to generate the ~50 Hz / 1-2 ms RC-servo signal —
/// the kernel's existing scheduler tick (10 ms) is far too coarse to
/// represent that pulse width at all. Deliberately deferred to hardware
/// bring-up (decided 2026-08): not attempted in this pass.
pub const PAYLOAD_PWM_GRIPPER: u32 = 4;

// ── Servo PWM constants (standard 50 Hz hobby servo) ─────────────────────────

/// Servo frame period: 20 ms = 50 Hz.
pub const GRIPPER_PWM_PERIOD_NS: u32 = 20_000_000;
/// Pulse width for fully open position (~180°): 2 ms.
pub const GRIPPER_PWM_OPEN_NS: u32 = 2_000_000;
/// Pulse width for fully closed position (~0°): 1 ms.
pub const GRIPPER_PWM_CLOSED_NS: u32 = 1_000_000;
/// Pulse width range used for proportional position mapping.
pub const GRIPPER_PWM_RANGE_NS: u32 = GRIPPER_PWM_OPEN_NS - GRIPPER_PWM_CLOSED_NS;

// ── Camera trigger constants ──────────────────────────────────────────────────

/// Camera shutter trigger pulse width in CLINT ticks.
/// At the default 10 MHz CLINT clock: 500_000 ticks = 50 ms.
/// Long enough for any DSLR / mirrorless shutter to register.
pub const CAM_TRIGGER_PULSE_TICKS: u64 = 500_000;

// ── Runtime state (lock-free atomics) ────────────────────────────────────────

/// True while the spray pump GPIO is driven high.
static SPRAY_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Current gripper position: 0 = fully closed, 100 = fully open.
static GRIPPER_POS: AtomicU8 = AtomicU8::new(0);

// ── Initialisation ────────────────────────────────────────────────────────────

/// Initialise payload GPIO outputs and PWM channels.
///
/// Must be called once after `gpio_init()` and `pwm_init()` during boot.
pub fn payload_init() {
    // Spray pump: GPIO output, default off.
    gpio_set_direction(PAYLOAD_GPIO_SPRAY, GpioDir::Output);
    gpio_write(PAYLOAD_GPIO_SPRAY, 0);

    // Camera trigger: GPIO output, default low.
    gpio_set_direction(PAYLOAD_GPIO_CAM_TRIGGER, GpioDir::Output);
    gpio_write(PAYLOAD_GPIO_CAM_TRIGGER, 0);

    // Gripper servo: PWM at 50 Hz, parked at closed position.
    // Every step's return code is checked and logged — silently discarding
    // these previously meant the gripper could fail to move on real
    // hardware with no diagnostic trail (see `drivers::pwm::pwm_set_duty`
    // doc comment: unimplemented on vf2/k1 pending JH7110 TRM duty
    // comparator info, so this WILL fail there today).
    if pwm_set_period(PAYLOAD_PWM_GRIPPER, GRIPPER_PWM_PERIOD_NS) != 0 {
        robot_os_drivers::kprintln!(
            "[PAYLOAD] gripper: pwm_set_period(ch={}) failed", PAYLOAD_PWM_GRIPPER
        );
    }
    if pwm_set_duty(PAYLOAD_PWM_GRIPPER, GRIPPER_PWM_CLOSED_NS) != 0 {
        robot_os_drivers::kprintln!(
            "[PAYLOAD] gripper: pwm_set_duty(ch={}) failed — gripper will not move \
             to the closed position on this platform", PAYLOAD_PWM_GRIPPER
        );
    }
    if pwm_enable(PAYLOAD_PWM_GRIPPER) != 0 {
        robot_os_drivers::kprintln!(
            "[PAYLOAD] gripper: pwm_enable(ch={}) failed", PAYLOAD_PWM_GRIPPER
        );
    }
}

// ── Command dispatch ──────────────────────────────────────────────────────────

/// Execute a decoded `PayloadCmd` received from the Brain Server.
///
/// Returns `true` if the command type was recognised and handled.
pub fn payload_exec(cmd: PayloadCmd) -> bool {
    match cmd.payload_type {
        PAYLOAD_TYPE_SPRAY       => payload_spray(cmd.value != PAYLOAD_OFF),
        PAYLOAD_TYPE_GRIPPER     => payload_gripper(cmd.value),
        PAYLOAD_TYPE_CAM_TRIGGER => payload_cam_trigger(),
        _                        => false,
    }
}

// ── Individual payload actions ────────────────────────────────────────────────

/// Activate or deactivate the spray pump.
///
/// `on = true` → GPIO high (MOSFET on → pump runs).
/// `on = false` → GPIO low (pump off).
pub fn payload_spray(on: bool) -> bool {
    gpio_write(PAYLOAD_GPIO_SPRAY, if on { 1 } else { 0 });
    SPRAY_ACTIVE.store(on, Ordering::Relaxed);
    true
}

/// Set the gripper servo position.
///
/// `pos` is clamped to `0..=100` where 0 = fully closed, 100 = fully open.
/// Maps linearly to the PWM pulse width range 1 ms … 2 ms.
pub fn payload_gripper(pos: u8) -> bool {
    let clamped = pos.min(100) as u32;
    // Linear map: pos=0 → CLOSED_NS, pos=100 → OPEN_NS
    let duty_ns = GRIPPER_PWM_CLOSED_NS + clamped * GRIPPER_PWM_RANGE_NS / 100;
    if pwm_set_duty(PAYLOAD_PWM_GRIPPER, duty_ns) != 0 {
        robot_os_drivers::kprintln!(
            "[PAYLOAD] gripper: pwm_set_duty(ch={}, pos={}) failed — gripper did not move",
            PAYLOAD_PWM_GRIPPER, clamped
        );
        return false;
    }
    GRIPPER_POS.store(clamped as u8, Ordering::Relaxed);
    true
}

/// Fire a single external camera shutter trigger pulse.
///
/// Drives `PAYLOAD_GPIO_CAM_TRIGGER` high for `CAM_TRIGGER_PULSE_TICKS`
/// CLINT ticks, then drives it low again.  Uses a busy-wait; call only
/// from a non-RT task (behavior_task is fine).
pub fn payload_cam_trigger() -> bool {
    gpio_write(PAYLOAD_GPIO_CAM_TRIGGER, 1);
    let start = get_time();
    while get_time().wrapping_sub(start) < CAM_TRIGGER_PULSE_TICKS {}
    gpio_write(PAYLOAD_GPIO_CAM_TRIGGER, 0);
    true
}

// ── State accessors ───────────────────────────────────────────────────────────

/// Returns `true` if the spray pump is currently active.
pub fn payload_spray_active() -> bool {
    SPRAY_ACTIVE.load(Ordering::Relaxed)
}

/// Returns the current gripper position (0 = closed, 100 = open).
pub fn payload_gripper_pos() -> u8 {
    GRIPPER_POS.load(Ordering::Relaxed)
}
