//! aarch64 entry — defines the kernel-side TrapFrame layout
//! that aarch64-native trap-vector asm will populate.
//!
//! The asm (VBAR_EL1 vector table, save x0..x30 + SPSR_EL1 +
//! ELR_EL1 + FAR_EL1 + ESR_EL1) lives in `arch_aarch64::boot`
//! today as a demo trampoline; once Stage 5 wires this entry
//! module into the real kernel boot path, the same asm will
//! drop into [`trap_entry`] with `&mut TrapFrame`.

#![cfg(target_arch = "aarch64")]

use super::{TrapClass, TrapContext};

/// Register count for the AArch64 general-purpose file — x0..x30
/// (x31 is sp/zr depending on context, saved separately).
const NUM_GPR: usize = 31;

/// AArch64-native TrapFrame.  Layout chosen so the asm saves
/// registers in order: GPRs x0..x30 → ELR_EL1 (return PC) →
/// SPSR_EL1 (saved status) → SP_EL0 (user SP) → FAR_EL1 (fault
/// addr) → ESR_EL1 (cause). 38 × 8 = 304 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TrapFrame {
    pub regs:    [u64; NUM_GPR], // x0..x30
    pub elr_el1: u64,            // saved PC
    pub spsr_el1: u64,           // saved PSTATE (used for came_from_user)
    pub sp_el0:  u64,            // user-mode stack pointer
    pub far_el1: u64,            // faulting VA on page faults
    pub esr_el1: u64,            // exception syndrome (cause)
    pub vector:  u64,            // vector index (synch/irq/fiq/serr)
}

/// Vector-index encoding written by the asm wrapper. Mirrors
/// the ARM vector table slots (§D1.10).
pub const VEC_SYNC_CURRENT_EL_SP0: u64 = 0;
pub const VEC_IRQ_CURRENT_EL_SP0:  u64 = 1;
pub const VEC_SYNC_LOWER_EL:        u64 = 4; // user → kernel SVC / fault
pub const VEC_IRQ_LOWER_EL:         u64 = 5;

/// ESR_EL1.EC field — bits [31:26].  We only categorise a few.
const ESR_EC_SHIFT:       u64 = 26;
const ESR_EC_MASK:        u64 = 0x3F;
const EC_SVC64:           u64 = 0x15; // SVC from aarch64 EL0
const EC_INSTR_ABORT_EL0: u64 = 0x20;
const EC_INSTR_ABORT_EL1: u64 = 0x21;
const EC_DATA_ABORT_EL0:  u64 = 0x24;
const EC_DATA_ABORT_EL1:  u64 = 0x25;

/// SPSR_EL1.M[3:0] = 0 ⇒ came from EL0 (user).
const SPSR_M_MASK: u64 = 0xF;

/// AArch64 syscall calling convention: x8 = syscall number,
/// x0..x5 = arguments, x0 = return value.
const REG_X0: usize = 0;
const REG_X8: usize = 8;
const MAX_SYSCALL_ARGS: usize = 6;

impl TrapContext for TrapFrame {
    #[inline]
    fn cause(&self) -> usize { self.esr_el1 as usize }

    fn class(&self) -> TrapClass {
        if self.vector == VEC_IRQ_CURRENT_EL_SP0 || self.vector == VEC_IRQ_LOWER_EL {
            return TrapClass::Interrupt;
        }
        let ec = (self.esr_el1 >> ESR_EC_SHIFT) & ESR_EC_MASK;
        match ec {
            EC_SVC64 => TrapClass::Syscall,
            EC_INSTR_ABORT_EL0 | EC_INSTR_ABORT_EL1
            | EC_DATA_ABORT_EL0 | EC_DATA_ABORT_EL1 => TrapClass::PageFault,
            _ => TrapClass::OtherException,
        }
    }

    /// AArch64 IRQs are read from GICv3 ICC_IAR1_EL1, not from
    /// the trap frame. The vector asm reads IAR1 and stores it
    /// into `esr_el1` as a convenience (`esr_el1` is unused for
    /// IRQ entries on aarch64). The shared handler treats this
    /// as "IRQ number" for portability.
    #[inline]
    fn irq_number(&self) -> usize { self.esr_el1 as usize }

    #[inline]
    fn fault_addr(&self) -> usize { self.far_el1 as usize }

    #[inline]
    fn pc(&self) -> usize { self.elr_el1 as usize }

    #[inline]
    fn set_pc(&mut self, pc: usize) { self.elr_el1 = pc as _; }

    #[inline]
    fn user_sp(&self) -> usize { self.sp_el0 as usize }

    #[inline]
    fn came_from_user(&self) -> bool {
        // SPSR_EL1.M[3:0] = 0 ⇒ EL0t (user). Anything else
        // (4, 5, 8, 9, 12, 13) means EL1+ kernel context.
        (self.spsr_el1 & SPSR_M_MASK) == 0
    }

    #[inline]
    fn syscall_number(&self) -> usize { self.regs[REG_X8] as usize }

    #[inline]
    fn syscall_arg(&self, n: usize) -> usize {
        debug_assert!(n < MAX_SYSCALL_ARGS);
        self.regs[REG_X0 + n] as usize
    }

    #[inline]
    fn set_syscall_return(&mut self, v: usize) {
        self.regs[REG_X0] = v as _;
    }
}

/// Entry point the trap-vector asm jumps to once it's saved the
/// register file + system registers. Today it's a stub — the
/// real kernel hasn't been wired to call it yet; that's S3.b7.next
/// once we have a kernel build that targets aarch64.
#[unsafe(no_mangle)]
pub extern "C" fn aarch64_trap_entry(_frame: &mut TrapFrame) {
    // S3.b7.next will route this into the shared trap-handler
    // core (currently inline in kernel::main).
}
