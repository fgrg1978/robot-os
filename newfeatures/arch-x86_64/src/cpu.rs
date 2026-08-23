//! Per-CPU identity + low-level idle on x86_64.

/// Read the local APIC ID via `CPUID.01:EBX[31:24]`. This is the
/// "initial APIC ID" — on systems with x2APIC enabled the kernel
/// should instead read `IA32_X2APIC_APICID` (MSR 0x802) via the
/// APIC driver. For early-boot identity the CPUID path is
/// sufficient and works under both APIC modes.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn hart_id() -> usize {
    let ebx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "mov {0:e}, ebx",
            "pop rbx",
            out(reg) ebx,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nostack, preserves_flags),
        );
    }
    (ebx >> 24) as usize
}

/// `HLT` — halt until next interrupt. Equivalent to RISC-V `WFI`
/// and ARM `WFI`. Must run with interrupts enabled, otherwise the
/// CPU sleeps forever.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn hlt() {
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

/// Cross-arch alias for [`hlt`] — kernel code reads `cpu::wfi()`
/// regardless of ISA (RISC-V `wfi`, ARM `WFI`, x86_64 `hlt`).
/// Required for the facade re-export `robot_os_arch::cpu::wfi` to
/// resolve on x86_64 builds (Item 2 Stage 3 batch 3).
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn wfi() {
    hlt();
}
