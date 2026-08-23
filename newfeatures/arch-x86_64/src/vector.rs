//! SSE2-based vector helpers for x86_64.
//!
//! SSE2 is part of the x86_64 baseline ABI (System V AMD64 spec),
//! so every CPU that can run a 64-bit binary supports it. That
//! means we don't need a CPUID probe before using SSE2 intrinsics
//! — mirror of how NEON is mandatory on ARMv8 in the aarch64 impl.
//!
//! AVX/AVX2/AVX-512 upgrades will land as separate functions
//! gated on a one-shot CPUID probe; for now SSE2 is the floor and
//! the dispatch in `api_impl::Vector` always takes this path.
//!
//! ## Soft-float caveat
//!
//! The upstream `x86_64-unknown-none` target uses `+soft-float` +
//! `-sse` (and friends). On that target the intrinsics below still
//! compile — `#[target_feature(enable = "sse2")]` quiets the
//! checker — but LLVM lowers them to scalar soft-float helpers, so
//! the disassembly contains zero `xmm` register references.
//!
//! That is fine for a tier-0 boot binary that just needs to call
//! into the trait; it's *not* fine for a kernel that genuinely
//! wants SSE2 throughput. The plan for the real PHANES x86_64
//! kernel target is a `x86_64-phanes-kernel.json` custom spec
//! with `+sse,+sse2,-soft-float` (and a context-switch path that
//! actually saves XMM state). Until that lands, callers can
//! request it locally via `RUSTFLAGS=-C target-feature=+sse2,-soft-float`
//! on a hard-float target — never on `x86_64-unknown-none`, which
//! refuses the override at link time.

/// SSE2 4×f32 dot product. Returns the scalar fallback's result.
///
/// # Safety
/// `target_feature = "sse2"` makes this `unsafe` even though SSE2
/// is always available on x86_64 — the attribute itself imposes
/// the restriction. The intrinsics inside are safe to call once
/// the feature is enabled.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
pub unsafe fn dot_f32_sse2(a: &[f32], b: &[f32]) -> f32 {
    use core::arch::x86_64::{
        _mm_add_ps, _mm_loadu_ps, _mm_mul_ps, _mm_setzero_ps, _mm_storeu_ps,
    };

    let n = a.len().min(b.len());
    let chunks = n / 4;
    let tail = n - chunks * 4;

    unsafe {
        let mut acc = _mm_setzero_ps();
        let mut ap = a.as_ptr();
        let mut bp = b.as_ptr();
        for _ in 0..chunks {
            let va = _mm_loadu_ps(ap);
            let vb = _mm_loadu_ps(bp);
            acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
            ap = ap.add(4);
            bp = bp.add(4);
        }

        // Horizontal sum without SSE3 `_mm_hadd_ps` — keeps the
        // floor at SSE2 (mandatory baseline). Spill the 4-lane
        // accumulator to scalar memory and fold sequentially.
        let mut lanes = [0.0f32; 4];
        _mm_storeu_ps(lanes.as_mut_ptr(), acc);
        let mut sum = lanes[0] + lanes[1] + lanes[2] + lanes[3];

        for i in 0..tail {
            sum += *ap.add(i) * *bp.add(i);
        }
        sum
    }
}

/// Pure-scalar fallback. Kept for audit comparison against the
/// SIMD path — invariant: `(dot_f32_sse2(a, b) - dot_f32_scalar(a,
/// b)).abs() < f32::EPSILON * n` for any same-length inputs.
pub fn dot_f32_scalar(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut sum = 0.0_f32;
    for i in 0..n {
        sum += a[i] * b[i];
    }
    sum
}

/// AVX (256-bit) 8×f32 dot product — doubles the SSE2 lane width.
///
/// Detected via the one-shot CPUID probe in [`has_avx`]; the
/// dispatcher in `api_impl::Vector::dot_f32` only takes this path
/// if the probe returned `true`.
///
/// # Safety
/// Caller must ensure the CPU supports AVX. The intrinsics
/// themselves are safe once `target_feature = "avx"` is enabled.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
pub unsafe fn dot_f32_avx(a: &[f32], b: &[f32]) -> f32 {
    use core::arch::x86_64::{
        _mm256_add_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_setzero_ps,
        _mm256_storeu_ps,
    };

    let n = a.len().min(b.len());
    let chunks = n / 8;
    let tail = n - chunks * 8;

    unsafe {
        let mut acc = _mm256_setzero_ps();
        let mut ap = a.as_ptr();
        let mut bp = b.as_ptr();
        for _ in 0..chunks {
            let va = _mm256_loadu_ps(ap);
            let vb = _mm256_loadu_ps(bp);
            acc = _mm256_add_ps(acc, _mm256_mul_ps(va, vb));
            ap = ap.add(8);
            bp = bp.add(8);
        }
        // Horizontal sum via scalar fold of the 8 lanes — keeps
        // the floor at plain AVX (no AVX2 `_mm256_hadd_ps`).
        let mut lanes = [0.0f32; 8];
        _mm256_storeu_ps(lanes.as_mut_ptr(), acc);
        let mut sum = lanes[0] + lanes[1] + lanes[2] + lanes[3]
                    + lanes[4] + lanes[5] + lanes[6] + lanes[7];
        for i in 0..tail {
            sum += *ap.add(i) * *bp.add(i);
        }
        sum
    }
}

// ── CPUID-based AVX detection ──────────────────────────────────

/// One-shot AVX-availability probe. Caches the result so repeated
/// `dot_f32` calls don't re-issue CPUID.
#[cfg(target_arch = "x86_64")]
fn has_avx() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    /// 0 = unprobed, 1 = no AVX, 2 = AVX present.
    static CACHE: AtomicU8 = AtomicU8::new(0);
    match CACHE.load(Ordering::Acquire) {
        1 => false,
        2 => true,
        _ => {
            // CPUID leaf 1 → ECX bit 28 = AVX (256-bit). Also
            // gate on bit 27 (OSXSAVE) — without OSXSAVE the OS
            // hasn't enabled YMM via XCR0 and AVX traps with #UD.
            let cpuid = unsafe { core::arch::x86_64::__cpuid(1) };
            let osxsave = (cpuid.ecx >> 27) & 1 != 0;
            let avx     = (cpuid.ecx >> 28) & 1 != 0;
            let v = if osxsave && avx { 2 } else { 1 };
            CACHE.store(v, Ordering::Release);
            v == 2
        }
    }
}

/// Dispatcher used by `api_impl::Vector::dot_f32` — picks the
/// widest path the CPU supports. AVX (8-lane) when available,
/// SSE2 (4-lane) otherwise.
#[cfg(target_arch = "x86_64")]
pub fn dot_f32_best(a: &[f32], b: &[f32]) -> f32 {
    if has_avx() {
        unsafe { dot_f32_avx(a, b) }
    } else {
        unsafe { dot_f32_sse2(a, b) }
    }
}

/// Diagnostic accessor — what backend `dot_f32_best` will pick.
/// Useful for procfs / boot-log reporting.
#[cfg(target_arch = "x86_64")]
pub fn active_backend() -> &'static str {
    if has_avx() { "avx" } else { "sse2" }
}
