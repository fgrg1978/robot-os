//! Legacy 8259 PIC — remap + disable.
//!
//! The PC platform ships with a pair of cascaded 8259 PICs that
//! the BIOS leaves enabled by default. Before the kernel takes
//! over with the APIC (`apic` + `ioapic` modules) it must:
//!
//!   1. **Remap** the PIC vectors from the legacy 0x08–0x0F /
//!      0x70–0x77 (which collide with CPU exceptions) up to
//!      0x20–0x2F (the standard "after exceptions" base).
//!   2. **Mask all 16 IRQs** so the PIC doesn't deliver
//!      anything — the I/O APIC will take over.
//!   3. (Optional) **Disable** entirely by writing 0xFF to both
//!      data ports.
//!
//! Without this, a spurious IRQ from a still-enabled PIC line
//! delivers as a CPU exception with garbage `error_code` and the
//! kernel double-faults.
//!
//! Ports:
//!   - **PIC1**: command 0x20, data 0x21 (master, IRQs 0–7)
//!   - **PIC2**: command 0xA0, data 0xA1 (slave, IRQs 8–15)

#![allow(dead_code)]

/// Write a byte to an I/O port. Kept local because port-mapped
/// I/O is only relevant to a handful of legacy paths (here +
/// the SeaBIOS shutdown port in `api_impl::Boot::shutdown`).
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nostack, preserves_flags),
        );
    }
}

/// Port addresses + ICW1 init command.
const PIC1_CMD:  u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD:  u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
const ICW1_INIT: u8  = 0x10 | 0x01; // bit 0 = "ICW4 needed"
const ICW4_8086: u8  = 0x01;        // 8086 mode (vs MCS-80)
/// Standard remap target — vectors 0x20..=0x2F. CPU exceptions
/// occupy 0x00..=0x1F so this is the next aligned slot.
pub const REMAP_BASE_MASTER: u8 = 0x20;
pub const REMAP_BASE_SLAVE:  u8 = 0x28;

/// Remap both PICs to `REMAP_BASE_MASTER` / `REMAP_BASE_SLAVE`,
/// then mask all 16 IRQs. Standard "we're using APIC now"
/// teardown — call once at boot before enabling the I/O APIC.
///
/// # Safety
/// Touches well-known IO ports; safe to call from CPL=0 during
/// boot. Not safe to call concurrently from multiple harts.
#[cfg(target_arch = "x86_64")]
pub unsafe fn remap_and_disable() {
    unsafe {
        // ICW1: start initialisation, expect ICW4.
        outb(PIC1_CMD, ICW1_INIT);
        io_wait();
        outb(PIC2_CMD, ICW1_INIT);
        io_wait();
        // ICW2: vector base offsets.
        outb(PIC1_DATA, REMAP_BASE_MASTER);
        io_wait();
        outb(PIC2_DATA, REMAP_BASE_SLAVE);
        io_wait();
        // ICW3: cascading topology — slave on PIC1's IRQ2 line.
        outb(PIC1_DATA, 0x04);     // master: bitmask of slave lines
        io_wait();
        outb(PIC2_DATA, 0x02);     // slave: cascade identity
        io_wait();
        // ICW4: 8086 mode (vs the obsolete 8080/8085).
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();

        // Mask every IRQ on both PICs — the I/O APIC takes over
        // and we never want the legacy PIC to deliver again.
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

/// Acknowledge an IRQ at the legacy PIC. The kernel only needs
/// this if it accidentally takes a PIC IRQ before [`remap_and_disable`]
/// completes — once that's run, every IRQ comes through the
/// LAPIC and uses `apic::eoi` instead.
#[cfg(target_arch = "x86_64")]
pub unsafe fn legacy_eoi(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(PIC2_CMD, 0x20);
        }
        outb(PIC1_CMD, 0x20);
    }
}

/// I/O delay — historically writing to port 0x80 (unused on the
/// PC) gives the slow PICs a chance to latch the previous write.
/// Modern emulators don't need it, but it's cheap insurance and
/// the canonical sequence everyone publishes.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn io_wait() {
    unsafe { outb(0x80, 0); }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn remap_and_disable() {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn legacy_eoi(_irq: u8) {}
