#![no_std]

extern crate alloc;

pub mod addr;
pub mod pmm;
#[cfg(not(feature = "esp32c3"))]
pub mod vmm;
pub mod kheap;
