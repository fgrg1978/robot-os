#![no_std]
extern crate alloc;

pub mod numbers;
pub mod handlers;
pub mod dispatch;

pub use dispatch::syscall_dispatch;
pub use numbers::*;
