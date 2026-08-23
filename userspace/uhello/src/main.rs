//! Minimal Rust user-space program (E11.AQ3 phase-1 de-risk).
//!
//! Proves that a `no_std` / `no_main` Rust binary built against `libsys` and
//! linked at VA 0x10000 loads and runs via the kernel's ELF exec path — the
//! prerequisite for a real userspace driver process. Prints a line and exits.

#![no_std]
#![no_main]

use robot_os_libsys as sys;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::println(b"[uhello] Rust user-space ELF running");
    sys::exit(0);
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::exit(1);
}
