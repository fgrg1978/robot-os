/// PLIC (Platform-Level Interrupt Controller) driver.
///
/// Manages external device interrupts on RISC-V.
/// Base address: 0x0c00_0000 (QEMU virt machine).
///
/// Ported from kernel/core/irq.c (PLIC parts) + kernel/include/irq.h

// ---- PLIC register layout ----
// Same base address on QEMU virt and VisionFive 2 / JH7110 (both at 0x0C00_0000).

use crate::platform::hw::PLIC_BASE;

/// Priority register for IRQ `n` (0-127). Write priority 0-7.
#[inline(always)]
fn priority_addr(irq: u32) -> usize {
    PLIC_BASE + (irq as usize) * 4
}

/// Enable register base for a hart's S-mode context.
/// QEMU virt: S-mode context for hart N = context 2*N + 1.
#[inline(always)]
fn enable_addr(hart: u32, irq: u32) -> usize {
    let context = hart as usize * 2 + 1;
    PLIC_BASE + 0x2000 + context * 0x80 + (irq as usize / 32) * 4
}

/// Threshold register for a hart's S-mode context.
#[inline(always)]
fn threshold_addr(hart: u32) -> usize {
    let context = hart as usize * 2 + 1;
    PLIC_BASE + 0x20_0000 + context * 0x1000
}

/// Claim/complete register for a hart's S-mode context.
#[inline(always)]
fn claim_addr(hart: u32) -> usize {
    let context = hart as usize * 2 + 1;
    PLIC_BASE + 0x20_0000 + context * 0x1000 + 4
}

// ---- MMIO helpers (same volatile pattern as UART) ----

#[inline(always)]
fn mmio_read32(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

#[inline(always)]
fn mmio_write32(addr: usize, val: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

// ---- Public API ----

/// Maximum number of interrupt sources.
/// K1 (SpacemiT) supports 256 sources; QEMU/VF2 support 128.
#[cfg(feature = "k1")]
pub const MAX_IRQS: u32 = 256;
#[cfg(not(feature = "k1"))]
pub const MAX_IRQS: u32 = 128;

/// Initialize the PLIC: set all priorities to 1, threshold to 0.
pub fn init(hart: u32) {
    // Set priority for all interrupts to 1 (default)
    for irq in 1..MAX_IRQS {
        set_priority(irq, 1);
    }
    // Accept all priorities (threshold = 0)
    mmio_write32(threshold_addr(hart), 0);
}

/// Set the priority (0-7) for an interrupt source.
pub fn set_priority(irq: u32, priority: u32) {
    if irq == 0 || irq >= MAX_IRQS { return; }
    mmio_write32(priority_addr(irq), priority & 0x7);
}

/// Enable a specific IRQ for a hart.
pub fn enable_irq(hart: u32, irq: u32) {
    if irq == 0 || irq >= MAX_IRQS { return; }
    let addr = enable_addr(hart, irq);
    let bit = 1u32 << (irq % 32);
    let current = mmio_read32(addr);
    mmio_write32(addr, current | bit);
}

/// Disable a specific IRQ for a hart.
pub fn disable_irq(hart: u32, irq: u32) {
    if irq == 0 || irq >= MAX_IRQS { return; }
    let addr = enable_addr(hart, irq);
    let bit = 1u32 << (irq % 32);
    let current = mmio_read32(addr);
    mmio_write32(addr, current & !bit);
}

/// Claim the highest-priority pending interrupt. Returns 0 if none.
pub fn claim(hart: u32) -> u32 {
    mmio_read32(claim_addr(hart))
}

/// Signal completion of interrupt handling.
///
/// # SMP safety (CVE-2026-23287 / Linux 7.0 irqchip fix)
///
/// Per PLIC spec: "If the completion ID does not match an interrupt source
/// that is currently enabled for the target, the completion is silently
/// ignored." On SMP, if another hart disables this IRQ between the `claim`
/// and this `complete`, the completion is dropped and the IRQ freezes
/// permanently. Fix: read the hardware enable register and only complete
/// when the bit is still set.
pub fn complete(hart: u32, irq: u32) {
    if irq == 0 || irq >= MAX_IRQS { return; }
    let addr = enable_addr(hart, irq);
    let bit  = 1u32 << (irq % 32);
    // Re-read enable register from hardware (not a cached software flag).
    if mmio_read32(addr) & bit != 0 {
        mmio_write32(claim_addr(hart), irq);
    }
}
