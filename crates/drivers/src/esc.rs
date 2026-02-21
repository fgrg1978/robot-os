/// ESC (Electronic Speed Controller) driver — PWM output for brushless motors.
///
/// Phase J1: provides ESC initialization, arming sequence, and per-channel
/// throttle control.  In QEMU, stores state in memory (no real PWM).
/// On real hardware, wraps the PWM driver with ESC-specific timing.
///
/// ESC protocol: standard PWM 400 Hz
/// - 1000 µs pulse = 0% throttle (idle)
/// - 2000 µs pulse = 100% throttle (full power)
/// - Arm sequence: hold 1000 µs for 2 seconds

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};

/// Maximum ESC channels.
pub const ESC_MAX_CH: usize = 8;

/// ESC state: throttle per channel (0-1000 = 0%-100%).
static ESC_THROTTLE: [AtomicU16; ESC_MAX_CH] = [
    AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0),
    AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0),
];

static ESC_ARMED: AtomicBool = AtomicBool::new(false);
static ESC_COUNT: AtomicU8 = AtomicU8::new(0);
static ESC_READY: AtomicBool = AtomicBool::new(false);

/// Initialize ESC outputs.
///
/// `count`: number of ESC channels to use (1-8).
pub fn esc_init(count: u8) {
    let count = if count > ESC_MAX_CH as u8 { ESC_MAX_CH as u8 } else { count };
    ESC_COUNT.store(count, Ordering::Relaxed);

    // Set all channels to 0 (idle).
    for i in 0..ESC_MAX_CH {
        ESC_THROTTLE[i].store(0, Ordering::Relaxed);
    }

    ESC_ARMED.store(false, Ordering::Relaxed);
    ESC_READY.store(true, Ordering::Release);

    crate::kprintln!("[ESC] Initialized {} channels (simulated PWM 400 Hz)", count);
}

/// Arm the ESCs (send minimum throttle for arming sequence).
///
/// In real hardware, this would hold 1000 µs pulse for 2 seconds.
/// In QEMU simulation, just sets the armed flag.
pub fn esc_arm() {
    if !ESC_READY.load(Ordering::Acquire) { return; }

    // Set all channels to 0 (minimum throttle signal).
    let count = ESC_COUNT.load(Ordering::Relaxed) as usize;
    for i in 0..count {
        ESC_THROTTLE[i].store(0, Ordering::Relaxed);
    }

    ESC_ARMED.store(true, Ordering::Release);
    crate::kprintln!("[ESC] Armed ({} channels)", count);
}

/// Disarm the ESCs (cut all motor power).
pub fn esc_disarm() {
    let count = ESC_COUNT.load(Ordering::Relaxed) as usize;
    for i in 0..count {
        ESC_THROTTLE[i].store(0, Ordering::Relaxed);
    }
    ESC_ARMED.store(false, Ordering::Release);
    crate::kprintln!("[ESC] Disarmed");
}

/// Set throttle for a single ESC channel.
///
/// - `ch`: channel index (0-based)
/// - `pct`: throttle percentage × 10 (0-1000 = 0.0%-100.0%)
///
/// Only works when armed.  If not armed, silently ignored.
pub fn esc_set_throttle(ch: u8, pct: u16) {
    if !ESC_ARMED.load(Ordering::Acquire) { return; }
    let ch = ch as usize;
    if ch >= ESC_MAX_CH { return; }

    let pct = if pct > 1000 { 1000 } else { pct };
    ESC_THROTTLE[ch].store(pct, Ordering::Relaxed);

    // On real hardware, this would update the PWM duty cycle:
    // pulse_us = 1000 + pct (range 1000-2000 µs)
    // crate::pwm::pwm_set_duty(ch, 1000 + pct as u32);
}

/// Read current throttle value for a channel.
pub fn esc_get_throttle(ch: u8) -> u16 {
    let ch = ch as usize;
    if ch >= ESC_MAX_CH { return 0; }
    ESC_THROTTLE[ch].load(Ordering::Relaxed)
}

/// Check if ESCs are armed.
pub fn esc_is_armed() -> bool {
    ESC_ARMED.load(Ordering::Acquire)
}

/// Check if ESC driver is initialized.
pub fn esc_is_ready() -> bool {
    ESC_READY.load(Ordering::Acquire)
}

/// Get configured channel count.
pub fn esc_count() -> u8 {
    ESC_COUNT.load(Ordering::Relaxed)
}

/// Print ESC status info.
pub fn esc_info() {
    let ready = ESC_READY.load(Ordering::Acquire);
    if !ready {
        crate::kprintln!("[ESC] Not initialized");
        return;
    }
    let count = ESC_COUNT.load(Ordering::Relaxed);
    let armed = ESC_ARMED.load(Ordering::Acquire);
    crate::kprintln!("[ESC] Channels: {}  Armed: {}  PWM: 400 Hz (sim)", count, armed);

    for i in 0..count as usize {
        let thr = ESC_THROTTLE[i].load(Ordering::Relaxed);
        crate::kprintln!("[ESC]   M{}: {}‰", i + 1, thr);
    }
}
