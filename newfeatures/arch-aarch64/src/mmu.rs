//! VMSAv8-64 Stage-1 page-table entry encoding.
//!
//! The PTE format documented here is for the **4 KB granule, 4-level
//! translation** (TG=0b00, T0SZ=25 → 39-bit input range, matches the
//! RISC-V Sv39 we already support). Encoding for 16 KB / 64 KB
//! granules is identical at the bit level — only the address-field
//! widths differ — and can be added as a follow-up.
//!
//! Reference: Arm ARM §D8 (VMSAv8-64).

use bitflags::bitflags;

/// Page size in bytes (4 KiB granule).
pub const PAGE_SIZE: usize = 4096;

/// log2(PAGE_SIZE) — bits shifted off a VA / PA to get a page number.
pub const PAGE_SHIFT: usize = 12;

/// Round `addr` up to the next page boundary (inclusive of `addr`
/// when already aligned). Mirrors `arch-riscv64::mmu::page_align_up`
/// so kernel call sites can use the facade re-export unchanged.
pub const fn page_align_up(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// Round `addr` down to the start of its containing page.
pub const fn page_align_down(addr: usize) -> usize {
    addr & !(PAGE_SIZE - 1)
}

/// True iff `addr` is the start of a page.
pub const fn is_page_aligned(addr: usize) -> bool {
    addr & (PAGE_SIZE - 1) == 0
}

bitflags! {
    /// Low / upper attribute bits of a leaf PTE.
    ///
    /// `[1:0]` = type (0b11 = block/page at L3 = leaf; 0b01 = block
    /// at L1/L2 = leaf-large). For a 4 KB leaf we always emit `0b11`.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct PteAttrs: u64 {
        /// Bits [1:0] = 0b11 — valid + page descriptor at L3.
        const TYPE_PAGE      = 0b11;
        /// `AttrIndx[2:0]` — index into MAIR_EL1. We pre-program
        /// MAIR_EL1 in early boot:
        ///   - idx 0 = `0xFF` (Normal, Inner+Outer Write-Back, RW alloc)
        ///   - idx 1 = `0x04` (Device-nGnRE) for MMIO
        const ATTRIDX_NORMAL = 0b000 << 2;
        const ATTRIDX_DEVICE = 0b001 << 2;
        /// Non-secure bit (NS, EL3-relevant only — we live in
        /// secure or NS depending on boot mode; leaving 0 is safe).
        const NS             = 1 << 5;
        /// Access-permission AP[2:1]:
        ///   AP[1] = 0 → EL1-only;  = 1 → EL0+EL1
        ///   AP[2] = 0 → R/W;       = 1 → R/O
        const AP_EL1_RW      = 0b00 << 6;
        const AP_EL0_RW      = 0b01 << 6;
        const AP_EL1_RO      = 0b10 << 6;
        const AP_EL0_RO      = 0b11 << 6;
        /// Shareability SH[1:0]:
        ///   0b10 = Outer Shareable, 0b11 = Inner Shareable.
        /// Normal cacheable mappings on SMP **must** be IS for
        /// coherence with other PEs.
        const SH_INNER       = 0b11 << 8;
        /// AF — Access Flag. Pre-set to avoid an Access Fault on
        /// the first dereference of the page (saves an exception).
        const AF             = 1 << 10;
        /// nG — non-global. Set on user mappings so they don't
        /// pollute the global TLB.
        const NG             = 1 << 11;
        /// UXN — Unprivileged eXecute-Never. Set on any mapping
        /// EL0 must not execute.
        const UXN            = 1 << 54;
        /// PXN — Privileged eXecute-Never. Set on any mapping EL1
        /// must not execute (typical for data pages).
        const PXN            = 1 << 53;
    }
}

/// PPN bits in a leaf PTE: `[47:12]`.
const PPN_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// Encode a leaf PTE word for `phys` with `attrs`. Returns the
/// raw `u64` written into the page table.
#[inline]
pub const fn make_pte(phys: usize, attrs: PteAttrs) -> u64 {
    ((phys as u64) & PPN_MASK) | attrs.bits()
}
