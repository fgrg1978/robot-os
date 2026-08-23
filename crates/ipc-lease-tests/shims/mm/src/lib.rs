//! Host stand-in for `robot_os_mm` — the page allocator only.
//!
//! **WHY this exists.** `crates/ipc/src/io_ring.rs` allocates one physical
//! page per ring and dereferences it as `*mut IoRing`. The real
//! `robot_os_mm::pmm` manages the board's physical bitmap and cannot run on
//! the host, so the `#[path]` trick stops at `alloc_page()`.
//!
//! **What this is for.** The property under test in `io_ring_release_all` is
//! not *which* physical page is used — it is **whether the page is freed, and
//! whether it is freed before or after the in-flight submit pass ends**. A
//! counting allocator answers exactly that: `shim_free_count(addr)` proves
//! "freed exactly once, and not one instruction earlier". Pages come from a
//! 4 KiB-aligned static pool so the ring's atomics are properly aligned and
//! the module's real writes land in real memory.
//!
//! Pulled in under the name `robot_os_mm` via a Cargo dependency rename. The
//! kernel never sees it.

use std::cell::UnsafeCell;
use std::sync::Mutex;

pub mod addr {
    /// Same surface `io_ring.rs` uses: `PhysAddr::new` / `as_usize`.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct PhysAddr(usize);

    impl PhysAddr {
        pub const fn new(a: usize) -> Self {
            PhysAddr(a)
        }
        pub const fn as_usize(&self) -> usize {
            self.0
        }
    }
}

pub const PAGE_SIZE: usize = 4096;
/// `MAX_IO_RINGS` is 16; a couple spare so exhaustion tests hit the ring
/// table's limit rather than the allocator's.
pub const SHIM_PAGES: usize = 24;

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct Page([u8; PAGE_SIZE]);

struct Pool(UnsafeCell<[Page; SHIM_PAGES]>);
// Access is serialised by `STATE` below; the tests are single-threaded per
// module anyway (see each suite's SERIAL mutex).
unsafe impl Sync for Pool {}

static POOL: Pool = Pool(UnsafeCell::new([Page([0u8; PAGE_SIZE]); SHIM_PAGES]));

struct State {
    used: [bool; SHIM_PAGES],
    /// How many times each page has been handed to `free_page`. Anything
    /// other than 0 or 1 at the end of a test is a double free.
    frees: [u32; SHIM_PAGES],
}

static STATE: Mutex<State> = Mutex::new(State {
    used: [false; SHIM_PAGES],
    frees: [0; SHIM_PAGES],
});

fn base() -> usize {
    POOL.0.get() as usize
}

fn index_of(a: usize) -> Option<usize> {
    let b = base();
    if a < b {
        return None;
    }
    let off = a - b;
    if off % PAGE_SIZE != 0 {
        return None;
    }
    let i = off / PAGE_SIZE;
    if i < SHIM_PAGES {
        Some(i)
    } else {
        None
    }
}

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

pub mod pmm {
    use super::*;
    pub use crate::addr::PhysAddr;

    /// The real signature is `KResult<PhysAddr>`; `io_ring.rs` only ever calls
    /// `.ok()?` on it, so any `Result` with the same shape works.
    pub fn alloc_page() -> Result<PhysAddr, ()> {
        let mut s = state();
        for i in 0..SHIM_PAGES {
            if !s.used[i] {
                s.used[i] = true;
                // The real `alloc_page` hands back a zeroed page and
                // `io_ring_create`'s comment relies on that.
                unsafe {
                    let p = (base() + i * PAGE_SIZE) as *mut u8;
                    std::ptr::write_bytes(p, 0, PAGE_SIZE);
                }
                return Ok(PhysAddr::new(base() + i * PAGE_SIZE));
            }
        }
        Err(())
    }

    pub fn free_page(a: PhysAddr) -> Result<(), ()> {
        let i = match index_of(a.as_usize()) {
            Some(i) => i,
            None => return Err(()),
        };
        let mut s = state();
        s.frees[i] += 1;
        s.used[i] = false;
        Ok(())
    }

    pub fn free_pages() -> usize {
        let s = state();
        SHIM_PAGES - s.used.iter().filter(|u| **u).count()
    }
}

// ── Test-only observability ────────────────────────────────────────────────

pub fn shim_reset() {
    let mut s = state();
    s.used = [false; SHIM_PAGES];
    s.frees = [0; SHIM_PAGES];
}

/// How many times the page at `addr` has been freed. 0 = still held,
/// 1 = released cleanly, >1 = double free.
pub fn shim_free_count(addr: usize) -> u32 {
    match index_of(addr) {
        Some(i) => state().frees[i],
        None => u32::MAX,
    }
}

/// Pages currently checked out of the pool.
pub fn shim_pages_in_use() -> usize {
    state().used.iter().filter(|u| **u).count()
}
