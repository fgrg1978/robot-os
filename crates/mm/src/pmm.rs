/// Physical Memory Manager (PMM).
///
/// Bitmap-based page allocator. Each bit represents one 4 KiB page:
///   0 = free, 1 = allocated.
///
/// Ported from kernel/mm/pmm.c

use robot_os_arch::mmu::PAGE_SIZE;
use robot_os_sync::SpinLock;
use crate::addr::PhysAddr;
use robot_os_common::error::{KResult, KernelError};

/// Maximum number of physical pages we support.
///
/// `small-mem` builds (PROFILE_EMBEDDED): 128 pages max, bitmap = 2 u64s (16 bytes).
/// Other targets: derived from the board's real Kconfig `RAM_SIZE` (MiB),
/// so the static bitmap in `.bss` matches actual RAM instead of a flat
/// 16 GiB worst case (was 512 KiB regardless of board; vf2/k1/qemu all
/// have far less RAM than that ceiling assumed).
#[cfg(feature = "small-mem")]
const MAX_PAGES: usize = 128;
#[cfg(not(feature = "small-mem"))]
const MAX_PAGES: usize = robot_os_limits::RAM_SIZE * 1024 * 1024 / PAGE_SIZE;

/// Bitmap words needed: MAX_PAGES / 64 (rounded up).
const BITMAP_WORDS: usize = (MAX_PAGES + 63) / 64;

struct PmmInner {
    /// Allocation bitmap (1 bit per page). 0=free, 1=used.
    bitmap: [u64; BITMAP_WORDS],
    /// Total number of pages being managed.
    total_pages: usize,
    /// Number of currently free pages.
    free_pages: usize,
    /// Physical address of the first managed page.
    managed_start: usize,
    /// Initialized flag.
    initialized: bool,
}

static PMM: SpinLock<PmmInner> = SpinLock::new(PmmInner {
    bitmap: [0; BITMAP_WORDS],
    total_pages: 0,
    free_pages: 0,
    managed_start: 0,
    initialized: false,
});

#[inline]
fn bitmap_set(bitmap: &mut [u64; BITMAP_WORDS], page: usize) {
    bitmap[page / 64] |= 1u64 << (page % 64);
}

#[inline]
fn bitmap_clear(bitmap: &mut [u64; BITMAP_WORDS], page: usize) {
    bitmap[page / 64] &= !(1u64 << (page % 64));
}

#[inline]
fn bitmap_test(bitmap: &[u64; BITMAP_WORDS], page: usize) -> bool {
    bitmap[page / 64] & (1u64 << (page % 64)) != 0
}

/// Initialize the PMM.
///
/// `mem_start`: physical start of RAM (e.g., 0x8000_0000).
/// `mem_size`: total RAM size in bytes.
/// `kernel_end`: physical address after the last byte of the kernel + bitmap.
///
/// Pages from `mem_start` to `kernel_end` are marked as reserved.
pub fn init(mem_start: usize, mem_size: usize, kernel_end: usize) {
    let mut pmm = PMM.lock();

    let total = core::cmp::min(mem_size / PAGE_SIZE, MAX_PAGES);
    pmm.total_pages = total;
    pmm.managed_start = mem_start;

    // Clear bitmap (all free)
    pmm.bitmap = [0; BITMAP_WORDS];

    // Reserve pages from start to kernel_end
    let reserved_pages = (kernel_end - mem_start + PAGE_SIZE - 1) / PAGE_SIZE;
    for i in 0..reserved_pages {
        if i < total {
            bitmap_set(&mut pmm.bitmap, i);
        }
    }

    // Count free pages via popcount on bitmap words — O(BITMAP_WORDS)
    // (~128K iter for 4M-page systems via bit ops) vs the previous
    // O(total) bit-by-bit loop (~16M iter). 100× faster on init.
    let used_bits: u32 = pmm.bitmap.iter().map(|w| w.count_ones()).sum();
    let used = used_bits as usize;
    pmm.free_pages = total.saturating_sub(used);
    pmm.initialized = true;
}

/// Allocate a single 4 KiB physical page.
/// Returns the physical address of the page, or `OutOfMemory`.
pub fn alloc_page() -> KResult<PhysAddr> {
    let mut pmm = PMM.lock();

    // Fast scan: check whole u64 words first
    for word_idx in 0..BITMAP_WORDS {
        if word_idx * 64 >= pmm.total_pages {
            break;
        }
        // If all bits set, skip this word
        if pmm.bitmap[word_idx] == u64::MAX {
            continue;
        }
        // Find first zero bit in this word
        let word = pmm.bitmap[word_idx];
        let bit = (!word).trailing_zeros() as usize;
        let page = word_idx * 64 + bit;
        if page >= pmm.total_pages {
            break;
        }
        bitmap_set(&mut pmm.bitmap, page);
        pmm.free_pages -= 1;

        let addr = pmm.managed_start + page * PAGE_SIZE;

        // Zero the page (identity-mapped, so phys == virt before paging,
        // and after vmm_init kernel has identity mapping)
        unsafe {
            core::ptr::write_bytes(addr as *mut u8, 0, PAGE_SIZE);
        }

        return Ok(PhysAddr::new(addr));
    }

    Err(KernelError::OutOfMemory)
}

/// Free a single 4 KiB physical page.
pub fn free_page(addr: PhysAddr) -> KResult<()> {
    let mut pmm = PMM.lock();

    let phys = addr.as_usize();
    if phys < pmm.managed_start {
        return Err(KernelError::InvalidArg);
    }
    if !addr.is_page_aligned() {
        return Err(KernelError::NotAligned);
    }

    let page = (phys - pmm.managed_start) / PAGE_SIZE;
    if page >= pmm.total_pages {
        return Err(KernelError::InvalidArg);
    }
    if !bitmap_test(&pmm.bitmap, page) {
        return Err(KernelError::DoubleFree);
    }

    bitmap_clear(&mut pmm.bitmap, page);
    pmm.free_pages += 1;
    Ok(())
}

/// Get the total number of managed pages.
pub fn total_pages() -> usize {
    PMM.lock().total_pages
}

/// Get the number of currently free pages.
pub fn free_pages() -> usize {
    PMM.lock().free_pages
}

/// Get the number of currently used pages.
pub fn used_pages() -> usize {
    let pmm = PMM.lock();
    pmm.total_pages - pmm.free_pages
}

/// Reserve a range of physical memory in the PMM (mark pages as allocated).
///
/// Used after `kheap::init()` to prevent PMM from handing out heap pages
/// to later callers (e.g. VirtIO DMA buffers).
pub fn reserve_range(start: usize, size: usize) {
    let mut pmm = PMM.lock();
    if start < pmm.managed_start { return; }
    let first_page = (start - pmm.managed_start) / PAGE_SIZE;
    let num_pages  = (size + PAGE_SIZE - 1) / PAGE_SIZE;
    for i in 0..num_pages {
        let page = first_page + i;
        if page >= pmm.total_pages { break; }
        if !bitmap_test(&pmm.bitmap, page) {
            bitmap_set(&mut pmm.bitmap, page);
            if pmm.free_pages > 0 { pmm.free_pages -= 1; }
        }
    }
}

/// Return the physical address of the first free page.
///
/// Used to determine where the kernel heap should start after vmm::init()
/// has allocated all page table pages from the PMM.
pub fn next_free_addr() -> usize {
    let pmm = PMM.lock();
    for word_idx in 0..BITMAP_WORDS {
        if word_idx * 64 >= pmm.total_pages { break; }
        if pmm.bitmap[word_idx] == u64::MAX { continue; }
        let word = pmm.bitmap[word_idx];
        let bit = (!word).trailing_zeros() as usize;
        let page = word_idx * 64 + bit;
        if page < pmm.total_pages {
            return pmm.managed_start + page * PAGE_SIZE;
        }
    }
    // No free pages — return end of managed range
    pmm.managed_start + pmm.total_pages * PAGE_SIZE
}
