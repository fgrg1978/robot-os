/// Virtual Memory Manager (VMM).
///
/// Sv39 3-level page table management with identity mapping, COW,
/// and page table reference counting.
///
/// Ported from kernel/mm/vmm.c

use robot_os_arch::csr;
use robot_os_arch::mmu::{self, Pte, PteFlags, PAGE_SIZE, PT_ENTRIES};
use robot_os_sync::SpinLock;
use robot_os_common::error::{KResult, KernelError};
use crate::addr::PhysAddr;
use crate::pmm;

/// Maximum number of page tables we track for reference counting.
const MAX_PT_TRACKED: usize = 128;

struct PtMetadata {
    pt: usize,    // Physical address of page table (0 = empty slot)
    refcount: i32,
}

static PT_META: SpinLock<[PtMetadata; MAX_PT_TRACKED]> = SpinLock::new({
    const EMPTY: PtMetadata = PtMetadata { pt: 0, refcount: 0 };
    [EMPTY; MAX_PT_TRACKED]
});

/// The kernel page table (set during vmm_init).
static KERNEL_PT: SpinLock<usize> = SpinLock::new(0);

// ---- Reference counting ----

fn meta_find(meta: &[PtMetadata; MAX_PT_TRACKED], pt: usize) -> Option<usize> {
    meta.iter().position(|m| m.pt == pt && pt != 0)
}

fn meta_add(pt: usize) -> KResult<()> {
    let mut meta = PT_META.lock();
    if meta_find(&meta, pt).is_some() {
        return Ok(()); // Already tracked
    }
    for slot in meta.iter_mut() {
        if slot.pt == 0 {
            slot.pt = pt;
            slot.refcount = 1;
            return Ok(());
        }
    }
    Err(KernelError::CapacityFull)
}

/// Increment reference count for a page table.
pub fn pt_addref(pt: usize) -> KResult<i32> {
    let mut meta = PT_META.lock();
    if let Some(idx) = meta_find(&meta, pt) {
        meta[idx].refcount += 1;
        Ok(meta[idx].refcount)
    } else {
        Err(KernelError::NotFound)
    }
}

/// Decrement reference count. If it reaches 0, the page table is destroyed.
pub fn pt_release(pt: usize) -> KResult<i32> {
    let mut meta = PT_META.lock();
    if let Some(idx) = meta_find(&meta, pt) {
        meta[idx].refcount -= 1;
        let rc = meta[idx].refcount;
        if rc <= 0 {
            meta[idx].pt = 0;
            meta[idx].refcount = 0;
            // Drop lock before destroying (destroy_pagetable may allocate locks)
            drop(meta);
            destroy_pagetable(pt);
        }
        Ok(rc)
    } else {
        Err(KernelError::NotFound)
    }
}

/// Get current reference count.
pub fn pt_getref(pt: usize) -> Option<i32> {
    let meta = PT_META.lock();
    meta_find(&meta, pt).map(|idx| meta[idx].refcount)
}

// ---- Page table operations ----

/// Create a new (zeroed) page table. Registered with refcount=1.
pub fn create_pagetable() -> KResult<usize> {
    let page = pmm::alloc_page()?; // alloc_page already zeroes the page
    let pt_phys = page.as_usize();
    meta_add(pt_phys)?;
    Ok(pt_phys)
}

/// Walk the page table to find the PTE for `vaddr`.
/// If `alloc` is true, allocate missing intermediate tables.
/// Returns a pointer to the leaf PTE (L0 for 4K pages).
/// If a megapage (leaf at L1) is encountered, returns a pointer to that PTE.
fn walk(pt_phys: usize, vaddr: usize, alloc: bool) -> KResult<*mut Pte> {
    let mut pt = pt_phys;

    for level in (1..=2).rev() {
        let vpn = match level {
            2 => mmu::vpn2(vaddr),
            1 => mmu::vpn1(vaddr),
            _ => unreachable!(),
        };

        let pte_ptr = (pt + vpn * 8) as *mut Pte;
        let pte = unsafe { core::ptr::read_volatile(pte_ptr) };

        if pte.is_valid() {
            // If this is a leaf PTE (megapage at L1 or gigapage at L2),
            // return it directly — do NOT follow phys_addr as a PT pointer.
            if pte.is_leaf() {
                return Ok(pte_ptr);
            }
            pt = pte.phys_addr();
        } else {
            if !alloc {
                return Err(KernelError::NotMapped);
            }
            // Allocate intermediate page table
            let new_pt = pmm::alloc_page()?;
            let new_pte = Pte::new(new_pt.as_usize(), PteFlags::VALID);
            unsafe { core::ptr::write_volatile(pte_ptr, new_pte) };
            pt = new_pt.as_usize();
        }
    }

    // Return pointer to level-0 PTE
    let vpn0 = mmu::vpn0(vaddr);
    Ok((pt + vpn0 * 8) as *mut Pte)
}

/// Map a virtual address to a physical address with given flags.
pub fn map(pt_phys: usize, vaddr: usize, paddr: usize, flags: PteFlags) -> KResult<()> {
    if !mmu::is_page_aligned(vaddr) || !mmu::is_page_aligned(paddr) {
        return Err(KernelError::NotAligned);
    }

    let pte_ptr = walk(pt_phys, vaddr, true)?;
    let old = unsafe { core::ptr::read_volatile(pte_ptr) };
    if old.is_valid() {
        return Err(KernelError::AlreadyMapped);
    }

    let pte = Pte::new(paddr, flags | PteFlags::VALID);
    unsafe { core::ptr::write_volatile(pte_ptr, pte) };
    Ok(())
}

/// 2 MiB megapage size.
pub const MEGA_SIZE: usize = 2 * 1024 * 1024;

/// Map a 2 MiB megapage (leaf PTE at L1 level).
///
/// Both `vaddr` and `paddr` must be 2 MiB aligned.
pub fn map_mega(pt_phys: usize, vaddr: usize, paddr: usize, flags: PteFlags) -> KResult<()> {
    if vaddr & (MEGA_SIZE - 1) != 0 || paddr & (MEGA_SIZE - 1) != 0 {
        return Err(KernelError::NotAligned);
    }

    // Walk L2 to find/create the L1 table
    let vpn2 = mmu::vpn2(vaddr);
    let l2_pte_ptr = (pt_phys + vpn2 * 8) as *mut Pte;
    let l2_pte = unsafe { core::ptr::read_volatile(l2_pte_ptr) };

    let l1_pt = if l2_pte.is_valid() {
        if l2_pte.is_leaf() {
            return Err(KernelError::AlreadyMapped); // gigapage here
        }
        l2_pte.phys_addr()
    } else {
        let new_pt = pmm::alloc_page()?;
        let new_pte = Pte::new(new_pt.as_usize(), PteFlags::VALID);
        unsafe { core::ptr::write_volatile(l2_pte_ptr, new_pte) };
        new_pt.as_usize()
    };

    // Write leaf PTE at L1 (megapage)
    let vpn1 = mmu::vpn1(vaddr);
    let l1_pte_ptr = (l1_pt + vpn1 * 8) as *mut Pte;
    let old = unsafe { core::ptr::read_volatile(l1_pte_ptr) };
    if old.is_valid() {
        return Err(KernelError::AlreadyMapped);
    }

    let pte = Pte::new(paddr, flags | PteFlags::VALID);
    unsafe { core::ptr::write_volatile(l1_pte_ptr, pte) };
    Ok(())
}

/// Unmap a virtual address.
pub fn unmap(pt_phys: usize, vaddr: usize) {
    if let Ok(pte_ptr) = walk(pt_phys, vaddr, false) {
        unsafe { core::ptr::write_volatile(pte_ptr, Pte::empty()) };
        csr::sfence_vma_addr(vaddr);
    }
}

/// Translate a virtual address to a physical address.
/// Returns `None` if not mapped.
/// Handles 4 KiB pages and 2 MiB megapages correctly.
pub fn translate(pt_phys: usize, vaddr: usize) -> Option<usize> {
    // Walk inline to detect megapages at each level.
    let mut pt = pt_phys;

    // L2
    let vpn2 = mmu::vpn2(vaddr);
    let l2_pte = unsafe { core::ptr::read_volatile((pt + vpn2 * 8) as *const Pte) };
    if !l2_pte.is_valid() { return None; }
    if l2_pte.is_leaf() {
        // 1 GiB gigapage
        return Some(l2_pte.phys_addr() + (vaddr & 0x3FFF_FFFF));
    }
    pt = l2_pte.phys_addr();

    // L1
    let vpn1 = mmu::vpn1(vaddr);
    let l1_pte = unsafe { core::ptr::read_volatile((pt + vpn1 * 8) as *const Pte) };
    if !l1_pte.is_valid() { return None; }
    if l1_pte.is_leaf() {
        // 2 MiB megapage
        return Some(l1_pte.phys_addr() + (vaddr & 0x1F_FFFF));
    }
    pt = l1_pte.phys_addr();

    // L0
    let vpn0 = mmu::vpn0(vaddr);
    let l0_pte = unsafe { core::ptr::read_volatile((pt + vpn0 * 8) as *const Pte) };
    if !l0_pte.is_valid() { return None; }
    Some(l0_pte.phys_addr() + (vaddr & (PAGE_SIZE - 1)))
}

/// Switch to a page table by writing the SATP register.
pub fn switch_pagetable(pt_phys: usize) {
    let satp = mmu::make_satp(pt_phys, 0);
    csr::write_satp(satp);
}

/// Get the kernel page table physical address.
pub fn kernel_pagetable() -> usize {
    *KERNEL_PT.lock()
}

/// Recursively destroy a page table, freeing all intermediate PT pages.
/// Does NOT free leaf (mapped) physical pages.
pub fn destroy_pagetable(pt_phys: usize) {
    if pt_phys == 0 {
        return;
    }
    // Walk root entries
    for i in 0..PT_ENTRIES {
        let pte_ptr = (pt_phys + i * 8) as *const Pte;
        let pte = unsafe { core::ptr::read_volatile(pte_ptr) };
        if pte.is_valid() && !pte.is_leaf() {
            // Intermediate table — recurse
            destroy_pagetable(pte.phys_addr());
        }
    }
    // Free this page table page
    let _ = pmm::free_page(PhysAddr::new(pt_phys));
}

/// Initialize the VMM: create kernel page table with identity mapping.
///
/// `mem_start`: physical RAM start (e.g., 0x8000_0000)
/// `mem_size`: total RAM in bytes
///
/// Uses 2 MiB megapages for the bulk of RAM (reduces PT pages from ~90 to ~2
/// for 128 MiB), with 4 KiB pages for unaligned head/tail regions.
pub fn init(mem_start: usize, mem_size: usize) -> KResult<()> {
    let kpt = create_pagetable()?;
    *KERNEL_PT.lock() = kpt;

    let mem_end = mem_start + mem_size;

    // Phase 1: 4 KiB pages for any unaligned head (mem_start → first 2M boundary)
    let mega_start = (mem_start + MEGA_SIZE - 1) & !(MEGA_SIZE - 1);
    let mut addr = mem_start;
    while addr < mega_start && addr < mem_end {
        let _ = map(kpt, addr, addr, PteFlags::KERNEL_RWX);
        addr += PAGE_SIZE;
    }

    // Phase 2: 2 MiB megapages for the aligned bulk
    let mega_end = mem_end & !(MEGA_SIZE - 1);
    addr = mega_start;
    while addr < mega_end {
        let _ = map_mega(kpt, addr, addr, PteFlags::KERNEL_RWX);
        addr += MEGA_SIZE;
    }

    // Phase 3: 4 KiB pages for any unaligned tail (last partial 2M block)
    while addr < mem_end {
        let _ = map(kpt, addr, addr, PteFlags::KERNEL_RWX);
        addr += PAGE_SIZE;
    }

    // NOTE: MMIO mappings are NOT done here — they are platform-specific
    // and are added by kernel_main() after vmm::init() returns.
    // Use map_mmio_region() for each platform's device addresses.

    Ok(())
}

/// Map an MMIO region with identity mapping (vaddr == paddr).
///
/// Maps `size` bytes of device memory starting at `base` using 4 KiB pages
/// with KERNEL_RW flags (no execute).
pub fn map_mmio_region(base: usize, size: usize) -> KResult<()> {
    let kpt = *KERNEL_PT.lock();
    let aligned_base = base & !(PAGE_SIZE - 1);
    let end = (base + size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let mut addr = aligned_base;
    while addr < end {
        let _ = map(kpt, addr, addr, PteFlags::KERNEL_RW);
        addr += PAGE_SIZE;
    }
    Ok(())
}

/// Activate the kernel page table (write SATP, enable Sv39 paging).
pub fn enable_paging() {
    let kpt = *KERNEL_PT.lock();
    switch_pagetable(kpt);
}

/// Copy kernel page-table entries into a user page table.
///
/// After this call the user PT contains all kernel mappings (code, MMIO)
/// alongside the user-space mappings.  Kernel pages have no USER bit so
/// U-mode code cannot access them directly; they are only reachable in
/// S-mode (trap handler, syscall dispatch).
///
/// This is required so that when an ecall fires while the user PT is active
/// the CPU can still fetch from trap_vector (~0x80200000, VPN[2]=2) and
/// the trap handler can write to UART/MMIO (VPN[2]=0, high VPN[1] slots).
///
/// For VPN[2] entries present only in the kernel PT, the L2 PTE is copied
/// directly.  For VPN[2]=0, where both PTs have an intermediate L1 table,
/// the two tables are merged at L1 level: kernel L1 entries (MMIO) are
/// written into any empty slots in the user L1 table (user code occupies
/// different VPN[1] slots, so there is no collision).
pub fn copy_kernel_entries_to_user(user_pt: usize) {
    let kpt = *KERNEL_PT.lock();

    for vpn2 in 0..PT_ENTRIES {
        let kpte: Pte = unsafe {
            core::ptr::read_volatile((kpt + vpn2 * 8) as *const Pte)
        };
        if !kpte.is_valid() {
            continue;
        }

        let upte_ptr = (user_pt + vpn2 * 8) as *mut Pte;
        let upte: Pte = unsafe { core::ptr::read_volatile(upte_ptr) };

        if !upte.is_valid() {
            // User has no L2 entry here — copy kernel's directly.
            unsafe { core::ptr::write_volatile(upte_ptr, kpte) };
        } else if !kpte.is_leaf() && !upte.is_leaf() {
            // Both sides have intermediate L1 tables — merge kernel L1
            // entries into user L1.  Kernel entries fill only empty slots
            // (user mappings are never overwritten).
            let k_l1 = kpte.phys_addr();
            let u_l1 = upte.phys_addr();
            for vpn1 in 0..PT_ENTRIES {
                let kl1pte: Pte = unsafe {
                    core::ptr::read_volatile((k_l1 + vpn1 * 8) as *const Pte)
                };
                if !kl1pte.is_valid() {
                    continue;
                }
                let ul1pte_ptr = (u_l1 + vpn1 * 8) as *mut Pte;
                let ul1pte: Pte = unsafe { core::ptr::read_volatile(ul1pte_ptr) };
                if !ul1pte.is_valid() {
                    unsafe { core::ptr::write_volatile(ul1pte_ptr, kl1pte) };
                }
            }
        }
        // Both sides have leaf entries (megapages) — kernel PT owns it, skip.
    }
}
