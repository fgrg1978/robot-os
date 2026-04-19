/// CLINT (Core-Local Interruptor) / SYSTIMER driver.
///
/// S-mode platforms: timer via SBI calls + rdtime instruction.
/// ESP32-C3: SYSTIMER peripheral (52-bit free-running counter).
///
/// Ported from kernel/core/irq.c (CLINT parts)

use core::sync::atomic::{AtomicU32, Ordering};

/// Timer (`mtime`) frequency — platform-specific.
pub const TIMER_FREQ: u64 = crate::platform::hw::TIMER_FREQ;

// ── Configurable scheduler rate (Phase E1) ──────────────────────────────────

// Changed from AtomicU64 to AtomicU32 for RV32 compatibility.
// Range 10-10000 fits in u32; API still returns u64.
static SCHED_HZ: AtomicU32 = AtomicU32::new(100);

/// Set the scheduler tick rate (10..=10_000 Hz).  Out-of-range values are ignored.
pub fn sched_hz_set(hz: u64) {
    if hz >= 10 && hz <= 10_000 {
        SCHED_HZ.store(hz as u32, Ordering::Relaxed);
    }
}

/// Get the current scheduler tick rate in Hz.
pub fn sched_hz_get() -> u64 {
    SCHED_HZ.load(Ordering::Relaxed) as u64
}

// ============================================================
// S-mode platforms (QEMU / VF2 / K1): rdtime + SBI set_timer
// ============================================================

#[cfg(not(feature = "esp32c3"))]
mod smode_timer {
    use robot_os_arch::sbi;

    /// Read the current time counter (`rdtime` instruction).
    #[inline(always)]
    pub fn get_time() -> u64 {
        let time: u64;
        unsafe { core::arch::asm!("rdtime {}", out(reg) time) };
        time
    }

    /// Schedule the next timer interrupt via SBI set_timer.
    #[inline(always)]
    pub fn set_timer(_hart: u32, time: u64) {
        sbi::set_timer(time);
    }
}

// ============================================================
// ESP32-C3 SYSTIMER (M-mode, 52-bit counter at 16 MHz)
// ============================================================

#[cfg(feature = "esp32c3")]
mod systimer {
    use crate::platform::hw::SYSTIMER_BASE;

    // SYSTIMER register offsets
    const UNIT0_OP: usize = 0x04;        // Write 1<<30 to latch value
    const UNIT0_VALUE_HI: usize = 0x0C;  // Bits [51:32]
    const UNIT0_VALUE_LO: usize = 0x10;  // Bits [31:0]
    const TARGET0_HI: usize = 0x18;      // Target alarm hi
    const TARGET0_LO: usize = 0x1C;      // Target alarm lo
    const TARGET0_CONF: usize = 0x20;    // bit0 = period_mode, bit30 = timer_unit_sel, bit31 = enable
    const INT_ENA: usize = 0x34;         // bit0 = TARGET0_INT_ENA
    const INT_CLR: usize = 0x3C;         // bit0 = TARGET0_INT_CLR

    #[inline(always)]
    fn read32(off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((SYSTIMER_BASE + off) as *const u32) }
    }

    #[inline(always)]
    fn write32(off: usize, val: u32) {
        unsafe { core::ptr::write_volatile((SYSTIMER_BASE + off) as *mut u32, val) }
    }

    /// Read the 52-bit free-running counter (latch + read).
    pub fn get_time() -> u64 {
        // Latch the counter value
        write32(UNIT0_OP, 1 << 30);
        // Small fence to ensure latch completes
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let lo = read32(UNIT0_VALUE_LO) as u64;
        let hi = (read32(UNIT0_VALUE_HI) as u64) & 0xF_FFFF; // 20 bits
        (hi << 32) | lo
    }

    /// Program TARGET0 alarm for a one-shot interrupt at the given time.
    pub fn set_timer(_hart: u32, time: u64) {
        // Clear pending interrupt
        write32(INT_CLR, 1);

        // Set target value
        write32(TARGET0_HI, ((time >> 32) & 0xF_FFFF) as u32);
        write32(TARGET0_LO, (time & 0xFFFF_FFFF) as u32);

        // Configure: one-shot mode, unit0, enable
        write32(TARGET0_CONF, (1 << 31) | (0 << 30)); // enable, unit0, period_mode=0

        // Enable interrupt
        write32(INT_ENA, 1);
    }
}

// ============================================================
// Public API
// ============================================================

/// Read the current time counter.
#[inline(always)]
pub fn get_time() -> u64 {
    #[cfg(not(feature = "esp32c3"))]
    { smode_timer::get_time() }
    #[cfg(feature = "esp32c3")]
    { systimer::get_time() }
}

/// Schedule the next timer interrupt at an absolute time value.
#[inline(always)]
pub fn set_timer(hart: u32, time: u64) {
    #[cfg(not(feature = "esp32c3"))]
    smode_timer::set_timer(hart, time);
    #[cfg(feature = "esp32c3")]
    systimer::set_timer(hart, time);
}

/// Schedule the next timer interrupt relative to now.
/// `interval_us` is in microseconds.
pub fn set_timer_relative(hart: u32, interval_us: u64) {
    let now = get_time();
    let ticks = interval_us * TIMER_FREQ / 1_000_000;
    set_timer(hart, now + ticks);
}

/// Schedule the next periodic tick at the configured rate (default 100 Hz).
pub fn set_next_tick(hart: u32) {
    let now = get_time();
    let hz = SCHED_HZ.load(Ordering::Relaxed) as u64;
    let next = now + TIMER_FREQ / hz;
    set_timer(hart, next);
}

/// M03: Tickless timer — schedule the timer at the earliest useful deadline.
///
/// `nearest_deadline` is the earliest pending `WaitReason::Timer(t)` deadline
/// from the scheduler (pass `robot_os_sched::nearest_timer_deadline()`).
/// If `None`, falls back to the standard periodic tick.
///
/// Programs the hardware timer at `min(nearest_deadline, next_periodic_tick)`
/// so sleeping tasks wake at exactly the right time while preemption still fires.
pub fn set_next_tick_smart(hart: u32, nearest_deadline: Option<u64>) {
    let now  = get_time();
    let hz   = SCHED_HZ.load(Ordering::Relaxed) as u64;
    let tick = now + TIMER_FREQ / hz;

    let next = match nearest_deadline {
        Some(deadline) => deadline.min(tick),
        None           => tick,
    };
    set_timer(hart, next);
}
