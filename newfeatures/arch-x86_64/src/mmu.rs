//! x86_64 4-level page-table entry encoding (PML4 → PDP → PD →
//! PT). PTE format is identical across the four levels; bit
//! meanings change only for the PS (page-size) bit at PD/PDP
//! when used for large/huge pages.
//!
//! Reference: Intel SDM Vol. 3A §4.5 (IA-32e paging).

use bitflags::bitflags;

/// Page size in bytes (4 KiB — the only granularity used in
/// Phase 2; 2 MiB / 1 GiB large pages are a follow-up).
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
    /// 4 KB-leaf PTE flags. Bit numbers from Intel SDM Table 4-19.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct PteFlags: u64 {
        /// Present (P).
        const PRESENT       = 1 << 0;
        /// Read/Write (R/W). 0 = read-only.
        const RW            = 1 << 1;
        /// User/Supervisor (U/S). 0 = supervisor-only.
        const USER          = 1 << 2;
        /// Page-level Write-Through (PWT).
        const WRITE_THROUGH = 1 << 3;
        /// Page-level Cache Disable (PCD).
        const CACHE_DISABLE = 1 << 4;
        /// Accessed (A). Hardware-set; pre-set to skip A-faults.
        const ACCESSED      = 1 << 5;
        /// Dirty (D). Hardware-set on write; pre-set for RW pages.
        const DIRTY         = 1 << 6;
        /// Page-Attribute-Table index bit (PAT, leaf only).
        const PAT           = 1 << 7;
        /// Global (G) — entry survives a CR3 reload. Used for
        /// kernel mappings shared across all address spaces.
        const GLOBAL        = 1 << 8;
        /// No-Execute (NX) — bit 63. Requires `IA32_EFER.NXE = 1`
        /// (set during boot init).
        const NX            = 1 << 63;
    }
}

/// PPN bits in a 4 KB leaf PTE: `[51:12]`.
const PPN_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Encode a leaf PTE word for `phys` with `flags`. Returns the
/// raw `u64` written into the page table.
#[inline]
pub const fn make_pte(phys: usize, flags: PteFlags) -> u64 {
    ((phys as u64) & PPN_MASK) | flags.bits()
}
