#![no_std]

pub mod cpu;
pub mod csr;
pub mod mmu;
pub mod pmp;
pub mod rvv;
// Cross-arch portable face of `rvv` — mirrors arch-aarch64::vector
// and arch-x86_64::vector.
pub mod vector;
pub mod sbi;
pub mod trap;

// B0.2 — adapter that implements the cross-ISA `robot_os_arch_api`
// traits in terms of the legacy free-function modules above. Pure
// additive; existing callers are unaffected.
pub mod api_impl;
