#![no_std]
extern crate alloc;

pub mod numbers;
pub mod handlers;
pub mod dispatch;

pub use dispatch::{syscall_dispatch, syscall_dispatch_out, SyscallOut, SYSCALL_OUT_REGS};
pub use numbers::*;
