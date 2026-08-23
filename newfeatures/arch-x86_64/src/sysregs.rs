//! x86_64 control-register + RFLAGS helpers.

/// RFLAGS.IF — interrupt enable bit (bit 9).
pub const RFLAGS_IF: u64 = 1 << 9;

/// Read RFLAGS via `pushfq; pop`.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn read_rflags() -> u64 {
    let v: u64;
    unsafe {
        core::arch::asm!(
            "pushfq",
            "pop {0}",
            out(reg) v,
            options(preserves_flags),
        );
    }
    v
}

/// Write RFLAGS via `push; popfq`. Note: not all bits are
/// writable from CPL > 0; only IF, OF, etc. survive.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn write_rflags(val: u64) {
    unsafe {
        core::arch::asm!(
            "push {0}",
            "popfq",
            in(reg) val,
            options(),
        );
    }
}

/// `CLI` — clear RFLAGS.IF (mask interrupts).
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn cli() {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }
}

/// `STI` — set RFLAGS.IF (enable interrupts).
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn sti() {
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
}

// ── CR3 — top-level page-table root ──

/// Write CR3 with `root_phys`. The low 12 bits are flags (PCD,
/// PWT, and on PCIDE the PCID slot); we always write a
/// page-aligned address with flags = 0.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn write_cr3(root_phys: usize) {
    unsafe {
        core::arch::asm!(
            "mov cr3, {0}",
            in(reg) root_phys,
            options(nostack, preserves_flags),
        );
    }
}

/// Read CR3.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn read_cr3() -> usize {
    let v: usize;
    unsafe {
        core::arch::asm!(
            "mov {0}, cr3",
            out(reg) v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

/// `INVLPG` — invalidate one TLB entry by virtual address. The
/// per-ASID TLB invalidation on x86 requires PCIDE + `INVPCID`,
/// which is a separate, optional feature; we fall back to a full
/// CR3 reload in `flush_tlb_asid` until that lands.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn invlpg(virt: usize) {
    unsafe {
        core::arch::asm!(
            "invlpg [{0}]",
            in(reg) virt,
            options(nostack, preserves_flags),
        );
    }
}

/// Full TLB flush via CR3 reload (write the same value back).
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn flush_tlb_full() {
    let cr3 = read_cr3();
    write_cr3(cr3);
}
