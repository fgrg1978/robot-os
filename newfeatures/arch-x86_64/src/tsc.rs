//! Time-Stamp Counter (TSC) reader + ns/µs helpers.
//!
//! x86 mirror of `arch-aarch64::timer`. The TSC is a 64-bit
//! free-running counter incremented at a fixed rate on modern
//! CPUs (Invariant TSC, advertised via CPUID 8000_0007 EDX bit
//! 8). The frequency is *not* discoverable from the CPU alone —
//! the kernel must calibrate it against PIT, HPET, or the ACPI
//! PM timer, then call [`set_tsc_hz`].
//!
//! Until calibrated, [`now_ns`] returns raw TSC ticks; useful
//! for relative timing but not wall-clock-meaningful.

#![allow(dead_code)]

const NS_PER_S: u64 = 1_000_000_000;
const NS_PER_US: u64 = 1_000;

/// Calibrated TSC frequency in Hz. 0 ⇒ uncalibrated, `now_ns`
/// returns raw TSC values. Single-writer (BSP at boot); after
/// that all CPUs read.
static TSC_HZ: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Read the time-stamp counter. Not serialising: surrounding
/// loads + stores may execute before or after RDTSC. Use
/// [`rdtscp`] (or pair with `lfence`) when ordering matters.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Serialising TSC read: waits for all prior instructions to
/// retire before reading. Costs more cycles but gives ordered
/// timing measurements.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn rdtscp() -> u64 {
    let (lo, hi, _aux): (u32, u32, u32);
    unsafe {
        core::arch::asm!(
            "rdtscp",
            out("eax") lo,
            out("edx") hi,
            out("ecx") _aux,
            options(nostack, nomem, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Tell the module how fast the TSC ticks. The kernel runs this
/// after its calibration loop (typically against the PIT for a
/// rough first estimate, then refined against the ACPI PM timer
/// once that's mapped).
pub fn set_tsc_hz(hz: u64) {
    TSC_HZ.store(hz, core::sync::atomic::Ordering::Release);
}

/// Read the calibrated TSC frequency. 0 means uncalibrated.
pub fn tsc_hz() -> u64 {
    TSC_HZ.load(core::sync::atomic::Ordering::Acquire)
}

/// Current TSC value in nanoseconds. If [`set_tsc_hz`] hasn't
/// been called, returns raw TSC ticks (relative-only).
#[cfg(target_arch = "x86_64")]
pub fn now_ns() -> u64 {
    let hz = tsc_hz();
    if hz == 0 {
        return rdtsc();
    }
    let t = rdtsc();
    let secs = t / hz;
    let rem  = t % hz;
    secs * NS_PER_S + (rem * NS_PER_S) / hz
}

/// Convenience for the common "what time is it in µs" caller.
#[cfg(target_arch = "x86_64")]
pub fn now_us() -> u64 {
    now_ns() / NS_PER_US
}

/// True if CPUID reports the TSC frequency is invariant (won't
/// drift with CPU frequency or C-state transitions). All Intel
/// Nehalem+ and AMD Bulldozer+ are invariant; if this returns
/// `false` the kernel should fall back to HPET for wall time.
#[cfg(target_arch = "x86_64")]
pub fn is_invariant() -> bool {
    let cpuid = unsafe { core::arch::x86_64::__cpuid(0x8000_0007) };
    (cpuid.edx >> 8) & 1 != 0
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub fn rdtsc() -> u64 { 0 }
#[cfg(not(target_arch = "x86_64"))]
pub fn rdtscp() -> u64 { 0 }
#[cfg(not(target_arch = "x86_64"))]
pub fn now_ns() -> u64 { 0 }
#[cfg(not(target_arch = "x86_64"))]
pub fn now_us() -> u64 { 0 }
#[cfg(not(target_arch = "x86_64"))]
pub fn is_invariant() -> bool { false }
