//! SMP support for the Robot OS scheduler.
//!
//! Starts secondary harts via SBI HSM `hart_start` (same approach as the C kernel's
//! `sbi_hart_start()` in kernel/core/smp.c). OpenSBI parks secondary harts in M-mode
//! by default; they must be explicitly started via HSM, not via a polling flag.
//!
//! ESP32-C3: single core, no SBI — wake_hart/wake_harts are no-ops.

use core::sync::atomic::AtomicUsize;

/// Number of CPUs currently considered online for task distribution.
///
/// Set by the boot CPU before task creation to ensure proper load balancing.
/// Secondary CPUs do NOT increment this — the boot CPU sets the final value.
pub static NUM_ONLINE_CPUS: AtomicUsize = AtomicUsize::new(1);

// ---- External symbols (S-mode platforms only) ----

#[cfg(not(feature = "esp32c3"))]
unsafe extern "C" {
    /// Secondary CPU entry point defined in kernel/src/asm/boot.S.
    /// OpenSBI will jump to this address (in S-mode) when `sbi::hart_start` is called.
    fn _secondary_start();
}

// ---- Hart wakeup via SBI HSM (S-mode platforms) ----

#[cfg(not(feature = "esp32c3"))]
pub unsafe fn wake_hart(hart_id: usize) {
    let entry = _secondary_start as *const () as usize;
    let ret = robot_os_arch::sbi::hart_start(hart_id, entry, hart_id);
    let _ = ret;
}

#[cfg(not(feature = "esp32c3"))]
pub unsafe fn wake_harts(num_cpus: usize) {
    let boot = current_cpu_id();
    for hart_id in 0..num_cpus {
        if hart_id != boot {
            wake_hart(hart_id);
        }
    }
}

// ---- ESP32-C3: single core, no SBI ----

#[cfg(feature = "esp32c3")]
pub unsafe fn wake_hart(_hart_id: usize) {}

#[cfg(feature = "esp32c3")]
pub unsafe fn wake_harts(_num_cpus: usize) {}

// ---- Current CPU identity ----

/// Returns the current CPU's hart ID by reading the `tp` (thread pointer) register.
///
/// `tp` is set to `hart_id` in boot.S for all CPUs (both primary and secondary).
/// Rust does not use `tp` in `no_std` bare-metal builds.
#[inline(always)]
pub fn current_cpu_id() -> usize {
    let id: usize;
    unsafe {
        core::arch::asm!(
            "mv {}, tp",
            out(reg) id,
            options(nostack, nomem)
        );
    }
    id
}
