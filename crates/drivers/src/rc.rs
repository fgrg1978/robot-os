/// RC (Remote Control) receiver driver — SBUS / PPM input.
///
/// Phase K1: provides RC receiver initialization and channel reading.
/// In QEMU, returns simulated neutral stick positions.
/// On real hardware, would parse SBUS (100K baud, inverted serial) or
/// PPM (timer capture) from a dedicated UART.
///
/// Standard channel mapping:
/// - CH1: Roll     (1000-2000, center 1500)
/// - CH2: Pitch    (1000-2000, center 1500)
/// - CH3: Throttle (1000-2000, min 1000)
/// - CH4: Yaw      (1000-2000, center 1500)
/// - CH5: Mode switch
/// - CH6+: Auxiliary

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use crate::clint;

/// RC input mode.
#[derive(Clone, Copy, PartialEq)]
pub enum RcMode {
    /// SBUS: 100000 baud, 8E2, inverted — most common in drones.
    Sbus,
    /// PPM: sum signal on single wire, timer capture.
    Ppm,
    /// Simulated: returns fixed values (QEMU).
    Simulated,
}

static RC_READY: AtomicBool = AtomicBool::new(false);
static RC_MODE: AtomicU8 = AtomicU8::new(0); // 0=Sbus, 1=Ppm, 2=Simulated
static RC_FAILSAFE: AtomicBool = AtomicBool::new(true);

/// Simulated RC channels (neutral sticks, throttle low).
static mut RC_CHANNELS: [u16; 16] = [
    1500, 1500, 1000, 1500,  // Roll, Pitch, Throttle, Yaw
    1000, 1500, 1500, 1500,  // Mode(low), Aux1-3
    1500, 1500, 1500, 1500,  // Aux4-7
    1500, 1500, 1500, 1500,  // Aux8-11
];

/// Last update timestamp.
static mut RC_LAST_UPDATE: u64 = 0;

/// Initialize RC receiver.
///
/// In QEMU, uses simulated mode.  On real hardware, configures UART
/// for SBUS (100000 baud, 8E2) or timer for PPM capture.
pub fn rc_init(mode: RcMode) {
    let mode_val = match mode {
        RcMode::Sbus => 0,
        RcMode::Ppm => 1,
        RcMode::Simulated => 2,
    };
    RC_MODE.store(mode_val, Ordering::Relaxed);
    RC_FAILSAFE.store(false, Ordering::Relaxed);
    RC_READY.store(true, Ordering::Release);

    unsafe { RC_LAST_UPDATE = clint::get_time(); }

    let mode_name = match mode {
        RcMode::Sbus => "SBUS",
        RcMode::Ppm => "PPM",
        RcMode::Simulated => "Simulated",
    };
    crate::kprintln!("[RC] Initialized (mode: {})", mode_name);
}

/// Read current RC channel values.
///
/// Returns an array of 16 channel values (1000-2000 µs range)
/// and failsafe flag.  Returns `None` if not initialized.
pub fn rc_read() -> Option<([u16; 16], bool)> {
    if !RC_READY.load(Ordering::Acquire) { return None; }

    let channels = unsafe { RC_CHANNELS };
    let failsafe = RC_FAILSAFE.load(Ordering::Acquire);
    Some((channels, failsafe))
}

/// Get timestamp of last RC update.
pub fn rc_last_update() -> u64 {
    unsafe { RC_LAST_UPDATE }
}

/// Feed simulated RC data (for testing).
///
/// Updates channel values and resets failsafe timer.
pub fn rc_set_channels(channels: &[u16; 16]) {
    unsafe {
        RC_CHANNELS = *channels;
        RC_LAST_UPDATE = clint::get_time();
    }
    RC_FAILSAFE.store(false, Ordering::Release);
}

/// Set failsafe state (simulates signal loss).
pub fn rc_set_failsafe(fs: bool) {
    RC_FAILSAFE.store(fs, Ordering::Release);
}

/// Check if RC is initialized.
pub fn rc_is_ready() -> bool {
    RC_READY.load(Ordering::Acquire)
}

/// Print RC status info.
pub fn rc_info() {
    if !RC_READY.load(Ordering::Acquire) {
        crate::kprintln!("[RC] Not initialized");
        return;
    }
    let mode_val = RC_MODE.load(Ordering::Relaxed);
    let mode_name = match mode_val {
        0 => "SBUS",
        1 => "PPM",
        _ => "Simulated",
    };
    let failsafe = RC_FAILSAFE.load(Ordering::Acquire);
    crate::kprintln!("[RC] Mode: {}  Failsafe: {}", mode_name, failsafe);

    let channels = unsafe { RC_CHANNELS };
    crate::kprintln!("[RC] CH1(roll)={} CH2(pitch)={} CH3(thr)={} CH4(yaw)={}",
        channels[0], channels[1], channels[2], channels[3]);
    crate::kprintln!("[RC] CH5(mode)={} CH6(aux)={}",
        channels[4], channels[5]);
}
