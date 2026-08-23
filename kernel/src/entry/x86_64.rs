//! x86_64 entry — kernel-side TrapFrame layout that the IDT
//! handler stubs (in `arch_x86_64::idt`) push registers into.
//!
//! Same status as the aarch64 entry: scaffolding only.  Real
//! IDT vector wrappers + invocation of this module happen in
//! S3.b7.next once a kernel builds for x86_64.

#![cfg(target_arch = "x86_64")]

use super::{TrapClass, TrapContext};

/// x86_64-native TrapFrame.  Layout follows the standard SystemV
/// register order so the asm push sequence reads naturally:
/// general-purpose 15 regs (no RAX — that's the syscall return,
/// set separately) → RAX → vector → error_code → RIP → CS → RFLAGS →
/// RSP → SS.  Last 5 are the CPU-pushed exception frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TrapFrame {
    // GPRs in SystemV save order (rbx, rcx, ..., r15). rax stored
    // separately so syscall return can be written without
    // disturbing the rest.
    pub rbx: u64, pub rcx: u64, pub rdx: u64, pub rsi: u64,
    pub rdi: u64, pub rbp: u64, pub r8:  u64, pub r9:  u64,
    pub r10: u64, pub r11: u64, pub r12: u64, pub r13: u64,
    pub r14: u64, pub r15: u64, pub rax: u64,

    /// Vector index (0..=255).  Filled by the per-vector stub
    /// asm; used to derive [`TrapClass`].
    pub vector: u64,
    /// CPU-pushed error code, or 0 for vectors that don't push one.
    pub error_code: u64,

    /// CPU-pushed exception frame (Intel SDM Vol 3A §6.12).
    pub rip:    u64,
    pub cs:     u64,
    pub rflags: u64,
    pub rsp:    u64,
    pub ss:     u64,

    /// CR2 — faulting virtual address (page faults only). Read
    /// from CR2 by the stub before pushing.
    pub cr2: u64,
}

/// Vector ranges (Intel SDM Vol 3A §6.15).
pub const VEC_PAGE_FAULT: u64 = 14;
pub const VEC_SYSCALL:    u64 = 0x80; // legacy int 0x80 path
/// IRQs land in [32..=255]; APIC IRQs we route start at 32.
pub const VEC_IRQ_BASE:   u64 = 32;

/// CS RPL bits — lower 2 bits of CS.  RPL=3 ⇒ came from user.
const CS_RPL_MASK: u64 = 0x3;

/// Linux x86_64 syscall ABI: rax = number, args in rdi rsi rdx r10 r8 r9.
const MAX_SYSCALL_ARGS: usize = 6;

impl TrapContext for TrapFrame {
    #[inline]
    fn cause(&self) -> usize { self.vector as usize }

    fn class(&self) -> TrapClass {
        match self.vector {
            VEC_PAGE_FAULT => TrapClass::PageFault,
            VEC_SYSCALL    => TrapClass::Syscall,
            v if v >= VEC_IRQ_BASE => TrapClass::Interrupt,
            _ => TrapClass::OtherException,
        }
    }

    /// On x86_64 IRQ entries the vector IS the IRQ number
    /// (subtract `VEC_IRQ_BASE` to get the APIC/legacy IRQ).
    #[inline]
    fn irq_number(&self) -> usize {
        (self.vector.saturating_sub(VEC_IRQ_BASE)) as usize
    }

    #[inline]
    fn fault_addr(&self) -> usize { self.cr2 as usize }

    #[inline]
    fn pc(&self) -> usize { self.rip as usize }

    #[inline]
    fn set_pc(&mut self, pc: usize) { self.rip = pc as _; }

    #[inline]
    fn user_sp(&self) -> usize { self.rsp as usize }

    #[inline]
    fn came_from_user(&self) -> bool {
        (self.cs & CS_RPL_MASK) == CS_RPL_MASK
    }

    #[inline]
    fn syscall_number(&self) -> usize { self.rax as usize }

    #[inline]
    fn syscall_arg(&self, n: usize) -> usize {
        debug_assert!(n < MAX_SYSCALL_ARGS);
        match n {
            0 => self.rdi as usize,
            1 => self.rsi as usize,
            2 => self.rdx as usize,
            3 => self.r10 as usize, // syscall ABI uses r10, not rcx
            4 => self.r8  as usize,
            5 => self.r9  as usize,
            _ => 0,
        }
    }

    #[inline]
    fn set_syscall_return(&mut self, v: usize) {
        self.rax = v as _;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn x86_64_trap_entry(_frame: &mut TrapFrame) {
    // S3.b7.next routes this into the shared trap-handler core.
}
