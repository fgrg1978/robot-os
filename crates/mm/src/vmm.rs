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

/// Lowest user VA a page-fault handler is allowed to materialize a page at.
///
/// Everything strictly below this is a **null guard region**: no fault in it
/// is ever resolvable, so the faulting task dies instead of continuing.
///
/// Why 64 KiB, and why this is not an invented number:
///   - Every ring-3 `user.ld` in `userspace/*/` starts its first section at
///     `. = 0x10000`, and the ten ELFs currently in `build/` all report
///     `min PT_LOAD p_vaddr = 0x10000` (entry points at `0x10000..0x10bb2`).
///     So `0x10000` is exactly the lowest VA any legitimate binary uses —
///     the guard is as wide as it can be without touching a real image.
///   - Everything else a user address space contains sits far above it:
///     the `brk` heap grows up from the image and is capped at
///     `USER_LOW_MAX = 0x0200_0000`, the vDSO is at `0x5000_0000`, the
///     driver/shm MMIO window at `0x6000_0000`, the stack just under
///     `USER_STACK_TOP = 0x8000_0000` (see `sched::process`).
///
/// What it closes: an instruction fetch or a load/store at VA 0 is the most
/// common bug there is (null pointer / jump through a null function
/// pointer). Before this, the fault path tried COW, then demand paging, and
/// a demand-marked PTE at a low VA — `sys_alloc_demand` bases its
/// reservation at `update_user_brk(0)`, which is 0 for a task whose brk was
/// never initialized — made the *demand* attempt SUCCEED: the kernel mapped
/// a zero page over the null pointer and let ring 3 keep running on it.
/// A null dereference then executes zeros silently instead of killing the
/// task. On a robot that is a control task that never stops and never
/// reports; a dead task at least trips the supervisor.
///
/// If this is ever widened, check `userspace/*/user.ld` first: a binary
/// linked below the new limit stops loading (it will fault on its own entry
/// point and be killed, which is the correct-but-confusing symptom).
pub const USER_GUARD_LIMIT: usize = 0x1_0000; // 64 KiB — lowest legit user VA

/// True when `vaddr` falls in the null guard region (see [`USER_GUARD_LIMIT`]).
#[inline]
pub fn in_null_guard(vaddr: usize) -> bool {
    vaddr < USER_GUARD_LIMIT
}

/// Faults resolved without any UART output, split by kind.
///
/// Bumped by the kernel's page-fault arm (`kernel/src/main.rs`) on the paths
/// that used to print a `[PAGE FAULT]` banner for a fault that was about to
/// be fixed. They exist so removing that print does not remove the evidence:
/// the counters are dumped in the post-mortem block of an *unresolved* fault
/// (and by `trace_dump` consumers), so a crash report still says "this
/// system fixed N COW faults and M demand faults before dying here".
///
/// Cost on the common path is one relaxed `fetch_add` — no lock, no UART.
static COW_FAULTS_RESOLVED:    core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DEMAND_FAULTS_RESOLVED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Record one silently-resolved COW fault. See [`faults_resolved`].
#[inline]
pub fn note_cow_resolved() {
    COW_FAULTS_RESOLVED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

/// Record one silently-resolved demand-paging fault. See [`faults_resolved`].
#[inline]
pub fn note_demand_resolved() {
    DEMAND_FAULTS_RESOLVED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

/// `(cow, demand)` faults resolved since boot, system-wide.
///
/// Consulted from the unresolved-fault post-mortem in `kernel/src/main.rs`
/// (both the U-mode kill path and the S-mode fatal path). Free to call from
/// a shell command too — it is a pair of relaxed loads.
pub fn faults_resolved() -> (u64, u64) {
    (
        COW_FAULTS_RESOLVED.load(core::sync::atomic::Ordering::Relaxed),
        DEMAND_FAULTS_RESOLVED.load(core::sync::atomic::Ordering::Relaxed),
    )
}

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

/// Drop a page table's tracking slot.
///
/// Must be called by every teardown path that frees a root page table
/// obtained from [`create_pagetable`]. `PT_META` has only `MAX_PT_TRACKED`
/// (128) slots and `meta_add` never reclaims them on its own: a loader that
/// frees the root page but leaves the slot occupied turns a repeatable
/// ring-3 failure (a malformed ELF handed to `exec`) into a permanent kernel
/// resource kill — after 128 attempts `create_pagetable` returns
/// `CapacityFull` forever and no process can ever be created again.
fn meta_remove(pt: usize) {
    if pt == 0 { return; }
    let mut meta = PT_META.lock();
    if let Some(idx) = meta_find(&meta, pt) {
        meta[idx].pt = 0;
        meta[idx].refcount = 0;
    }
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
    // Give the page back if we cannot track it. Propagating `?` here used to
    // drop the freshly allocated frame on the floor: once `PT_META` is full
    // every further `create_pagetable` both fails *and* burns a physical page,
    // so a ring-3 fork/exec loop drains the PMM rather than just being denied.
    if let Err(e) = meta_add(pt_phys) {
        let _ = pmm::free_page(page);
        return Err(e);
    }
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
/// Add permission bits to an existing **user** leaf mapping, refusing any
/// combination that would produce a writable-executable page.
///
/// Exists because a single 4 KiB page can be shared by two ELF `PT_LOAD`
/// segments: the loader maps the page when the first segment touches it and,
/// until this function existed, never revisited the flags. An ELF laid out as
///
/// ```text
///     LOAD 0x10b48  R    (ends 0x11391 — page 0x11)
///     LOAD 0x11394  RW   (starts    — page 0x11)
/// ```
///
/// therefore ran with page 0x11 read-only, and the first store to a `static
/// mut` living there took a Store/AMO page fault. That is not hypothetical:
/// `userspace/abitest` hit it the first time it ran, and `userspace/captest`
/// has the same layout and survives only because it never writes its failure
/// counter — the bug was latent behind a passing test.
///
/// **W^X is preserved deliberately.** Granting WRITE on a page that already
/// carries EXEC is refused, so an `.rodata`/`.data` overlap can be repaired
/// while a `.text`/`.data` overlap still cannot — the second is a genuinely
/// unsafe layout and should fail loudly rather than silently produce a W+X
/// page. Returns `Err(KernelError::InvalidArg)` in that case.
///
/// Only ever widens: bits already present are kept, and the USER bit is
/// required up front so this cannot be aimed at a kernel mapping.
pub fn add_user_leaf_perms(pt_phys: usize, vaddr: usize, add: PteFlags) -> KResult<()> {
    let mut pt = pt_phys;
    // Walk to the leaf. Only L0 leaves are produced by the user-image mapper,
    // so a superpage here means the VA belongs to the kernel's merged entries
    // and is not ours to touch.
    for level in (0..3).rev() {
        let vpn = match level {
            2 => mmu::vpn2(vaddr),
            1 => mmu::vpn1(vaddr),
            _ => mmu::vpn0(vaddr),
        };
        let pte_ptr = (pt + vpn * 8) as *mut Pte;
        let pte: Pte = unsafe { core::ptr::read_volatile(pte_ptr) };
        if !pte.is_valid() { return Err(KernelError::InvalidArg); }
        if pte.is_leaf() {
            if level != 0 { return Err(KernelError::InvalidArg); }
            let f = pte.flags();
            if !f.contains(PteFlags::USER) { return Err(KernelError::InvalidArg); }
            if add.contains(PteFlags::WRITE) && f.contains(PteFlags::EXEC) {
                return Err(KernelError::InvalidArg);
            }
            let merged = f | add;
            if merged == f { return Ok(()); } // already sufficient
            unsafe {
                core::ptr::write_volatile(pte_ptr, Pte::new(pte.phys_addr(), merged));
            }
            csr::sfence_vma_addr(vaddr);
            return Ok(());
        }
        pt = pte.phys_addr();
    }
    Err(KernelError::InvalidArg)
}

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

/// Translate a **user** virtual address for a `copy_from_user` /
/// `copy_to_user` access, enforcing the permission bits that separate user
/// memory from kernel/MMIO memory.
///
/// Unlike [`translate`] (which checks only `VALID`), this rejects any address
/// whose leaf PTE lacks the `USER` bit. That distinction is load-bearing:
/// [`copy_kernel_entries_to_user`] merges every kernel L2/L1 entry — kernel
/// text/data **and all MMIO** (UART, CLINT, PLIC, …) — into every user page
/// table. Those pages are `VALID` but not `USER`, so a plain `translate` of a
/// kernel VA (e.g. `0x1000_0000` UART, `0x8020_xxxx` kernel text) succeeds and
/// hands the syscall path a pointer it will read or write on the caller's
/// behalf — a sandbox escape / arbitrary-write primitive. Requiring `USER`
/// closes both directions.
///
/// The `USER` bit is the authoritative user/kernel boundary here; a numeric
/// `vaddr < USER_STACK_TOP` split would be both redundant (the bit already
/// separates them) and insufficient (MMIO at `0x0200_0000` / `0x1000_0000`
/// lives *below* any such split).
///
/// Permission checked on the leaf PTE:
///   - always: `VALID` + `USER` + `READ`
///   - when `write`: also `WRITE`. A user page whose `WRITE` bit is clear but
///     which carries the OS-defined `COW` marker (a shared page from a
///     copy-on-write `fork`) is broken via [`crate::cow::handle_cow_fault`]
///     and re-translated, so a legitimate post-fork write copies into a
///     private page instead of spuriously failing (or corrupting the shared
///     page, which is what the old unchecked `translate` path did). A
///     genuinely read-only user page (e.g. the vDSO, `.text`) is rejected.
///
/// Returns the physical address on success, `None` on any permission failure
/// or unmapped page. Never panics.
pub fn translate_user(pt_phys: usize, vaddr: usize, write: bool) -> Option<usize> {
    let mut pt = pt_phys;

    // L2 — kernel gigapages reach this leaf (copied wholesale into user PTs),
    // so the USER check must be applied here too.
    let vpn2 = mmu::vpn2(vaddr);
    let l2_pte = unsafe { core::ptr::read_volatile((pt + vpn2 * 8) as *const Pte) };
    if !l2_pte.is_valid() { return None; }
    if l2_pte.is_leaf() {
        return user_leaf_ok(pt_phys, vaddr, l2_pte, 0x3FFF_FFFF, write);
    }
    pt = l2_pte.phys_addr();

    // L1 — kernel megapages (MMIO, kernel image) reach this leaf.
    let vpn1 = mmu::vpn1(vaddr);
    let l1_pte = unsafe { core::ptr::read_volatile((pt + vpn1 * 8) as *const Pte) };
    if !l1_pte.is_valid() { return None; }
    if l1_pte.is_leaf() {
        return user_leaf_ok(pt_phys, vaddr, l1_pte, 0x1F_FFFF, write);
    }
    pt = l1_pte.phys_addr();

    // L0 — ordinary 4 KiB user pages (and any 4 KiB kernel leaves).
    let vpn0 = mmu::vpn0(vaddr);
    let l0_pte = unsafe { core::ptr::read_volatile((pt + vpn0 * 8) as *const Pte) };
    if !l0_pte.is_valid() { return None; }
    user_leaf_ok(pt_phys, vaddr, l0_pte, PAGE_SIZE - 1, write)
}

/// Permission gate for one leaf PTE reached by [`translate_user`].
/// `offset_mask` selects the in-page offset for the leaf's page size.
#[inline]
fn user_leaf_ok(
    pt_phys: usize,
    vaddr: usize,
    pte: Pte,
    offset_mask: usize,
    write: bool,
) -> Option<usize> {
    let f = pte.flags();
    // Reject kernel/MMIO (no USER bit) and unreadable pages outright.
    if !f.contains(PteFlags::USER) || !f.contains(PteFlags::READ) {
        return None;
    }
    if write && !f.contains(PteFlags::WRITE) {
        // A copy-on-write page: break it (allocate a private copy) and re-walk.
        // Any other read-only user page is genuinely not writable → reject.
        if f.contains(PteFlags::COW) {
            crate::cow::handle_cow_fault(pt_phys, vaddr).ok()?;
            // Re-translate: the fresh leaf now has WRITE set. Guard against a
            // pathological re-fault by using the plain permission read.
            let mut pt = pt_phys;
            let l2 = unsafe { core::ptr::read_volatile((pt + mmu::vpn2(vaddr) * 8) as *const Pte) };
            if !l2.is_valid() { return None; }
            if l2.is_leaf() {
                return if l2.flags().contains(PteFlags::WRITE) {
                    Some(l2.phys_addr() + (vaddr & 0x3FFF_FFFF))
                } else { None };
            }
            pt = l2.phys_addr();
            let l1 = unsafe { core::ptr::read_volatile((pt + mmu::vpn1(vaddr) * 8) as *const Pte) };
            if !l1.is_valid() { return None; }
            if l1.is_leaf() {
                return if l1.flags().contains(PteFlags::WRITE) {
                    Some(l1.phys_addr() + (vaddr & 0x1F_FFFF))
                } else { None };
            }
            pt = l1.phys_addr();
            let l0 = unsafe { core::ptr::read_volatile((pt + mmu::vpn0(vaddr) * 8) as *const Pte) };
            if !l0.is_valid() || !l0.flags().contains(PteFlags::WRITE) { return None; }
            return Some(l0.phys_addr() + (vaddr & (PAGE_SIZE - 1)));
        }
        return None;
    }
    Some(pte.phys_addr() + (vaddr & offset_mask))
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
pub use crate::demand::{map_demand, map_demand_range};

/// Resolve a demand-paging fault, refusing anything in the null guard region.
///
/// This is the entry point the kernel's page-fault arm uses. It is a wrapper
/// rather than a plain re-export of [`crate::demand::handle_demand_fault`]
/// because that function will happily materialize a page for *any* VA that
/// carries a `DEMAND`-marked PTE, VA 0 included — and a demand PTE at VA 0 is
/// reachable (`sys_alloc_demand` bases its reservation at the task's `brk`,
/// which is 0 for a task that never got one). Materializing it turns a jump
/// through a null pointer into a task that keeps running on a page of zeros
/// instead of dying. See [`USER_GUARD_LIMIT`] for the threshold's derivation.
///
/// `InvalidArg` (not `NotMapped`) on a guard hit, so the caller can tell
/// "there was nothing to resolve here" from "I refuse to resolve this".
pub fn handle_demand_fault(pt: usize, fault_addr: usize) -> KResult<()> {
    if in_null_guard(fault_addr) {
        return Err(KernelError::InvalidArg);
    }
    crate::demand::handle_demand_fault(pt, fault_addr)
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
///
/// # Ordering invariant — call this LAST
///
/// This must run **after** every user mapping is installed, never before.
/// On an empty user PT the wholesale branch below fires for *every* kernel
/// L2 slot, so `user_pt.L2[vpn2]` ends up holding a pointer to the kernel's
/// own L1 table — shared, not copied. Userspace links at `0x10000` and the
/// kernel maps the CLINT at `0x0200_0000`; both are VPN[2]=0, so a
/// subsequent `map(user_pt, 0x10000, ..)` allocates an L0 table *inside the
/// kernel's L1* and installs USER leaves in the kernel page table. Every
/// address space then inherits the previous process's mappings, user pages
/// become visible to (and clobberable by) the kernel, and the next
/// `load_elf` memcpy's over the live `.text` of an already-running process.
/// Mapping first means the user PT owns its own L1 tables and the merge
/// path below — the branch that keeps kernel and user separate — is the one
/// that actually runs.
///
/// Because the copy grafts kernel-owned tables into `user_pt`, any teardown
/// of that PT must go through [`destroy_user_pagetable`], which knows how to
/// tell the borrowed kernel tables from the user's own.
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

/// The kernel's L1 table for VPN[2] slot `vpn2`, if the kernel PT has one.
///
/// Returns `None` when the kernel has no entry there, or when the entry is a
/// gigapage leaf (no L1 table to speak of). Used by the teardown and COW
/// walkers to recognise a table that is *borrowed* from the kernel PT rather
/// than owned by the user PT they are traversing.
pub(crate) fn kernel_l1_table(vpn2: usize) -> Option<usize> {
    let kpt = *KERNEL_PT.lock();
    if kpt == 0 || vpn2 >= PT_ENTRIES { return None; }
    let kpte: Pte = unsafe { core::ptr::read_volatile((kpt + vpn2 * 8) as *const Pte) };
    if kpte.is_valid() && !kpte.is_leaf() { Some(kpte.phys_addr()) } else { None }
}

/// Check, without writing anything, whether [`copy_kernel_entries_to_user`]
/// would be able to install *every* kernel mapping into `user_pt`.
///
/// The merge only fills empty slots — it never overwrites a user mapping. That
/// is the right precedence for memory safety, but it means a user layout that
/// happens to occupy a VPN[2] (or VPN[2]/VPN[1]) slot the kernel also needs
/// silently *loses* the kernel entry. The failure is not a fault at load time:
/// the process starts, runs, and then the first timer interrupt or `kprintln`
/// taken while its SATP is live faults in S-mode on a CLINT/UART address the
/// kernel believes is identity-mapped. On a robot that is a hang with the
/// actuators still energised.
///
/// So exec refuses the image instead. Returns `None` when everything fits, or
/// `Some((vpn2, vpn1))` naming the first slot that collides (`vpn1 ==
/// PT_ENTRIES` means the collision is at L2 itself). Call it *before*
/// `copy_kernel_entries_to_user` so the rejection path still sees a page table
/// that contains nothing but user-owned tables.
///
/// Note for platforms whose RAM base is 0 (K1: `RAM_BASE = 0x0000_0000`): the
/// kernel identity-maps a megapage over the very VA range userspace links at
/// (`0x10000`), so this predicate fires and exec fails. That is intentional and
/// strictly better than today's behaviour there — see the report on the K1 VA
/// layout; fixing it needs a user-image relocation, not a change here.
pub fn kernel_entry_collision(user_pt: usize) -> Option<(usize, usize)> {
    let kpt = *KERNEL_PT.lock();
    if kpt == 0 { return None; }

    for vpn2 in 0..PT_ENTRIES {
        let kpte: Pte = unsafe {
            core::ptr::read_volatile((kpt + vpn2 * 8) as *const Pte)
        };
        if !kpte.is_valid() { continue; }

        let upte: Pte = unsafe {
            core::ptr::read_volatile((user_pt + vpn2 * 8) as *const Pte)
        };
        if !upte.is_valid() {
            continue; // wholesale copy will install it
        }
        if kpte.is_leaf() || upte.is_leaf() {
            // A gigapage on either side cannot be merged — the kernel entry
            // would be dropped entirely.
            return Some((vpn2, PT_ENTRIES));
        }

        let k_l1 = kpte.phys_addr();
        let u_l1 = upte.phys_addr();
        for vpn1 in 0..PT_ENTRIES {
            let kl1: Pte = unsafe {
                core::ptr::read_volatile((k_l1 + vpn1 * 8) as *const Pte)
            };
            if !kl1.is_valid() { continue; }
            let ul1: Pte = unsafe {
                core::ptr::read_volatile((u_l1 + vpn1 * 8) as *const Pte)
            };
            if ul1.is_valid() {
                return Some((vpn2, vpn1));
            }
        }
    }
    None
}

/// Tear down a **user** page table: free its own intermediate tables and its
/// USER leaf pages, and release its `PT_META` slot.
///
/// This is the teardown [`destroy_pagetable`] cannot be: that one recurses
/// into every valid non-leaf entry, which on a user PT that has been through
/// [`copy_kernel_entries_to_user`] means walking into — and freeing — the
/// kernel's own L1/L0 tables. Handing those frames back to the PMM while the
/// kernel is still executing out of them is not a leak, it is a machine that
/// stops.
///
/// Kernel tables are grafted in at two different depths and both must be
/// recognised:
///   - **L2**: a VPN[2] slot the user never touched holds a copy of the
///     kernel's L2 PTE, i.e. a pointer to the kernel's L1 table.
///   - **L1**: at VPN[2]=0 the user owns the L1 table, but individual slots
///     inside it (CLINT, PLIC, UART, …) point at the kernel's L0 tables.
///     A teardown that compared only at L2 would recurse into the user's own
///     L1, reach slot 16, and free the kernel's CLINT L0 table — a corruption
///     that surfaces at a random later moment, nowhere near this code.
///
/// Leaf pages are released through [`crate::cow::page_decref`], so a page
/// still shared with a forked peer survives; an untracked (sole-owner) page is
/// freed. The vDSO frame is kernel-owned and mapped USER_RO into every address
/// space, so it is skipped explicitly.
pub fn destroy_user_pagetable(pt_phys: usize) {
    // No reserved window: on the construction paths nothing can have mapped
    // shm/MMIO into this PT yet (see `destroy_user_pagetable_skip_range`).
    destroy_user_pagetable_skip_range(pt_phys, 0, 0)
}

/// [`destroy_user_pagetable`] for a *post-construction* address space: leaf
/// frames whose VA falls in `[skip_lo, skip_hi)` are left un-freed.
///
/// K-C22 wired teardown into exec replacement and task-slot reuse — page
/// tables that have LIVED, which the plain variant was never safe for: a
/// running process may have shm and MMIO frames mapped USER into its PT
/// (`shm_map_user` / `mmio_map_user` in the sched crate), and those frames
/// are not the address space's to free. Shm pages are PMM pages owned by the
/// shm registry and possibly mapped by other processes — `page_decref` has
/// never tracked them, so it would report "sole owner" and this walk would
/// hand a page another process is actively using back to the allocator. MMIO
/// frames merely bounce off `pmm::free_page`'s range check, but skipping
/// them keeps the ownership rule uniform instead of leaning on that.
///
/// Both kinds only ever land in one VA window (`reserve_mmio_va` is the
/// single allocator for it), so callers pass that window. The window's own
/// L1/L0 *tables* were allocated by `vmm::map` on this PT and ARE freed —
/// the mappings die with the address space; the frames survive under their
/// real owner.
///
/// **Free order is load-bearing**: the ROOT frame is freed last, after the
/// full 512-slot L2 walk. The reuse-time reclaim in the scheduler
/// (`try_task_create_affinity`, K-C22(B)) may run while the hart that
/// zombified this address space is still a few instructions short of its
/// `csrw satp` away from it — kernel text/stack resolve through the
/// *borrowed* kernel L1s (skipped below), so the root is the only frame
/// that hart still translates through. Do not reorder the root free
/// earlier.
pub fn destroy_user_pagetable_skip_range(pt_phys: usize, skip_lo: usize, skip_hi: usize) {
    if pt_phys == 0 { return; }

    let kpt = *KERNEL_PT.lock();
    let vdso_phys = crate::vdso::vdso_phys();

    for vpn2 in 0..PT_ENTRIES {
        let l2: Pte = unsafe {
            core::ptr::read_volatile((pt_phys + vpn2 * 8) as *const Pte)
        };
        // Gigapage leaves are only ever created by the kernel mapper.
        if !l2.is_valid() || l2.is_leaf() { continue; }
        let u_l1 = l2.phys_addr();

        // Which L1 table does the kernel use for this slot (if any)?
        let k_l1 = if kpt != 0 {
            let kpte: Pte = unsafe {
                core::ptr::read_volatile((kpt + vpn2 * 8) as *const Pte)
            };
            if kpte.is_valid() && !kpte.is_leaf() { kpte.phys_addr() } else { 0 }
        } else { 0 };

        if k_l1 != 0 && k_l1 == u_l1 {
            continue; // borrowed wholesale from the kernel PT — not ours to free
        }

        for vpn1 in 0..PT_ENTRIES {
            let l1: Pte = unsafe {
                core::ptr::read_volatile((u_l1 + vpn1 * 8) as *const Pte)
            };
            // Megapage leaves at L1 are kernel-created (map_mega is never used
            // for user mappings) — leave them alone.
            if !l1.is_valid() || l1.is_leaf() { continue; }
            let u_l0 = l1.phys_addr();

            if k_l1 != 0 {
                let kl1: Pte = unsafe {
                    core::ptr::read_volatile((k_l1 + vpn1 * 8) as *const Pte)
                };
                if kl1.is_valid() && !kl1.is_leaf() && kl1.phys_addr() == u_l0 {
                    continue; // merged kernel L0 table — not ours to free
                }
            }

            for vpn0 in 0..PT_ENTRIES {
                let l0: Pte = unsafe {
                    core::ptr::read_volatile((u_l0 + vpn0 * 8) as *const Pte)
                };
                if !l0.is_valid() || !l0.is_leaf() { continue; }
                // A leaf without USER is a kernel mapping that found its way
                // in — never ours to free.
                //
                // USER leaves installed by `shm_map_user`/`mmio_map_user` are
                // *not* owned by this address space either — that is what
                // `skip_lo..skip_hi` exists for (see the function doc); the
                // construction-failure paths pass an empty window because no
                // such mapping can exist before the loader/fork returns.
                if !l0.flags().contains(PteFlags::USER) { continue; }
                let va = (vpn2 << 30) | (vpn1 << 21) | (vpn0 << 12);
                if va >= skip_lo && va < skip_hi { continue; }
                let phys = l0.phys_addr();
                if vdso_phys != 0 && phys == vdso_phys { continue; }
                if crate::cow::page_decref(phys) {
                    let _ = pmm::free_page(PhysAddr::new(phys));
                }
            }
            let _ = pmm::free_page(PhysAddr::new(u_l0));
        }
        let _ = pmm::free_page(PhysAddr::new(u_l1));
    }

    meta_remove(pt_phys);
    let _ = pmm::free_page(PhysAddr::new(pt_phys));
}
