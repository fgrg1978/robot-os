//! Boot helpers for early aarch64 init.
//!
//! Today this module exposes a single helper, [`drop_to_el1`], the
//! EL2→EL1 trampoline that's needed before any GICv3 sysreg /
//! generic-timer / MMU programming runs at EL1 on QEMU virt and
//! similar bare-metal entry environments where firmware (or QEMU
//! itself) drops the kernel at EL2 by default.
//!
//! Lifted out of `crates/aarch64-hello/src/main.rs` so the kernel
//! and any future bare-metal binary that links against
//! `arch-aarch64` can share the same one-shot trampoline instead of
//! reimplementing it. The asm body is the same as the one that the
//! aarch64-hello demo has been booting through since B1.gic.smp.real.

#[cfg(target_arch = "aarch64")]
use core::arch::global_asm;

// EL2→EL1 trampoline. Called via `bl _phanes_drop_to_el1` from EL2
// code; the asm preserves the caller's LR, programs the minimum
// set of sysregs EL1 needs to run safely, then ERETs into EL1 at
// the saved LR. Clobbers x10 only — AAPCS-clean.
//
// Sysreg setup, in order:
//   HCR_EL2.RW=1               EL1 runs in AArch64
//   HCR_EL2 other bits = 0     defends vs stale TGE / E2H
//   CNTHCTL_EL2 EL1PCEN+PCTEN  EL1 may use the physical timer
//   CNTVOFF_EL2 = 0            no virtual-timer offset
//   ICC_SRE_EL2 SRE+Enable=1   EL1 may use GICv3 sysregs
//   SCTLR_EL1 = 0x30C50838     reserved-bits mask, MMU off
//   CPACR_EL1.FPEN = 0b11      EL0/EL1 may use FP/SIMD/NEON
//   SPSR_EL2 = 0x3C5           EL1h, DAIF masked
//   ELR_EL2 = LR               return into caller at EL1
//   eret
#[cfg(target_arch = "aarch64")]
global_asm!(r#"
.section .text.phanes_boot, "ax"
.globl _phanes_drop_to_el1
_phanes_drop_to_el1:
    // HCR_EL2 = 0x8000_0000 exactly (RW=1, everything else 0).
    movz    x10, #0x8000, lsl #16
    msr     HCR_EL2, x10

    mrs     x10, CNTHCTL_EL2
    orr     x10, x10, #3            // EL1PCTEN | EL1PCEN
    msr     CNTHCTL_EL2, x10
    msr     CNTVOFF_EL2, xzr

    mov     x10, #1                 // ICC_SRE_EL2.SRE
    orr     x10, x10, #(1 << 3)     // ICC_SRE_EL2.Enable
    msr     S3_4_C12_C9_5, x10      // ICC_SRE_EL2
    isb

    mov     x10, #0x0838            // SCTLR_EL1 reserved-bits mask
    movk    x10, #0x30C5, lsl #16
    msr     SCTLR_EL1, x10

    // CPACR_EL1.FPEN = 0b11 (bits [21:20]). Explicit movz with
    // `lsl #16` because `mov #(0b11 << 20)` has no single-immediate
    // encoding and assemblers can silently emit the wrong value.
    movz    x10, #0x30, lsl #16     // 0x30 << 16 = 0x300000 = 0b11 << 20
    msr     CPACR_EL1, x10

    mov     x10, #0x3C5             // SPSR_EL2 = EL1h, DAIF masked
    msr     SPSR_EL2, x10

    msr     ELR_EL2, lr
    eret
"#);

/// Drop the calling thread from EL2 to EL1.
///
/// On return, the caller is running at EL1h with DAIF masked and a
/// minimal sysreg setup suitable for the existing aarch64 boot
/// path: GICv3 sysregs and the physical timer are accessible,
/// MMU is still off, FP/SIMD trap is cleared. The caller is
/// responsible for checking `CurrentEL` first — calling this from
/// EL1 will either undef-trap or behave undefined-ly.
///
/// # Safety
///
/// - The caller must be running at EL2.
/// - The trampoline reads the link register (LR) on entry and
///   uses it as the EL1 entry point. Any code that wraps this call
///   in inline asm or otherwise mutates LR before the asm body
///   runs will return to the wrong address — call it from a plain
///   Rust callsite that uses the standard AAPCS `bl` lowering.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
pub unsafe fn drop_to_el1() {
    extern "C" {
        fn _phanes_drop_to_el1();
    }
    unsafe { _phanes_drop_to_el1() }
}

/// Host-build stub so the trait surface compiles cross-target. A
/// non-aarch64 target should never reach this; the panic catches
/// any accidental call (e.g. from a test that forgot to gate the
/// arch crate).
#[cfg(not(target_arch = "aarch64"))]
pub unsafe fn drop_to_el1() {
    unreachable!("drop_to_el1() is aarch64-only")
}

/// SPSR_EL1 value used by [`eret_to_el0`]: mode bits = `EL0t`
/// (0b0000), DAIF = 1111 (all four exception classes masked).
/// EL0 may unmask them later if it wants; the kernel side doesn't
/// want to take an unexpected IRQ during the transition itself.
pub const SPSR_EL0T_DAIF_MASKED: u64 = 0x3C0;

/// ERET from EL1 to EL0 with the given user PC + SP. Never
/// returns — execution resumes at `user_pc` in EL0 with
/// `SP_EL0 = user_sp`, `SPSR_EL1 = `[`SPSR_EL0T_DAIF_MASKED`].
///
/// Pure shim around the four-instruction `msr…msr…isb;eret`
/// sequence so any future kernel transitioning to user mode for
/// the first time can call one function instead of re-coding the
/// asm. The caller is responsible for having a valid EL0 mapping
/// at `user_pc` (AP[2:1]=01 for the code page, AP[2:1]=01 for the
/// stack page) — see the L1→L2→L3 split landed by B1.user.split
/// for the canonical setup.
///
/// # Safety
///
/// - Caller must be at EL1.
/// - `user_pc` must be a virtual address mapped EL0-readable +
///   EL0-executable.
/// - `user_sp` must be a 16-byte-aligned virtual address mapped
///   EL0-readable + EL0-writable; AAPCS requires alignment.
/// - There is no way back — any return from `user_pc` to EL1 must
///   go through a trap (SVC, abort, IRQ).
#[cfg(target_arch = "aarch64")]
#[inline(never)]
pub unsafe fn eret_to_el0(user_pc: u64, user_sp: u64) -> ! {
    unsafe {
        core::arch::asm!(
            "msr SP_EL0, {sp}",
            "msr ELR_EL1, {pc}",
            "msr SPSR_EL1, {spsr}",
            "isb",
            "eret",
            sp = in(reg) user_sp,
            pc = in(reg) user_pc,
            spsr = in(reg) SPSR_EL0T_DAIF_MASKED,
            options(noreturn, nomem, nostack),
        )
    }
}

/// Host-build stub mirroring [`eret_to_el0`].
#[cfg(not(target_arch = "aarch64"))]
pub unsafe fn eret_to_el0(_user_pc: u64, _user_sp: u64) -> ! {
    unreachable!("eret_to_el0() is aarch64-only")
}
