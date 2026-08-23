/// RISC-V CSR (Control and Status Register) access.
///
/// All functions use inline asm to read/write CSRs.

// ============================================================
// S-mode CSR functions
// ============================================================

#[inline(always)]
pub fn read_satp() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) val) };
    val
}

#[inline(always)]
pub fn write_satp(val: usize) {
    unsafe {
        core::arch::asm!(
            "csrw satp, {}",
            "sfence.vma zero, zero",
            in(reg) val,
            options(nostack)
        );
    }
}

#[inline(always)]
pub fn sfence_vma() {
    unsafe { core::arch::asm!("sfence.vma zero, zero", options(nostack)) };
}

#[inline(always)]
pub fn sfence_vma_addr(vaddr: usize) {
    unsafe { core::arch::asm!("sfence.vma {}, zero", in(reg) vaddr, options(nostack)) };
}

#[inline(always)]
pub fn read_sstatus() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) val) };
    val
}

#[inline(always)]
pub fn write_sstatus(val: usize) {
    unsafe { core::arch::asm!("csrw sstatus, {}", in(reg) val, options(nostack)) };
}

#[inline(always)]
pub fn read_stvec() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, stvec", out(reg) val) };
    val
}

#[inline(always)]
pub fn write_stvec(val: usize) {
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) val, options(nostack)) };
}

#[inline(always)]
pub fn read_sie() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sie", out(reg) val) };
    val
}

#[inline(always)]
pub fn write_sie(val: usize) {
    unsafe { core::arch::asm!("csrw sie, {}", in(reg) val, options(nostack)) };
}

#[inline(always)]
pub fn write_sscratch(val: usize) {
    unsafe { core::arch::asm!("csrw sscratch, {}", in(reg) val, options(nostack)) };
}

#[inline(always)]
pub fn write_scounteren(val: usize) {
    unsafe { core::arch::asm!("csrw scounteren, {}", in(reg) val, options(nostack)) };
}

/// Clear S-mode software interrupt pending bit (SIP.SSIP, bit 1).
/// Used after handling an IPI to acknowledge the interrupt.
#[inline(always)]
pub fn clear_sip_ssip() {
    unsafe { core::arch::asm!("csrc sip, {}", in(reg) 1usize << 1, options(nostack)) };
}

// ---- sstatus bit definitions ----

pub const SSTATUS_SIE: usize = 1 << 1;
pub const SSTATUS_SPIE: usize = 1 << 5;
pub const SSTATUS_SPP: usize = 1 << 8;

// ---- sie bit definitions ----

pub const SIE_SSIE: usize = 1 << 1;
pub const SIE_STIE: usize = 1 << 5;
pub const SIE_SEIE: usize = 1 << 9;
