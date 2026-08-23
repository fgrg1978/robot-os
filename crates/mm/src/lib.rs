#![no_std]

extern crate alloc;

pub mod addr;
pub mod pmm;
pub mod vmm;
/// E11 / AQ9 — Copy-on-Write support for `fork()`.
pub mod cow;
/// E11 / AQ10 — Demand paging (allocate-on-first-access).
pub mod demand;
pub mod kheap;
pub mod vdso;
