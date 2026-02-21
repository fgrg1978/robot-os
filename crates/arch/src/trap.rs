/// Trap handling: TrapFrame, cause codes, and dispatch.
///
/// The assembly `trap_entry.S` saves all registers into a TrapFrame on the
/// stack, then calls `trap_handler()` (defined here). On return the assembly
/// restores registers and executes `sret` (or `mret` on ESP32-C3).
///
/// Ported from kernel/include/trap.h + kernel/core/trap.c

// ---- Register value type (u64 on RV64, u32 on RV32) ----

#[cfg(target_pointer_width = "64")]
pub type RegVal = u64;
#[cfg(target_pointer_width = "32")]
pub type RegVal = u32;

// ---- Exception codes (scause/mcause without interrupt bit) ----

pub const TRAP_INSTR_MISALIGNED: usize = 0;
pub const TRAP_INSTR_ACCESS_FAULT: usize = 1;
pub const TRAP_ILLEGAL_INSTR: usize = 2;
pub const TRAP_BREAKPOINT: usize = 3;
pub const TRAP_LOAD_MISALIGNED: usize = 4;
pub const TRAP_LOAD_ACCESS_FAULT: usize = 5;
pub const TRAP_STORE_MISALIGNED: usize = 6;
pub const TRAP_STORE_ACCESS_FAULT: usize = 7;
pub const TRAP_ECALL_FROM_U: usize = 8;
pub const TRAP_ECALL_FROM_S: usize = 9;
pub const TRAP_ECALL_FROM_M: usize = 11;
pub const TRAP_INSTR_PAGE_FAULT: usize = 12;
pub const TRAP_LOAD_PAGE_FAULT: usize = 13;
pub const TRAP_STORE_PAGE_FAULT: usize = 15;

// ---- Interrupt codes (scause/mcause with MSB set) ----

#[cfg(target_pointer_width = "64")]
pub const INTERRUPT_BIT: usize = 1 << 63;
#[cfg(target_pointer_width = "32")]
pub const INTERRUPT_BIT: usize = 1 << 31;

// S-mode interrupt codes (default); M-mode aliases for ESP32-C3
#[cfg(not(feature = "esp32c3"))]
pub const INT_SOFTWARE_S: usize = 1;
#[cfg(not(feature = "esp32c3"))]
pub const INT_TIMER_S: usize = 5;
#[cfg(not(feature = "esp32c3"))]
pub const INT_EXTERNAL_S: usize = 9;

#[cfg(feature = "esp32c3")]
pub const INT_SOFTWARE_S: usize = 3;  // M-mode software interrupt
#[cfg(feature = "esp32c3")]
pub const INT_TIMER_S: usize = 7;     // M-mode timer interrupt
#[cfg(feature = "esp32c3")]
pub const INT_EXTERNAL_S: usize = 11; // M-mode external interrupt

/// Trap frame layout — must match `trap_entry*.S` exactly.
///
/// RV64: 32 regs × 8 + 4 CSRs × 8 = 288 bytes
/// RV32: 32 regs × 4 + 4 CSRs × 4 = 144 bytes
///
/// Field names (sepc/sstatus/scause/stval) are aliases:
/// on ESP32-C3 the assembly stores mepc/mstatus/mcause/mtval
/// at the same offsets.
#[repr(C)]
pub struct TrapFrame {
    pub regs: [RegVal; 32], // x0-x31 (x0 always 0 but space reserved)
    pub sepc: RegVal,       // sepc (S-mode) or mepc (M-mode)
    pub sstatus: RegVal,    // sstatus or mstatus
    pub scause: RegVal,     // scause or mcause
    pub stval: RegVal,      // stval or mtval
}

// Compile-time verification of struct size
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<TrapFrame>() == 288);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(core::mem::size_of::<TrapFrame>() == 144);

/// Convert a trap cause code to a human-readable string.
pub fn cause_str(cause: usize) -> &'static str {
    if cause & INTERRUPT_BIT != 0 {
        match cause & !INTERRUPT_BIT {
            #[cfg(not(feature = "esp32c3"))]
            1 => "Supervisor software interrupt",
            #[cfg(not(feature = "esp32c3"))]
            5 => "Supervisor timer interrupt",
            #[cfg(not(feature = "esp32c3"))]
            9 => "Supervisor external interrupt",
            #[cfg(feature = "esp32c3")]
            3 => "Machine software interrupt",
            #[cfg(feature = "esp32c3")]
            7 => "Machine timer interrupt",
            #[cfg(feature = "esp32c3")]
            11 => "Machine external interrupt",
            _ => "Unknown interrupt",
        }
    } else {
        match cause {
            TRAP_INSTR_MISALIGNED => "Instruction address misaligned",
            TRAP_INSTR_ACCESS_FAULT => "Instruction access fault",
            TRAP_ILLEGAL_INSTR => "Illegal instruction",
            TRAP_BREAKPOINT => "Breakpoint",
            TRAP_LOAD_MISALIGNED => "Load address misaligned",
            TRAP_LOAD_ACCESS_FAULT => "Load access fault",
            TRAP_STORE_MISALIGNED => "Store/AMO address misaligned",
            TRAP_STORE_ACCESS_FAULT => "Store/AMO access fault",
            TRAP_ECALL_FROM_U => "Environment call from U-mode",
            TRAP_ECALL_FROM_S => "Environment call from S-mode",
            TRAP_ECALL_FROM_M => "Environment call from M-mode",
            TRAP_INSTR_PAGE_FAULT => "Instruction page fault",
            TRAP_LOAD_PAGE_FAULT => "Load page fault",
            TRAP_STORE_PAGE_FAULT => "Store/AMO page fault",
            _ => "Unknown exception",
        }
    }
}
