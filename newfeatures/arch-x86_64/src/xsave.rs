//! x87 + SSE state save / restore via `fxsave64` / `fxrstor64`.
//!
//! Closes the TODO from B2.target.spec: any kernel built against
//! `targets/x86_64-phanes-kernel.json` (the hard-float target
//! with `+sse2 -soft-float`) MUST save XMM state across context
//! switches, otherwise switching between two SSE-using tasks
//! corrupts whatever was in `xmm0..xmm15` when the other task
//! was preempted.
//!
//! Scope of this module:
//!   - 512-byte FXSAVE area (`FxsaveArea`) with the right repr +
//!     16-byte alignment.
//!   - `save_fp_state` / `restore_fp_state` thin wrappers around
//!     `fxsave64` / `fxrstor64`.
//!   - One-shot `fp_init` that issues `fninit` + sets MXCSR to a
//!     safe default — needed once per CPU before the first task
//!     touches FP.
//!
//! Out of scope (follow-up B2.ctx.xsave.full):
//!   - YMM / ZMM / AVX-512 mask regs — those need the wider
//!     `xsave64` instruction + XCR0 setup via XSETBV. Same shape,
//!     larger save area (~3 KiB depending on enabled state).
//!   - Lazy FPU save (CR0.TS trap on first FP use) — current
//!     impl is unconditional save/restore, which is the right
//!     default on modern HW where most tasks touch SSE.

#![allow(dead_code)]

use core::mem::{size_of, align_of};

/// FXSAVE area — fixed 512 bytes, 16-byte aligned per Intel SDM
/// §10.5.1. Layout:
///   [0]     FCW, FSW, FTW, FOP, ...      (legacy x87 state)
///   [32]    MXCSR + MXCSR_MASK           (SSE control + mask)
///   [160]   XMM0..XMM15  (16 × 16 B)     (SSE register file)
///   [416]   reserved / available
#[repr(C, align(16))]
pub struct FxsaveArea(pub [u8; 512]);

impl FxsaveArea {
    /// Zero-initialised area — safe as a `static mut` or per-task
    /// initialiser; first `fxsave` overwrites it.
    pub const fn zero() -> Self {
        FxsaveArea([0u8; 512])
    }
}

/// Save the current FPU + SSE state into `area`.
///
/// # Safety
/// `area` must be writable + 16-byte aligned (repr(align(16))
/// guarantees the second; the caller chooses the storage).
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn save_fp_state(area: &mut FxsaveArea) {
    unsafe {
        core::arch::asm!(
            "fxsave64 [{ptr}]",
            ptr = in(reg) area.0.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
}

/// Restore FPU + SSE state from `area` previously written by
/// [`save_fp_state`].
///
/// # Safety
/// `area` must contain a valid fxsave snapshot (zero-initialised
/// is valid — gives a clean FPU). 16-byte alignment required.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn restore_fp_state(area: &FxsaveArea) {
    unsafe {
        core::arch::asm!(
            "fxrstor64 [{ptr}]",
            ptr = in(reg) area.0.as_ptr(),
            options(nostack, preserves_flags, readonly),
        );
    }
}

/// One-shot per-CPU FPU init — `fninit` + sane MXCSR. Call once
/// during boot before any task does FP. Does NOT enable AVX /
/// YMM via XCR0; that's the B2.ctx.xsave.full extension.
///
/// # Safety
/// Caller must be at CPL=0 and FPU must be present (always true
/// on x86_64 — the spec mandates SSE2 + FPU as the baseline).
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn fp_init() {
    // MXCSR default: all exceptions masked, round to nearest,
    // no flush-to-zero. Matches what the SysV AMD64 ABI assumes.
    const MXCSR_DEFAULT: u32 = 0x1F80;
    unsafe {
        core::arch::asm!(
            "fninit",
            "ldmxcsr [{p}]",
            p = in(reg) &MXCSR_DEFAULT,
            options(nostack, preserves_flags),
        );
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn save_fp_state(_area: &mut FxsaveArea) {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn restore_fp_state(_area: &FxsaveArea) {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn fp_init() {}

// ── Compile-time sanity ─────────────────────────────────────

const _: () = {
    if size_of::<FxsaveArea>() != 512 {
        panic!("FxsaveArea must be exactly 512 bytes");
    }
    if align_of::<FxsaveArea>() != 16 {
        panic!("FxsaveArea must be 16-byte aligned for fxsave64");
    }
};
