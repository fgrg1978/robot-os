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
//
// NOTE (scope gap, not fixed here — out of scope for this pass, flagged for
// follow-up): this module is gated `not(feature = "vf2")`, so a `k1` build
// falls through to the in-memory simulation below, NOT to real MMIO. Yet
// `pwm_driver.rs` advertises an MMIO range under `any(vf2, k1)` and
// `platform::hw` defines a K1 `PWM_BASE`/`PWM_STRIDE`, implying a real path
// was intended. There is no such path: on BananaPi K1 today, motor/gripper
// PWM writes silently land in the QEMU simulation and never reach hardware.
// Do NOT fix by widening `mmio` below to `any(vf2, k1)` — K1 is
// `spacemit,k1x-pwm`, a different IP block from the JH7110 SiFive PWM
// modelled below; that would silently mis-program K1 hardware.

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

    /// Emergency duty-cycle write for the panic handler — bypasses the
    /// `PWM` spinlock entirely instead of calling `.lock()`.
    ///
    /// Deliberately sacrifices mutual exclusion: same rationale as
    /// `gpio::gpio_write_panic` (see its doc comment). If another hart
    /// holds `PWM` at panic time, `.lock()` would spin forever here and
    /// the panic message would never reach UART — stopping the motor and
    /// printing the crash reason matters more than a torn write to the
    /// simulated PWM state. Conscious trade-off, not an oversight.
    ///
    /// # Safety
    /// May race with a concurrent `pwm_set_duty`/`pwm_set_duty_pct`/
    /// `pwm_set_period` on another hart, producing a torn read-modify-write
    /// of `PwmState`. Only call this from the panic handler.
    pub fn pwm_set_duty_pct_panic(ch: u32, pct: u32) -> i32 {
        if ch as usize >= PWM_MAX_CHANNELS { return -1; }
        let p = unsafe { PWM.get_mut_unchecked() };
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
// JH7110 PWM controller (sifive,pwm-v0 compatible), real register map —
// confirmed against Linux mainline drivers/pwm/pwm-sifive.c, 2026-08.
// 4 real channels (not 8). PWMCFG is ONE shared config register for the
// whole instance (not per-channel); its "scale" prescaler is a bitfield
// within PWMCFG (bits [3:0]), not a separate register. Only PWMCMP is
// genuinely per-channel:
//   +0x00        PWMCFG    — shared: scale bitfield [3:0], sticky (8),
//                             zero-cmp (9), deglitch (10), en-always (12),
//                             en-once (13)
//   +0x08        PWMCOUNT  — shared free-running counter (unused here)
//   +0x10        PWMS      — shared scaled counter, read-only (unused here)
//   +0x20+4*i    PWMCMP(i) — per-channel compare/duty value, i = 0..3
//
// Reference: SiFive PWM v0 / Linux drivers/pwm/pwm-sifive.c.

#[cfg(feature = "vf2")]
mod mmio {
    use super::*;
    use crate::platform::hw::PWM_BASE;

    // Real JH7110 SiFive PWM v0 register map — confirmed against Linux
    // mainline drivers/pwm/pwm-sifive.c (PWM_SIFIVE_PWMCFG/PWMCOUNT/PWMS/
    // PWMCMP + PWM_SIFIVE_PWMCFG_* bitfield macros), 2026-08. Corrects the
    // earlier per-channel-block model (PWM_BASE + ch*PWM_STRIDE) this file
    // used, which does not match real hardware: PWMCFG is ONE shared
    // register for the whole 4-channel instance (not per-channel), and its
    // "scale" is a bitfield within PWMCFG, not a separate PWMSCALE
    // register. Only PWMCMP is genuinely per-channel, and only for
    // indices 0-3 — there are 4 real channels here, not 8.
    const PWMCFG:   usize = 0x00; // shared: enable + scale bitfield
    // PWMCOUNT/PWMS documented for completeness (full real register map)
    // but not read by this driver today — nothing here needs the raw
    // free-running counter or its scaled read-only view.
    #[allow(dead_code)]
    const PWMCOUNT: usize = 0x08; // shared free-running counter
    #[allow(dead_code)]
    const PWMS:     usize = 0x10; // shared scaled counter, read-only

    #[inline(always)]
    fn pwmcmp_offset(ch: u32) -> usize { 0x20 + 4 * ch as usize }

    // PWMCFG bitfield (PWM_SIFIVE_PWMCFG_* in the Linux driver).
    const CFG_SCALE_MASK:  u32 = 0x0F;      // bits [3:0] — prescaler
    const CFG_ZERO_CMP:    u32 = 1 << 9;    // reset counter on compare match
    const CFG_EN_ALWAYS:   u32 = 1 << 12;   // continuous PWM output

    /// Real hardware channel count — only PWMCMP0..3 exist. Deliberately
    /// separate from the module-level `PWM_MAX_CHANNELS` (8), which is
    /// shared with the `sim` module used by QEMU/K1 and out of scope here.
    const PWM_MMIO_CHANNELS: usize = 4;

    // PWM counter max (16-bit compare) — unverified against real hardware
    // counter width, kept as-is from the prior model pending real hardware
    // access; not part of this fix's scope.
    const CMP_MAX: u32 = 0xFFFF;

    // JH7110 PWM8's only clock input is "apb" (JH7110_SYSCLK_PWM_APB in
    // clk-starfive-jh7110-sys.c), gated from APB_BUS = STG_AXIAHB / 8.
    // Unlike WDT_CLK_HZ (crates/drivers/src/platform.rs — gated directly
    // off the 24 MHz crystal, no divider chain), STG_AXIAHB is supplied
    // externally by boot firmware and isn't a statically-defined rate in
    // the Linux clock driver — traced this as far as mainline source
    // allows and hit a genuine dead end pending real hardware/TRM access.
    // Still assumed 24 MHz (unconfirmed) pending that.
    const PWM_CLK_HZ: u64 = 24_000_000;

    #[inline(always)]
    fn reg_read(off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((PWM_BASE + off) as *const u32) }
    }

    #[inline(always)]
    fn reg_write(off: usize, val: u32) {
        unsafe { core::ptr::write_volatile((PWM_BASE + off) as *mut u32, val) }
    }

    pub fn pwm_init() {
        // One shared instance — disable once, not per channel.
        reg_write(PWMCFG, 0);
    }

    pub fn pwm_enable(ch: u32) -> i32 {
        if ch as usize >= PWM_MMIO_CHANNELS { return -1; }
        // Enable is instance-wide (PWMCFG is shared) — read-modify-write so
        // enabling one channel doesn't disturb the scale bits or another
        // already-configured channel's comparator (PWMCMP is independent
        // per channel and untouched by this write).
        let cfg = reg_read(PWMCFG) | CFG_EN_ALWAYS | CFG_ZERO_CMP;
        reg_write(PWMCFG, cfg);
        0
    }

    pub fn pwm_disable(ch: u32) -> i32 {
        if ch as usize >= PWM_MMIO_CHANNELS { return -1; }
        // NOTE: this disables the WHOLE instance (all 4 channels), since
        // enable is instance-wide on real hardware — there is no way to
        // disable just one channel's output while leaving another running.
        // Callers with more than one active channel must account for this;
        // today's only two callers (motor.rs, PWM channels 0 and 1) always
        // disable in a stop-everything context, so this is not a live bug,
        // but it's a real hardware constraint worth flagging.
        let cfg = reg_read(PWMCFG) & !CFG_EN_ALWAYS;
        reg_write(PWMCFG, cfg);
        0
    }

    /// Set the SHARED period (nanoseconds) for the whole PWM instance —
    /// programs PWMCFG's scale bitfield. `ch` is accepted for API
    /// compatibility with the per-channel call sites in motor.rs but is
    /// otherwise unused: all channels on this instance share one period.
    /// Today's real usage (2 motors, channels 0-1, both configured with
    /// the same `MOTOR_PWM_PERIOD_NS`) never actually needs two different
    /// periods, so this is not a behavior change for this codebase — it's
    /// the model finally matching what the hardware always did.
    pub fn pwm_set_period(ch: u32, period_ns: u32) -> i32 {
        if ch as usize >= PWM_MMIO_CHANNELS || period_ns == 0 { return -1; }
        let period_cycles = PWM_CLK_HZ * period_ns as u64 / 1_000_000_000;
        let mut scale = 0u32;
        let mut counts = period_cycles;
        while counts > CMP_MAX as u64 && scale < 15 {
            scale += 1;
            counts >>= 1;
        }
        let cfg = (reg_read(PWMCFG) & !CFG_SCALE_MASK) | (scale & CFG_SCALE_MASK);
        reg_write(PWMCFG, cfg);
        let _ = counts; // no separate per-channel period register to write further
        0
    }

    /// UNIMPLEMENTED — absolute-nanosecond duty needs the shared period
    /// (see `pwm_set_period`) to convert to a percentage or a raw
    /// comparator count; no software-side period cache exists to do that
    /// conversion correctly today. Use `pwm_set_duty_pct` instead — it no
    /// longer aliases with `pwm_set_period` now that PWMCMP is correctly
    /// modeled as per-channel-only (that was the actual root cause of the
    /// previous aliasing bug, now fixed).
    pub fn pwm_set_duty(_ch: u32, _duty_ns: u32) -> i32 {
        -1
    }

    /// Sets duty as a percentage of CMP_MAX. Writes ONLY this channel's
    /// PWMCMP — no longer touches PWMCFG/scale, so it can no longer
    /// corrupt the period (the bug this file previously documented at
    /// length is fixed by this register-model correction).
    pub fn pwm_set_duty_pct(ch: u32, pct: u32) -> i32 {
        if ch as usize >= PWM_MMIO_CHANNELS { return -1; }
        let duty = (CMP_MAX as u64 * pct.min(100) as u64 / 100) as u32;
        reg_write(pwmcmp_offset(ch), duty);
        0
    }

    /// Emergency duty-cycle write for the panic handler — same rationale
    /// as `gpio::gpio_write_panic` (see its doc comment): no software lock
    /// exists on this MMIO path today (same as before this fix), so this
    /// is already non-blocking; kept as a distinct name purely so callers
    /// don't need a `cfg` branch at the call site.
    pub fn pwm_set_duty_pct_panic(ch: u32, pct: u32) -> i32 {
        pwm_set_duty_pct(ch, pct)
    }

    pub fn pwm_get(ch: u32) -> Option<PwmChannel> {
        if ch as usize >= PWM_MMIO_CHANNELS { return None; }
        let cfg = reg_read(PWMCFG);
        Some(PwmChannel {
            enabled:   cfg & CFG_EN_ALWAYS != 0,
            period_ns: 0, // would need to reverse-compute from the scale bitfield
            duty_ns:   0,
        })
    }

    pub fn pwm_info() {
        crate::kprintln!("[PWM] JH7110 SiFive PWM @ {:#010x} ({} channels, shared period)",
            PWM_BASE, PWM_MMIO_CHANNELS);
        let cfg = reg_read(PWMCFG);
        crate::kprintln!("[PWM]   shared CFG={:#010x} (scale={})", cfg, cfg & CFG_SCALE_MASK);
        for ch in 0..PWM_MMIO_CHANNELS as u32 {
            let cmp = reg_read(pwmcmp_offset(ch));
            crate::kprintln!("[PWM]   ch{}: CMP={:#06x}", ch, cmp);
        }
    }
}

#[cfg(feature = "vf2")]
pub use mmio::*;
