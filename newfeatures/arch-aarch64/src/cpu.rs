//! Per-CPU identity + low-level idle on ARMv8-A.

/// Read `MPIDR_EL1` and return the per-CPU identifier
/// (`Aff0 | Aff1 << 8 | Aff2 << 16` — bottom 24 bits, ignoring
/// the `U` and `RES1` bits). This is the same composition U-Boot
/// / Linux / EDK2 use to construct logical CPU indices.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn hart_id() -> usize {
    let mpidr: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, MPIDR_EL1",
            out(reg) mpidr,
            options(nomem, nostack, preserves_flags),
        );
    }
    (mpidr as usize) & 0x00FF_FFFF
}

/// Wait For Interrupt — low-power idle. Maps directly to the ARM
/// `WFI` instruction.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn wfi() {
    unsafe {
        core::arch::asm!(
            "wfi",
            options(nomem, nostack, preserves_flags),
        );
    }
}
