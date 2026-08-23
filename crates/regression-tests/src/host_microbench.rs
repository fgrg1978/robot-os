//! Host-side microbenchmarks for pure-logic kernel functions.
//!
//! These run on the host (cargo test --release) and measure
//! per-iteration latency of functions that are also exercised in the
//! kernel (via `#[wcet(...)]` annotations).  Goals:
//!
//! 1. **Detect logic regressions before bench.** If `parse_packet`
//!    suddenly gets 5× slower because someone added an allocation,
//!    a host microbench catches it in ~seconds, vs the 3-minute
//!    QEMU bench cycle.
//! 2. **Establish a portable baseline.** Host numbers are
//!    deterministic (no QEMU TCG rdcycle inflation), so two
//!    developers on the same hardware get matching results.
//! 3. **Complement kernel `wcet_per_fn`** — same functions
//!    measured under two regimes (host pure-logic vs kernel
//!    runtime), divergence between the two signals an
//!    integration cost (e.g. syscall overhead, lock contention).
//!
//! Output format mirrors the kernel's `[WCET]` log lines so the
//! brain `parse_wcet` collector can ingest it unchanged:
//!
//!     [HOST-UBENCH] <name> min=<ns> max=<ns> avg=<ns> samples=<n>
//!
//! Run with:
//!     cargo test --release --target aarch64-apple-darwin host_microbench
//!     # add `-- --nocapture` to see the printed measurements
//!
//! These are NOT gated as `#[ignore]` because they're fast (~ms
//! total) and serve as regression guards even under normal `cargo
//! test` runs.

use core::time::Duration;
use std::hint::black_box;
use std::time::Instant;

// Pull the same brain_protocol source the kernel ships, via #[path].
// Mirrors the pattern in property.rs.
#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../../behavior/src/brain_protocol.rs"]
mod brain_protocol_src;

// ── Microbench primitives ────────────────────────────────────────────────────

/// Number of inner iterations per outer sample.  Picked so each sample
/// takes ~100 µs on a 2024-era host — long enough to amortise clock
/// jitter, short enough that 100 samples complete in 10 ms total.
const INNER_ITERS: u32 = 1000;

/// Number of outer samples to collect.  Final report uses min/max/avg
/// across these samples to dampen single-sample jitter.
const OUTER_SAMPLES: u32 = 100;

/// Run `body` `INNER_ITERS * OUTER_SAMPLES` times, return
/// (min_ns, max_ns, avg_ns) per single iteration.
fn measure<F: FnMut()>(name: &str, mut body: F) -> (u64, u64, u64) {
    let mut min_ns = u64::MAX;
    let mut max_ns = 0u64;
    let mut total_ns = 0u64;

    for _ in 0..OUTER_SAMPLES {
        let t0 = Instant::now();
        for _ in 0..INNER_ITERS {
            body();
        }
        let elapsed = t0.elapsed();
        let per_iter_ns = elapsed.as_nanos() as u64 / INNER_ITERS as u64;
        if per_iter_ns < min_ns { min_ns = per_iter_ns; }
        if per_iter_ns > max_ns { max_ns = per_iter_ns; }
        total_ns += per_iter_ns;
    }
    let avg_ns = total_ns / OUTER_SAMPLES as u64;

    // Format mirrors kernel [WCET] line for collector compatibility.
    println!(
        "[HOST-UBENCH] {} min={}ns max={}ns avg={}ns samples={}",
        name, min_ns, max_ns, avg_ns, OUTER_SAMPLES,
    );
    (min_ns, max_ns, avg_ns)
}

/// Sanity check: measure overhead of the timer-reading itself.  Any
/// per-op result smaller than this is hitting timing-resolution noise.
fn measure_timer_floor() -> u64 {
    let mut min_ns = u64::MAX;
    for _ in 0..OUTER_SAMPLES {
        let t0 = Instant::now();
        let _ = Instant::now().duration_since(t0);
        let elapsed = t0.elapsed().as_nanos() as u64;
        if elapsed < min_ns { min_ns = elapsed; }
    }
    min_ns
}

// ── Microbenchmarks ──────────────────────────────────────────────────────────

#[test]
fn bench_crc8() {
    let payload = [0x12u8; 64];
    let (_min, max, _avg) = measure("crc8_64B", || {
        // black_box prevents the optimiser from constant-folding or
        // dead-stripping the call when its result is unused.  Without
        // it, release builds report 0 ns/iter because the loop body is
        // optimised away entirely.
        black_box(brain_protocol_src::crc8(black_box(&payload)));
    });
    // Sanity bound: 64-byte CRC on a 2024-era host should be ≤ 1 µs.
    // If this fires, someone replaced the table with a loop or added an
    // allocation per call.
    assert!(max < 5_000, "crc8(64B) max {}ns exceeds 5µs ceiling", max);
}

#[test]
fn bench_build_parse_packet_roundtrip() {
    let payload = [0x42u8; 32];
    let mut frame = [0u8; 256];
    let (_min, max, _avg) = measure("build_parse_packet_32B", || {
        let n = black_box(brain_protocol_src::build_packet(
            0x01, black_box(&payload), &mut frame,
        ));
        black_box(brain_protocol_src::parse_packet(black_box(&frame[..n])));
    });
    // Pure parser, no I/O.  Should be ≤ 2 µs even with allocation
    // (which there shouldn't be any of).
    assert!(max < 10_000, "build+parse_packet roundtrip max {}ns > 10µs", max);
}

#[test]
fn bench_parse_packet_only() {
    // Pre-build the frame outside the timed loop so we measure only
    // parsing.  Mirrors what the kernel's `parse_packet` `#[wcet(50_us)]`
    // annotation observes at runtime.
    let payload = [0x42u8; 32];
    let mut frame = [0u8; 256];
    let n = brain_protocol_src::build_packet(0x01, &payload, &mut frame);
    let frame_slice = &frame[..n];

    let (_min, max, _avg) = measure("parse_packet_32B", || {
        black_box(brain_protocol_src::parse_packet(black_box(frame_slice)));
    });
    // Parser-only — should be sub-microsecond on host.  CRC verify
    // dominates; for 32-byte payload that's ~40 byte CRC.
    assert!(max < 5_000, "parse_packet(32B) max {}ns > 5µs", max);
}

#[test]
fn bench_timer_floor_diagnostic() {
    // Not a regression test — just prints the measurement floor so
    // results above are interpreted with the right resolution context.
    let floor_ns = measure_timer_floor();
    println!("[HOST-UBENCH] timer_floor min={}ns", floor_ns);
    // Sanity: timer reads should be < 1 µs on modern hardware.
    assert!(floor_ns < 10_000, "timer_floor {}ns > 10µs — broken timer?", floor_ns);
}

// ── Compile-time alignment check ─────────────────────────────────────────────

/// Ensure the same `brain_protocol_src` path used by `property.rs` is in
/// scope here too.  Catches accidental relocation of the source file.
#[test]
fn brain_protocol_src_path_is_intact() {
    let _ = brain_protocol_src::crc8;
    let _ = brain_protocol_src::build_packet;
    let _ = brain_protocol_src::parse_packet;
}

#[allow(dead_code)]
const _: Duration = Duration::from_nanos(1);  // silence unused-import
