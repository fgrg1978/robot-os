//! PWM as a [`Driver`] impl — fourth migration after UART, GPIO,
//! I2C. Validates the trait against a multi-parameter actuator
//! family (channel + nanosecond period/duty).
//!
//! The legacy `crate::pwm` API stays in place for internal callers
//! (motor PID, ESC); this driver provides the unified API for
//! client tasks via `runtime::REGISTRY`.

use crate::api::{Driver, DriverError, DriverIsolation, DriverManifest};
#[cfg(any(feature = "vf2", feature = "k1"))]
use crate::api::MmioRange;
use crate::pwm;
use core::sync::atomic::{AtomicBool, Ordering};
use robot_os_abi::cap::CapPerms;

// ──────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────

/// Mirrors `robot_os_driver_server::DRV_KIND_PWM`.
const DRV_KIND_PWM: u32 = 0x0005;

/// MMIO window on real platforms (SiFive PWM block, 8 channels ×
/// 16-byte stride = 128 bytes, page-aligned for completeness).
#[cfg(any(feature = "vf2", feature = "k1"))]
const PWM_MMIO_BYTES: u64 = 0x100;

/// Common input layout: `[channel u32 LE, payload u32 LE]`.
/// ENABLE / DISABLE ignore the payload; the rest interpret it as
/// the period/duty nanoseconds (`u32`) or duty percent (`u32`).
const PWM_INPUT_BYTES: usize = 8;

/// Stable wire-format ops.
pub const PWM_OP_ENABLE: u32 = 0;
pub const PWM_OP_DISABLE: u32 = 1;
/// `input[4..8]` = period nanoseconds (`u32 LE`).
pub const PWM_OP_SET_PERIOD: u32 = 2;
/// `input[4..8]` = duty nanoseconds (`u32 LE`).
pub const PWM_OP_SET_DUTY: u32 = 3;
/// `input[4..8]` = duty percent (`u32 LE`, 0..=100).
pub const PWM_OP_SET_DUTY_PCT: u32 = 4;

// ──────────────────────────────────────────────────────────────────────────
// Manifest
// ──────────────────────────────────────────────────────────────────────────

const fn build_manifest() -> DriverManifest {
    let m = DriverManifest::new(
        DRV_KIND_PWM,
        "pwm",
        DriverIsolation::InKernel,
        CapPerms::RW,
    );
    #[cfg(any(feature = "vf2", feature = "k1"))]
    let m = m.with_mmio(MmioRange::new(
        crate::platform::hw::PWM_BASE as u64,
        PWM_MMIO_BYTES,
    ));
    m
}

// ──────────────────────────────────────────────────────────────────────────
// Driver state
// ──────────────────────────────────────────────────────────────────────────

pub struct PwmDriver {
    initialized: AtomicBool,
    manifest: DriverManifest,
}

impl PwmDriver {
    pub const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            manifest: build_manifest(),
        }
    }

    /// Decode `[channel u32 LE, payload u32 LE]`.
    fn decode(input: &[u8]) -> Result<(u32, u32), DriverError> {
        if input.len() < PWM_INPUT_BYTES {
            return Err(DriverError::BadInput);
        }
        let ch = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        let pl = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
        Ok((ch, pl))
    }
}

impl Default for PwmDriver {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Driver impl
// ──────────────────────────────────────────────────────────────────────────

impl Driver for PwmDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn init(&self) -> Result<(), DriverError> {
        if self
            .initialized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            pwm::pwm_init();
        }
        Ok(())
    }

    fn handle_request(
        &self,
        op: u32,
        input: &[u8],
        _output: &mut [u8],
    ) -> Result<usize, DriverError> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(DriverError::NotInitialized);
        }
        let (ch, payload) = Self::decode(input)?;
        let rc = match op {
            PWM_OP_ENABLE => pwm::pwm_enable(ch),
            PWM_OP_DISABLE => pwm::pwm_disable(ch),
            PWM_OP_SET_PERIOD => pwm::pwm_set_period(ch, payload),
            PWM_OP_SET_DUTY => pwm::pwm_set_duty(ch, payload),
            PWM_OP_SET_DUTY_PCT => pwm::pwm_set_duty_pct(ch, payload),
            _ => return Err(DriverError::BadOp),
        };
        if rc == 0 {
            Ok(0)
        } else {
            Err(DriverError::IoFault)
        }
    }

    fn shutdown(&self) -> Result<(), DriverError> {
        self.initialized.store(false, Ordering::Release);
        Ok(())
    }
}
