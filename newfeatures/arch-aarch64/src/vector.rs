//! NEON-backed Vector implementations.
//!
//! ARMv8-A makes Advanced SIMD (NEON) mandatory — every PE that
//! executes aarch64 instructions supports it. That's the
//! opposite of x86 (AVX is optional) and RISC-V (V is optional);
//! no runtime probe is needed.
//!
//! Scope today: just `dot_f32`, which the kernel's ML inner
//! loops use. SVE / SVE2 (variable-length vectors) is a B1.sve
//! follow-up — it offers larger throughput on Cortex-A510 /
//! Neoverse-V1 but requires runtime detection and is not on
//! Cortex-A72 (the QEMU virt default).

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::{vaddvq_f32, vdupq_n_f32, vfmaq_f32, vld1q_f32};

/// NEON-accelerated f32 dot product. Loads 4 lanes at a time
/// via VLD1, accumulates via VFMA (fused multiply-add), and
/// horizontally sums via VADDVQ. Tail elements (< 4 left) get
/// scalar treatment.
///
/// # Safety
///
/// `target_feature(enable = "neon")` is required for the NEON
/// intrinsics; on ARMv8 NEON is mandatory so this is always
/// safe to call from aarch64 code, but Rust's
/// target-feature-attribute discipline requires `unsafe fn`.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub unsafe fn dot_f32_neon(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(
        a.len(),
        b.len(),
        "dot_f32_neon: input slice lengths differ",
    );
    let n = a.len().min(b.len());
    let chunks = n / 4;
    let tail = n - chunks * 4;

    unsafe {
        let mut acc = vdupq_n_f32(0.0);
        let mut ap = a.as_ptr();
        let mut bp = b.as_ptr();
        for _ in 0..chunks {
            let va = vld1q_f32(ap);
            let vb = vld1q_f32(bp);
            acc = vfmaq_f32(acc, va, vb);
            ap = ap.add(4);
            bp = bp.add(4);
        }
        let mut sum = vaddvq_f32(acc);
        // Handle tail (0..=3 elements).
        for i in 0..tail {
            sum += *ap.add(i) * *bp.add(i);
        }
        sum
    }
}

/// Scalar fallback. Kept compiled even on aarch64 so tests +
/// auditors can compare numeric results between the SIMD and
/// scalar paths (the two should agree bit-for-bit modulo FP
/// associativity — the SIMD form accumulates in a different
/// order, so the result CAN drift by ~1 ULP for long inputs).
pub fn dot_f32_scalar(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut acc: f32 = 0.0;
    for i in 0..n {
        acc += a[i] * b[i];
    }
    acc
}

// ── SVE detection (ID_AA64PFR0_EL1.SVE bits [35:32]) ─────────
//
// Symmetric to x86_64::vector's CPUID-based AVX detection. SVE
// (Scalable Vector Extension) is optional in ARMv8.2+ and isn't
// present on Cortex-A72 (the QEMU virt default), so the current
// dispatcher always falls back to NEON. The detection +
// `dot_f32_best` shim are in place so:
//
//   1. The kernel can already query "what's the widest path?"
//      and surface it through procfs (mirror of `active_backend`
//      on the x86_64 side).
//   2. When we eventually run on a Neoverse / Cortex-X core
//      with SVE, plugging in a real `dot_f32_sve` is a one-
//      function add — the dispatcher already exists.

/// Read ID_AA64PFR0_EL1 and return true iff the SVE field
/// (bits [35:32]) is non-zero. Caches the answer in a static so
/// subsequent calls are a single atomic load.
#[cfg(target_arch = "aarch64")]
pub fn has_sve() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    /// 0 = unprobed, 1 = no SVE, 2 = SVE present.
    static CACHE: AtomicU8 = AtomicU8::new(0);
    match CACHE.load(Ordering::Acquire) {
        1 => false,
        2 => true,
        _ => {
            let pfr0: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {0}, ID_AA64PFR0_EL1",
                    out(reg) pfr0,
                    options(nomem, nostack, preserves_flags),
                );
            }
            let sve_field = (pfr0 >> 32) & 0xF;
            let v = if sve_field != 0 { 2 } else { 1 };
            CACHE.store(v, Ordering::Release);
            v == 2
        }
    }
}

/// Dispatcher used by `api_impl::Vector::dot_f32`. SVE path is
/// not implemented yet (no test hardware) so this always falls
/// back to NEON; the structure mirrors the x86_64 AVX shim so
/// the migration story is uniform across the two arches.
#[cfg(target_arch = "aarch64")]
pub fn dot_f32_best(a: &[f32], b: &[f32]) -> f32 {
    // SVE path lives here once we have Cortex-X / Neoverse:
    //   if has_sve() { unsafe { dot_f32_sve(a, b) } } else { … }
    let _ = has_sve(); // prime the cache for `active_backend`
    unsafe { dot_f32_neon(a, b) }
}

/// Diagnostic accessor — what backend `dot_f32_best` will pick
/// today. Will return "sve" once a real impl lands.
#[cfg(target_arch = "aarch64")]
pub fn active_backend() -> &'static str {
    if has_sve() { "neon (sve detected but no impl yet)" } else { "neon" }
}
