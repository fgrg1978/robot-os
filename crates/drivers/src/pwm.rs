/// PWM driver — port of kernel/drivers/pwm.c + kernel/include/pwm.h
///
/// QEMU: in-memory simulation.
/// VF2:  JH7110 SiFive-compatible PWM controller — real MMIO.

pub const PWM_MAX_CHANNELS: usize = 8;

#[derive(Clone, Copy)]
pub struct PwmChannel {
    pub enabled:    bool,
    pub period_ns:  u32,
    pub duty_ns:    u32,
}

impl PwmChannel {
    pub const fn new() -> Self {
        PwmChannel { enabled: false, period_ns: 1_000_000, duty_ns: 500_000 }
    }

    pub fn duty_pct(&self) -> u32 {
        if self.period_ns == 0 { return 0; }
        (self.duty_ns as u64 * 100 / self.period_ns as u64) as u32
    }
}

// ── QEMU: in-memory simulation ────────────────────────────────────────────────

#[cfg(not(feature = "vf2"))]
mod sim {
    use super::*;
    use robot_os_sync::SpinLock;

    struct PwmState { channels: [PwmChannel; PWM_MAX_CHANNELS] }
    impl PwmState { const fn new() -> Self { PwmState { channels: [PwmChannel::new(); PWM_MAX_CHANNELS] } } }

    static PWM: SpinLock<PwmState> = SpinLock::new(PwmState::new());

    pub fn pwm_init() {}

    pub fn pwm_enable(ch: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        PWM.lock().channels[ch as usize].enabled = true; 0
    }

    pub fn pwm_disable(ch: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        PWM.lock().channels[ch as usize].enabled = false; 0
    }

    pub fn pwm_set_period(ch: u32, period_ns: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        PWM.lock().channels[ch as usize].period_ns = period_ns; 0
    }

    pub fn pwm_set_duty(ch: u32, duty_ns: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        let mut p = PWM.lock();
        p.channels[ch as usize].duty_ns = duty_ns.min(p.channels[ch as usize].period_ns); 0
    }

    pub fn pwm_set_duty_pct(ch: u32, pct: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        let mut p = PWM.lock();
        let period = p.channels[ch as usize].period_ns;
        p.channels[ch as usize].duty_ns = (period as u64 * pct.min(100) as u64 / 100) as u32; 0
    }

    pub fn pwm_get(ch: u32) -> Option<PwmChannel> {
        if ch as usize >= PWM_MAX_CHANNELS { return None; }
        Some(PWM.lock().channels[ch as usize])
    }

    pub fn pwm_info() {
        crate::kprintln!("[PWM] Simulated PWM — {} channels", PWM_MAX_CHANNELS);
        let p = PWM.lock();
        for i in 0..PWM_MAX_CHANNELS {
            let ch = &p.channels[i];
            if ch.enabled {
                crate::kprintln!("[PWM]   ch{}: enabled, period={}ns, duty={}ns ({}%)",
                    i, ch.period_ns, ch.duty_ns, ch.duty_pct());
            } else {
                crate::kprintln!("[PWM]   ch{}: disabled", i);
            }
        }
    }
}

#[cfg(not(feature = "vf2"))]
pub use sim::*;

// ── VisionFive 2 / JH7110: SiFive PWM MMIO ───────────────────────────────────
//
// JH7110 PWM controller (sifive,pwm-v0 compatible).
// 8 channels; each occupies PWM_STRIDE bytes.
//
// Per-channel registers (base = PWM_BASE + ch * PWM_STRIDE):
//   +0x00  PWMCFG  — config: en (bit 0), center (bit 1), gang (bit 8..15), deglitch (bit 16)
//   +0x04  PWMCMP  — compare value (duty); when counter < CMP → output high
//   +0x08  PWMSCALE — clock prescaler
//
// The counter runs at (SYS_CLK / (1 << scale)) Hz.
// period = (CMP_MAX + 1) cycles; duty fraction = cmp / CMP_MAX.
//
// Reference: SiFive PWM v0 specification.
// NOTE: Exact register offsets must be confirmed on hardware.

#[cfg(feature = "vf2")]
mod mmio {
    use super::*;
    use crate::platform::hw::{PWM_BASE, PWM_STRIDE};

    // SiFive PWM per-channel register offsets
    const PWMCFG:   usize = 0x00;
    const PWMCMP:   usize = 0x04;
    const PWMSCALE: usize = 0x08;

    // PWMCFG bits
    const CFG_EN:   u32 = 1 << 0;   // enable output
    const CFG_ZEROCMP: u32 = 1 << 9; // reset counter on compare match

    // PWM counter max (16-bit compare)
    const CMP_MAX: u32 = 0xFFFF;

    // JH7110 sys_clk for PWM (assumed 24 MHz; verify in DTS `assigned-clock-rates`).
    const PWM_CLK_HZ: u64 = 24_000_000;

    #[inline(always)]
    fn ch_base(ch: u32) -> usize { PWM_BASE + ch as usize * PWM_STRIDE }

    #[inline(always)]
    fn reg_read(ch: u32, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((ch_base(ch) + off) as *const u32) }
    }

    #[inline(always)]
    fn reg_write(ch: u32, off: usize, val: u32) {
        unsafe { core::ptr::write_volatile((ch_base(ch) + off) as *mut u32, val) }
    }

    pub fn pwm_init() {
        // Disable all channels on boot
        for ch in 0..PWM_MAX_CHANNELS as u32 {
            reg_write(ch, PWMCFG, 0);
        }
    }

    pub fn pwm_enable(ch: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        let cfg = reg_read(ch, PWMCFG) | CFG_EN | CFG_ZEROCMP;
        reg_write(ch, PWMCFG, cfg);
        0
    }

    pub fn pwm_disable(ch: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        let cfg = reg_read(ch, PWMCFG) & !CFG_EN;
        reg_write(ch, PWMCFG, cfg);
        0
    }

    /// Set period (nanoseconds) — programs scale + CMP_MAX.
    pub fn pwm_set_period(ch: u32, period_ns: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS || period_ns == 0 { return -1; }
        // Find the smallest prescaler such that counter fits in 16 bits.
        let period_cycles = PWM_CLK_HZ * period_ns as u64 / 1_000_000_000;
        let mut scale = 0u32;
        let mut counts = period_cycles;
        while counts > CMP_MAX as u64 && scale < 15 {
            scale += 1;
            counts >>= 1;
        }
        reg_write(ch, PWMSCALE, scale);
        reg_write(ch, PWMCMP, counts.min(CMP_MAX as u64) as u32);
        0
    }

    pub fn pwm_set_duty(ch: u32, duty_ns: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        let period_reg = reg_read(ch, PWMCMP); // current period == CMP_MAX
        if period_reg == 0 { return -1; }
        // Approximate: duty_ns / period_ns * CMP_MAX
        // We don't cache period_ns, so use a fixed ratio against CMP_MAX.
        let _ = duty_ns; // duty programming requires knowing the period — set via duty_pct
        -1
    }

    pub fn pwm_set_duty_pct(ch: u32, pct: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        let cmp = reg_read(ch, PWMCMP);
        let duty = (cmp as u64 * pct.min(100) as u64 / 100) as u32;
        // SiFive PWM: output high while counter < duty_cmp, which requires
        // a second "duty" compare register. Simplified: rewrite CMP to duty value.
        // Full implementation needs ganged mode or dual comparators.
        reg_write(ch, PWMCMP, duty);
        0
    }

    pub fn pwm_get(ch: u32) -> Option<PwmChannel> {
        if ch as usize >= PWM_MAX_CHANNELS { return None; }
        let cfg = reg_read(ch, PWMCFG);
        Some(PwmChannel {
            enabled:   cfg & CFG_EN != 0,
            period_ns: 0,   // would need to reverse-compute from scale+CMP
            duty_ns:   0,
        })
    }

    pub fn pwm_info() {
        crate::kprintln!("[PWM] JH7110 SiFive PWM @ {:#010x} ({} channels)", PWM_BASE, PWM_MAX_CHANNELS);
        for ch in 0..PWM_MAX_CHANNELS as u32 {
            let cfg = reg_read(ch, PWMCFG);
            let cmp = reg_read(ch, PWMCMP);
            let scale = reg_read(ch, PWMSCALE);
            crate::kprintln!("[PWM]   ch{}: CFG={:#010x} CMP={:#06x} SCALE={}", ch, cfg, cmp, scale);
        }
    }
}

#[cfg(feature = "vf2")]
pub use mmio::*;
