//! RISC-V Vector Extension (RVV 1.0) — float32 math kernel.
//!
//! Feature gate : `rvv`
//! QEMU target  : `make qemu-rvv`  (-cpu rv64,v=true,vlen=128,vext_spec=v1.0)
//! VisionFive 2 : NOT supported — SiFive U74 has no V extension.
//!
//! Phase 12+ will use these primitives for the embedded ML runtime.
//!
//! TODO (Phase 12): Save/restore vector registers (v0-v31, vl, vtype, vstart)
//!   on context switch.  Until then, callers must disable the timer interrupt
//!   (SIE_STIE) for the duration of any RVV operation to prevent corruption.

#![allow(dead_code)]

use core::arch::asm;

/// Read the `cycle` CSR (rdcycle) — used for benchmarking.
#[inline(always)]
pub fn rdcycle() -> u64 {
    let c: u64;
    unsafe { asm!("rdcycle {0}", out(reg) c, options(nomem, nostack)) }
    c
}

// ── Scalar reference implementations ─────────────────────────────────────────

/// Scalar f32 dot product  a · b.
pub fn dot_f32_scalar(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut acc = 0.0f32;
    for i in 0..n { acc += a[i] * b[i]; }
    acc
}

/// Scalar f32 matrix multiply  C[m×n] = A[m×k] × B[k×n]  (row-major).
pub fn matmul_f32_scalar(
    c: &mut [f32], a: &[f32], b: &[f32], m: usize, k: usize, n: usize,
) {
    for i in 0..m {
        for j in 0..n {
            let mut acc = 0.0f32;
            for l in 0..k { acc += a[i*k+l] * b[l*n+j]; }
            c[i*n+j] = acc;
        }
    }
}

// ── RVV 1.0 implementations ───────────────────────────────────────────────────

/// Max K dimension for `matmul_f32_rvv` column-gather buffer (stack-allocated).
pub const MATMUL_MAX_K: usize = 64;

/// RVV 1.0 f32 dot product using LMUL=m4 (16 f32/iter at VLEN=128).
///
/// # Assembly notes
/// - `.option arch, +v, +f, +d` enables vector/float instructions locally in
///   the asm block; the Rust target (`riscv64imac-unknown-none-elf`) does not
///   include F/V at the type level, so this directive is mandatory.
/// - Result is extracted via `vse32.v` to a stack slot to avoid `freg`
///   constraints (no F extension in the Rust ABI for this target).
/// - `vfredusum.vs vd, vs2, vs1` with vd = vs1 is legal per RVV 1.0 §5.1.1.
/// - Register allocation:
///     v0:v3  — a chunk  (m4 LMUL)
///     v4:v7  — b chunk  (m4 LMUL)
///     v8:v11 — product  (m4 LMUL)
///     v16    — scalar accumulator (m1, reduction destination)
#[cfg(feature = "rvv")]
pub fn dot_f32_rvv(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 { return 0.0; }

    let mut result_bits: u32 = 0;
    let rp = &mut result_bits as *mut u32;
    let ap  = a.as_ptr() as usize;
    let bp  = b.as_ptr() as usize;
    let rem = n;

    unsafe {
        asm!(
            ".option arch, +v, +f, +d",
            // Initialize scalar accumulator v16 = 0.0  (bit-pattern 0x00000000)
            "vsetivli zero, 1, e32, m1, ta, ma",
            "vmv.v.i  v16, 0",

            // Main reduction loop: LMUL=m4, up to 16 f32 per iteration (VLEN=128)
            "1:",
            "beqz    {rem}, 2f",
            "vsetvli {vl}, {rem}, e32, m4, ta, ma",   // vl ← min(rem, VLMAX)
            "vle32.v v0,  ({ap})",                     // v0:v3 ← a[0..vl)
            "vle32.v v4,  ({bp})",                     // v4:v7 ← b[0..vl)
            "vfmul.vv v8, v0, v4",                     // v8:v11 ← a * b
            "vfredusum.vs v16, v8, v16",               // v16[0] += sum(v8..v11)
            "slli    {tmp}, {vl}, 2",                  // tmp ← vl * 4 bytes
            "add     {ap}, {ap}, {tmp}",
            "add     {bp}, {bp}, {tmp}",
            "sub     {rem}, {rem}, {vl}",
            "j       1b",

            // Extract scalar result to memory
            "2:",
            "vsetivli zero, 1, e32, m1, ta, ma",
            "vse32.v v16, ({rp})",

            ap  = inout(reg) ap  => _,
            bp  = inout(reg) bp  => _,
            rem = inout(reg) rem => _,
            vl  = out(reg)  _,
            tmp = out(reg)  _,
            rp  = in(reg)   rp,
        );
    }

    f32::from_bits(result_bits)
}

/// RVV 1.0 f32 matrix multiply  C[m×n] = A[m×k] × B[k×n]  (row-major).
///
/// Vectorises the K reduction via `dot_f32_rvv`.  Each B column is gathered
/// into a contiguous stack buffer before the dot product (k ≤ `MATMUL_MAX_K`).
/// A tiled / transposed-B implementation is planned for Phase 12.
#[cfg(feature = "rvv")]
pub fn matmul_f32_rvv(
    c: &mut [f32], a: &[f32], b: &[f32], m: usize, k: usize, n: usize,
) {
    let kk = k.min(MATMUL_MAX_K);
    let mut b_col = [0.0f32; MATMUL_MAX_K];
    for i in 0..m {
        for j in 0..n {
            for l in 0..kk { b_col[l] = b[l*n+j]; }
            c[i*n+j] = dot_f32_rvv(&a[i*k..(i+1)*k], &b_col[..kk]);
        }
    }
}

// ── Vector context save / restore (Phase 12) ─────────────────────────────────
//
// Each task gets a dedicated VecState slot indexed by its TID (Task offset 120).
// rvv_ctx_save / rvv_ctx_restore are `no_mangle extern "C"` so they can be
// called directly from context_switch_rvv.S.
//
// VecState layout (VLEN=128):
//   offset   0 : v0-v7   (128 bytes, vs8r / vl8r group 0)
//   offset 128 : v8-v15  (128 bytes, vs8r / vl8r group 1)
//   offset 256 : v16-v23 (128 bytes, vs8r / vl8r group 2)
//   offset 384 : v24-v31 (128 bytes, vs8r / vl8r group 3)
//   offset 512 : vl      (u64)
//   offset 520 : vtype   (u64)
//   offset 528 : vstart  (u64)
//   offset 536 : _pad    (40 bytes → total 576 = 9×64 for align(64))

/// Maximum number of tasks with tracked vector state.
#[cfg(feature = "rvv")]
const MAX_VEC_TASKS: usize = 64;

/// Per-task vector register state (VLEN=128: 32 regs × 16 bytes = 512 bytes).
#[cfg(feature = "rvv")]
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct VecState {
    /// v0-v31 packed contiguously (32 × 16 = 512 bytes at VLEN=128).
    pub vregs:  [u8; 512],
    /// `vl` CSR value at save time.
    pub vl:     u64,        // offset 512
    /// `vtype` CSR value at save time.
    pub vtype:  u64,        // offset 520
    /// `vstart` CSR value at save time.
    pub vstart: u64,        // offset 528
    /// Padding to reach 576 bytes (9 × 64).
    _pad: [u8; 40],
}

#[cfg(feature = "rvv")]
const ZERO_VEC_STATE: VecState = VecState {
    vregs: [0u8; 512], vl: 0, vtype: 0, vstart: 0, _pad: [0u8; 40],
};

#[cfg(feature = "rvv")]
static mut VEC_STATES: [VecState; MAX_VEC_TASKS] = [ZERO_VEC_STATE; MAX_VEC_TASKS];

/// Save v0-v31 + vl/vtype/vstart for the task at `task_ptr`.
///
/// # Safety
/// `task_ptr` must point to a valid `Task` (TID is a `u32` at byte offset 120).
/// Called from `context_switch_rvv.S` with a0 = task_ptr.
#[cfg(feature = "rvv")]
#[no_mangle]
pub unsafe extern "C" fn rvv_ctx_save(task_ptr: *const u8) {
    let tid = (task_ptr.add(120) as *const u32).read() as usize;
    if tid >= MAX_VEC_TASKS { return; }

    let base = (&raw mut VEC_STATES[tid]) as *mut u8;
    let mut vl:     u64 = 0;
    let mut vtype:  u64 = 0;
    let mut vstart: u64 = 0;

    asm!(
        ".option arch, +v",
        // Save v0-v31 as four groups of 8 whole registers.
        // vs8r.v does not depend on vtype/vl — safe to call unconditionally.
        "vs8r.v v0,  ({b})",
        "addi   {t}, {b},  128",
        "vs8r.v v8,  ({t})",
        "addi   {t}, {t},  128",
        "vs8r.v v16, ({t})",
        "addi   {t}, {t},  128",
        "vs8r.v v24, ({t})",
        // Save vl / vtype / vstart CSRs.
        "csrr {vl},     vl",
        "csrr {vtype},  vtype",
        "csrr {vstart}, vstart",
        b      = in(reg)  base,
        t      = out(reg) _,
        vl     = out(reg) vl,
        vtype  = out(reg) vtype,
        vstart = out(reg) vstart,
    );

    (base.add(512) as *mut u64).write(vl);
    (base.add(520) as *mut u64).write(vtype);
    (base.add(528) as *mut u64).write(vstart);
}

/// Restore v0-v31 + vl/vtype/vstart for the task at `task_ptr`.
///
/// # Safety
/// `task_ptr` must point to a valid `Task` (TID is a `u32` at byte offset 120).
/// Called from `context_switch_rvv.S` with a0 = task_ptr.
#[cfg(feature = "rvv")]
#[no_mangle]
pub unsafe extern "C" fn rvv_ctx_restore(task_ptr: *const u8) {
    let tid = (task_ptr.add(120) as *const u32).read() as usize;
    if tid >= MAX_VEC_TASKS { return; }

    let base   = (&raw const VEC_STATES[tid]) as *const u8;
    let vl:     u64 = (base.add(512) as *const u64).read();
    let vtype:  u64 = (base.add(520) as *const u64).read();
    let vstart: u64 = (base.add(528) as *const u64).read();

    asm!(
        ".option arch, +v",
        // Restore vtype and vl: vsetvl rd=x0, rs1=saved_vl, rs2=saved_vtype.
        "vsetvl zero, {vl}, {vtype}",
        // Restore v0-v31 (whole-register — independent of vtype/vl).
        "vl8r.v v0,  ({b})",
        "addi   {t}, {b},  128",
        "vl8r.v v8,  ({t})",
        "addi   {t}, {t},  128",
        "vl8r.v v16, ({t})",
        "addi   {t}, {t},  128",
        "vl8r.v v24, ({t})",
        // Restore vstart.
        "csrw vstart, {vstart}",
        b      = in(reg) base,
        t      = out(reg) _,
        vl     = in(reg) vl,
        vtype  = in(reg) vtype,
        vstart = in(reg) vstart,
    );
}

// ── Benchmarks ────────────────────────────────────────────────────────────────

/// Benchmark scalar vs RVV dot product.
///
/// `a` and `b` must have the same length.
/// Returns `(scalar_cycles, rvv_cycles, scalar_result, rvv_result)`.
#[cfg(feature = "rvv")]
pub fn bench_dot(a: &[f32], b: &[f32]) -> (u64, u64, f32, f32) {
    let t0 = rdcycle();
    let s  = dot_f32_scalar(a, b);
    let t1 = rdcycle();
    let v  = dot_f32_rvv(a, b);
    let t2 = rdcycle();
    (t1 - t0, t2 - t1, s, v)
}

/// Benchmark scalar vs RVV matmul for an m×k×n problem.
///
/// Caller provides pre-allocated output slices `cs` and `cv` (each of length m*n).
/// Returns `(scalar_cycles, rvv_cycles)`.
#[cfg(feature = "rvv")]
pub fn bench_matmul(
    cs: &mut [f32], cv: &mut [f32],
    a: &[f32], b: &[f32], m: usize, k: usize, n: usize,
) -> (u64, u64) {
    let t0 = rdcycle();
    matmul_f32_scalar(cs, a, b, m, k, n);
    let t1 = rdcycle();
    matmul_f32_rvv(cv, a, b, m, k, n);
    let t2 = rdcycle();
    (t1 - t0, t2 - t1)
}
