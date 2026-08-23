//! RISC-V entry — wraps the existing `arch_riscv64::trap::TrapFrame`
//! in the cross-arch [`TrapContext`] trait.
//!
//! The actual trap-entry asm (saving x0..x31 + CSRs onto the
//! kernel stack) lives in `arch_riscv64` boot/trap modules and
//! has been working since v1.0.0 — this module is purely the
//! portable lens over that data. No behaviour change today.

#![cfg(target_arch = "riscv64")]

use super::{TrapClass, TrapContext};
// Kernel only depends on the `robot_os_arch` facade, which
// re-exports the active ISA crate. On riscv64 the facade resolves
// to arch-riscv64's `trap` + `csr` modules.
use robot_os_arch::{csr, trap};
use trap::{
    TrapFrame, INTERRUPT_BIT,
    TRAP_ECALL_FROM_U, TRAP_ECALL_FROM_S,
    TRAP_INSTR_PAGE_FAULT, TRAP_LOAD_PAGE_FAULT, TRAP_STORE_PAGE_FAULT,
};

/// Position of the syscall argument registers within `regs[]`.
/// RISC-V calling convention: a0..a7 = x10..x17. Syscall number
/// in a7 (x17); first 6 args in a0..a5 (x10..x15).
const REG_A0: usize = 10;
const REG_SP: usize =  2;
const REG_A7: usize = 17;
const MAX_SYSCALL_ARGS: usize = 6;

impl TrapContext for TrapFrame {
    #[inline]
    fn cause(&self) -> usize {
        self.scause as usize
    }

    fn class(&self) -> TrapClass {
        let cause = self.scause as usize;
        if cause & INTERRUPT_BIT != 0 {
            TrapClass::Interrupt
        } else {
            match cause {
                TRAP_ECALL_FROM_U | TRAP_ECALL_FROM_S => TrapClass::Syscall,
                TRAP_INSTR_PAGE_FAULT
                | TRAP_LOAD_PAGE_FAULT
                | TRAP_STORE_PAGE_FAULT => TrapClass::PageFault,
                _ => TrapClass::OtherException,
            }
        }
    }

    #[inline]
    fn irq_number(&self) -> usize {
        (self.scause as usize) & !INTERRUPT_BIT
    }

    #[inline]
    fn fault_addr(&self) -> usize {
        self.stval as usize
    }

    #[inline]
    fn pc(&self) -> usize {
        self.sepc as usize
    }

    #[inline]
    fn set_pc(&mut self, pc: usize) {
        self.sepc = pc as _;
    }

    #[inline]
    fn user_sp(&self) -> usize {
        self.regs[REG_SP] as usize
    }

    #[inline]
    fn came_from_user(&self) -> bool {
        // SPP bit: 0 = came from U-mode, 1 = came from S-mode.
        (self.sstatus as usize) & csr::SSTATUS_SPP == 0
    }

    #[inline]
    fn syscall_number(&self) -> usize {
        self.regs[REG_A7] as usize
    }

    #[inline]
    fn syscall_arg(&self, n: usize) -> usize {
        debug_assert!(n < MAX_SYSCALL_ARGS,
            "syscall_arg index out of range (RISC-V passes 6 args in a0..a5)");
        self.regs[REG_A0 + n] as usize
    }

    #[inline]
    fn set_syscall_return(&mut self, v: usize) {
        self.regs[REG_A0] = v as _;
    }
}
