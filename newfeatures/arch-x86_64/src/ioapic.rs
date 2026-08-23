//! I/O APIC driver — legacy IRQ routing into the LAPIC.
//!
//! Companion to `apic.rs` (local APIC). The LAPIC handles IPIs
//! + the LAPIC-timer + per-CPU sources; the I/O APIC is what
//! routes legacy + platform IRQs (PIT IRQ 0, COM1 IRQ 4, keyboard
//! IRQ 1, real-time clock IRQ 8, PCI IRQs 16+) to specific LAPIC
//! vectors on specific CPUs.
//!
//! MMIO at the address `acpi::MadtSummary::ioapic_pa` (default
//! 0xFEC00000, but always set per platform via ACPI). Access is
//! indirect:
//!
//!   - **IOREGSEL** at offset 0x00 (32-bit write): index of the
//!     register to read/write.
//!   - **IOWIN** at offset 0x10 (32-bit read/write): the
//!     selected register.
//!
//! Each IRQ has a **redirection-table** entry (REG_REDIR_BASE +
//! 2*irq, both halves), 64 bits total:
//!
//!   bits [7:0]   vector — the LAPIC vector the IRQ delivers as
//!   bits [10:8]  delivery mode (000 = Fixed)
//!   bit 11       destination mode (0 = physical APIC ID)
//!   bit 12       delivery status (RO)
//!   bit 13       polarity (0 = active high)
//!   bit 14       remote IRR (RO, level-triggered only)
//!   bit 15       trigger mode (0 = edge, 1 = level)
//!   bit 16       mask (1 = disabled)
//!   bits [63:56] destination APIC ID

#![allow(dead_code)]

#[cfg(target_arch = "x86_64")]
use core::ptr::{read_volatile, write_volatile};

/// Default I/O APIC MMIO base — the ACPI MADT type-1 entry
/// reports the real address; use that whenever it's available.
pub const IOAPIC_BASE_PA_DEFAULT: usize = 0xFEC0_0000;

/// MMIO offsets for the indirect-access registers.
const IOREGSEL_OFFSET: usize = 0x00;
const IOWIN_OFFSET:    usize = 0x10;

/// Indices into the indirect register space.
const REG_ID:         u32 = 0x00;
const REG_VERSION:    u32 = 0x01;
const REG_ARB_ID:     u32 = 0x02;
const REG_REDIR_BASE: u32 = 0x10;

// ── Redirection-entry low-half bits ─────────────────────────
pub const REDIR_VECTOR_MASK:   u32 = 0xFF;
pub const REDIR_DELIVERY_FIXED:u32 = 0b000 << 8;
pub const REDIR_DEST_PHYSICAL: u32 = 0;
pub const REDIR_TRIGGER_EDGE:  u32 = 0;
pub const REDIR_TRIGGER_LEVEL: u32 = 1 << 15;
pub const REDIR_MASK_OFF:      u32 = 0;
pub const REDIR_MASK_ON:       u32 = 1 << 16;

/// Stored base PA — set by [`init`]. Default to the architectural
/// fallback so pre-init reads/writes have *some* address rather
/// than crashing on null.
static mut IOAPIC_BASE: usize = IOAPIC_BASE_PA_DEFAULT;

/// Init the driver with a base address (typically from
/// `acpi::MadtSummary::ioapic_pa`).
///
/// # Safety
/// Single-writer: BSP at boot before any other I/O APIC call.
#[cfg(target_arch = "x86_64")]
pub unsafe fn init(base_pa: usize) {
    unsafe { IOAPIC_BASE = base_pa; }
}

/// Number of redirection entries the I/O APIC supports.
/// Reported in IOAPICVER bits [23:16] as `max_redir_entry`,
/// the count is `max + 1`. Typical: 24 on the original 82093AA;
/// modern chipsets have 32 or 120 (split into multiple chips).
#[cfg(target_arch = "x86_64")]
pub fn num_redirections() -> u8 {
    let ver = unsafe { read_indirect(REG_VERSION) };
    (((ver >> 16) & 0xFF) as u8).wrapping_add(1)
}

/// Set the redirection entry for `irq` (the I/O APIC pin number,
/// NOT the same as the LAPIC vector). `vector` is the LAPIC
/// vector the IRQ delivers as; `dest_apic_id` is the CPU to
/// route it to.
///
/// `trigger_level` = `true` for level-triggered (PCI), `false`
/// for edge (legacy ISA + most platform IRQs).
#[cfg(target_arch = "x86_64")]
pub unsafe fn set_redirection(
    irq: u8,
    vector: u8,
    dest_apic_id: u8,
    trigger_level: bool,
) {
    let low = (vector as u32)
        | REDIR_DELIVERY_FIXED
        | REDIR_DEST_PHYSICAL
        | if trigger_level { REDIR_TRIGGER_LEVEL } else { REDIR_TRIGGER_EDGE };
    let high = (dest_apic_id as u32) << 24;
    unsafe {
        write_indirect(REG_REDIR_BASE + (irq as u32) * 2, low);
        write_indirect(REG_REDIR_BASE + (irq as u32) * 2 + 1, high);
    }
}

/// Disable delivery for `irq` (sets the mask bit).
#[cfg(target_arch = "x86_64")]
pub unsafe fn mask_irq(irq: u8) {
    unsafe {
        let reg = REG_REDIR_BASE + (irq as u32) * 2;
        let low = read_indirect(reg);
        write_indirect(reg, low | REDIR_MASK_ON);
    }
}

/// Enable delivery for `irq` (clears the mask bit).
#[cfg(target_arch = "x86_64")]
pub unsafe fn unmask_irq(irq: u8) {
    unsafe {
        let reg = REG_REDIR_BASE + (irq as u32) * 2;
        let low = read_indirect(reg);
        write_indirect(reg, low & !REDIR_MASK_ON);
    }
}

// ── Indirect register helpers ────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn read_indirect(index: u32) -> u32 {
    unsafe {
        let sel = (IOAPIC_BASE + IOREGSEL_OFFSET) as *mut u32;
        let win = (IOAPIC_BASE + IOWIN_OFFSET)    as *const u32;
        write_volatile(sel, index);
        read_volatile(win)
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn write_indirect(index: u32, val: u32) {
    unsafe {
        let sel = (IOAPIC_BASE + IOREGSEL_OFFSET) as *mut u32;
        let win = (IOAPIC_BASE + IOWIN_OFFSET)    as *mut u32;
        write_volatile(sel, index);
        write_volatile(win, val);
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn init(_base_pa: usize) {}
#[cfg(not(target_arch = "x86_64"))]
pub fn num_redirections() -> u8 { 0 }
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn set_redirection(_i: u8, _v: u8, _d: u8, _l: bool) {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn mask_irq(_i: u8) {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn unmask_irq(_i: u8) {}
