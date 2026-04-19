/// vDSO — Virtual Dynamic Shared Object (M01).
///
/// A single read-only physical page is shared into every user process at a
/// fixed virtual address (VDSO_USER_BASE).  The kernel writes monotonic timing
/// data to this page under a seqlock; user-space reads it without issuing an
/// ecall, eliminating syscall overhead for the most common time queries.
///
/// ## Seqlock protocol
/// Writer (kernel, timer ISR):
///   1. seq += 1  →  odd   (write in progress)
///   2. store data fields
///   3. seq += 1  →  even  (data stable)
///
/// Reader (user-space via libsys):
///   loop:
///     seq1 = load seq;  if seq1 is odd → spin
///     read data fields
///     seq2 = load seq;  if seq2 != seq1 → retry
///     // data is consistent
///
/// VDSO_USER_BASE is exported to libsys so it can read without a syscall.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use robot_os_arch::mmu::PAGE_SIZE;
use crate::pmm;

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// Fixed user-space virtual address of the vDSO page.
/// Placed at 0x5000_0000 — well within Sv39 user space and below the stack
/// (USER_STACK_TOP = 0x8000_0000), well above typical ELF load addresses.
pub const VDSO_USER_BASE: usize = 0x5000_0000;

/// Magic value stored at the start of the vDSO page.
pub const VDSO_MAGIC: u32 = 0x5644_534F; // "VDSO"

/// Kernel version encoded as (major << 16 | minor << 8 | patch).
pub const VDSO_KERNEL_VERSION: u32 = (0 << 16) | (1 << 8) | 0; // 0.1.0

// ---------------------------------------------------------------------------
// VdsoData — layout of the vDSO page (first 32 bytes)
// ---------------------------------------------------------------------------

/// Data written by the kernel into the vDSO page.
///
/// # Safety
/// This struct is placed at a physical address returned by `pmm::alloc_page`.
/// All fields are accessed through raw pointers with volatile semantics.
/// The seqlock (seq field) guards consistency.
#[repr(C, align(8))]
pub struct VdsoData {
    /// VDSO_MAGIC — lets userspace verify the page is mapped correctly.
    pub magic: AtomicU32,
    /// Kernel version (major.minor.patch packed into u32).
    pub kernel_version: AtomicU32,
    /// Seqlock counter.  Even = data stable, odd = write in progress.
    pub seq: AtomicU32,
    pub _pad: AtomicU32,
    /// Monotonic tick counter (incremented every timer IRQ).
    pub uptime_ticks: AtomicU64,
    /// Milliseconds since boot.
    pub uptime_ms: AtomicU64,
}

// ---------------------------------------------------------------------------
// Kernel-side state
// ---------------------------------------------------------------------------

/// Physical address of the vDSO page (0 = not initialised).
static VDSO_PHYS: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Kernel API
// ---------------------------------------------------------------------------

/// Allocate and initialise the vDSO page.  Called once during boot.
pub fn vdso_init() {
    if let Ok(page) = pmm::alloc_page() {
        let phys = page.as_usize();

        // Zero the page first.
        unsafe { core::ptr::write_bytes(phys as *mut u8, 0, PAGE_SIZE); }

        // Write the magic and version (seq = 0 = stable, no data yet).
        let data = unsafe { &*(phys as *const VdsoData) };
        data.magic.store(VDSO_MAGIC, Ordering::Release);
        data.kernel_version.store(VDSO_KERNEL_VERSION, Ordering::Release);

        VDSO_PHYS.store(phys as u64, Ordering::Release);
    }
}

/// Return the physical address of the vDSO page, or 0 if not initialised.
pub fn vdso_phys() -> usize {
    VDSO_PHYS.load(Ordering::Acquire) as usize
}

/// Update the vDSO timing data.  Called from the timer ISR.
///
/// Uses the seqlock write protocol: increment seq to odd, write, increment
/// to even.  All stores use Release ordering so readers see consistent data.
#[inline]
pub fn vdso_update(uptime_ticks: u64, uptime_ms: u64) {
    let phys = VDSO_PHYS.load(Ordering::Relaxed) as usize;
    if phys == 0 { return; }

    // SAFETY: phys is a valid page allocated at init time; this is the sole
    // writer (timer ISR, single-threaded per core; other cores only read).
    let data = unsafe { &*(phys as *const VdsoData) };

    // Seqlock: open write (seq → odd)
    let seq = data.seq.load(Ordering::Relaxed);
    data.seq.store(seq.wrapping_add(1), Ordering::Release);
    core::sync::atomic::fence(Ordering::SeqCst);

    data.uptime_ticks.store(uptime_ticks, Ordering::Release);
    data.uptime_ms.store(uptime_ms, Ordering::Release);

    core::sync::atomic::fence(Ordering::SeqCst);
    // Seqlock: close write (seq → even)
    data.seq.store(seq.wrapping_add(2), Ordering::Release);
}
