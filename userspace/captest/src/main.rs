//! Capability + syscall ABI test for a ring-3 process.
//!
//! Until 2026-08-20 nothing in the tree ever called `handle_grant`, so the
//! global handle table was permanently empty and `cap_check` denied every
//! hardware syscall made from userspace. `reflex` and `brain_client` could be
//! loaded, exec'd, and would then sit forever doing nothing — reflex reads a
//! failed rangefinder as "no obstacle", because `obstacle_front()` guards the
//! comparison with `range_front > 0`. A blind daemon and a daemon on a clear
//! road print exactly the same thing: nothing.
//!
//! This binary makes that state observable. It asserts BOTH halves:
//!
//!   POSITIVE — a granted resource is usable. Every `sensor_read` and
//!   `motor_speed` must return something other than `E_PERM`. Note it may
//!   still return -1 ("sensor not ready"): QEMU has no IMU. -1 means the
//!   capability was accepted and the driver had nothing to give, which is a
//!   pass for this test. Only E_PERM is a failure.
//!
//!   NEGATIVE — an ungranted resource is still refused. The autorun grant
//!   covers sensors and the two drive motors and nothing else, so GPIO and
//!   I2C must still come back E_PERM. Without this half the test would pass
//!   just as happily if someone deleted `cap_check` altogether, which is the
//!   opposite of what it is meant to prove.

#![no_std]
#![no_main]

use robot_os_libsys as sys;

/// Permission denied, from `crates/syscall/src/handlers.rs`.
const E_PERM: isize = -99;

static mut FAILURES: u32 = 0;

fn report(name: &[u8], ok: bool, rc: isize) {
    sys::print(if ok { b"[CAPTEST]   ok   " } else { b"[CAPTEST]  FAIL  " });
    sys::print(name);
    sys::print(b" rc=");
    print_i(rc);
    sys::print(b"\n");
    if !ok {
        unsafe { FAILURES += 1; }
    }
}

/// Minimal signed-decimal print — no core::fmt in a no_std ring-3 binary.
fn print_i(v: isize) {
    if v < 0 {
        sys::print(b"-");
    }
    let mut n = if v < 0 { (-(v as i64)) as u64 } else { v as u64 };
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if n == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::print(&buf[i..]);
}

/// Granted → must NOT be E_PERM. -1 (driver has nothing) still counts.
fn expect_allowed(name: &[u8], rc: isize) {
    report(name, rc != E_PERM, rc);
}

/// Not granted → must be exactly E_PERM.
fn expect_denied(name: &[u8], rc: isize) {
    report(name, rc == E_PERM, rc);
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::println(b"[CAPTEST] Starting...");

    // ── POSITIVE: granted sensors are reachable ──────────────────────────
    let mut imu = [0u8; 24];
    expect_allowed(b"sensor_read(IMU)", sys::sensor_read(0, &mut imu));
    let mut odom = [0u8; 16];
    expect_allowed(b"sensor_read(ODOM)", sys::sensor_read(1, &mut odom));
    let mut enc = [0u8; 16];
    expect_allowed(b"sensor_read(ENCODER)", sys::sensor_read(2, &mut enc));
    let mut range = [0u8; 4];
    expect_allowed(b"sensor_read(RANGE)", sys::sensor_read(3, &mut range));
    let mut batt = [0u8; 2];
    expect_allowed(b"sensor_read(BATTERY)", sys::sensor_read(4, &mut batt));

    // ── POSITIVE: granted motors accept a write ──────────────────────────
    // Speed 0 on purpose: this asserts the capability, and must not move a
    // real robot if the same image is ever booted on hardware.
    expect_allowed(b"motor_speed(L,0)", sys::motor_speed(0, 0));
    expect_allowed(b"motor_speed(R,0)", sys::motor_speed(1, 0));

    // ── NEGATIVE: ungranted resources stay refused ───────────────────────
    expect_denied(b"gpio_read(0) [ungranted]", sys::gpio_read(0));
    expect_denied(b"motor_speed(9,0) [ungranted id]", sys::motor_speed(9, 0));

    // ── DIAGNOSTIC: what the sensors actually reported ───────────────────
    // Not assertions — QEMU has no ultrasonic rangefinder, no IMU and no ADC,
    // so there are no correct values to assert against. They are printed
    // because the alternative is inferring them, and inferring is what cost
    // time here: `reflex` sat silent and it was not possible to tell from its
    // output whether it was reading a clear road or reading nothing at all.
    //
    // On the RANGE line: under QEMU the driver IS initialised and serves the
    // simulated distance array, so these are real numbers (1500/800 mm at
    // boot), not zeros — `reflex-smoke` drives them to force a decision.
    //
    // The ABI still has a latent ambiguity worth recording. `sys_sensor_read`
    // computes the value as `us_read_mm(0).unwrap_or(0)`, so an
    // *uninitialised or absent* sensor would reach userspace as 0 mm, which
    // is also what an obstacle pressed against the bumper reads. reflex
    // guards its comparison with `range_front > 0`, so that case becomes "no
    // obstacle" — a safety daemon failing silent-open. It does not arise in
    // QEMU and is not patched from userspace: it is a decision about what the
    // sensor ABI should say when there is nothing to report.
    sys::print(b"[CAPTEST] RANGE front_mm=");
    print_i(u16::from_le_bytes([range[0], range[1]]) as isize);
    sys::print(b" right_mm=");
    print_i(u16::from_le_bytes([range[2], range[3]]) as isize);
    sys::print(b"  (0 = no sensor OR touching; the ABI cannot say which)\n");
    sys::print(b"[CAPTEST] BATTERY mv=");
    print_i(u16::from_le_bytes([batt[0], batt[1]]) as isize);
    sys::print(b"\n");

    let failed = unsafe { FAILURES };
    if failed == 0 {
        sys::println(b"[CAPTEST] ALL PASSED");
        sys::exit(0);
    } else {
        sys::print(b"[CAPTEST] FAILED: ");
        print_i(failed as isize);
        sys::println(b" check(s)");
        sys::exit(1);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    sys::println(b"[CAPTEST] PANIC");
    sys::exit(2);
}
