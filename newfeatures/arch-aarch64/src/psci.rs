//! PSCI v1.0 — Power State Coordination Interface.
//!
//! ARM's standard SMC/HVC interface for shutdown, reboot, and
//! starting secondary CPUs. Function IDs from `ARM DEN 0022D.b`
//! Table 5-1.
//!
//! **Conduit selection.** PSCI calls can travel through either
//! `SMC` (handled by EL3 firmware — TF-A, OP-TEE) or `HVC`
//! (handled by EL2 — a hypervisor or, in our case, QEMU's
//! built-in PSCI emulator). The right one depends on the
//! platform: real hardware with ATF wants SMC; QEMU `-machine
//! virt` defaults to HVC (psci-conduit=hvc). We expose a runtime
//! selector so both work without a recompile.
//!
//! Default = HVC, which is what QEMU virt expects. Call
//! [`set_conduit`] before any PSCI call to switch to SMC on
//! platforms with EL3 firmware.

/// PSCI function IDs (32-bit + SMC32 convention; 64-bit variants
/// add `0x40000000` to the FID).
pub const PSCI_VERSION:        u32 = 0x84000000;
pub const PSCI_CPU_SUSPEND_32: u32 = 0x84000001;
pub const PSCI_CPU_OFF:        u32 = 0x84000002;
pub const PSCI_CPU_ON_64:      u32 = 0xC4000003;
pub const PSCI_SYSTEM_OFF:     u32 = 0x84000008;
pub const PSCI_SYSTEM_RESET:   u32 = 0x84000009;

/// PSCI standard return codes.
pub const PSCI_OK:                    i32 = 0;
pub const PSCI_NOT_SUPPORTED:         i32 = -1;
pub const PSCI_INVALID_PARAMS:        i32 = -2;
pub const PSCI_DENIED:                i32 = -3;
pub const PSCI_ALREADY_ON:            i32 = -4;
pub const PSCI_ON_PENDING:            i32 = -5;
pub const PSCI_INTERNAL_FAILURE:      i32 = -6;
pub const PSCI_NOT_PRESENT:           i32 = -7;
pub const PSCI_DISABLED:              i32 = -8;
pub const PSCI_INVALID_ADDRESS:       i32 = -9;

/// PSCI conduit selector — which instruction carries the call.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Conduit {
    /// `HVC #0` — handled by EL2. Right for QEMU virt's emulated
    /// PSCI and for KVM guests; right whenever there is no EL3
    /// firmware (ATF/TF-A) installed.
    Hvc,
    /// `SMC #0` — handled by EL3. Right for production hardware
    /// running ATF/TF-A, OP-TEE, or any other secure-monitor that
    /// implements PSCI.
    Smc,
}

/// Active conduit. AtomicU8: 0 = HVC, 1 = SMC. Defaults to HVC
/// because QEMU virt (our primary aarch64 test target) routes
/// PSCI through HVC.
static CONDUIT: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(0);

const CONDUIT_HVC: u8 = 0;
const CONDUIT_SMC: u8 = 1;

/// Override the active PSCI conduit. Safe to call from any EL;
/// takes effect on the next PSCI call.
pub fn set_conduit(c: Conduit) {
    let v = match c {
        Conduit::Hvc => CONDUIT_HVC,
        Conduit::Smc => CONDUIT_SMC,
    };
    CONDUIT.store(v, core::sync::atomic::Ordering::Release);
}

/// Read the currently active conduit.
pub fn conduit() -> Conduit {
    match CONDUIT.load(core::sync::atomic::Ordering::Acquire) {
        CONDUIT_SMC => Conduit::Smc,
        _ => Conduit::Hvc,
    }
}

/// Issue a PSCI call through the active conduit. Returns the X0
/// register on return — for PSCI calls that's the standard return
/// code (see `PSCI_*` constants above).
#[cfg(target_arch = "aarch64")]
#[inline]
fn psci_call(fn_id: u32, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    let mut x0: u64 = fn_id as u64;
    match conduit() {
        Conduit::Hvc => unsafe {
            core::arch::asm!(
                "hvc #0",
                inout("x0") x0,
                in("x1") arg0,
                in("x2") arg1,
                in("x3") arg2,
                options(nostack, preserves_flags),
            );
        },
        Conduit::Smc => unsafe {
            core::arch::asm!(
                "smc #0",
                inout("x0") x0,
                in("x1") arg0,
                in("x2") arg1,
                in("x3") arg2,
                options(nostack, preserves_flags),
            );
        },
    }
    x0 as i64
}

/// `PSCI_CPU_ON_64`: bring secondary CPU `target_cpu` (MPIDR
/// affinity) up at `entry_point_phys` with `context_id` placed in
/// the new CPU's X0 register.
#[cfg(target_arch = "aarch64")]
pub fn cpu_on(target_cpu: u64, entry_point_phys: u64, context_id: u64) -> i32 {
    psci_call(PSCI_CPU_ON_64, target_cpu, entry_point_phys, context_id) as i32
}

/// `PSCI_SYSTEM_OFF`: shut the system down. Does not return.
#[cfg(target_arch = "aarch64")]
pub fn system_off() -> ! {
    let _ = psci_call(PSCI_SYSTEM_OFF, 0, 0, 0);
    // PSCI promises SYSTEM_OFF never returns. If it does (broken
    // firmware) we park forever.
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

/// `PSCI_SYSTEM_RESET`: warm reboot. Does not return.
#[cfg(target_arch = "aarch64")]
pub fn system_reset() -> ! {
    let _ = psci_call(PSCI_SYSTEM_RESET, 0, 0, 0);
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}
