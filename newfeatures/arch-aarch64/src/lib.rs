//! `arch-aarch64` — PHANES Phase 2 aarch64 (ARMv8-A) ISA impl of
//! the `robot_os_arch_api` trait surface.
//!
//! # Scope of B1 (this commit)
//!
//! - Five trait families implemented end-to-end against ARMv8-A:
//!   [`api_impl::Aarch64`] satisfies [`Cpu`], [`Interrupts`],
//!   [`Mmu`], and [`Boot`] (the `Vector` impl follows in B1.vec).
//! - System-register read/write helpers in [`sysregs`].
//! - VMSAv8-64 PTE encoding in [`mmu`].
//! - PSCI v1.0 calls (CPU_ON / SYSTEM_OFF / SYSTEM_RESET) in
//!   [`psci`].
//! - GIC v3 programming + early-boot `.S` are **out of scope** —
//!   they land in B1.boot once the crate compiles for the
//!   `aarch64-unknown-none-softfloat` target.
//!
//! # Why everything is `cfg(target_arch = "aarch64")`
//!
//! `crates/arch-aarch64` is a workspace member, but the workspace
//! default target is `riscv64gc-unknown-none-elf`. The asm bodies
//! cannot assemble on RISC-V, so the inner functions are
//! cfg-gated. On the workspace target the crate compiles as
//! essentially-empty (struct definitions + module declarations
//! only); on `aarch64-unknown-none-softfloat` the real impls
//! light up. This way:
//!
//! - `bash scripts/build.sh` keeps catching breakage in the
//!   type-level surface (struct shapes, trait method signatures).
//! - Standalone `cargo build --target aarch64-unknown-none-softfloat
//!   -p robot_os_arch_aarch64` exercises the asm.
//!
//! No `cfg` shenanigans leak into the public API: callers always
//! see [`api_impl::Aarch64`] + a trait impl, regardless of
//! target. Calling those impls from a non-aarch64 target is a
//! link-time error rather than a runtime panic (the `impl`
//! blocks themselves are cfg-gated).

#![no_std]
#![allow(dead_code)] // some helpers are referenced only by asm bodies

pub use robot_os_arch_api::{
    Boot, Cpu, HartStartError, InterruptState, Interrupts, Mmu, MmuError,
    PagePerms, Vector,
};

pub mod api_impl;
pub mod boot;
pub mod cache;
pub mod cpu;
pub mod fp_state;
pub mod gic;
pub mod midr;
pub mod mmu;
pub mod mmu_setup;
pub mod mpidr;
pub mod psci;
pub mod sysregs;
pub mod timer;
pub mod vector;

/// Architectural identifier surfaced through arch-api.
pub const ARCH_ID: robot_os_arch_api::ArchId =
    robot_os_arch_api::ArchId::Aarch64;
