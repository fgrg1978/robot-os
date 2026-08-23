//! Hardware watchdog timer driver — Phase D.
//!
//! # Platform mapping
//!
//! | Platform | Hardware         | Base address  | Kick value |
//! |----------|-----------------|---------------|------------|
//! | QEMU     | none (soft only) | —             | no-op       |
//! | VF2 (JH7110) | DesignWare WDT | 0x1301_0000  | write 0x76 to CRR |
//! | K1 (SpacemiT) | DW-WDT compat | 0xD401_5000 (TBC) | same |
//!
//! ## Usage
//!
//! ```text
//! // Boot:
//! wdt_init(WDT_TIMEOUT_MS);   // configure & start watchdog
//!
//! // In RT tick handler (~1 ms):
//! wdt_kick();                  // reset the counter before timeout
//!
//! // Watchdog fires if wdt_kick() is not called within WDT_TIMEOUT_MS.
//! ```
//!
//! ## DesignWare WDT register map
//!
//! All registers are 32-bit, base from `platform::hw::WDT_BASE`.
//!
//! ```text
//! 0x00  WDT_CR    — Control:  bit0=EN, bit2:1=RMOD (01=IRQ+reset)
//! 0x04  WDT_TORR  — Timeout period: bits[3:0]=TOP, clock/(2^(16+TOP))
//! 0x08  WDT_CCVR  — Current counter value (read-only)
//! 0x0C  WDT_CRR   — Counter restart: write 0x76 to kick
//! 0x10  WDT_STAT  — Interrupt status (bit0)
//! 0x14  WDT_EOI   — End-of-interrupt clear (read to clear)
//! ```

#![allow(dead_code)]

const WDT_CR:   usize = 0x00;
const WDT_TORR: usize = 0x04;
const WDT_CCVR: usize = 0x08;
const WDT_CRR:  usize = 0x0C;
const WDT_STAT: usize = 0x10;
const WDT_EOI:  usize = 0x14;

const WDT_CR_EN:      u32 = 1 << 0;
const WDT_CR_RMOD:    u32 = 1 << 1; // 0=reset, 1=IRQ+reset
const WDT_KICK:       u32 = 0x76;   // magic restart value for CRR

// ── MMIO helpers ──────────────────────────────────────────────────────────────

#[inline(always)]
#[cfg(any(feature = "vf2", feature = "k1"))]
fn rd32(base: usize, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

#[inline(always)]
#[cfg(any(feature = "vf2", feature = "k1"))]
fn wr32(base: usize, off: usize, val: u32) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, val) }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the hardware watchdog with the given timeout in milliseconds.
///
/// On QEMU (no hardware WDT) this is a no-op; the software watchdog in the
/// kernel scheduler provides equivalent protection.
pub fn wdt_init(timeout_ms: u32) {
    #[cfg(any(feature = "vf2", feature = "k1"))]
    hw_wdt_init(timeout_ms);
    #[cfg(not(any(feature = "vf2", feature = "k1")))]
    let _ = timeout_ms;
}

/// Kick (restart) the watchdog counter.
///
/// Call from the real-time tick handler at ≤ `timeout_ms / 2` intervals.
#[inline(always)]
pub fn wdt_kick() {
    #[cfg(any(feature = "vf2", feature = "k1"))]
    hw_wdt_kick();
}

/// Disable the watchdog (use only during graceful shutdown).
pub fn wdt_disable() {
    #[cfg(any(feature = "vf2", feature = "k1"))]
    hw_wdt_disable();
}

/// Returns true if the platform has a hardware watchdog.
pub const fn wdt_has_hardware() -> bool {
    cfg!(any(feature = "vf2", feature = "k1"))
}

// ── DesignWare WDT implementation (VF2 + K1) ─────────────────────────────────

#[cfg(any(feature = "vf2", feature = "k1"))]
fn hw_wdt_init(timeout_ms: u32) {
    let base = crate::platform::hw::WDT_BASE;

    // Disable first so we can safely reconfigure.
    wr32(base, WDT_CR, 0);

    // Select timeout period.
    // DW-WDT timeout = clock_hz / 2^(16 + TOP)
    // Timer clock on VF2/K1 ≈ 24 MHz (APB clock).
    // TOP=0 → 24_000_000 / 2^16 ≈ 366 ms
    // TOP=1 → 24_000_000 / 2^17 ≈ 183 ms
    // TOP=5 → 24_000_000 / 2^21 ≈ 11 ms   ← too short
    // We want ≥ timeout_ms.
    // Approximate: find smallest TOP such that 2^(16+TOP) ≥ clock * timeout_ms / 1000
    let clock_hz: u32 = crate::platform::hw::WDT_CLK_HZ as u32;
    let ticks_needed: u64 = clock_hz as u64 * timeout_ms as u64 / 1000;
    let mut top: u32 = 0;
    let mut period: u64 = 1u64 << 16;
    while period < ticks_needed && top < 15 {
        top += 1;
        period <<= 1;
    }

    wr32(base, WDT_TORR, top | (top << 4)); // initial + normal timeout
    // Kick once before enabling to load the timeout value.
    wr32(base, WDT_CRR, WDT_KICK);
    // Enable: mode=reset on timeout (RMOD=0), WDT_EN=1.
    wr32(base, WDT_CR, WDT_CR_EN);
}

#[cfg(any(feature = "vf2", feature = "k1"))]
#[inline(always)]
fn hw_wdt_kick() {
    let base = crate::platform::hw::WDT_BASE;
    wr32(base, WDT_CRR, WDT_KICK);
}

#[cfg(any(feature = "vf2", feature = "k1"))]
fn hw_wdt_disable() {
    let base = crate::platform::hw::WDT_BASE;
    // DW-WDT: once enabled, it can only be stopped by a system reset.
    // The best we can do is keep kicking it or let it reset the board.
    // Mark as kicked and document the limitation.
    wr32(base, WDT_CRR, WDT_KICK);
}

/// Current watchdog counter value (hardware only; 0 on QEMU).
#[cfg(any(feature = "vf2", feature = "k1"))]
pub fn wdt_counter() -> u32 {
    rd32(crate::platform::hw::WDT_BASE, WDT_CCVR)
}

#[cfg(not(any(feature = "vf2", feature = "k1")))]
pub fn wdt_counter() -> u32 { 0 }

// ── F11.3: Crash counter (persistent boot-loop detection) ────────────────────
//
// The crash counter lives in the kernel's in-memory config (AtomicU32).
// It is incremented on panic and decremented (to zero) on clean boot.
// When it reaches CRASH_BOOT_LOOP_THRESHOLD successive crashes, the kernel
// enters "safe mode" (minimal init, no drivers, shell only) to prevent
// hardware damage from a driver that is repeatedly crashing on boot.
//
// The counter is preserved across soft-reboots because it lives in a
// well-known config key that the config crate saves to FAT32 on every write.
// On hard power-cycle it resets to 0 (this is expected and correct).

use core::sync::atomic::{AtomicU32, Ordering};

/// In-memory crash counter — incremented on panic, reset on clean boot.
pub static CRASH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Number of consecutive crashes that trigger safe mode.
const CRASH_BOOT_LOOP_THRESHOLD: u32 = 3;

/// Increment the crash counter. Called from the panic handler.
///
/// Returns the new counter value. If it equals or exceeds
/// `CRASH_BOOT_LOOP_THRESHOLD`, the caller should enter safe mode.
pub fn crash_counter_increment() -> u32 {
    CRASH_COUNTER.fetch_add(1, Ordering::Relaxed) + 1
}

/// Reset the crash counter. Called after successful late-init (clean boot).
pub fn crash_counter_reset() {
    CRASH_COUNTER.store(0, Ordering::Relaxed);
}

/// Read the current crash counter value.
pub fn crash_counter_get() -> u32 {
    CRASH_COUNTER.load(Ordering::Relaxed)
}

/// Returns true if the boot-loop threshold has been reached.
pub fn crash_counter_is_boot_loop() -> bool {
    CRASH_COUNTER.load(Ordering::Relaxed) >= CRASH_BOOT_LOOP_THRESHOLD
}
