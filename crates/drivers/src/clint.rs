/// CLINT (Core-Local Interruptor) / SYSTIMER driver.
///
/// S-mode platforms: timer via SBI calls + rdtime instruction.
///
/// Ported from kernel/core/irq.c (CLINT parts)

use core::sync::atomic::{AtomicU32, Ordering};

/// Timer (`mtime`) frequency — platform-specific.
pub const TIMER_FREQ: u64 = crate::platform::hw::TIMER_FREQ;

// ── Configurable scheduler rate (Phase E1) ──────────────────────────────────

// Stored as AtomicU32: the 10-10000 range fits in u32; API still returns u64.
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
// Public API
// ============================================================

/// Read the current time counter.
#[inline(always)]
pub fn get_time() -> u64 {
    smode_timer::get_time()
}

/// Schedule the next timer interrupt at an absolute time value.
#[inline(always)]
pub fn set_timer(hart: u32, time: u64) {
    smode_timer::set_timer(hart, time);
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
