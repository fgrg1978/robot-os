/// RISC-V CSR (Control and Status Register) access.
///
/// All functions use inline asm to read/write CSRs.
///
/// On ESP32-C3 (M-mode only), S-mode CSR functions are aliased to
/// their M-mode equivalents so that callers compile without changes.

// ============================================================
// S-mode CSR functions (default, non-ESP32-C3)
// ============================================================

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn read_satp() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) val) };
    val
}

#[cfg(not(feature = "esp32c3"))]
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

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn sfence_vma() {
    unsafe { core::arch::asm!("sfence.vma zero, zero", options(nostack)) };
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn sfence_vma_addr(vaddr: usize) {
    unsafe { core::arch::asm!("sfence.vma {}, zero", in(reg) vaddr, options(nostack)) };
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn read_sstatus() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) val) };
    val
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn write_sstatus(val: usize) {
    unsafe { core::arch::asm!("csrw sstatus, {}", in(reg) val, options(nostack)) };
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn read_stvec() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, stvec", out(reg) val) };
    val
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn write_stvec(val: usize) {
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) val, options(nostack)) };
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn read_sie() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sie", out(reg) val) };
    val
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn write_sie(val: usize) {
    unsafe { core::arch::asm!("csrw sie, {}", in(reg) val, options(nostack)) };
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn write_sscratch(val: usize) {
    unsafe { core::arch::asm!("csrw sscratch, {}", in(reg) val, options(nostack)) };
}

#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn write_scounteren(val: usize) {
    unsafe { core::arch::asm!("csrw scounteren, {}", in(reg) val, options(nostack)) };
}

/// Clear S-mode software interrupt pending bit (SIP.SSIP, bit 1).
/// Used after handling an IPI to acknowledge the interrupt.
#[cfg(not(feature = "esp32c3"))]
#[inline(always)]
pub fn clear_sip_ssip() {
    unsafe { core::arch::asm!("csrc sip, {}", in(reg) 1usize << 1, options(nostack)) };
}

/// Clear M-mode software interrupt pending bit (MIP.MSIP, bit 3).
#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn clear_sip_ssip() {
    unsafe { core::arch::asm!("csrc mip, {}", in(reg) 1usize << 3, options(nostack)) };
}

// ============================================================
// ESP32-C3: M-mode aliases (same function names, M-mode CSRs)
// ============================================================

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn read_satp() -> usize { 0 } // No MMU

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn write_satp(_val: usize) {} // No MMU

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn sfence_vma() {} // No MMU

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn sfence_vma_addr(_vaddr: usize) {} // No MMU

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn read_sstatus() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, mstatus", out(reg) val) };
    val
}

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn write_sstatus(val: usize) {
    unsafe { core::arch::asm!("csrw mstatus, {}", in(reg) val, options(nostack)) };
}

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn read_stvec() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, mtvec", out(reg) val) };
    val
}

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn write_stvec(val: usize) {
    unsafe { core::arch::asm!("csrw mtvec, {}", in(reg) val, options(nostack)) };
}

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn read_sie() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, mie", out(reg) val) };
    val
}

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn write_sie(val: usize) {
    unsafe { core::arch::asm!("csrw mie, {}", in(reg) val, options(nostack)) };
}

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn write_sscratch(val: usize) {
    unsafe { core::arch::asm!("csrw mscratch, {}", in(reg) val, options(nostack)) };
}

#[cfg(feature = "esp32c3")]
#[inline(always)]
pub fn write_scounteren(_val: usize) {} // No-op in M-mode

// ---- sstatus / mstatus bit definitions ----
// On ESP32-C3, the same constants apply to mstatus:
// - MSTATUS_MIE is bit 3 (vs SSTATUS_SIE bit 1)
// - MSTATUS_MPIE is bit 7 (vs SSTATUS_SPIE bit 5)
// - MSTATUS_MPP is bits 12:11 (vs SSTATUS_SPP bit 8)

#[cfg(not(feature = "esp32c3"))]
pub const SSTATUS_SIE: usize = 1 << 1;
#[cfg(not(feature = "esp32c3"))]
pub const SSTATUS_SPIE: usize = 1 << 5;
#[cfg(not(feature = "esp32c3"))]
pub const SSTATUS_SPP: usize = 1 << 8;

#[cfg(feature = "esp32c3")]
pub const SSTATUS_SIE: usize = 1 << 3;   // MIE in mstatus
#[cfg(feature = "esp32c3")]
pub const SSTATUS_SPIE: usize = 1 << 7;  // MPIE in mstatus
#[cfg(feature = "esp32c3")]
pub const SSTATUS_SPP: usize = 3 << 11;  // MPP in mstatus

// ---- sie / mie bit definitions ----

#[cfg(not(feature = "esp32c3"))]
pub const SIE_SSIE: usize = 1 << 1;
#[cfg(not(feature = "esp32c3"))]
pub const SIE_STIE: usize = 1 << 5;
#[cfg(not(feature = "esp32c3"))]
pub const SIE_SEIE: usize = 1 << 9;

#[cfg(feature = "esp32c3")]
pub const SIE_SSIE: usize = 1 << 3;   // MSIE in mie
#[cfg(feature = "esp32c3")]
pub const SIE_STIE: usize = 1 << 7;   // MTIE in mie
#[cfg(feature = "esp32c3")]
pub const SIE_SEIE: usize = 1 << 11;  // MEIE in mie
