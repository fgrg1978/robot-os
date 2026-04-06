//! Shared Memory regions (F00.4).
//!
//! Allows two or more processes to map the same physical pages into their
//! address spaces for zero-copy data sharing (e.g., camera frames, LiDAR scans).

use core::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of shared memory regions system-wide.
pub const MAX_SHM_REGIONS: usize = 16;

/// Maximum pages per shared memory region (64 pages = 256 KiB).
pub const MAX_SHM_PAGES: usize = 64;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Permission flags for a shared memory region.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShmPerms {
    ReadOnly,
    ReadWrite,
}

/// A shared memory region.
pub struct ShmRegion {
    /// Physical addresses of allocated pages (0 = unused slot in page array).
    pub phys_pages: [usize; MAX_SHM_PAGES],
    /// Number of pages allocated.
    pub page_count: usize,
    /// Reference count — how many processes have this mapped.
    pub ref_count: AtomicU32,
    /// Task that created this region.
    pub owner_task: u32,
    /// Permissions.
    pub perms: ShmPerms,
    /// Whether this slot is active.
    pub active: bool,
}

impl ShmRegion {
    pub const fn empty() -> Self {
        Self {
            phys_pages: [0; MAX_SHM_PAGES],
            page_count: 0,
            ref_count: AtomicU32::new(0),
            owner_task: 0,
            perms: ShmPerms::ReadOnly,
            active: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static mut SHM_REGIONS: [ShmRegion; MAX_SHM_REGIONS] = {
    const EMPTY: ShmRegion = ShmRegion::empty();
    [EMPTY; MAX_SHM_REGIONS]
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a shared memory region with `page_count` pages.
/// Returns shm_id or None if no slots or OOM.
pub fn shm_create(owner_task: u32, page_count: usize, perms: ShmPerms) -> Option<u32> {
    if page_count == 0 || page_count > MAX_SHM_PAGES {
        return None;
    }

    unsafe {
        // Find free slot
        let slot = (0..MAX_SHM_REGIONS).find(|&i| !SHM_REGIONS[i].active)?;
        let region = &mut SHM_REGIONS[slot];

        // Allocate physical pages
        for i in 0..page_count {
            match robot_os_mm::pmm::alloc_page() {
                Ok(page) => {
                    let phys = page.as_usize();
                    // Zero the page for security
                    core::ptr::write_bytes(phys as *mut u8, 0, robot_os_arch::mmu::PAGE_SIZE);
                    region.phys_pages[i] = phys;
                }
                Err(_) => {
                    // OOM — free already-allocated pages
                    for j in 0..i {
                        let _ = robot_os_mm::pmm::free_page(
                            robot_os_mm::addr::PhysAddr::new(region.phys_pages[j]),
                        );
                        region.phys_pages[j] = 0;
                    }
                    return None;
                }
            }
        }

        region.page_count = page_count;
        region.ref_count.store(1, Ordering::Release);
        region.owner_task = owner_task;
        region.perms = perms;
        region.active = true;

        Some(slot as u32)
    }
}

/// Acquire a reference to a shared memory region.
/// Increments ref_count. Returns (phys_pages slice, page_count, perms) or None.
pub fn shm_acquire(shm_id: u32) -> Option<(usize, ShmPerms)> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    unsafe {
        let region = &SHM_REGIONS[shm_id as usize];
        if !region.active {
            return None;
        }
        region.ref_count.fetch_add(1, Ordering::AcqRel);
        Some((region.page_count, region.perms))
    }
}

/// Get the physical address of page `page_idx` in a shared memory region.
pub fn shm_page_phys(shm_id: u32, page_idx: usize) -> Option<usize> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    unsafe {
        let region = &SHM_REGIONS[shm_id as usize];
        if !region.active || page_idx >= region.page_count {
            return None;
        }
        Some(region.phys_pages[page_idx])
    }
}

/// Release a reference to a shared memory region.
/// If ref_count reaches 0, free all physical pages.
pub fn shm_release(shm_id: u32) {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return;
    }
    unsafe {
        let region = &mut SHM_REGIONS[shm_id as usize];
        if !region.active {
            return;
        }
        let prev = region.ref_count.fetch_sub(1, Ordering::AcqRel);
        if prev <= 1 {
            // Last reference — free all pages
            for i in 0..region.page_count {
                if region.phys_pages[i] != 0 {
                    let _ = robot_os_mm::pmm::free_page(
                        robot_os_mm::addr::PhysAddr::new(region.phys_pages[i]),
                    );
                }
            }
            *region = ShmRegion::empty();
        }
    }
}

/// Get info about a shared memory region: (page_count, ref_count, perms).
pub fn shm_info(shm_id: u32) -> Option<(usize, u32, ShmPerms)> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    unsafe {
        let region = &SHM_REGIONS[shm_id as usize];
        if !region.active {
            return None;
        }
        Some((
            region.page_count,
            region.ref_count.load(Ordering::Acquire),
            region.perms,
        ))
    }
}
