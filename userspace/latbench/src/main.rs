//! Ring-3 syscall latency microbenchmark.
//!
//! The point is not "syscalls take N ns" — on QEMU TCG that number means
//! little in absolute terms. The point is the *differences* between four
//! measurements, which isolate where the time goes:
//!
//!   floor      `getpid()`         — trap in, dispatch, trap out. No
//!                                    capability check, no user copy.
//!   cap-hit-0  `sensor_read(IMU)` — + cap_check finding its handle at
//!                                    index 0, + a 24-byte copy_to_user.
//!   cap-hit-10 `motor_speed(0,0)` — + cap_check walking to index 10.
//!                                    No copy at all.
//!   cap-miss   `gpio_read(0)`     — + cap_check finding NOTHING, which
//!                                    means all MAX_HANDLES_GLOBAL (256)
//!                                    iterations, each one acquiring and
//!                                    releasing the global handle spinlock
//!                                    with interrupts saved.
//!
//! `cap-miss` minus `floor` is the cost of a full failed scan. `cap-hit-10`
//! minus `cap-hit-0` is the cost of ten more scan steps, which extrapolates
//! the per-step price. A robot's control loop pays the hit path every tick,
//! and any denied or not-yet-granted resource pays the miss path.
//!
//! Timing uses `rdtime` (CLINT mtime, 10 MHz on QEMU virt = 100 ns/tick), so
//! a single call is below the clock's resolution — everything is measured in
//! batches and divided. `scounteren = 0x7` in `trap_init` is what makes the
//! counter readable from U-mode at all.

#![no_std]
#![no_main]

use robot_os_libsys as sys;

/// Iterations per batch. Large enough that the 100 ns tick granularity is
/// noise against the total, small enough to finish promptly under TCG.
const N: u64 = 2000;

/// CLINT mtime frequency on QEMU virt.
const TIMER_HZ: u64 = 10_000_000;

#[inline(always)]
fn rdtime() -> u64 {
    let t: u64;
    unsafe { core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack)) };
    t
}

fn print_u(mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if n == 0 { i -= 1; buf[i] = b'0'; }
    while n > 0 { i -= 1; buf[i] = b'0' + (n % 10) as u8; n /= 10; }
    sys::print(&buf[i..]);
}

/// Report one batch as nanoseconds per operation.
fn report(label: &[u8], ticks: u64) {
    // ticks * 1e9 / TIMER_HZ / N, ordered to avoid overflow and keep
    // integer precision (no FPU in this binary).
    let ns_per_op = ticks * (1_000_000_000 / TIMER_HZ) / N;
    sys::print(b"[LATBENCH] ");
    sys::print(label);
    sys::print(b" = ");
    print_u(ns_per_op);
    sys::print(b" ns/op  (");
    print_u(ticks);
    sys::print(b" ticks / ");
    print_u(N);
    sys::print(b" ops)\n");
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::println(b"[LATBENCH] Starting - measuring ring-3 syscall latency");

    // Warm up: first-touch page faults and I-cache misses belong to nobody.
    for _ in 0..64 { let _ = sys::getpid(); }

    let t0 = rdtime();
    for _ in 0..N { let _ = sys::getpid(); }
    let floor = rdtime() - t0;
    report(b"floor      getpid()        ", floor);

    let mut imu = [0u8; 24];
    let t0 = rdtime();
    for _ in 0..N { let _ = sys::sensor_read(0, &mut imu); }
    let cap0 = rdtime() - t0;
    report(b"cap-hit-0  sensor_read(IMU)", cap0);

    let t0 = rdtime();
    for _ in 0..N { let _ = sys::motor_speed(0, 0); }
    let cap10 = rdtime() - t0;
    report(b"cap-hit-10 motor_speed(0,0)", cap10);

    let t0 = rdtime();
    for _ in 0..N { let _ = sys::gpio_read(0); }
    let miss = rdtime() - t0;
    report(b"cap-miss   gpio_read(0)    ", miss);

    // write() to stdout. Two sizes, because sys_write zeroes a fixed 4 KiB
    // kernel stack buffer regardless of `count` — if that memset dominates,
    // a 1-byte write costs about the same as a 64-byte one, and both cost
    // far more than the syscall floor.
    //
    // Writing to fd 2 (stderr) on purpose: it takes the same UART path as
    // stdout but keeps the benchmark's own report lines on fd 1 readable.
    // Far fewer iterations than the other batches: every one of these
    // actually reaches the console, and N=2000 x 64 bytes buried the report
    // itself under 128 KB of filler. WN is scaled back and `report_n` divides
    // by the right count.
    const WN: u64 = 100;
    let one = b"x";
    let t0 = rdtime();
    for _ in 0..WN { let _ = sys::write(2, one); }
    let w1 = (rdtime() - t0) * N / WN;   // normalise to the N-op scale
    report(b"write(2, 1 byte)           ", w1);

    let sixty4 = &[b'y'; 64];
    let t0 = rdtime();
    for _ in 0..WN { let _ = sys::write(2, sixty4); }
    let w64 = (rdtime() - t0) * N / WN;
    sys::print(b"\n");                    // close the filler line
    report(b"write(2, 64 bytes)         ", w64);

    // ── Deltas: the actual result ────────────────────────────────────────
    sys::println(b"[LATBENCH] ---- deltas vs floor ----");
    let d = |x: u64| -> u64 {
        if x > floor { (x - floor) * (1_000_000_000 / TIMER_HZ) / N } else { 0 }
    };
    sys::print(b"[LATBENCH] cap_check hit@0 + 24B copy  = +");
    print_u(d(cap0));
    sys::print(b" ns\n[LATBENCH] cap_check hit@10, no copy   = +");
    print_u(d(cap10));
    sys::print(b" ns\n[LATBENCH] cap_check MISS (256 locked) = +");
    print_u(d(miss));
    sys::println(b" ns");

    sys::print(b"[LATBENCH] write 1B vs floor           = +");
    print_u(d(w1));
    sys::print(b" ns\n[LATBENCH] write 64B vs floor          = +");
    print_u(d(w64));
    sys::println(b" ns");

    sys::println(b"[LATBENCH] DONE");
    sys::exit(0);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    sys::println(b"[LATBENCH] PANIC");
    sys::exit(2);
}
