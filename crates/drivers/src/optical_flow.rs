//! Optical Flow Sensor Driver (F26).
//!
//! Supports two widely-used optical flow sensors via SPI:
//! - **PMW3901** (Pixart) — 35×35 pixel correlation, ±1000 frames/s, 80 mm min range.
//! - **PAA5100JE** (PixArt) — enhanced version with improved low-light performance.
//!
//! Both sensors share the same SPI register interface at different register addresses.
//! The driver auto-detects the sensor by reading the PRODUCT_ID register.
//!
//! ## Integration
//! Optical flow data feeds the navigation stack for:
//! - Drone horizontal velocity estimation (fuses with IMU via EKF).
//! - Wheeled robot slip detection (compare encoder vs. optical flow).
//! - Indoor positioning without GPS.
//!
//! ## Communication
//! SPI mode 3 (CPOL=1, CPHA=1), up to 2 MHz.
//! CS active-low.  Each register access is a 1-byte address + 1-byte data transfer.
//!
//! ## Coordinate frame
//! ```text
//!   +X → robot forward
//!   +Y → robot left
//! ```
//! Both `delta_x` and `delta_y` are in *raw counts*; scale by
//! `FLOW_SCALE_MM_PER_COUNT / height_mm` to get mm/frame.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use crate::spi;

// ── SPI register map (PMW3901 / PAA5100JE) ───────────────────────────────────

/// Product ID register — read to identify sensor.
const REG_PRODUCT_ID:       u8 = 0x00;
/// Revision ID register.
const REG_REVISION_ID:      u8 = 0x01;
/// Motion status register — bit 7 set when new data is available.
const REG_MOTION:           u8 = 0x02;
/// X-axis raw motion delta (8-bit 2's complement, low byte).
const REG_DELTA_X_L:        u8 = 0x03;
/// X-axis raw motion delta (high byte).
const REG_DELTA_X_H:        u8 = 0x04;
/// Y-axis raw motion delta (low byte).
const REG_DELTA_Y_L:        u8 = 0x05;
/// Y-axis raw motion delta (high byte).
const REG_DELTA_Y_H:        u8 = 0x06;
/// Quality of correlation (0-127).
const REG_SQUAL:            u8 = 0x07;
/// Raw data sum — useful for surface quality check.
const REG_RAW_DATA_SUM:     u8 = 0x0B;
/// Maximum raw data value in the frame.
const REG_MAX_RAW_DATA:     u8 = 0x0C;
/// Minimum raw data value in the frame.
const REG_MIN_RAW_DATA:     u8 = 0x0D;
/// Shutter upper byte.
const REG_SHUTTER_UPPER:    u8 = 0x0E;
/// Shutter lower byte.
const REG_SHUTTER_LOWER:    u8 = 0x0F;
/// Power-up reset register — write RESET_MAGIC to trigger reset.
const REG_POWER_UP_RESET:   u8 = 0x3A;
/// Soft reset — write ORIENT_MAGIC to apply orientation.
const REG_ORIENTATION:      u8 = 0x5B;

/// Write flag: OR this into the address byte for write operations.
const SPI_WRITE_FLAG:       u8 = 0x80;

// ── Product IDs ───────────────────────────────────────────────────────────────

/// PMW3901 product ID value in REG_PRODUCT_ID.
pub const PMW3901_PRODUCT_ID:  u8 = 0x49;
/// PAA5100JE product ID value in REG_PRODUCT_ID.
pub const PAA5100JE_PRODUCT_ID:u8 = 0x51;

// ── Control constants ────────────────────────────────────────────────────────

/// Magic value for power-up reset.
const RESET_MAGIC:           u8 = 0x5A;
/// Orientation register value for default mounting (X forward, Y left).
const ORIENT_DEFAULT:        u8 = 0x00;
/// Orientation register value for 90° CW rotation.
const ORIENT_90_CW:          u8 = 0x01;
/// Orientation register value for 180° rotation.
const ORIENT_180:            u8 = 0x02;

/// Motion bit in REG_MOTION register.
const MOTION_FLAG:           u8 = 1 << 7;

/// Minimum surface quality (SQUAL) for a valid reading.
/// Below this threshold the measurement is discarded.
pub const MIN_SQUAL:          u8 = 10;

/// Scale factor: raw counts × FLOW_SCALE_NM_PER_COUNT / height_mm = nm/frame motion.
/// Derived from PMW3901 datasheet: 1 count ≈ 1/100 of the sensor's angular resolution.
/// At 100 mm height: ~0.98 mm/count → 980_000 nm/count.
pub const FLOW_SCALE_NM_PER_COUNT: i32 = 980_000;

// ── Sensor variant ────────────────────────────────────────────────────────────

/// Detected sensor variant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpticalFlowSensor {
    /// PMW3901 (product ID 0x49).
    Pmw3901,
    /// PAA5100JE (product ID 0x51).
    Paa5100Je,
    /// Unknown or not detected.
    Unknown,
}

// ── Driver state ──────────────────────────────────────────────────────────────

/// Detected sensor variant.
static SENSOR_TYPE:     AtomicU32  = AtomicU32::new(2); // 2 = Unknown
/// Latest X motion delta (raw counts, signed).
static FLOW_DELTA_X:    AtomicI32  = AtomicI32::new(0);
/// Latest Y motion delta (raw counts, signed).
static FLOW_DELTA_Y:    AtomicI32  = AtomicI32::new(0);
/// Latest surface quality score (0-127).
static FLOW_SQUAL:      AtomicU32  = AtomicU32::new(0);
/// Total valid samples since init.
static FLOW_SAMPLES:    AtomicU32  = AtomicU32::new(0);
/// Total dropped samples (SQUAL below threshold).
static FLOW_DROPPED:    AtomicU32  = AtomicU32::new(0);
/// Driver initialized flag.
static FLOW_READY:      AtomicBool = AtomicBool::new(false);

// ── SPI configuration ─────────────────────────────────────────────────────────

/// SPI bus index for the optical flow sensor.
/// Bus 0 = primary SPI on all supported platforms.
const FLOW_SPI_BUS: u8 = 0;
/// Chip-select index for the optical flow sensor.
/// CS 1 = second device on the bus (CS 0 reserved for IMU on some boards).
const FLOW_SPI_CS:  u8 = 1;

// ── SPI helpers ───────────────────────────────────────────────────────────────

/// Read one register from the sensor.
fn reg_read(addr: u8) -> u8 {
    let tx = [addr & !SPI_WRITE_FLAG, 0x00]; // clear write flag
    let mut rx = [0u8; 2];
    spi::spi_transfer(FLOW_SPI_BUS, FLOW_SPI_CS, &tx, &mut rx);
    rx[1]
}

/// Write one register to the sensor.
fn reg_write(addr: u8, val: u8) {
    let tx = [addr | SPI_WRITE_FLAG, val];
    let mut rx = [0u8; 2];
    spi::spi_transfer(FLOW_SPI_BUS, FLOW_SPI_CS, &tx, &mut rx);
}

/// Read a burst of 12 bytes starting at REG_MOTION.
/// The sensor auto-increments the address in burst mode.
fn burst_read() -> [u8; 12] {
    let mut tx = [0u8; 13];
    let mut rx = [0u8; 13];
    tx[0] = REG_MOTION & !SPI_WRITE_FLAG; // burst read starts at REG_MOTION
    spi::spi_transfer(FLOW_SPI_BUS, FLOW_SPI_CS, &tx, &mut rx);
    let mut out = [0u8; 12];
    out.copy_from_slice(&rx[1..13]);
    out
}

/// Spin-wait for approximately `us` microseconds (busy-loop calibrated for ~100 MHz).
/// Not a precise delay — good enough for SPI reset sequencing.
fn delay_us(us: u32) {
    // ~100 cycles per microsecond on a 100 MHz RISC-V core.
    let iters = us * 100;
    for _ in 0..iters {
        core::hint::spin_loop();
    }
}

// ── Initialization ────────────────────────────────────────────────────────────

/// Initialize the optical flow sensor.
///
/// Performs a power-up reset, reads the product ID to detect the sensor variant,
/// and configures default orientation.
///
/// Returns `true` on successful initialization (known product ID detected).
pub fn optical_flow_init() -> bool {
    // Power-up reset sequence (PMW3901 datasheet §5.1).
    reg_write(REG_POWER_UP_RESET, RESET_MAGIC);
    delay_us(50_000); // 50 ms minimum post-reset delay

    // Consume 5 dummy motion reads to clear internal state after reset.
    for _ in 0..5 {
        let _ = reg_read(REG_MOTION);
        delay_us(100);
    }

    // Identify sensor.
    let prod_id = reg_read(REG_PRODUCT_ID);
    let rev_id  = reg_read(REG_REVISION_ID);
    let _ = rev_id; // used in debug builds only

    let sensor = match prod_id {
        PMW3901_PRODUCT_ID   => OpticalFlowSensor::Pmw3901,
        PAA5100JE_PRODUCT_ID => OpticalFlowSensor::Paa5100Je,
        _                    => OpticalFlowSensor::Unknown,
    };

    if sensor == OpticalFlowSensor::Unknown {
        return false;
    }

    SENSOR_TYPE.store(sensor as u32, Ordering::Release);

    // Set default orientation (X forward, Y left, no rotation).
    reg_write(REG_ORIENTATION, ORIENT_DEFAULT);

    FLOW_READY.store(true, Ordering::Release);
    true
}

// ── Polling ───────────────────────────────────────────────────────────────────

/// Poll the sensor for new motion data.
///
/// Should be called at the sensor's frame rate (100-200 Hz for PMW3901).
/// Updates the global motion state if SQUAL ≥ `MIN_SQUAL`.
///
/// Returns `Some((delta_x, delta_y, squal))` on valid data, `None` if no new
/// data or quality too low.
pub fn optical_flow_poll() -> Option<(i16, i16, u8)> {
    if !FLOW_READY.load(Ordering::Acquire) { return None; }

    // Burst-read motion registers (12 bytes: motion + dx_l/h + dy_l/h + squal + ...).
    let b = burst_read();

    // b[0] = REG_MOTION — check motion flag.
    if b[0] & MOTION_FLAG == 0 { return None; }

    // b[1..2] = DELTA_X_L, b[2] = DELTA_X_H (16-bit signed).
    let dx = i16::from_le_bytes([b[1], b[2]]);
    // b[3..4] = DELTA_Y_L, b[4] = DELTA_Y_H.
    let dy = i16::from_le_bytes([b[3], b[4]]);
    // b[5] = SQUAL.
    let squal = b[5];

    if squal < MIN_SQUAL {
        FLOW_DROPPED.fetch_add(1, Ordering::Relaxed);
        return None;
    }

    FLOW_DELTA_X.store(dx as i32, Ordering::Relaxed);
    FLOW_DELTA_Y.store(dy as i32, Ordering::Relaxed);
    FLOW_SQUAL.store(squal as u32, Ordering::Relaxed);
    FLOW_SAMPLES.fetch_add(1, Ordering::Relaxed);

    Some((dx, dy, squal))
}

// ── Accessors ─────────────────────────────────────────────────────────────────

/// Latest raw X-axis motion delta (counts).
#[inline]
pub fn flow_delta_x() -> i32 { FLOW_DELTA_X.load(Ordering::Relaxed) }

/// Latest raw Y-axis motion delta (counts).
#[inline]
pub fn flow_delta_y() -> i32 { FLOW_DELTA_Y.load(Ordering::Relaxed) }

/// Latest surface quality (0-127).  Higher = more reliable.
#[inline]
pub fn flow_squal() -> u8 { FLOW_SQUAL.load(Ordering::Relaxed) as u8 }

/// Convert raw delta counts to velocity in nm/s given height in mm and
/// sensor update rate in Hz.
///
/// `velocity_nm_s = delta_counts × FLOW_SCALE_NM_PER_COUNT / height_mm × rate_hz`
#[inline]
pub fn flow_to_velocity_nm_s(counts: i32, height_mm: u32, rate_hz: u32) -> i32 {
    if height_mm == 0 || rate_hz == 0 { return 0; }
    counts * FLOW_SCALE_NM_PER_COUNT / height_mm as i32 * rate_hz as i32
}

/// Detected sensor variant.
pub fn flow_sensor_type() -> OpticalFlowSensor {
    match SENSOR_TYPE.load(Ordering::Relaxed) {
        0 => OpticalFlowSensor::Pmw3901,
        1 => OpticalFlowSensor::Paa5100Je,
        _ => OpticalFlowSensor::Unknown,
    }
}

/// `(samples_valid, samples_dropped)` since init.
pub fn flow_stats() -> (u32, u32) {
    (
        FLOW_SAMPLES.load(Ordering::Relaxed),
        FLOW_DROPPED.load(Ordering::Relaxed),
    )
}

/// `true` if the driver has been initialized and a known sensor was found.
#[inline]
pub fn flow_is_ready() -> bool { FLOW_READY.load(Ordering::Acquire) }

/// Set sensor rotation (call after `optical_flow_init()` if sensor is mounted
/// at an angle different from the default forward-facing orientation).
pub fn flow_set_orientation(degrees_cw: u16) {
    let reg_val = match degrees_cw % 360 {
        0          => ORIENT_DEFAULT,
        45..=134   => ORIENT_90_CW,
        135..=224  => ORIENT_180,
        225..=314  => 0x03, // 270° CW = 90° CCW
        _          => ORIENT_DEFAULT,
    };
    reg_write(REG_ORIENTATION, reg_val);
}
