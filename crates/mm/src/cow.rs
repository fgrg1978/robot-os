//! Copy-on-Write (COW) support for user address spaces (AQ9).
//!
//! Tracks per-physical-page reference counts so that `fork()` can share
//! pages between parent and child read-only.  A store page fault on a page
//! with the COW flag triggers `handle_cow_fault()`, which allocates a new
//! physical page, copies the data, restores the WRITE bit, and decrements
//! the refcount on the original page.
//!
//! Design notes:
//!   - Refcount table is a fixed-size array (no heap): `MAX_TRACKED_PAGES`
//!     entries.  An entry with `phys == 0` is free.
//!   - A page that isn't in the table is assumed to be single-owner
//!     (refcount = 1).  This keeps the common case fast: private user pages
//!     that never fork stay out of the table entirely.
//!   - The COW marker bit is stored in PTE bit 8 (RSW field, OS-defined) —
//!     see `PteFlags::COW` in `robot_os_arch::mmu`.
//!   - Works only under a real MMU.  On `no-mmu` builds the module still
//!     compiles, but nothing drives it: there are no page tables to mark
//!     COW and no page-fault path to resolve them.
//!
//! Known limitations:
//!   - The refcount table has a hard cap (`MAX_TRACKED_PAGES`).  When full,
//!     further `page_addref()` calls return `CapacityFull` and the caller
//!     (currently `fork_cow`) aborts the fork with an error.  For robotic
//!     workloads with <=4 concurrent forked tasks this is sufficient.
//!   - Refcount decrement is guarded by a `SpinLock`, not atomic increment
//!     on the raw entry.  Two concurrent COW fault handlers touching the
//!     *same* physical page are serialized correctly, but the PTE write
//!     that flips WRITE back on is not atomic with the refcount decrement;
//!     this is acceptable because the fault is per-PT and a given PT has
//!     only one running task inside it at a time (fork creates a new PT).

use robot_os_arch::csr;
use robot_os_arch::mmu::{Pte, PteFlags, PAGE_SIZE, PT_ENTRIES};
use robot_os_sync::SpinLock;
use robot_os_common::error::{KResult, KernelError};
use crate::addr::PhysAddr;
use crate::{pmm, vmm};

// ── Refcount table configuration ─────────────────────────────────────────────

/// Maximum number of distinct physical pages tracked for COW sharing.
///
/// Each entry is (phys: usize, refcount: u16) → ~10 bytes on RV64.
/// 512 entries ≈ 5 KiB in BSS.  Sufficient for typical fork workloads
/// (one or two forked processes each sharing ~100 pages).
pub const MAX_TRACKED_PAGES: usize = 512;

/// Initial refcount when a page first enters the table via `page_addref()`
/// (parent keeps its reference + child gets a new one = 2).
const INITIAL_SHARED_REFCOUNT: u16 = 2;

/// Per-page reference count entry. `phys == 0` means empty slot.
struct PageRefEntry {
    phys: usize,
    refcount: u16,
}

static PAGE_REFCOUNT: SpinLock<[PageRefEntry; MAX_TRACKED_PAGES]> = SpinLock::new({
    const EMPTY: PageRefEntry = PageRefEntry { phys: 0, refcount: 0 };
    [EMPTY; MAX_TRACKED_PAGES]
});

// ── Refcount API ─────────────────────────────────────────────────────────────

/// Increment refcount for a physical page (used by COW fork).
///
/// If the page is not yet tracked, adds it with `INITIAL_SHARED_REFCOUNT`
/// (one for parent, one for child).  Subsequent calls just `+= 1`.
///
/// Returns `CapacityFull` if the table has no free slots.
pub fn page_addref(phys: usize) -> KResult<()> {
    let mut table = PAGE_REFCOUNT.lock();
    // Look for an existing entry first.
    for entry in table.iter_mut() {
        if entry.phys == phys {
            entry.refcount = entry.refcount.saturating_add(1);
            return Ok(());
        }
    }
    // Not found — insert into first free slot.
    for entry in table.iter_mut() {
        if entry.phys == 0 {
            entry.phys = phys;
            entry.refcount = INITIAL_SHARED_REFCOUNT;
            return Ok(());
        }
    }
    Err(KernelError::CapacityFull)
}

/// Decrement refcount for a physical page.
///
/// Returns `true` if the page should be freed (refcount dropped to 0, or
/// the page was never tracked — i.e. assumed single-owner).
pub fn page_decref(phys: usize) -> bool {
    let mut table = PAGE_REFCOUNT.lock();
    for entry in table.iter_mut() {
        if entry.phys == phys {
            if entry.refcount <= 1 {
                entry.phys = 0;
                entry.refcount = 0;
                return true;
            }
            entry.refcount -= 1;
            return false;
        }
    }
    // Not tracked → caller is the sole owner.
    true
}

/// Query the current refcount for a physical page.  Returns 0 if untracked.
pub fn page_getref(phys: usize) -> u16 {
    let table = PAGE_REFCOUNT.lock();
    for entry in table.iter() {
        if entry.phys == phys {
            return entry.refcount;
        }
    }
    0
}

// ── COW fork (AQ9) ───────────────────────────────────────────────────────────

/// Create a COW copy of a user page table.
///
/// Marks every USER leaf (4 KiB) page as read-only + COW in both parent
/// and child.  Kernel entries (megapages / no USER bit) are skipped — use
/// `vmm::copy_kernel_entries_to_user()` separately.
///
/// Returns the new page table's physical address.
///
/// On any error the partially built `child_pt` is torn down here (see
/// [`vmm::destroy_user_pagetable`]) before returning. It used to be the
/// caller's job, and no caller did it: `fork()` is reachable from ring 3 in a
/// loop, so every `CapacityFull` from the refcount table leaked a root page
/// table plus every intermediate table built so far.
pub fn fork_cow(parent_pt: usize) -> KResult<usize> {
    let child_pt = vmm::create_pagetable()?;

    match fork_cow_inner(parent_pt, child_pt) {
        Ok(()) => {
            // Parent's WRITE bits changed — flush its TLB so the first store
            // actually traps into `handle_cow_fault`.
            csr::sfence_vma();
            Ok(child_pt)
        }
        Err(e) => {
            // The parent's PTEs we already flipped to COW stay flipped: that
            // is harmless (a store just takes one extra fault and copies), and
            // the pages the child did reach were addref'd *before* being
            // installed, so the teardown's decref leaves them alive for the
            // parent. Flush anyway — some parent PTEs lost their WRITE bit.
            vmm::destroy_user_pagetable(child_pt);
            csr::sfence_vma();
            Err(e)
        }
    }
}

fn fork_cow_inner(parent_pt: usize, child_pt: usize) -> KResult<()> {
    // Walk L2.
    for vpn2 in 0..PT_ENTRIES {
        let l2_pte = unsafe {
            core::ptr::read_volatile((parent_pt + vpn2 * 8) as *const Pte)
        };
        if !l2_pte.is_valid() || l2_pte.is_leaf() {
            continue; // Skip gigapages (kernel mapping).
        }
        let l1_pt = l2_pte.phys_addr();

        // Defence in depth against the loader ordering bug (audit finding 2 /
        // 3): if `copy_kernel_entries_to_user` ever runs on an empty user PT
        // again, `parent_pt.L2[vpn2]` is a pointer to the *kernel's* L1 table,
        // and this walk would rewrite kernel PTEs — clearing WRITE and setting
        // the COW marker on the kernel's own mappings, in every address space
        // at once. Recognise a borrowed kernel table and never descend into it;
        // there is nothing forkable down there in any case (kernel leaves have
        // no USER bit).
        let kernel_l1 = vmm::kernel_l1_table(vpn2);
        if kernel_l1 == Some(l1_pt) {
            continue;
        }

        // Walk L1.
        for vpn1 in 0..PT_ENTRIES {
            let l1_pte = unsafe {
                core::ptr::read_volatile((l1_pt + vpn1 * 8) as *const Pte)
            };
            if !l1_pte.is_valid() || l1_pte.is_leaf() {
                continue; // Skip megapages.
            }
            let l0_pt = l1_pte.phys_addr();

            // Same guard one level down: at VPN[2]=0 the user owns the L1
            // table but individual slots point at the kernel's L0 tables
            // (CLINT, PLIC, UART), merged in by
            // `copy_kernel_entries_to_user`.
            if let Some(k_l1) = kernel_l1 {
                let kl1_pte = unsafe {
                    core::ptr::read_volatile((k_l1 + vpn1 * 8) as *const Pte)
                };
                if kl1_pte.is_valid() && !kl1_pte.is_leaf()
                    && kl1_pte.phys_addr() == l0_pt
                {
                    continue;
                }
            }

            // Walk L0 (4 KiB pages).
            for vpn0 in 0..PT_ENTRIES {
                let l0_pte_ptr = (l0_pt + vpn0 * 8) as *mut Pte;
                let l0_pte = unsafe { core::ptr::read_volatile(l0_pte_ptr) };
                if !l0_pte.is_valid() || !l0_pte.is_leaf() {
                    continue;
                }

                let flags = l0_pte.flags();
                // Only COW user pages.
                if !flags.contains(PteFlags::USER) {
                    continue;
                }

                let phys = l0_pte.phys_addr();

                // Track the shared page BEFORE either PTE is touched.
                //
                // Ordering matters for safety, not just tidiness: if the
                // refcount table fills up after the child's PTE is written,
                // the child maps a page the table does not know about, and the
                // error teardown then decrefs it, gets `true` ("sole owner"),
                // and frees a frame the parent is still executing out of.
                // Addref-first means "present in the child" implies "tracked
                // with refcount >= 2", so teardown can never free a live page.
                page_addref(phys)?;

                // Parent: clear WRITE, set COW marker.
                let cow_flags = (flags - PteFlags::WRITE) | PteFlags::COW;
                let parent_new_pte = Pte::new(phys, cow_flags);
                unsafe { core::ptr::write_volatile(l0_pte_ptr, parent_new_pte) };

                // Reconstruct virtual address (Sv39 VPN layout).
                const VPN2_SHIFT: usize = 30;
                const VPN1_SHIFT: usize = 21;
                const VPN0_SHIFT: usize = 12;
                let vaddr = (vpn2 << VPN2_SHIFT)
                          | (vpn1 << VPN1_SHIFT)
                          | (vpn0 << VPN0_SHIFT);

                // Child: same physical page, same COW flags.  `walk()` will
                // allocate intermediate tables as needed.
                let child_pte_ptr = match vmm::walk(child_pt, vaddr, true) {
                    Ok(p) => p,
                    Err(e) => {
                        // Give back the reference we took a moment ago; the
                        // page never made it into the child. `page_decref`
                        // returns false here (2 -> 1) so nothing is freed and
                        // the parent keeps its page.
                        let _ = page_decref(phys);
                        return Err(e);
                    }
                };
                let child_pte = Pte::new(phys, cow_flags);
                unsafe { core::ptr::write_volatile(child_pte_ptr, child_pte) };
            }
        }
    }

    Ok(())
}

/// Handle a store page fault on a COW-marked page.
///
/// If the faulting PTE has the COW flag: allocate a fresh physical page,
/// copy the contents, restore the WRITE bit (+ clear COW), and decrement
/// the refcount on the old page (freeing it if it hit zero).
///
/// Returns `Ok(())` on success, `NotMapped` if the fault isn't actually COW.
pub fn handle_cow_fault(pt: usize, fault_addr: usize) -> KResult<()> {
    // Null guard, same rule as the demand-paging path (see
    // `vmm::USER_GUARD_LIMIT`). The hole here is narrower — a COW break needs
    // an already-VALID, COW-marked leaf at that VA, so page 0 has to have been
    // mapped by the parent before the fork — but it is not closed: the ELF
    // loader in `sched::process` bounds `p_vaddr` only from ABOVE
    // (`p_vaddr >= USER_LOW_MAX` is rejected, nothing rejects `p_vaddr == 0`),
    // so an image that declares a PT_LOAD at VA 0 gets page 0 mapped and its
    // children inherit it as COW. Refusing here means a store through a null
    // pointer kills the task instead of quietly gaining a private zero page.
    //
    // This also gates `vmm::translate_user`, the other caller: a syscall
    // pointer below the guard limit now fails translation outright rather
    // than breaking COW on it first.
    if crate::vmm::in_null_guard(fault_addr) {
        return Err(KernelError::InvalidArg);
    }

    let aligned_addr = fault_addr & !(PAGE_SIZE - 1);
    let pte_ptr = vmm::walk(pt, aligned_addr, false)?;
    let pte = unsafe { core::ptr::read_volatile(pte_ptr) };

    // Must be a valid, leaf, COW-marked PTE.
    if !pte.is_valid() || !pte.flags().contains(PteFlags::COW) || !pte.is_leaf() {
        return Err(KernelError::NotMapped);
    }

    let old_phys = pte.phys_addr();
    let flags = pte.flags();

    // Allocate + copy.
    let new_page = pmm::alloc_page()?;
    let new_phys = new_page.as_usize();
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_phys as *const u8,
            new_phys as *mut u8,
            PAGE_SIZE,
        );
    }

    // Install the new PTE: WRITE back, COW off, DIRTY set (we just wrote).
    let new_flags = (flags - PteFlags::COW) | PteFlags::WRITE | PteFlags::DIRTY;
    let new_pte = Pte::new(new_phys, new_flags);
    unsafe { core::ptr::write_volatile(pte_ptr, new_pte) };

    // Drop our reference on the old page.  If we were the last holder,
    // free it back to the PMM.
    if page_decref(old_phys) {
        let _ = pmm::free_page(PhysAddr::new(old_phys));
    }

    // Invalidate this address in the TLB.
    csr::sfence_vma_addr(aligned_addr);

    Ok(())
}
