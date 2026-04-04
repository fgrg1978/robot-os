//! Sensor bus — shared sensor state between producer tasks and behavior consumer.
//!
//! Dedicated sensor tasks write to the bus at their own rate.
//! The behavior task reads the latest snapshot atomically.
//!
//! This replaces the monolithic "read all sensors in behavior_task" pattern
//! with priority-separated sensor tasks that use IO-wait.
//!
//! Architecture:
//!   imu_task (RT, 100Hz)     → sensor_bus.update_imu(accel, gyro)
//!   odom_task (normal, 50Hz) → sensor_bus.update_odom(dist, heading, enc_l, enc_r)
//!   range_task (normal, 20Hz)→ sensor_bus.update_range(front, right)
//!   battery_task (low, 1Hz)  → sensor_bus.update_battery(mv)
//!   gpio_task (normal, 20Hz) → sensor_bus.update_flags(flags)
//!
//!   behavior_task (normal)   → state = sensor_bus.snapshot()

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU16, AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Shared sensor bus (lock-free, atomic fields)
// ---------------------------------------------------------------------------

/// Atomic sensor bus — producers write individual fields, consumer reads snapshot.
/// Uses relaxed ordering for sensor data (eventual consistency is fine for robotics).
pub struct SensorBus {
    // IMU
    pub accel_x_mg: AtomicI32,
    pub accel_y_mg: AtomicI32,
    pub accel_z_mg: AtomicI32,
    pub gyro_x_mdps: AtomicI32,
    pub gyro_y_mdps: AtomicI32,
    pub gyro_z_mdps: AtomicI32,
    pub imu_valid: AtomicBool,

    // Odometry
    pub odom_dist_mm: AtomicI64,
    pub odom_heading_cdeg: AtomicI64,
    pub enc_left: AtomicI64,
    pub enc_right: AtomicI64,

    // Rangefinder
    pub range_front: AtomicU16,
    pub range_right: AtomicU16,

    // Battery
    pub battery_mv: AtomicU16,

    // Temperature (centidegrees Celsius from IMU)
    pub temp_cdeg: AtomicI32,

    // GPIO sensor flags (PIR/sound/IR)
    pub sensor_flags: AtomicU16,

    // Timestamp of last update
    pub timestamp: AtomicU64,
}

impl SensorBus {
    pub const fn new() -> Self {
        Self {
            accel_x_mg: AtomicI32::new(0),
            accel_y_mg: AtomicI32::new(0),
            accel_z_mg: AtomicI32::new(1000), // 1g upright
            gyro_x_mdps: AtomicI32::new(0),
            gyro_y_mdps: AtomicI32::new(0),
            gyro_z_mdps: AtomicI32::new(0),
            imu_valid: AtomicBool::new(false),
            odom_dist_mm: AtomicI64::new(0),
            odom_heading_cdeg: AtomicI64::new(0),
            enc_left: AtomicI64::new(0),
            enc_right: AtomicI64::new(0),
            range_front: AtomicU16::new(0),
            range_right: AtomicU16::new(0),
            battery_mv: AtomicU16::new(0),
            temp_cdeg: AtomicI32::new(0),
            sensor_flags: AtomicU16::new(0),
            timestamp: AtomicU64::new(0),
        }
    }

    // ── Producer methods (called by sensor tasks) ────────────────────────

    pub fn update_temp(&self, cdeg: i32) {
        self.temp_cdeg.store(cdeg, Ordering::Relaxed);
    }

    pub fn update_imu(&self, accel: [i32; 3], gyro: [i32; 3]) {
        self.accel_x_mg.store(accel[0], Ordering::Relaxed);
        self.accel_y_mg.store(accel[1], Ordering::Relaxed);
        self.accel_z_mg.store(accel[2], Ordering::Relaxed);
        self.gyro_x_mdps.store(gyro[0], Ordering::Relaxed);
        self.gyro_y_mdps.store(gyro[1], Ordering::Relaxed);
        self.gyro_z_mdps.store(gyro[2], Ordering::Relaxed);
        self.imu_valid.store(true, Ordering::Release);
    }

    pub fn update_odom(&self, dist_mm: i64, heading_cdeg: i64, enc_l: i64, enc_r: i64) {
        self.odom_dist_mm.store(dist_mm, Ordering::Relaxed);
        self.odom_heading_cdeg.store(heading_cdeg, Ordering::Relaxed);
        self.enc_left.store(enc_l, Ordering::Relaxed);
        self.enc_right.store(enc_r, Ordering::Relaxed);
    }

    pub fn update_range(&self, front: u16, right: u16) {
        self.range_front.store(front, Ordering::Relaxed);
        self.range_right.store(right, Ordering::Relaxed);
    }

    pub fn update_battery(&self, mv: u16) {
        self.battery_mv.store(mv, Ordering::Relaxed);
    }

    pub fn update_flags(&self, flags: u16) {
        self.sensor_flags.store(flags, Ordering::Relaxed);
    }

    pub fn update_timestamp(&self, ts: u64) {
        self.timestamp.store(ts, Ordering::Release);
    }

    // ── Consumer method (called by behavior_task) ────────────────────────

    /// Take an atomic snapshot of all sensor data into a SensorState.
    pub fn snapshot(&self, state: &mut crate::types::SensorState) {
        state.accel_mg[0] = self.accel_x_mg.load(Ordering::Relaxed);
        state.accel_mg[1] = self.accel_y_mg.load(Ordering::Relaxed);
        state.accel_mg[2] = self.accel_z_mg.load(Ordering::Relaxed);
        state.gyro_mdps[0] = self.gyro_x_mdps.load(Ordering::Relaxed);
        state.gyro_mdps[1] = self.gyro_y_mdps.load(Ordering::Relaxed);
        state.gyro_mdps[2] = self.gyro_z_mdps.load(Ordering::Relaxed);
        state.imu_valid = self.imu_valid.load(Ordering::Acquire);

        state.odom_dist_mm = self.odom_dist_mm.load(Ordering::Relaxed);
        state.odom_heading_cdeg = self.odom_heading_cdeg.load(Ordering::Relaxed);
        state.enc_left = self.enc_left.load(Ordering::Relaxed);
        state.enc_right = self.enc_right.load(Ordering::Relaxed);

        state.cam_dist_front = self.range_front.load(Ordering::Relaxed);
        state.cam_dist_right = self.range_right.load(Ordering::Relaxed);

        state.battery_mv = self.battery_mv.load(Ordering::Relaxed);
        state.temp_cdeg = self.temp_cdeg.load(Ordering::Relaxed);
        state.sensor_flags = self.sensor_flags.load(Ordering::Relaxed);
        state.timestamp = self.timestamp.load(Ordering::Acquire);
    }
}

/// Global sensor bus instance.
pub static SENSOR_BUS: SensorBus = SensorBus::new();
