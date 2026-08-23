//! Minimal ARMv8-A system-register helpers. Only what the
//! Phase 2 arch-api trait implementations need today — full
//! coverage of EL1 sysregs is a follow-up alongside the boot
//! stub and GIC driver.

/// `DAIF` mask bits. Setting these in `DAIF` *masks* (disables)
/// the corresponding interrupt class — counter-intuitive vs
/// x86/RISC-V where the bit usually means "enabled".
#[allow(dead_code)]
pub const DAIF_D: u64 = 1 << 9; // Debug
#[allow(dead_code)]
pub const DAIF_A: u64 = 1 << 8; // SError
pub const DAIF_I: u64 = 1 << 7; // IRQ
pub const DAIF_F: u64 = 1 << 6; // FIQ

/// All interrupt classes masked. Convenience for
/// `Interrupts::disable_all`.
pub const DAIF_MASK_ALL: u64 = DAIF_D | DAIF_A | DAIF_I | DAIF_F;

/// Read `DAIF`. Returns the current mask bits in the low byte
/// (bits [9:6]).
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn read_daif() -> u64 {
    let v: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, DAIF",
            out(reg) v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

/// Write `DAIF`. Only bits [9:6] are architecturally defined.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn write_daif(val: u64) {
    unsafe {
        core::arch::asm!(
            "msr DAIF, {0}",
            in(reg) val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Disable IRQ + FIQ on the calling CPU. Equivalent to `MSR
/// DAIFSet, #0b1100`. We use the wider [`write_daif`] form so
/// callers can use the returned `DAIF` value as a token to
/// restore later.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn disable_irq_fiq() -> u64 {
    let prev = read_daif();
    write_daif(prev | DAIF_I | DAIF_F);
    prev
}

/// Unmask IRQ (clear DAIF.I). Equivalent to `MSR DAIFClr, #2`.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn enable_irq() {
    unsafe {
        core::arch::asm!(
            "msr DAIFClr, #2",
            options(nomem, nostack),
        );
    }
}

/// Install `addr` as the EL1 exception vector base.
///
/// The address MUST be 2 KiB-aligned (Arm ARM §D7); the symbol
/// itself should use `.align 11` in its asm declaration.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn set_vbar_el1(addr: u64) {
    unsafe {
        core::arch::asm!(
            "msr VBAR_EL1, {0}",
            "isb",
            in(reg) addr,
            options(nomem, nostack, preserves_flags),
        );
    }
}

// ── Generic timer (used by `Interrupts::set_timer_deadline`) ──

/// Read `CNTFRQ_EL0` — generic-timer frequency in Hz. Set by
/// firmware at boot (4 MHz on JH7110, 62.5 MHz on QEMU virt
/// cortex-a72 default).
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn read_cntfrq_el0() -> u64 {
    let v: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, CNTFRQ_EL0",
            out(reg) v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

/// Read `CNTPCT_EL0` — the physical count register. Tick units
/// are the generic-timer frequency, available in `CNTFRQ_EL0`.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn read_cntpct_el0() -> u64 {
    let v: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, CNTPCT_EL0",
            out(reg) v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

/// Write `CNTP_CVAL_EL0` — set the next physical-timer
/// comparator value. Generic-timer interrupt fires when
/// `CNTPCT_EL0 >= CNTP_CVAL_EL0` AND the timer is enabled in
/// `CNTP_CTL_EL0`.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn write_cntp_cval_el0(deadline: u64) {
    unsafe {
        core::arch::asm!(
            "msr CNTP_CVAL_EL0, {0}",
            in(reg) deadline,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Enable the physical timer + unmask its interrupt
/// (`CNTP_CTL_EL0.ENABLE = 1`, `IMASK = 0`).
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn enable_phys_timer() {
    let ctl: u64 = 1; // ENABLE bit, IMASK clear
    unsafe {
        core::arch::asm!(
            "msr CNTP_CTL_EL0, {0}",
            in(reg) ctl,
            options(nomem, nostack, preserves_flags),
        );
    }
}

// ── MMU control (consumed by Mmu::switch_pt / flush_tlb_*) ──

/// Write `TTBR0_EL1`. The low 16 bits are the ASID (when
/// `TCR_EL1.AS = 1`); the high 48 bits are the page-table root
/// physical address.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn write_ttbr0_el1(root_phys: usize, asid: u16) {
    let val = ((asid as u64) << 48) | (root_phys as u64);
    unsafe {
        core::arch::asm!(
            "msr TTBR0_EL1, {0}",
            in(reg) val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Invalidate the entire TLB at EL1 + inner-shareable DSB.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn tlbi_vmalle1is() {
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Invalidate TLB entries tagged with `asid`. Uses
/// `TLBI ASIDE1IS` which takes the ASID in bits [63:48] of the
/// operand register.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn tlbi_aside1is(asid: u16) {
    let op = (asid as u64) << 48;
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi aside1is, {0}",
            "dsb ish",
            "isb",
            in(reg) op,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Write `MAIR_EL1` (Memory Attribute Indirection Register). Each
/// 8-bit slot defines a memory type referenced by the AttrIndx
/// field of a stage-1 page-table descriptor.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn write_mair_el1(val: u64) {
    unsafe {
        core::arch::asm!(
            "msr MAIR_EL1, {0}",
            in(reg) val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Write `TCR_EL1` (Translation Control Register).
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn write_tcr_el1(val: u64) {
    unsafe {
        core::arch::asm!(
            "msr TCR_EL1, {0}",
            in(reg) val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Read `SCTLR_EL1`.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn read_sctlr_el1() -> u64 {
    let v: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, SCTLR_EL1",
            out(reg) v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

/// Write `SCTLR_EL1`. Caller is responsible for an `isb` after if
/// the change must take effect before the next instruction.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn write_sctlr_el1(val: u64) {
    unsafe {
        core::arch::asm!(
            "msr SCTLR_EL1, {0}",
            "isb",
            in(reg) val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// `SCTLR_EL1.M` (bit 0) — global MMU enable for EL1&0 stage 1.
pub const SCTLR_EL1_M: u64 = 1 << 0;
/// `SCTLR_EL1.C` (bit 2) — global data-cache enable.
pub const SCTLR_EL1_C: u64 = 1 << 2;
/// `SCTLR_EL1.I` (bit 12) — global instruction-cache enable.
pub const SCTLR_EL1_I: u64 = 1 << 12;
