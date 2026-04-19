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
///
/// `pub(crate)` so sibling modules (`cow`, `demand`) can share this walker.
pub(crate) fn walk(pt_phys: usize, vaddr: usize, alloc: bool) -> KResult<*mut Pte> {
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

/// Unmap a single 4 KiB virtual page.
///
/// If `vaddr` falls inside a 2 MiB megapage, the megapage is split into
/// 512 individual 4 KiB pages first, then the target page is unmapped.
/// This avoids accidentally invalidating the entire 2 MiB region.
pub fn unmap(pt_phys: usize, vaddr: usize) {
    // Walk L2 → L1 to detect megapage before reaching walk().
    let vpn2 = mmu::vpn2(vaddr);
    let l2_pte = unsafe { core::ptr::read_volatile((pt_phys + vpn2 * 8) as *const Pte) };
    if !l2_pte.is_valid() || l2_pte.is_leaf() {
        // Not mapped, or gigapage (1 GiB) — cannot split, just return.
        return;
    }
    let l1_pt = l2_pte.phys_addr();
    let vpn1 = mmu::vpn1(vaddr);
    let l1_pte_ptr = (l1_pt + vpn1 * 8) as *mut Pte;
    let l1_pte = unsafe { core::ptr::read_volatile(l1_pte_ptr) };

    if !l1_pte.is_valid() {
        return; // Not mapped.
    }

    if l1_pte.is_leaf() {
        // Megapage at L1 — must split into 512 × 4 KiB pages before unmapping.
        let mega_base = l1_pte.phys_addr();
        let flags = l1_pte.flags();

        let l0_pt = match pmm::alloc_page() {
            Ok(p) => p.as_usize(),
            Err(_) => return, // OOM — cannot split, bail out.
        };

        // Populate L0 table: 512 PTEs mapping each 4 KiB page of the megapage.
        for i in 0..PT_ENTRIES {
            let paddr = mega_base + i * PAGE_SIZE;
            let pte = Pte::new(paddr, flags);
            unsafe { core::ptr::write_volatile((l0_pt + i * 8) as *mut Pte, pte) };
        }

        // Replace the L1 leaf PTE with a pointer to the new L0 table.
        let new_l1_pte = Pte::new(l0_pt, PteFlags::VALID);
        unsafe { core::ptr::write_volatile(l1_pte_ptr, new_l1_pte) };

        // Full TLB flush — the megapage TLB entry must be invalidated.
        csr::sfence_vma();
    }

    // Now walk normally to the L0 PTE and unmap the single 4 KiB page.
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

/// Split megapages covering a range into 4K pages.
///
/// This is necessary before enforce_wx(), because different kernel sections
/// (text, rodata, data) within the same 2 MiB megapage need different
/// permissions. A megapage is one PTE covering 2 MiB — we can't set
/// text=RX and data=RW within the same PTE.
///
/// For each megapage that overlaps [start, end): allocate an L0 table,
/// create 512 individual 4K PTEs with the same physical addresses,
/// and replace the megapage L1 entry with a pointer to the L0 table.
pub fn split_mega_range(start: usize, end: usize) {
    let kpt = *KERNEL_PT.lock();

    // Align to megapage boundaries
    let mega_start = start & !(MEGA_SIZE - 1);
    let mega_end = (end + MEGA_SIZE - 1) & !(MEGA_SIZE - 1);

    let mut addr = mega_start;
    while addr < mega_end {
        // Check if this address is mapped as a megapage
        let vpn2 = mmu::vpn2(addr);
        let vpn1 = mmu::vpn1(addr);

        let l2_pte_ptr = (kpt + vpn2 * 8) as *const Pte;
        let l2_pte = unsafe { core::ptr::read_volatile(l2_pte_ptr) };
        if !l2_pte.is_valid() || l2_pte.is_leaf() {
            addr += MEGA_SIZE;
            continue;
        }

        let l1_pt = l2_pte.phys_addr();
        let l1_pte_ptr = (l1_pt + vpn1 * 8) as *mut Pte;
        let l1_pte = unsafe { core::ptr::read_volatile(l1_pte_ptr) };

        if !l1_pte.is_valid() || !l1_pte.is_leaf() {
            // Not a megapage (already 4K or invalid) — skip
            addr += MEGA_SIZE;
            continue;
        }

        // This is a megapage at L1. Split it into 512 × 4K pages.
        let mega_phys = l1_pte.phys_addr();
        let mega_flags = PteFlags::KERNEL_RWX; // preserve original flags

        // Allocate a new L0 page table
        let l0_page = match pmm::alloc_page() {
            Ok(p) => p,
            Err(_) => {
                addr += MEGA_SIZE;
                continue; // OOM — skip this megapage
            }
        };
        let l0_pt = l0_page.as_usize();

        // Fill L0 with 512 entries pointing to consecutive 4K pages
        for i in 0..512 {
            let pa = mega_phys + i * PAGE_SIZE;
            let pte = Pte::new(pa, mega_flags);
            unsafe {
                core::ptr::write_volatile((l0_pt + i * 8) as *mut Pte, pte);
            }
        }

        // Replace the L1 megapage entry with a pointer to the L0 table
        let new_l1 = Pte::new(l0_pt, PteFlags::VALID);
        unsafe { core::ptr::write_volatile(l1_pte_ptr, new_l1) };

        // Flush TLB for this range
        for i in 0..512 {
            csr::sfence_vma_addr(addr + i * PAGE_SIZE);
        }

        addr += MEGA_SIZE;
    }
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

/// Enforce W^X policy: remap kernel sections with correct permissions.
///
/// After init(), everything is KERNEL_RWX (needed during boot before paging).
/// This function tightens permissions per section:
///   .text      → RX (no write — prevent code injection)
///   .rodata    → RO (no write, no execute)
///   .data/.bss → RW (no execute — prevent data execution)
///
/// Page-boundary handling: if .text and .rodata share a 4K page (the
/// boundary falls mid-page), that page stays RX (text wins, since
/// removing X would crash any code in that page).
///
/// Must be called AFTER split_mega_range() and enable_paging().
pub fn enforce_wx(
    text_start: usize, text_end: usize,
    rodata_start: usize, rodata_end: usize,
    data_start: usize, kernel_end: usize,
) {
    let kpt = *KERNEL_PT.lock();

    // Page-align boundaries (round .text UP, .rodata/.data DOWN)
    // This ensures shared boundary pages keep the more permissive flags.
    let text_page_end = (text_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let rodata_page_start = rodata_start & !(PAGE_SIZE - 1);
    let rodata_page_end = (rodata_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let data_page_start = data_start & !(PAGE_SIZE - 1);

    // .text → Read + Execute (no Write)
    remap_range(kpt, text_start, text_page_end, PteFlags::KERNEL_RX);

    // .rodata → Read only — but skip pages already covered by .text
    let ro_flags = PteFlags::VALID | PteFlags::READ | PteFlags::ACCESSED;
    let ro_start = if rodata_page_start < text_page_end {
        text_page_end  // boundary page stays RX (text wins)
    } else {
        rodata_page_start
    };
    if ro_start < rodata_page_end {
        remap_range(kpt, ro_start, rodata_page_end, ro_flags);
    }

    // .data + .bss → Read + Write (no Execute) — skip rodata overlap
    let data_start_safe = if data_page_start < rodata_page_end {
        rodata_page_end
    } else {
        data_page_start
    };
    if data_start_safe < kernel_end {
        remap_range(kpt, data_start_safe, kernel_end, PteFlags::KERNEL_RW);
    }

    // Flush TLB on ALL harts to apply new permissions
    csr::sfence_vma();
}

/// Unmap page 0 (null pointer guard).
///
/// Dereferencing a null pointer (address 0x0) will cause a page fault
/// instead of silently reading/writing address 0.
pub fn null_guard() {
    let kpt = *KERNEL_PT.lock();
    unmap(kpt, 0);
}

/// Remap a range of 4K pages with new flags (for W^X enforcement).
/// Only modifies leaf PTEs that are already valid.
fn remap_range(pt_phys: usize, start: usize, end: usize, flags: PteFlags) {
    let mut addr = start & !(PAGE_SIZE - 1);
    while addr < end {
        if let Ok(pte_ptr) = walk(pt_phys, addr, false) {
            let old = unsafe { core::ptr::read_volatile(pte_ptr) };
            if old.is_valid() && old.is_leaf() {
                let new_pte = Pte::new(old.phys_addr(), flags);
                unsafe { core::ptr::write_volatile(pte_ptr, new_pte) };
            }
        }
        addr += PAGE_SIZE;
    }
}

/// Activate the kernel page table (write SATP, enable Sv39 paging).
pub fn enable_paging() {
    let kpt = *KERNEL_PT.lock();
    switch_pagetable(kpt);
}

// COW (AQ9) and Demand Paging (AQ10) live in sibling modules `cow` and
// `demand`.  Re-export their public API here for backward compatibility
// with callers that still reference them via `vmm::`.
pub use crate::cow::{fork_cow, handle_cow_fault, page_addref, page_decref, page_getref};
pub use crate::demand::{map_demand, map_demand_range, handle_demand_fault};

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
