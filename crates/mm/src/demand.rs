//! Demand paging (AQ10).
//!
//! Lets a user task reserve a virtual address range without consuming any
//! physical memory up front.  A physical page is allocated and zeroed on
//! the first access to each page (page fault → `handle_demand_fault()`).
//!
//! Encoding: a demand-mapped PTE has `VALID = 0` (so the MMU traps on
//! access) but the OS-defined `DEMAND` bit (bit 9, RSW field) is set.
//! The desired final flags (USER/READ/WRITE/EXEC) are stored in the same
//! PTE word so the fault handler can recover them.
//!
//! Interaction with the existing MM:
//!   - `map_demand()` is called for each page of the reservation.  It uses
//!     the same `walk()` helper as `vmm::map()`, so intermediate page
//!     tables are allocated lazily when the first demand PTE is written.
//!   - On fault, `handle_demand_fault()` calls `pmm::alloc_page()` (which
//!     already zeroes the page) and rewrites the PTE with the stored flags
//!     + VALID + DIRTY.
//!   - Free path is unchanged: `vmm::unmap()` will clear demand PTEs the
//!     same way it clears regular ones (no physical page to free until
//!     the fault has materialized it).
//!
//! `no-mmu` behaviour:
//!   - This module is not compiled on `esp32c3` (no MMU, no page tables).
//!   - On the RV64 `no-mmu` feature the module still compiles (same target
//!     as the normal RV64 kernel, vmm is available), but the kernel's
//!     page-fault dispatch is gated with `#[cfg(not(feature = "no-mmu"))]`
//!     so `handle_demand_fault` is never reached; `map_demand()` remains
//!     functional for unit-style use but should not be called.

use robot_os_arch::csr;
use robot_os_arch::mmu::{self, Pte, PteFlags, PAGE_SIZE};
use robot_os_common::error::{KResult, KernelError};
use crate::{pmm, vmm};

/// Maximum total bytes a single `sys_alloc_demand()` call may reserve.
///
/// 64 MiB = 16384 pages.  Protects against pathological sizes that would
/// consume a large chunk of the kernel's page-table budget even without
/// materializing physical memory.
pub const MAX_DEMAND_ALLOC_BYTES: usize = 64 * 1024 * 1024;

/// Default flags for user demand pages (read-write, user-accessible).
///
/// ACCESSED + DIRTY are set when the page materializes (see
/// `handle_demand_fault`) — before that the PTE is invalid anyway.
fn default_user_flags() -> PteFlags {
    PteFlags::USER_RW | PteFlags::ACCESSED
}

/// Reserve a single 4 KiB virtual page without allocating a physical page.
///
/// Writes a marker PTE: `VALID = 0` but `DEMAND` and the provided `flags`
/// are stored in-line so `handle_demand_fault()` can reconstitute them.
///
/// Errors:
///   - `NotAligned` if `vaddr` is not a 4 KiB multiple.
///   - `AlreadyMapped` if a mapping already exists there (either real or
///     demand-marked).
pub fn map_demand(pt_phys: usize, vaddr: usize, flags: PteFlags) -> KResult<()> {
    if !mmu::is_page_aligned(vaddr) {
        return Err(KernelError::NotAligned);
    }

    let pte_ptr = vmm::walk(pt_phys, vaddr, true)?;
    let old = unsafe { core::ptr::read_volatile(pte_ptr) };
    if old.is_valid() {
        return Err(KernelError::AlreadyMapped);
    }
    if old.0 & PteFlags::DEMAND.bits() != 0 {
        return Err(KernelError::AlreadyMapped);
    }

    // Store DEMAND + the desired flags.  The physical-address portion of
    // the PTE stays 0 since no page has been allocated yet.
    let marker = Pte(PteFlags::DEMAND.bits() | flags.bits());
    unsafe { core::ptr::write_volatile(pte_ptr, marker) };

    Ok(())
}

/// Reserve a contiguous virtual range `[base, base + pages*PAGE_SIZE)`
/// with demand paging.  All pages get `USER_RW` flags.
///
/// On error any partial mapping is left in place; caller is responsible
/// for tearing it down.
pub fn map_demand_range(pt_phys: usize, base: usize, pages: usize) -> KResult<()> {
    let flags = default_user_flags();
    for i in 0..pages {
        map_demand(pt_phys, base + i * PAGE_SIZE, flags)?;
    }
    Ok(())
}

/// Handle a page fault on a demand-mapped page.
///
/// Returns `Ok(())` when the fault was resolved; `NotMapped` / `InvalidArg`
/// when the fault doesn't concern a demand-marked PTE and the caller
/// should continue its normal fault-handling path.
pub fn handle_demand_fault(pt: usize, fault_addr: usize) -> KResult<()> {
    let aligned_addr = fault_addr & !(PAGE_SIZE - 1);
    let pte_ptr = vmm::walk(pt, aligned_addr, false)?;
    let pte = unsafe { core::ptr::read_volatile(pte_ptr) };

    // Must be invalid and have the DEMAND flag set.
    if pte.is_valid() {
        return Err(KernelError::AlreadyMapped);
    }
    if pte.0 & PteFlags::DEMAND.bits() == 0 {
        return Err(KernelError::NotMapped);
    }

    // Recover stored flags (everything minus the DEMAND marker).
    let stored_flags = PteFlags::from_bits_truncate(pte.0 & !PteFlags::DEMAND.bits());

    // Allocate a physical page (`alloc_page` zero-fills).
    let new_page = pmm::alloc_page()?;
    let new_phys = new_page.as_usize();

    // Defense-in-depth: zero again explicitly.  Cheap (PAGE_SIZE) and
    // guarantees the user never sees stale kernel data.
    unsafe {
        core::ptr::write_bytes(new_phys as *mut u8, 0, PAGE_SIZE);
    }

    // Install the real PTE: VALID + original flags (+ DIRTY because the
    // page is freshly written by the zeroing above; avoids a second fault
    // on implementations with software-managed A/D bits).
    let final_flags = stored_flags | PteFlags::VALID | PteFlags::DIRTY;
    let new_pte = Pte::new(new_phys, final_flags);
    unsafe { core::ptr::write_volatile(pte_ptr, new_pte) };

    csr::sfence_vma_addr(aligned_addr);

    Ok(())
}
