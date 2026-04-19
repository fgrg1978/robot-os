#![no_std]

extern crate alloc;

pub mod addr;
pub mod pmm;
#[cfg(not(feature = "esp32c3"))]
pub mod vmm;
/// E11 / AQ9 — Copy-on-Write support for `fork()`.
#[cfg(not(feature = "esp32c3"))]
pub mod cow;
/// E11 / AQ10 — Demand paging (allocate-on-first-access).
#[cfg(not(feature = "esp32c3"))]
pub mod demand;
pub mod kheap;
#[cfg(not(feature = "esp32c3"))]
pub mod vdso;
