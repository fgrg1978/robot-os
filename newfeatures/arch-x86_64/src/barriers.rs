//! Memory barriers + cache-line flush for x86_64.
//!
//! x86's memory model (TSO — Total Store Order) is much stronger
//! than ARM's: ordinary loads + stores are already ordered to
//! the same address, and stores don't reorder past other stores.
//! That collapses most of what `arch-aarch64::cache` does to
//! no-ops on x86 — the only barriers a kernel actually needs are:
//!
//!   - **`mfence`**: serialise everything across both load + store
//!     queues. Used around MMIO / lock-free data structures where
//!     the TSO guarantees aren't enough (e.g. store-then-load
//!     reorder is allowed and `mfence` blocks it).
//!   - **`lfence`**: serialise loads. Mostly useful as a
//!     speculation barrier post-Spectre.
//!   - **`sfence`**: serialise stores. Required after non-
//!     temporal stores (movnti / movntdq).
//!   - **`clflush`** (or `clflushopt` / `clwb` on newer CPUs):
//!     flush a single cache line. Used for persistent-memory
//!     stores and non-coherent DMA hand-off.
//!   - **`wbinvd`**: write-back + invalidate the entire D-cache.
//!     Privileged, very slow, used only for suspend-to-RAM /
//!     entering AP halt.
//!
//! Mirror on aarch64: `arch-aarch64::cache` with DC/IC + DSB/ISB.
//! Same surface shape (clean / invalidate / barrier), wildly
//! different implementations because the memory models differ.

#![allow(dead_code)]

/// Standard x86 cache line size — 64 bytes since Pentium 4. CPUID
/// leaf 0x80000006 ECX[7:0] is the runtime source if you need to
/// verify on weird parts.
pub const LINE_SIZE_BYTES: usize = 64;

/// Full memory barrier — orders both loads and stores. Stronger
/// than what TSO gives for free, so use only when needed.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn mfence() {
    unsafe {
        core::arch::asm!("mfence", options(nostack, nomem, preserves_flags));
    }
}

/// Load barrier — orders loads. Mostly relevant for non-temporal
/// loads, speculation barriers, or when reading from MMIO that
/// the platform doesn't auto-serialise.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn lfence() {
    unsafe {
        core::arch::asm!("lfence", options(nostack, nomem, preserves_flags));
    }
}

/// Store barrier — orders stores. Required after non-temporal
/// stores (movnti / movntdq).
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn sfence() {
    unsafe {
        core::arch::asm!("sfence", options(nostack, nomem, preserves_flags));
    }
}

/// Flush one cache line back to memory + invalidate it from
/// every level of cache, on every cache-coherency domain that
/// might hold it. Slow per-line but exact.
///
/// # Safety
/// `addr` must be a valid kernel virtual address; `clflush` on
/// an unmapped page faults.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn clflush(addr: usize) {
    unsafe {
        core::arch::asm!(
            "clflush [{0}]",
            in(reg) addr,
            options(nostack, preserves_flags),
        );
    }
}

/// Flush a virtual range, one cache line at a time. Equivalent
/// of the aarch64 `dcache_clean_and_invalidate` op.
///
/// # Safety
/// Range must be in the current page-table mapping for the
/// entire span — `clflush` doesn't fault gracefully on holes.
#[cfg(target_arch = "x86_64")]
pub unsafe fn clflush_range(va: usize, len: usize) {
    let line = LINE_SIZE_BYTES;
    let mut p = va & !(line - 1);
    let end = va + len;
    while p < end {
        unsafe { clflush(p); }
        p += line;
    }
    sfence(); // ensure all flushes have hit memory before we return
}

/// Write-back + invalidate every line in every D-cache on this
/// CPU. Privileged (CPL=0) and extremely slow — only use during
/// suspend-to-RAM or controlled CPU offline. Does NOT cross
/// cores; the kernel must IPI every other CPU to do the same.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn wbinvd() {
    unsafe {
        core::arch::asm!("wbinvd", options(nostack, preserves_flags));
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub fn mfence() {}
#[cfg(not(target_arch = "x86_64"))]
pub fn lfence() {}
#[cfg(not(target_arch = "x86_64"))]
pub fn sfence() {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn clflush(_addr: usize) {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn clflush_range(_va: usize, _len: usize) {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn wbinvd() {}
