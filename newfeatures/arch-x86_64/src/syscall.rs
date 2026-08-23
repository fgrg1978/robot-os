//! x86_64 `syscall` / `sysret` MSR setup — the fast user→kernel
//! entry path that skips IDT delivery.
//!
//! Mirror of how `svc` works on aarch64: a dedicated instruction
//! that the user issues to enter EL1, with a per-CPU MSR holding
//! the entry-point address. On x86_64 the MSRs are:
//!
//!   IA32_EFER   (0xC0000080) — bit 0 (SCE) enables `syscall`.
//!   IA32_STAR   (0xC0000081) — bits [47:32] = selectors loaded
//!                              into CS/SS on syscall (kernel
//!                              CS, kernel SS = CS+8). Bits
//!                              [63:48] = selectors on sysret
//!                              (user CS-16, user SS-8 — see
//!                              SDM Vol. 3 §6.8.8 for the offset
//!                              dance).
//!   IA32_LSTAR  (0xC0000082) — 64-bit RIP loaded into the CPU
//!                              on `syscall`.
//!   IA32_FMASK  (0xC0000084) — RFLAGS bits cleared on entry
//!                              (mask, not value). Typically
//!                              IF + DF + TF.
//!
//! The kernel calls [`init_syscall`] once per CPU during boot
//! with the address of its syscall-entry asm trampoline.
//!
//! Out of scope:
//!   - The trampoline itself — register layout is kernel-owned,
//!     lives in the kernel crate not here.
//!   - `compat-syscall` (32-bit IA32_CSTAR) — we never plan to
//!     execute legacy 32-bit user code.

#![allow(dead_code)]

#[cfg(target_arch = "x86_64")]
use crate::msr::{rdmsr, wrmsr};

/// MSR addresses (Intel SDM Vol. 4 Table 2-2).
pub const IA32_EFER:    u32 = 0xC000_0080;
pub const IA32_STAR:    u32 = 0xC000_0081;
pub const IA32_LSTAR:   u32 = 0xC000_0082;
pub const IA32_CSTAR:   u32 = 0xC000_0083; // unused
pub const IA32_FMASK:   u32 = 0xC000_0084;

/// `IA32_EFER` bits we care about.
pub const EFER_SCE:    u64 = 1 << 0;   // System Call Extensions
pub const EFER_LME:    u64 = 1 << 8;   // Long Mode Enable
pub const EFER_LMA:    u64 = 1 << 10;  // Long Mode Active (RO)
pub const EFER_NXE:    u64 = 1 << 11;  // NX-bit Enable

/// RFLAGS bits masked on syscall entry. IF off → interrupts
/// disabled until the kernel decides to re-enable; DF off →
/// rep-prefix string ops walk forward (SysV ABI requirement);
/// TF off → no single-step trap mid-syscall.
pub const SYSCALL_FMASK_DEFAULT: u64 = 0x0000_0200  // IF
                                     | 0x0000_0400  // DF
                                     | 0x0000_0100; // TF

/// Build the `IA32_STAR` value. On syscall the CPU loads CS from
/// bits [47:32], SS from CS+8. On sysret the CPU loads CS from
/// `(bits[63:48] + 16) | 3`, SS from `(bits[63:48] + 8) | 3` —
/// the +16/+8 dance assumes the kernel GDT places user code
/// 16 bytes *after* user data (the GDT we ship from
/// `arch-x86_64::gdt` already arranges this).
pub const fn make_star(kernel_cs: u16, user_cs_minus_16: u16) -> u64 {
    ((user_cs_minus_16 as u64) << 48) | ((kernel_cs as u64) << 32)
}

/// Set up `syscall` / `sysret` for this CPU.
///
/// `entry_rip` is the kernel virtual address of the syscall
/// trampoline. `kernel_cs` / `user_cs_minus_16` are the GDT
/// selectors — pass [`gdt::selector::KERNEL_CODE`] and
/// `gdt::selector::USER_DATA - 8` to use the canonical layout
/// (which makes sysret return into [`gdt::selector::USER_CODE`]).
///
/// # Safety
/// Must be called at CPL=0. Overwrites four MSRs; concurrent
/// calls from multiple harts on shared MSRs are safe (each MSR
/// is per-CPU) but the caller should still do this once during
/// per-CPU boot.
#[cfg(target_arch = "x86_64")]
pub unsafe fn init_syscall(entry_rip: u64, kernel_cs: u16, user_cs_minus_16: u16) {
    unsafe {
        // EFER.SCE = 1 (preserve LME / NXE if already set).
        let mut efer = rdmsr(IA32_EFER);
        efer |= EFER_SCE;
        wrmsr(IA32_EFER, efer);

        wrmsr(IA32_STAR,  make_star(kernel_cs, user_cs_minus_16));
        wrmsr(IA32_LSTAR, entry_rip);
        wrmsr(IA32_FMASK, SYSCALL_FMASK_DEFAULT);
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn init_syscall(_entry_rip: u64, _kernel_cs: u16, _user_cs_minus_16: u16) {}
