//! Host stand-in for the two `robot_os_sched` accessors that
//! `crates/ipc/src/{channel,pipe,signal}.rs` call from `caller_ctx`.
//!
//! The real crate is RV64-only (per-hart state, `context_switch.S`, CSR
//! access). Its library name is reused here (`[lib] name = "robot_os_sched"`)
//! so the kernel sources compile unedited.
//!
//! **These bodies are never exercised by the tests.** Each module's
//! `caller_ctx` has a `#[cfg(test)]` variant driven by `test_ctx` atomics,
//! and that is the one the suite runs. This shim exists only so the
//! `#[cfg(not(test))]` arm still *type-checks* when Cargo builds the plain
//! `lib` target alongside the test target. The values below therefore say
//! "kernel task, no current task", which is the safe reading if anything ever
//! did call them.

/// Always 0 — "no current task".
pub fn current_task_tid() -> u32 {
    0
}

/// Always 0 — "kernel task, no user address space".
pub fn current_user_pt() -> usize {
    0
}
