//! WCET (Worst-Case Execution Time) instrumentation — F16.
//!
//! Provides lightweight cycle-accurate measurement of function execution time
//! using the RISC-V `rdcycle` CSR.  Tracks min, max, and average over N
//! samples per named measurement point.
//!
//! # Usage
//!
//! ```ignore
//! let t = wcet_begin();
//! // ... critical section ...
//! wcet_end(WCET_PID_LOOP, t);
//!
//! // Periodically:
//! wcet_report();   // prints table to UART
//! wcet_check_bounds();  // logs violations
//! ```
//!
//! # Design
//!
//! - All measurement points are identified by a `u8` index (named constant).
//! - Statistics are stored in a fixed-size array (no allocation).
//! - `rdcycle` is read via inline assembly — sub-microsecond resolution.
//! - On platforms without `rdcycle` (ESP32-C3 / M-mode restrictions), falls
//!   back to CLINT mtime counter with ~100ns resolution.

// ── Named measurement point IDs ──────────────────────────────────────────────

/// Maximum number of WCET measurement points.
pub const WCET_MAX_POINTS: usize = 16;

/// Measurement point: PID flight controller loop.
pub const WCET_PID_LOOP:        u8 = 0;
/// Measurement point: IMU/sensor read.
pub const WCET_SENSOR_READ:     u8 = 1;
/// Measurement point: context switch (schedule() entry → exit).
pub const WCET_CTX_SWITCH:      u8 = 2;
/// Measurement point: timer ISR handler.
pub const WCET_TIMER_ISR:       u8 = 3;
/// Measurement point: motor/ESC actuator write.
pub const WCET_ACTUATOR_WRITE:  u8 = 4;
/// Measurement point: TCP send path.
pub const WCET_NET_SEND:        u8 = 5;
/// Measurement point: CNN inference (one frame).
pub const WCET_CNN_INFER:       u8 = 6;
/// Measurement point: LiDAR scan processing.
pub const WCET_LIDAR_SCAN:      u8 = 7;
/// Measurement point: A* path planning step.
pub const WCET_PATH_PLAN:       u8 = 8;

// ── WCET bound constants (microseconds) ─────────────────────────────────────

/// Bound: PID loop must complete within 50 µs (20 kHz max rate).
pub const WCET_BOUND_PID_US:        u64 = 50;
/// Bound: sensor read within 100 µs.
pub const WCET_BOUND_SENSOR_US:     u64 = 100;
/// Bound: context switch within 5 µs.
pub const WCET_BOUND_CTX_SWITCH_US: u64 = 5;
/// Bound: timer ISR within 10 µs (hardware). QEMU emulation is ~100x slower.
#[cfg(not(feature = "qemu"))]
pub const WCET_BOUND_TIMER_ISR_US:  u64 = 10;
#[cfg(feature = "qemu")]
pub const WCET_BOUND_TIMER_ISR_US:  u64 = 50_000;
/// Bound: actuator write within 10 µs.
pub const WCET_BOUND_ACTUATOR_US:   u64 = 10;

/// Names for each measurement point (indexed by point ID).
const WCET_NAMES: [&str; WCET_MAX_POINTS] = [
    "pid_loop",      // 0
    "sensor_read",   // 1
    "ctx_switch",    // 2
    "timer_isr",     // 3
    "actuator_write",// 4
    "net_send",      // 5
    "cnn_infer",     // 6
    "lidar_scan",    // 7
    "path_plan",     // 8
    "unused_9",      // 9
    "unused_10",     // 10
    "unused_11",     // 11
    "unused_12",     // 12
    "unused_13",     // 13
    "unused_14",     // 14
    "unused_15",     // 15
];

/// WCET bounds in microseconds (0 = no bound enforced).
const WCET_BOUNDS_US: [u64; WCET_MAX_POINTS] = [
    WCET_BOUND_PID_US,        // 0 pid_loop
    WCET_BOUND_SENSOR_US,     // 1 sensor_read
    WCET_BOUND_CTX_SWITCH_US, // 2 ctx_switch
    WCET_BOUND_TIMER_ISR_US,  // 3 timer_isr
    WCET_BOUND_ACTUATOR_US,   // 4 actuator_write
    0,                         // 5 net_send
    0,                         // 6 cnn_infer
    0,                         // 7 lidar_scan
    0,                         // 8 path_plan
    0, 0, 0, 0, 0, 0, 0,      // 9-15 unused
];

// ── Statistics ────────────────────────────────────────────────────────────────

/// Per-point WCET statistics.
#[derive(Clone, Copy)]
pub struct WcetStats {
    /// Minimum observed cycle count.
    pub min_cycles:    u64,
    /// Maximum observed cycle count (WCET).
    pub max_cycles:    u64,
    /// Cumulative sum for average computation.
    pub total_cycles:  u64,
    /// Number of samples recorded.
    pub count:         u64,
    /// Number of bound violations.
    pub violations:    u32,
}

impl WcetStats {
    const fn new() -> Self {
        WcetStats {
            min_cycles:   u64::MAX,
            max_cycles:   0,
            total_cycles: 0,
            count:        0,
            violations:   0,
        }
    }

    /// Average cycles (0 if no samples).
    pub fn avg_cycles(&self) -> u64 {
        if self.count == 0 { 0 } else { self.total_cycles / self.count }
    }
}

// ── Global state (lock-free via separate u64 atomics) ─────────────────────────

use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};

struct WcetTable {
    min:        [AtomicU64; WCET_MAX_POINTS],
    max:        [AtomicU64; WCET_MAX_POINTS],
    total:      [AtomicU64; WCET_MAX_POINTS],
    count:      [AtomicU64; WCET_MAX_POINTS],
    violations: [AtomicU32; WCET_MAX_POINTS],
}

macro_rules! au64_arr {
    ($val:expr) => {
        [
            AtomicU64::new($val), AtomicU64::new($val), AtomicU64::new($val),
            AtomicU64::new($val), AtomicU64::new($val), AtomicU64::new($val),
            AtomicU64::new($val), AtomicU64::new($val), AtomicU64::new($val),
            AtomicU64::new($val), AtomicU64::new($val), AtomicU64::new($val),
            AtomicU64::new($val), AtomicU64::new($val), AtomicU64::new($val),
            AtomicU64::new($val),
        ]
    };
}

macro_rules! au32_arr {
    ($val:expr) => {
        [
            AtomicU32::new($val), AtomicU32::new($val), AtomicU32::new($val),
            AtomicU32::new($val), AtomicU32::new($val), AtomicU32::new($val),
            AtomicU32::new($val), AtomicU32::new($val), AtomicU32::new($val),
            AtomicU32::new($val), AtomicU32::new($val), AtomicU32::new($val),
            AtomicU32::new($val), AtomicU32::new($val), AtomicU32::new($val),
            AtomicU32::new($val),
        ]
    };
}

static WCET: WcetTable = WcetTable {
    min:        au64_arr!(u64::MAX),
    max:        au64_arr!(0),
    total:      au64_arr!(0),
    count:      au64_arr!(0),
    violations: au32_arr!(0),
};

// ── RISC-V cycle counter ──────────────────────────────────────────────────────

/// Read the hardware cycle counter.
///
/// On RV64: `rdcycle` CSR (always available in U/S-mode when `mcounteren.CY` is set).
/// On RV32 (ESP32-C3): `rdcycle` reads low 32 bits; combine with `rdcycleh` for 64-bit.
/// Fallback: CLINT mtime (lower resolution, ~10 MHz).
#[inline(always)]
pub fn read_cycles() -> u64 {
    #[cfg(target_pointer_width = "64")]
    {
        let cycles: u64;
        unsafe { core::arch::asm!("rdcycle {}", out(reg) cycles); }
        cycles
    }
    #[cfg(target_pointer_width = "32")]
    {
        // ESP32-C3: read 64-bit cycle counter via rdcycle + rdcycleh
        // Loop to handle rollover between the two reads.
        loop {
            let hi1: u32;
            let lo: u32;
            let hi2: u32;
            unsafe {
                core::arch::asm!(
                    "rdcycleh {0}",
                    "rdcycle  {1}",
                    "rdcycleh {2}",
                    out(reg) hi1, out(reg) lo, out(reg) hi2
                );
            }
            if hi1 == hi2 {
                return (hi1 as u64) << 32 | (lo as u64);
            }
        }
    }
}

/// RISC-V CPU frequency used to convert cycles → microseconds.
/// Matches `TIMER_FREQ` from the CLINT driver (10 MHz on QEMU, 1 GHz on K1).
const CPU_FREQ_HZ: u64 = crate::clint::TIMER_FREQ;

/// Convert a cycle delta to microseconds.
#[inline]
pub fn cycles_to_us(cycles: u64) -> u64 {
    cycles / (CPU_FREQ_HZ / 1_000_000).max(1)
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Begin a WCET measurement — returns current cycle count.
///
/// Call this immediately before the section to measure.
#[inline(always)]
pub fn wcet_begin() -> u64 {
    read_cycles()
}

/// End a WCET measurement and record the sample.
///
/// `point` — one of the `WCET_*` constants.
/// `start`  — value returned by the matching `wcet_begin()` call.
///
/// Returns the elapsed cycles.
#[inline]
pub fn wcet_end(point: u8, start: u64) -> u64 {
    let end = read_cycles();
    let elapsed = end.wrapping_sub(start);

    let idx = point as usize;
    if idx >= WCET_MAX_POINTS { return elapsed; }

    // Update min (atomically).
    let mut cur_min = WCET.min[idx].load(Ordering::Relaxed);
    while elapsed < cur_min {
        match WCET.min[idx].compare_exchange_weak(
            cur_min, elapsed, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(v) => cur_min = v,
        }
    }

    // Update max (atomically).
    let mut cur_max = WCET.max[idx].load(Ordering::Relaxed);
    while elapsed > cur_max {
        match WCET.max[idx].compare_exchange_weak(
            cur_max, elapsed, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(v) => cur_max = v,
        }
    }

    // Accumulate total and count (relaxed — statistics, not synchronisation).
    WCET.total[idx].fetch_add(elapsed, Ordering::Relaxed);
    WCET.count[idx].fetch_add(1, Ordering::Relaxed);

    // Check bound violation.
    let bound_us = WCET_BOUNDS_US[idx];
    if bound_us > 0 {
        let elapsed_us = cycles_to_us(elapsed);
        if elapsed_us > bound_us {
            WCET.violations[idx].fetch_add(1, Ordering::Relaxed);
            // Non-blocking print: try_acquire avoids spinning in ISR context.
            // Write directly to Uart (bypasses acquire) since we already hold the lock.
            if let Some(_guard) = crate::uart::try_acquire() {
                use core::fmt::Write;
                let _ = writeln!(crate::Uart,
                    "[WCET] VIOLATION: {} took {}µs > bound {}µs",
                    WCET_NAMES[idx], elapsed_us, bound_us
                );
            }
        }
    }

    elapsed
}

/// Get a snapshot of statistics for one measurement point.
pub fn wcet_stats(point: u8) -> WcetStats {
    let idx = point as usize;
    if idx >= WCET_MAX_POINTS {
        return WcetStats::new();
    }
    WcetStats {
        min_cycles:   WCET.min[idx].load(Ordering::Relaxed),
        max_cycles:   WCET.max[idx].load(Ordering::Relaxed),
        total_cycles: WCET.total[idx].load(Ordering::Relaxed),
        count:        WCET.count[idx].load(Ordering::Relaxed),
        violations:   WCET.violations[idx].load(Ordering::Relaxed),
    }
}

/// Reset statistics for all measurement points.
pub fn wcet_reset_all() {
    for i in 0..WCET_MAX_POINTS {
        WCET.min[i].store(u64::MAX, Ordering::Relaxed);
        WCET.max[i].store(0, Ordering::Relaxed);
        WCET.total[i].store(0, Ordering::Relaxed);
        WCET.count[i].store(0, Ordering::Relaxed);
        WCET.violations[i].store(0, Ordering::Relaxed);
    }
}

/// Print a WCET report table to UART.
pub fn wcet_report() {
    crate::kprintln!("[WCET] ── Execution Time Report ────────────────────────────────────");
    crate::kprintln!("[WCET]  {:16} {:>10} {:>10} {:>10} {:>7} {:>6}",
        "name", "min_us", "max_us", "avg_us", "count", "viol");
    for i in 0..WCET_MAX_POINTS {
        let count = WCET.count[i].load(Ordering::Relaxed);
        if count == 0 { continue; }
        let min_c  = WCET.min[i].load(Ordering::Relaxed);
        let max_c  = WCET.max[i].load(Ordering::Relaxed);
        let total  = WCET.total[i].load(Ordering::Relaxed);
        let avg_c  = total / count;
        let viol   = WCET.violations[i].load(Ordering::Relaxed);

        let min_us = cycles_to_us(min_c);
        let max_us = cycles_to_us(max_c);
        let avg_us = cycles_to_us(avg_c);

        crate::kprintln!("[WCET]  {:16} {:>10} {:>10} {:>10} {:>7} {:>6}",
            WCET_NAMES[i], min_us, max_us, avg_us, count, viol);
    }
    crate::kprintln!("[WCET] ────────────────────────────────────────────────────────────");
}

/// Check all bounds and log any points that have recorded violations.
///
/// Returns the number of points with at least one violation.
pub fn wcet_check_bounds() -> usize {
    let mut total_violations = 0usize;
    for i in 0..WCET_MAX_POINTS {
        let viol = WCET.violations[i].load(Ordering::Relaxed);
        if viol > 0 {
            let max_c  = WCET.max[i].load(Ordering::Relaxed);
            let max_us = cycles_to_us(max_c);
            let bound  = WCET_BOUNDS_US[i];
            crate::kprintln!(
                "[WCET] BOUND EXCEEDED: {} max={}µs bound={}µs violations={}",
                WCET_NAMES[i], max_us, bound, viol
            );
            total_violations += 1;
        }
    }
    total_violations
}

// ── F16.4: Jitter measurement ─────────────────────────────────────────────────
//
// Measures the interval between successive events (e.g., timer ISR fires)
// to characterise scheduling jitter.

/// Maximum number of jitter measurement series.
pub const JITTER_MAX_SERIES: usize = 4;

/// Jitter series ID: timer ISR interval.
pub const JITTER_TIMER_ISR:     u8 = 0;
/// Jitter series ID: ISR → actuator write latency.
pub const JITTER_ISR_TO_ACT:    u8 = 1;

struct JitterTable {
    last:       [AtomicU64; JITTER_MAX_SERIES],
    min_delta:  [AtomicU64; JITTER_MAX_SERIES],
    max_delta:  [AtomicU64; JITTER_MAX_SERIES],
    count:      [AtomicU64; JITTER_MAX_SERIES],
}

static JITTER: JitterTable = JitterTable {
    last:      [AtomicU64::new(0), AtomicU64::new(0),
                AtomicU64::new(0), AtomicU64::new(0)],
    min_delta: [AtomicU64::new(u64::MAX), AtomicU64::new(u64::MAX),
                AtomicU64::new(u64::MAX), AtomicU64::new(u64::MAX)],
    max_delta: [AtomicU64::new(0), AtomicU64::new(0),
                AtomicU64::new(0), AtomicU64::new(0)],
    count:     [AtomicU64::new(0), AtomicU64::new(0),
                AtomicU64::new(0), AtomicU64::new(0)],
};

/// Record one jitter observation (call each time the event fires).
///
/// `series` — one of the `JITTER_*` constants.
pub fn jitter_record(series: u8) {
    let idx = series as usize;
    if idx >= JITTER_MAX_SERIES { return; }

    let now = read_cycles();
    let prev = JITTER.last[idx].swap(now, Ordering::Relaxed);

    if prev == 0 { return; } // first sample — no delta yet

    let delta = now.wrapping_sub(prev);

    let mut cur_min = JITTER.min_delta[idx].load(Ordering::Relaxed);
    while delta < cur_min {
        match JITTER.min_delta[idx].compare_exchange_weak(
            cur_min, delta, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(v) => cur_min = v,
        }
    }

    let mut cur_max = JITTER.max_delta[idx].load(Ordering::Relaxed);
    while delta > cur_max {
        match JITTER.max_delta[idx].compare_exchange_weak(
            cur_max, delta, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(v) => cur_max = v,
        }
    }

    JITTER.count[idx].fetch_add(1, Ordering::Relaxed);
}

/// Print a jitter report to UART.
pub fn jitter_report() {
    const NAMES: [&str; JITTER_MAX_SERIES] = [
        "timer_isr", "isr_to_act", "unused_2", "unused_3"
    ];

    crate::kprintln!("[JITTER] ── Jitter Report ──────────────────────────────────────────");
    crate::kprintln!("[JITTER]  {:12} {:>12} {:>12} {:>10}",
        "series", "min_ns", "max_ns", "samples");

    let freq = CPU_FREQ_HZ;
    for i in 0..JITTER_MAX_SERIES {
        let count = JITTER.count[i].load(Ordering::Relaxed);
        if count == 0 { continue; }

        let min_c = JITTER.min_delta[i].load(Ordering::Relaxed);
        let max_c = JITTER.max_delta[i].load(Ordering::Relaxed);

        // Convert cycles → nanoseconds: ns = cycles * 1_000_000_000 / freq
        let min_ns = if freq > 0 { min_c * 1_000_000_000 / freq } else { 0 };
        let max_ns = if freq > 0 { max_c * 1_000_000_000 / freq } else { 0 };

        crate::kprintln!("[JITTER]  {:12} {:>12} {:>12} {:>10}",
            NAMES[i], min_ns, max_ns, count);
    }
    crate::kprintln!("[JITTER] ────────────────────────────────────────────────────────────");
}
