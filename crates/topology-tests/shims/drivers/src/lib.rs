//! Host stand-in for `robot_os_drivers`, reduced to the handful of
//! functions/constants `crates/ipc/src/{gpio_cap,i2c_cap,pwm_cap,motor_cap}.rs`
//! reference.
//!
//! **WHY this exists.** The real `robot_os_drivers` is RV64-only (its
//! `robot_os_arch` MMIO/CSR chain), so it cannot be built for the host. But
//! `crates/topology-tests` needs `gpio_cap.rs`/`i2c_cap.rs`/`pwm_cap.rs`/
//! `motor_cap.rs` to compile in order to test the P1 topology→cap_store
//! bridge (`crates/ipc/src/cap_seed.rs`) end to end — the bridge's job is
//! `CapSpec.target` string → `resource` id → minted `Cap<T>`, and the only
//! way to prove that without reimplementing the minters is to compile the
//! real ones. This crate is bookkeeping stubs, not a device model: the
//! bridge tests only exercise the `*_grant_cap` minting path (a
//! `cap_store::grant` call plus, for gpio/pwm, a bounds check against the
//! constants below); the read/write/actuation wrappers in those files are
//! compiled for completeness but never called from here — that behaviour
//! is covered on real hardware / QEMU, not by this suite.

pub mod gpio {
    pub const GPIO_MAX_PINS: usize = 64;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum GpioDir {
        Input,
        Output,
    }

    pub fn gpio_read(_pin: u32) -> i32 {
        0
    }

    pub fn gpio_write(_pin: u32, _val: u32) -> i32 {
        0
    }

    pub fn gpio_set_direction(_pin: u32, _dir: GpioDir) -> i32 {
        0
    }
}

pub mod pwm {
    pub const PWM_MAX_CHANNELS: usize = 8;

    pub fn pwm_enable(_ch: u32) -> i32 {
        0
    }

    pub fn pwm_disable(_ch: u32) -> i32 {
        0
    }

    pub fn pwm_set_period(_ch: u32, _period_ns: u32) -> i32 {
        0
    }

    pub fn pwm_set_duty(_ch: u32, _duty_ns: u32) -> i32 {
        0
    }

    pub fn pwm_set_duty_pct(_ch: u32, _pct: u32) -> i32 {
        0
    }
}

pub mod i2c {
    pub fn i2c_read(_bus: u8, _addr: u8, _reg: u8, buf: &mut [u8]) -> i32 {
        buf.len() as i32
    }

    pub fn i2c_write(_bus: u8, _addr: u8, _data: &[u8]) -> i32 {
        0
    }

    pub fn i2c_detect(_bus: u8, _addr: u8) -> bool {
        false
    }
}

pub mod motor_pid {
    pub fn motor_pid_set_target(_speed_l: i16, _speed_r: i16) {}

    pub fn motor_pid_tick(_ticks_l: i64, _ticks_r: i64, _now: u64) -> (i32, i32) {
        (0, 0)
    }

    pub fn motor_pid_enable(_en: bool) {}

    pub fn motor_pid_enabled() -> bool {
        false
    }

    pub fn motor_pid_set_gains(_kp: i32, _ki: i32, _kd: i32) {}

    pub fn motor_pid_reset() {}
}
