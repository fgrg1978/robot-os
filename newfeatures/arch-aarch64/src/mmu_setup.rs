//! Identity-map MMU enable for early aarch64 boot.
//!
//! Lifted out of `crates/aarch64-hello/src/main.rs` so the kernel
//! and any future bare-metal binary linking against `arch-aarch64`
//! can share the same VMSAv8-64 setup instead of reimplementing it.
//!
//! The layout is the L1 → L2 → L3 split that B1.user.split shipped
//! in `aarch64-hello`:
//!
//! ```text
//!   L1[0] = 1 GiB block @ 0x00000000, Device-nGnRE
//!           — covers GIC + UART + flash, kernel-only
//!   L1[1] = table → L2 (covers DRAM base .. base + 1 GiB)
//!     L2[0]      = table → L3 (covers DRAM base .. base + 2 MiB)
//!       L3[i]     = 4 KiB page; AP=00 + UXN by default,
//!                   AP=01 (+ UXN/PXN for stack) on user pages
//!     L2[1..512] = 2 MiB blocks, kernel-only Normal WB IS
//! ```
//!
//! Two `Option<u64>` knobs in [`IdentityMapConfig`] let the caller
//! flag the *physical* addresses of one user-code page and one
//! user-stack page (typically one symbol from a `.user_text`
//! section and one from `.user_bss`). Those L3 entries get
//! AP=01; everything else stays kernel-only.

#[cfg(target_arch = "aarch64")]
use crate::sysregs;

/// 4 KiB-aligned, 512-entry translation table. Callers allocate
/// these as `static mut` (one each for L1/L2/L3) and pass mutable
/// references into [`enable_identity_map`].
#[repr(C, align(4096))]
pub struct PageTable(pub [u64; 512]);

impl PageTable {
    /// Zero-initialised table, suitable as a `static mut` initialiser.
    pub const fn zero() -> Self {
        PageTable([0; 512])
    }
}

// ── Bit-encoding constants (Arm ARM §D8.3) ───────────────────────

/// Common low bits for L1/L2 block descriptors: valid + block + AF.
const BLOCK_BASE: u64 = 1 | (1 << 10);
/// L3 page descriptor: valid + page (bit 1 = 1 at L3 means "leaf",
/// the opposite of its meaning at L1/L2) + AF.
const PAGE_BASE: u64 = 0b11 | (1 << 10);
/// Non-leaf table descriptor.
const TABLE_DESC: u64 = 0b11;
/// `AttrIdx` field, bits [5:2].
const fn attr_idx(i: u64) -> u64 {
    i << 2
}
/// `SH = 0b11` (Inner Shareable) in bits [9:8].
const SH_INNER: u64 = 0b11 << 8;
/// `AP[1] = 1` → EL0 + EL1 R/W (bit 6).
const AP_EL0_RW: u64 = 0b01 << 6;
/// `UXN` (bit 54) — Unprivileged eXecute-Never.
const UXN: u64 = 1 << 54;
/// `PXN` (bit 53) — Privileged eXecute-Never.
const PXN: u64 = 1 << 53;

const PAGE_SHIFT: u64 = 12;
const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
const L2_BLOCK_SHIFT: u64 = 21;
const L2_BLOCK_SIZE: u64 = 1 << L2_BLOCK_SHIFT;
const L3_INDEX_MASK: u64 = 0x1FF;

/// `MAIR_EL1`: AttrIdx 0 = Device-nGnRE (0x04),
/// AttrIdx 1 = Normal WB Inner+Outer non-transient RW-Allocate (0xFF).
const MAIR_VALUE: u64 = (0xFF << 8) | 0x04;

/// `TCR_EL1` for a 39-bit input range over TTBR0 only.
const TCR_VALUE: u64 = 25                  // T0SZ → walk starts at L1
    | (0b01 << 8)                          // IRGN0  Normal WB inner
    | (0b01 << 10)                         // ORGN0  Normal WB outer
    | (0b11 << 12)                         // SH0    Inner shareable
    | (0b00 << 14)                         // TG0    4 KiB granule
    | (1 << 23);                           // EPD1   disable TTBR1 walks

/// L3 table index for a PA inside the first 2 MiB above `base_pa`.
fn l3_index(pa_offset: u64) -> usize {
    ((pa_offset >> PAGE_SHIFT) & L3_INDEX_MASK) as usize
}

/// Caller-provided configuration for [`enable_identity_map`].
///
/// `l1`/`l2`/`l3` are kept as raw pointers (not `&mut`) because
/// they're typically `static mut` page tables shared across the
/// boot path — taking a `&mut` of a `static mut` at module level
/// would require fragile lifetimes and an exclusive borrow that
/// only holds during boot. The function writes through the
/// pointers exactly once each, before MMU enable.
pub struct IdentityMapConfig {
    pub l1: *mut PageTable,
    pub l2: *mut PageTable,
    pub l3: *mut PageTable,
    /// Base physical address of DRAM. The 1 GiB starting at this
    /// PA is mapped Normal WB through L1[1] → L2 → L3.
    pub base_pa: u64,
    /// If `Some(pa)`, the L3 entry covering `pa` gets AP=01 (EL0
    /// + EL1 RW + executable). Must lie in `[base_pa, base_pa + 2 MiB)`.
    pub user_code_pa: Option<u64>,
    /// If `Some(pa)`, the L3 entry covering `pa` gets AP=01 +
    /// UXN + PXN (EL0 + EL1 RW, no-exec — for stacks). Must lie
    /// in `[base_pa, base_pa + 2 MiB)`.
    pub user_stack_pa: Option<u64>,
}

/// Program the page tables, sysregs, and flip `SCTLR.M | C | I`.
///
/// After this returns, the CPU is running with stage-1 translation
/// on, I-cache + D-cache enabled, FP/SIMD trap cleared, and the
/// `IdentityMapConfig` mapping active.
///
/// # Safety
///
/// - Caller must be at EL1.
/// - The three `PageTable` pointers must each be exclusive and
///   live for the lifetime of the program.
/// - `base_pa` should match where the binary is loaded (typically
///   0x40000000 on QEMU virt / cortex-a72).
/// - `user_code_pa` / `user_stack_pa` (when `Some`) must be in
///   the first 2 MiB above `base_pa` — otherwise they fall outside
///   the L3 window and are silently ignored.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
pub unsafe fn enable_identity_map(cfg: IdentityMapConfig) {
    unsafe {
        // CPACR_EL1.FPEN = 0b11 → allow FP/SIMD at EL0+EL1. The
        // 512-iteration L3 loop below auto-vectorises into NEON
        // stores under hard-float aarch64 targets, and without
        // FPEN the first NEON op traps with EC=0x07.
        core::arch::asm!(
            "msr CPACR_EL1, {0}",
            "isb",
            in(reg) (0b11u64 << 20),
            options(nomem, nostack, preserves_flags),
        );

        let user_code_idx = cfg
            .user_code_pa
            .map(|pa| l3_index(pa.wrapping_sub(cfg.base_pa)));
        let user_stack_idx = cfg
            .user_stack_pa
            .map(|pa| l3_index(pa.wrapping_sub(cfg.base_pa)));

        // L3: 4 KiB pages for the first 2 MiB above base_pa.
        let l3 = &mut *cfg.l3;
        for i in 0..512usize {
            let pa = cfg.base_pa + (i as u64) * PAGE_SIZE;
            let mut entry = pa | PAGE_BASE | attr_idx(1) | SH_INNER;
            if Some(i) == user_code_idx {
                entry |= AP_EL0_RW; // EL0 may RWX
            } else if Some(i) == user_stack_idx {
                entry |= AP_EL0_RW | UXN | PXN; // EL0 RW, no exec
            } else {
                entry |= UXN; // kernel-only
            }
            l3.0[i] = entry;
        }

        // L2: L2[0] → L3, L2[1..512] = 2 MiB kernel-only blocks.
        let l2 = &mut *cfg.l2;
        l2.0[0] = (cfg.l3 as u64) | TABLE_DESC;
        for i in 1..512usize {
            let pa = cfg.base_pa + (i as u64) * L2_BLOCK_SIZE;
            l2.0[i] = pa | BLOCK_BASE | attr_idx(1) | SH_INNER;
        }

        // L1: [0] Device GiB @ 0, [1] table → L2.
        let l1 = &mut *cfg.l1;
        l1.0[0] = 0x0000_0000 | BLOCK_BASE | attr_idx(0);
        l1.0[1] = (cfg.l2 as u64) | TABLE_DESC;

        sysregs::write_mair_el1(MAIR_VALUE);
        sysregs::write_tcr_el1(TCR_VALUE);
        sysregs::write_ttbr0_el1(cfg.l1 as usize, 0);

        core::arch::asm!("isb", options(nomem, nostack));
        sysregs::tlbi_vmalle1is();

        // Enable MMU + caches in one write. Both attr indices set
        // correct memory types so D-cache enable doesn't cache MMIO.
        let sctlr = sysregs::read_sctlr_el1()
            | sysregs::SCTLR_EL1_M
            | sysregs::SCTLR_EL1_C
            | sysregs::SCTLR_EL1_I;
        sysregs::write_sctlr_el1(sctlr);
    }
}

/// Host-build stub.
#[cfg(not(target_arch = "aarch64"))]
pub unsafe fn enable_identity_map(_cfg: IdentityMapConfig) {
    unreachable!("enable_identity_map() is aarch64-only")
}
