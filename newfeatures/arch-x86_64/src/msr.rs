//! Shared `rdmsr` / `wrmsr` helpers.
//!
//! Several modules (`apic`, `syscall`, `xsave`, future `tsc`-
//! calibrate path) all need to read/write MSRs. Extracted here
//! so the same 12-line asm doesn't get copy-pasted into each one.

#![allow(dead_code)]

/// Read a Model-Specific Register. Returns the 64-bit value
/// combined from EDX:EAX.
///
/// # Safety
/// Caller must be at CPL=0. MSR address must be valid for the
/// current CPU (rdmsr on a reserved MSR raises #GP).
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Write a Model-Specific Register. Splits the 64-bit `val`
/// into EDX:EAX.
///
/// # Safety
/// Caller must be at CPL=0. Writing certain MSRs (IA32_EFER,
/// PAT, FS_BASE, etc.) has cross-cutting side effects — the
/// caller is responsible for understanding them.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn wrmsr(msr: u32, val: u64) {
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") val as u32,
            in("edx") (val >> 32) as u32,
            options(nostack, nomem, preserves_flags),
        );
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn rdmsr(_msr: u32) -> u64 { 0 }
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn wrmsr(_msr: u32, _val: u64) {}
