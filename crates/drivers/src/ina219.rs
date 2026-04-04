//! INA219 I2C current/voltage sensor driver — Phase AP.
//!
//! Measures battery voltage and current draw simultaneously.
//! Enables: voltage sag detection, mAh counting, capacity estimation.
//!
//! I2C address: 0x40 (default, A0=A1=GND).
//! Registers: config(0x00), shunt_voltage(0x01), bus_voltage(0x02),
//!            power(0x03), current(0x04), calibration(0x05).

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default I2C address for INA219.
pub const INA219_ADDR: u8 = 0x40;
/// I2C bus index (shared with MPU6050 and ADS1115).
const I2C_BUS: u64 = 1;

// Register addresses
const REG_CONFIG: u8 = 0x00;
#[allow(dead_code)] const REG_SHUNT_VOLTAGE: u8 = 0x01;
const REG_BUS_VOLTAGE: u8 = 0x02;
const REG_CURRENT: u8 = 0x04;
const REG_CALIBRATION: u8 = 0x05;

// Config register: 32V range, ±320mV shunt, 12-bit, continuous
const CONFIG_DEFAULT: u16 = 0x399F;

// Shunt resistor value in milliohms (standard INA219 module uses 100mΩ)
const SHUNT_RESISTOR_MOHM: u32 = 100;

// Voltage sag detection
/// Voltage drop threshold for sag detection (mV).
pub const VOLTAGE_SAG_THRESHOLD_MV: u16 = 500;
/// Time window for sag detection (samples, ~1s at 10Hz).
pub const VOLTAGE_SAG_WINDOW: u8 = 10;

// Capacity estimation
/// Battery nominal capacity (mAh) — 2S3P = 3600mAh.
pub const BATTERY_NOMINAL_MAH: u32 = 3600;

// Failsafe levels (percentage of capacity)
/// Warning level (%).
pub const FAILSAFE_WARNING_PCT: u8 = 25;
/// RTL level (%).
pub const FAILSAFE_RTL_PCT: u8 = 15;
/// Land level (%).
pub const FAILSAFE_LAND_PCT: u8 = 10;
/// Kill level (%).
pub const FAILSAFE_KILL_PCT: u8 = 5;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

static INA_INITIALIZED: AtomicBool = AtomicBool::new(false);
static VOLTAGE_MV: AtomicU16 = AtomicU16::new(0);
static CURRENT_MA: AtomicU16 = AtomicU16::new(0);  // unsigned, stored as u16
static MAH_USED_X10: AtomicU32 = AtomicU32::new(0); // mAh × 10 for precision
static PREV_VOLTAGE_MV: AtomicU16 = AtomicU16::new(0);
static SAG_DETECTED: AtomicBool = AtomicBool::new(false);
static SAMPLE_COUNT: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialize the INA219 sensor.
pub fn ina219_init() {
    // Write calibration register
    // Cal = 0.04096 / (current_lsb × R_shunt)
    // For 100mΩ shunt, current_lsb = 0.1mA → Cal = 4096
    let cal: u16 = 4096;
    let cal_bytes = cal.to_be_bytes();
    let buf = [REG_CALIBRATION, cal_bytes[0], cal_bytes[1]];
    crate::i2c::i2c_write(I2C_BUS as u8, INA219_ADDR, &buf);

    // Write config register
    let cfg_bytes = CONFIG_DEFAULT.to_be_bytes();
    let buf = [REG_CONFIG, cfg_bytes[0], cfg_bytes[1]];
    crate::i2c::i2c_write(I2C_BUS as u8, INA219_ADDR, &buf);

    INA_INITIALIZED.store(true, Ordering::Release);
    crate::kprintln!("[INA219] Initialized (addr=0x{:02X}, shunt={}mΩ)",
        INA219_ADDR, SHUNT_RESISTOR_MOHM);
}

/// Check if INA219 is initialized.
pub fn ina219_is_initialized() -> bool {
    INA_INITIALIZED.load(Ordering::Acquire)
}

/// Read voltage and current, update mAh counter and sag detection.
/// Call this periodically (~10Hz from behavior loop).
pub fn ina219_poll() {
    if !ina219_is_initialized() { return; }

    // Read bus voltage (register 0x02)
    let raw_v = read_register(REG_BUS_VOLTAGE);
    // Bus voltage: bits [15:3] × 4mV, bit 1 = conversion ready
    let voltage_mv = ((raw_v >> 3) * 4) as u16;

    // Read current (register 0x04)
    let raw_i = read_register(REG_CURRENT);
    // Current in 0.1mA units (from calibration)
    let current_ma = (raw_i / 10) as u16;

    // Voltage sag detection
    let prev_v = PREV_VOLTAGE_MV.load(Ordering::Relaxed);
    if prev_v > 0 && voltage_mv + VOLTAGE_SAG_THRESHOLD_MV < prev_v {
        SAG_DETECTED.store(true, Ordering::Release);
    } else {
        SAG_DETECTED.store(false, Ordering::Release);
    }
    PREV_VOLTAGE_MV.store(voltage_mv, Ordering::Relaxed);

    // Update mAh counter (integrate current over time)
    // At 10Hz: mAh_increment = current_ma / 3600 / 10
    // Using ×10 precision: mah_x10 += current_ma * 10 / 36000
    // Simplified: mah_x10 += current_ma / 3600
    if current_ma > 0 {
        let increment = (current_ma as u32).max(1);
        // At 10Hz sample rate: mAh = I(mA) × (1/10s) / 3600
        // mah_x10 += I * 10 / 36000 = I / 3600
        MAH_USED_X10.fetch_add(increment, Ordering::Relaxed);
    }

    VOLTAGE_MV.store(voltage_mv, Ordering::Relaxed);
    CURRENT_MA.store(current_ma, Ordering::Relaxed);
    SAMPLE_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Get current voltage in mV.
pub fn ina219_voltage_mv() -> u16 {
    VOLTAGE_MV.load(Ordering::Relaxed)
}

/// Get current draw in mA.
pub fn ina219_current_ma() -> u16 {
    CURRENT_MA.load(Ordering::Relaxed)
}

/// Get total mAh consumed since init.
pub fn ina219_mah_used() -> u32 {
    MAH_USED_X10.load(Ordering::Relaxed) / 10
}

/// Get estimated remaining capacity (%).
pub fn ina219_capacity_pct() -> u8 {
    let used = ina219_mah_used();
    if used >= BATTERY_NOMINAL_MAH {
        return 0;
    }
    let remaining = BATTERY_NOMINAL_MAH - used;
    ((remaining * 100) / BATTERY_NOMINAL_MAH) as u8
}

/// Check if voltage sag was detected.
pub fn ina219_sag_detected() -> bool {
    SAG_DETECTED.load(Ordering::Acquire)
}

/// Get failsafe level based on capacity.
/// Returns: 0=OK, 1=WARNING, 2=RTL, 3=LAND, 4=KILL
pub fn ina219_failsafe_level() -> u8 {
    let pct = ina219_capacity_pct();
    if pct <= FAILSAFE_KILL_PCT { 4 }
    else if pct <= FAILSAFE_LAND_PCT { 3 }
    else if pct <= FAILSAFE_RTL_PCT { 2 }
    else if pct <= FAILSAFE_WARNING_PCT { 1 }
    else { 0 }
}

/// Read power data into buffer for SYS_SENSOR_READ.
/// Format: voltage_mv(u16) + current_ma(u16) + mah_used(u32) + capacity_pct(u8) + sag(u8) + failsafe(u8) + pad(u8)
/// Total: 12 bytes.
pub const POWER_DATA_SIZE: usize = 12;

pub fn ina219_read_power(buf: &mut [u8]) -> usize {
    if buf.len() < POWER_DATA_SIZE { return 0; }
    let v = ina219_voltage_mv();
    let i = ina219_current_ma();
    let mah = ina219_mah_used();
    let pct = ina219_capacity_pct();
    let sag = if ina219_sag_detected() { 1u8 } else { 0u8 };
    let fs = ina219_failsafe_level();

    buf[0..2].copy_from_slice(&v.to_le_bytes());
    buf[2..4].copy_from_slice(&i.to_le_bytes());
    buf[4..8].copy_from_slice(&mah.to_le_bytes());
    buf[8] = pct;
    buf[9] = sag;
    buf[10] = fs;
    buf[11] = 0; // padding
    POWER_DATA_SIZE
}

// ---------------------------------------------------------------------------
// Internal: I2C register read
// ---------------------------------------------------------------------------

fn read_register(reg: u8) -> u16 {
    let mut buf = [0u8; 2];
    crate::i2c::i2c_read(I2C_BUS as u8, INA219_ADDR, reg, &mut buf);
    u16::from_be_bytes(buf)
}
