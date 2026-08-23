//! Shared Memory regions (F00.4).
//!
//! Allows two or more processes to map the same physical pages into their
//! address spaces for zero-copy data sharing (e.g., camera frames, LiDAR scans).

use core::sync::atomic::{AtomicU32, Ordering};
use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of shared memory regions system-wide.
pub const MAX_SHM_REGIONS: usize = 16;

/// Maximum pages per shared memory region (64 pages = 256 KiB).
pub const MAX_SHM_PAGES: usize = 64;

/// Maximum number of distinct tasks that may hold a reference to one region
/// at the same time.
///
/// WHY a bound exists at all: references are now tracked *per task*
/// (see [`ShmHolder`]), which needs somewhere to put the per-task counter.
/// A fixed array keeps the whole table in BSS with no allocator on the IPC
/// path. Eight simultaneous sharers per region is well beyond anything the
/// robot's pipelines do (producer + a handful of consumers); exhausting it
/// fails the *acquire*, it never corrupts accounting.
pub const MAX_SHM_HOLDERS: usize = 8;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Permission flags for a shared memory region.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShmPerms {
    ReadOnly,
    ReadWrite,
}

/// Per-task reference accounting for one region.
///
/// **WHY this exists (W3-F1):** `ref_count` alone is a bare integer that any
/// caller could decrement. `SYS_IPC_UNSHARE` took a raw userspace `shm_id`
/// and called `shm_release` unconditionally, so a task could drop references
/// it never took — sixteen guesses were enough to drive any live region's
/// count to zero, which frees every `phys_pages[i]` back to the PMM while the
/// real holders' `USER_RW` PTEs stay valid. Recording *who* took each
/// reference makes a release only able to give back what the caller actually
/// holds.
#[derive(Clone, Copy)]
pub struct ShmHolder {
    /// TID of the holding task. Meaningful only when `refs > 0`.
    pub tid: u32,
    /// References this task currently holds (create counts as one, each
    /// successful `shm_acquire` counts as one). `0` ⇒ free holder slot.
    pub refs: u32,
    /// User virtual base address this task mapped the region at via
    /// `shm_map_user`, or `0` if it has no live mapping.
    ///
    /// **WHY the VA is recorded:** the pages must never go back to the PMM
    /// while a user page table still points at them. Keeping the VA is what
    /// lets the release path tear the mapping down *before* the refcount can
    /// reach zero — see [`shm_take_mapping`].
    pub map_va: usize,
    /// Number of pages mapped at `map_va` (0 when `map_va == 0`).
    pub map_pages: usize,
}

impl ShmHolder {
    pub const fn empty() -> Self {
        Self { tid: 0, refs: 0, map_va: 0, map_pages: 0 }
    }
}

/// A shared memory region.
pub struct ShmRegion {
    /// Physical addresses of allocated pages (0 = unused slot in page array).
    pub phys_pages: [usize; MAX_SHM_PAGES],
    /// Number of pages allocated.
    pub page_count: usize,
    /// Reference count — how many processes have this mapped.
    ///
    /// Invariant: equals the sum of `holders[i].refs`. Kept as a separate
    /// field only because `shm_info` exposes it; the holder table is the
    /// authority.
    pub ref_count: AtomicU32,
    /// Task that created this region. Read by [`shm_owner`] — the syscall
    /// layer gates `SYS_IPC_MAP` on it.
    pub owner_task: u32,
    /// Permissions.
    pub perms: ShmPerms,
    /// Whether this slot is active.
    pub active: bool,
    /// Per-task reference accounting. See [`ShmHolder`].
    pub holders: [ShmHolder; MAX_SHM_HOLDERS],
}

impl ShmRegion {
    pub const fn empty() -> Self {
        const EMPTY_HOLDER: ShmHolder = ShmHolder::empty();
        Self {
            phys_pages: [0; MAX_SHM_PAGES],
            page_count: 0,
            ref_count: AtomicU32::new(0),
            owner_task: 0,
            perms: ShmPerms::ReadOnly,
            active: false,
            holders: [EMPTY_HOLDER; MAX_SHM_HOLDERS],
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

/// Global shm region table.
///
/// Protected by a single `SpinLock` covering the whole table (same shape as
/// `port.rs`'s `PORTS` / `lease.rs`'s `LEASES`). Was previously a bare
/// `static mut` with every accessor (`shm_create`/`shm_acquire`/
/// `shm_page_phys`/`shm_release`/`shm_info`) touching it under an `unsafe`
/// block with zero synchronization — reachable concurrently from any hart
/// via the syscall dispatch table, so e.g. two harts racing `shm_create`
/// could both find the same "free" slot and both write into it. Uses
/// `lock_irqsave()` (not plain `lock()`) for the same reason `PORTS` does:
/// keeping every accessor on the same IRQ-safe discipline is what makes it
/// safe to add an IRQ-context caller later without silently reopening a
/// same-hart deadlock.
const EMPTY_SHM: ShmRegion = ShmRegion::empty();
static SHM_REGIONS: SpinLock<[ShmRegion; MAX_SHM_REGIONS]> =
    SpinLock::new([EMPTY_SHM; MAX_SHM_REGIONS]);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a shared memory region with `page_count` pages.
/// Returns shm_id or None if no slots or OOM.
pub fn shm_create(owner_task: u32, page_count: usize, perms: ShmPerms) -> Option<u32> {
    if page_count == 0 || page_count > MAX_SHM_PAGES {
        return None;
    }

    let mut regions = SHM_REGIONS.lock_irqsave();

    // Find and claim a free slot. Marking `active` before releasing the
    // lock (we hold it for the whole function) is what stops two harts
    // racing shm_create() from finding and writing into the same slot.
    let slot = (0..MAX_SHM_REGIONS).find(|&i| !regions[i].active)?;
    regions[slot].active = true;

    let region = &mut regions[slot];

    // Allocate physical pages
    for i in 0..page_count {
        match robot_os_mm::pmm::alloc_page() {
            Ok(page) => {
                let phys = page.as_usize();
                // SAFETY: `phys` is a freshly allocated physical page from
                // the PMM (owned exclusively by this region under the lock),
                // sized PAGE_SIZE — zeroing it for security is in-bounds.
                unsafe {
                    core::ptr::write_bytes(phys as *mut u8, 0, robot_os_arch::mmu::PAGE_SIZE);
                }
                region.phys_pages[i] = phys;
            }
            Err(_) => {
                // OOM — free already-allocated pages and release the slot.
                for j in 0..i {
                    let _ = robot_os_mm::pmm::free_page(
                        robot_os_mm::addr::PhysAddr::new(region.phys_pages[j]),
                    );
                    region.phys_pages[j] = 0;
                }
                *region = ShmRegion::empty();
                return None;
            }
        }
    }

    region.page_count = page_count;
    region.ref_count.store(1, Ordering::Release);
    region.owner_task = owner_task;
    region.perms = perms;
    // The creator's initial reference is booked against *it*, so its own
    // later `shm_release` is the only thing that can give it back.
    region.holders[0] = ShmHolder {
        tid: owner_task,
        refs: 1,
        map_va: 0,
        map_pages: 0,
    };

    Some(slot as u32)
}

/// TID of the task that created `shm_id`, or `None` if the slot is not active.
///
/// **WHY this is public (W3-F1):** `owner_task` was written by `shm_create`
/// and then never read anywhere in the tree, so the field documented an
/// ownership model that nothing enforced. `SYS_IPC_MAP` now reads it to
/// reject a non-owner before mapping another task's camera / LiDAR /
/// inference buffers into its address space — the region index is a small
/// integer chosen by userspace, so without this the whole table was
/// enumerable in sixteen guesses.
pub fn shm_owner(shm_id: u32) -> Option<u32> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    let regions = SHM_REGIONS.lock_irqsave();
    let region = &regions[shm_id as usize];
    if region.active { Some(region.owner_task) } else { None }
}

/// Find the holder slot for `tid`, or the first free slot. Returns
/// `(index, is_existing)`. Caller holds the table lock.
fn holder_slot(region: &ShmRegion, tid: u32) -> Option<(usize, bool)> {
    let mut free: Option<usize> = None;
    for i in 0..MAX_SHM_HOLDERS {
        let h = &region.holders[i];
        if h.refs > 0 && h.tid == tid {
            return Some((i, true));
        }
        if h.refs == 0 && free.is_none() {
            free = Some(i);
        }
    }
    free.map(|i| (i, false))
}

/// Acquire a reference to a shared memory region **on behalf of `tid`**.
///
/// Increments the caller's per-task holder count and the region refcount.
/// Returns `(page_count, perms)` or `None` (inactive region, or the holder
/// table is full).
///
/// `tid` is threaded through (rather than read from the scheduler here) so
/// the kernel-internal callers — `shm_create_cap`'s rollback, the typed
/// `Cap<Shm>` path — stay explicit about whose reference they are taking.
pub fn shm_acquire(tid: u32, shm_id: u32) -> Option<(usize, ShmPerms)> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    let mut regions = SHM_REGIONS.lock_irqsave();
    let region = &mut regions[shm_id as usize];
    if !region.active {
        return None;
    }
    let (idx, existing) = holder_slot(region, tid)?;
    // `checked_add`: `refs` is driven by an unprivileged syscall loop, and
    // with `overflow-checks = true` a bare `+= 1` at u32::MAX would panic —
    // and a panic here is `panic = "abort"`, i.e. a board reset. Refuse the
    // acquire instead.
    let next = region.holders[idx].refs.checked_add(1)?;
    if existing {
        region.holders[idx].refs = next;
    } else {
        region.holders[idx] = ShmHolder { tid, refs: 1, map_va: 0, map_pages: 0 };
    }
    region.ref_count.fetch_add(1, Ordering::AcqRel);
    Some((region.page_count, region.perms))
}

/// Does `tid` already have a live mapping of `shm_id`?
///
/// The untyped `SYS_IPC_MAP` path records exactly one VA per (task, region)
/// so that the release path can always find the mapping it must tear down.
/// A second map by the same task is refused rather than silently creating an
/// untracked alias to pages the refcount thinks it can free.
pub fn shm_has_mapping(tid: u32, shm_id: u32) -> bool {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return false;
    }
    let regions = SHM_REGIONS.lock_irqsave();
    let region = &regions[shm_id as usize];
    if !region.active {
        return false;
    }
    for i in 0..MAX_SHM_HOLDERS {
        let h = &region.holders[i];
        if h.refs > 0 && h.tid == tid && h.map_va != 0 {
            return true;
        }
    }
    false
}

/// Record that `tid` mapped `shm_id` at `va` for `pages` pages.
///
/// Returns `false` if the caller holds no reference, already has a mapping
/// recorded, or the arguments are degenerate — in every one of those cases
/// the caller must undo its mapping, because an unrecorded mapping is
/// precisely the state that lets `shm_release` free pages out from under a
/// live user PTE.
pub fn shm_note_mapping(tid: u32, shm_id: u32, va: usize, pages: usize) -> bool {
    if shm_id as usize >= MAX_SHM_REGIONS || va == 0 || pages == 0 {
        return false;
    }
    let mut regions = SHM_REGIONS.lock_irqsave();
    let region = &mut regions[shm_id as usize];
    if !region.active {
        return false;
    }
    for i in 0..MAX_SHM_HOLDERS {
        let h = &mut region.holders[i];
        if h.refs > 0 && h.tid == tid {
            if h.map_va != 0 {
                return false; // already mapped — see shm_has_mapping()
            }
            h.map_va = va;
            h.map_pages = pages;
            return true;
        }
    }
    false
}

/// Clear and return `tid`'s recorded mapping of `shm_id` as `(va, pages)`.
///
/// The syscall layer calls this **before** `shm_release` and unmaps the
/// returned range from the caller's page table. That ordering is the whole
/// invariant: no reference may be dropped while the dropper still has PTEs
/// pointing into the region, so the refcount can never reach zero — and the
/// pages can never return to the PMM — with a live user mapping outstanding.
pub fn shm_take_mapping(tid: u32, shm_id: u32) -> Option<(usize, usize)> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    let mut regions = SHM_REGIONS.lock_irqsave();
    let region = &mut regions[shm_id as usize];
    if !region.active {
        return None;
    }
    for i in 0..MAX_SHM_HOLDERS {
        let h = &mut region.holders[i];
        if h.refs > 0 && h.tid == tid && h.map_va != 0 {
            let out = (h.map_va, h.map_pages);
            h.map_va = 0;
            h.map_pages = 0;
            return Some(out);
        }
    }
    None
}

/// Get the physical address of page `page_idx` in a shared memory region.
pub fn shm_page_phys(shm_id: u32, page_idx: usize) -> Option<usize> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    let regions = SHM_REGIONS.lock_irqsave();
    let region = &regions[shm_id as usize];
    if !region.active || page_idx >= region.page_count {
        return None;
    }
    Some(region.phys_pages[page_idx])
}

/// Release one reference to a shared memory region **held by `tid`**.
///
/// Returns `true` iff a reference was actually dropped.
///
/// Two refusals, both load-bearing (W3-F1):
///
///  1. **No reference held.** A caller that never acquired gets `false` and
///     the count is untouched. The old signature took only `shm_id`, so
///     `SYS_IPC_UNSHARE(n)` in a loop drove any region to zero and freed its
///     pages back to the PMM — a write-after-free with no race at all, since
///     the real holder's `USER_RW` PTEs survive the free and the frames get
///     reissued to somebody else.
///  2. **Caller still has a live mapping.** Dropping a reference while the
///     dropper's own page table still points into the region is the same
///     hazard one step removed. Callers must `shm_take_mapping` + unmap
///     first; see [`shm_take_mapping`].
///
/// Freeing the pages only when the *last* reference goes away is unchanged.
pub fn shm_release(tid: u32, shm_id: u32) -> bool {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return false;
    }
    let mut regions = SHM_REGIONS.lock_irqsave();
    let region = &mut regions[shm_id as usize];
    if !region.active {
        return false;
    }

    let mut found: Option<usize> = None;
    for i in 0..MAX_SHM_HOLDERS {
        let h = &region.holders[i];
        if h.refs > 0 && h.tid == tid {
            // Refuse while this holder still has PTEs into the region.
            if h.map_va != 0 {
                return false;
            }
            found = Some(i);
            break;
        }
    }
    let idx = match found {
        Some(i) => i,
        None => return false,
    };

    // `saturating_sub` rather than `- 1`: `refs > 0` is checked above, but
    // with `overflow-checks = true` an underflow here would abort the board.
    region.holders[idx].refs = region.holders[idx].refs.saturating_sub(1);
    if region.holders[idx].refs == 0 {
        region.holders[idx] = ShmHolder::empty();
    }

    let prev = region.ref_count.load(Ordering::Acquire);
    let now = prev.saturating_sub(1);
    region.ref_count.store(now, Ordering::Release);
    if now == 0 {
        // Last reference — and, by the refusal above, no holder can still
        // have a mapping, so freeing the frames is safe.
        for i in 0..region.page_count {
            if region.phys_pages[i] != 0 {
                let _ = robot_os_mm::pmm::free_page(
                    robot_os_mm::addr::PhysAddr::new(region.phys_pages[i]),
                );
            }
        }
        *region = ShmRegion::empty();
    }
    true
}

/// Drop every reference `tid` holds across all regions — called from the
/// task-exit hook.
///
/// **WHY the exit hook must do this (W3-F1):** per-task reference accounting
/// means a dead task's references are otherwise never given back, so one
/// crashed consumer pins a region (and its pages) for the life of the board.
/// Freeing them here is safe precisely because the exiting task's address
/// space stops being reachable: `task_exit` is the last thing that runs on
/// that task, nothing will ever dispatch into its page table again, and the
/// slot is only recycled by `do_schedule` after the context switch away.
///
/// The mapping record is cleared without unmapping for the same reason —
/// there is no live execution context left that could reach those PTEs.
pub fn shm_release_all(tid: u32) {
    for id in 0..MAX_SHM_REGIONS {
        loop {
            // Clear the mapping record first so `shm_release` will proceed;
            // see the note above on why not unmapping is sound here.
            let _ = shm_take_mapping(tid, id as u32);
            if !shm_release(tid, id as u32) {
                break;
            }
        }
    }
}

/// Get info about a shared memory region: (page_count, ref_count, perms).
pub fn shm_info(shm_id: u32) -> Option<(usize, u32, ShmPerms)> {
    if shm_id as usize >= MAX_SHM_REGIONS {
        return None;
    }
    let regions = SHM_REGIONS.lock_irqsave();
    let region = &regions[shm_id as usize];
    if !region.active {
        return None;
    }
    Some((
        region.page_count,
        region.ref_count.load(Ordering::Acquire),
        region.perms,
    ))
}

// ──────────────────────────────────────────────────────────────────────────
// Cap<Shm> typed wrappers (RFC-0003 W5 batch 2)
// ──────────────────────────────────────────────────────────────────────────
//
// Same shape as `port_*_cap`: each typed entry validates the cap against
// the caller's per-task `CapTable`, then delegates to the existing
// integer-handle logic. `shm_create_cap` allocates a region *and* mints
// the cap atomically (it cannot leave a region orphaned on cap-table
// exhaustion — the region is freed if the grant fails).

/// Errors returned by the typed `shm_*_cap` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShmCapError {
    /// Capability dereference failed (stale / wrong kind / missing perms).
    Cap(crate::cap::CapError),
    /// Out of memory or no free region slot.
    NoMem,
    /// Caller asked for 0 pages or > [`MAX_SHM_PAGES`].
    BadArg,
    /// Region doesn't exist or has been released.
    Closed,
    /// Cap-table slot table is full — the region was rolled back.
    Full,
}

impl From<crate::cap::CapError> for ShmCapError {
    fn from(e: crate::cap::CapError) -> Self {
        Self::Cap(e)
    }
}

/// Derive `CapPerms` for a freshly-minted Shm cap from the region's
/// own access mode. ReadOnly → READ; ReadWrite → RW. Callers that
/// want a more restricted grant (e.g. duplicating read-only into a
/// child) can `revoke` + `grant` with a tighter mask afterwards.
fn cap_perms_for(perms: ShmPerms) -> crate::cap::CapPerms {
    match perms {
        ShmPerms::ReadOnly => crate::cap::CapPerms::READ,
        ShmPerms::ReadWrite => crate::cap::CapPerms::RW,
    }
}

/// Typed `shm_create`: allocates a region with `page_count` pages and
/// mints a `Cap<Shm>` into `tid`'s cap-table.
///
/// On cap-table exhaustion the region is released so the caller never
/// observes a partial state.
pub fn shm_create_cap(
    tid: u32,
    page_count: usize,
    perms: ShmPerms,
) -> Result<crate::cap::Cap<crate::cap::targets::Shm>, ShmCapError> {
    if page_count == 0 || page_count > MAX_SHM_PAGES {
        return Err(ShmCapError::BadArg);
    }
    let shm_id = shm_create(tid, page_count, perms).ok_or(ShmCapError::NoMem)?;
    match crate::cap_store::grant::<crate::cap::targets::Shm>(
        tid,
        cap_perms_for(perms),
        shm_id,
    ) {
        Some(cap) => Ok(cap),
        None => {
            // Roll back the region so we don't leak it on cap-table
            // exhaustion. `shm_create` booked ref_count=1 against `tid`, so a
            // single `shm_release(tid, ..)` will free all pages and clear the
            // slot. The region has never been mapped at this point, so the
            // "no live mapping" refusal cannot fire.
            shm_release(tid, shm_id);
            Err(ShmCapError::Full)
        }
    }
}

/// Typed `shm_acquire`: validates the cap (requires `READ`) and bumps
/// `tid`'s reference on the region. Returns `(page_count, perms)`.
///
/// `tid` must be the task the cap table belongs to — the reference is
/// booked against it, and only it can give the reference back.
pub fn shm_acquire_cap(
    tid: u32,
    table: &crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::Shm>,
) -> Result<(usize, ShmPerms), ShmCapError> {
    let shm_id = table.get(cap, crate::cap::CapPerms::READ)?;
    shm_acquire(tid, shm_id).ok_or(ShmCapError::Closed)
}

/// Typed `shm_release`: validates the cap (requires `READ`, since
/// release is paired with acquire and any holder may drop its ref),
/// decrements `tid`'s reference, **and revokes the cap**.
///
/// **WHY the cap is revoked here (W3-F5):** shm ids are handed out
/// first-free-slot, and `CapTable::get` only validates the cap-table slot's
/// own generation — which a region teardown does not touch. Leaving the cap
/// live after the reference is given up means that once the region is freed
/// and id `n` is reissued to a different task, the stale cap still
/// dereferences to `n` and drives somebody else's region: a textbook
/// confused deputy. The previous doc explicitly told callers to revoke
/// separately; nothing did.
pub fn shm_release_cap(
    tid: u32,
    table: &mut crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::Shm>,
) -> Result<(), ShmCapError> {
    let shm_id = table.get(cap, crate::cap::CapPerms::READ)?;
    let dropped = shm_release(tid, shm_id);
    table.revoke(cap);
    if dropped { Ok(()) } else { Err(ShmCapError::Closed) }
}
