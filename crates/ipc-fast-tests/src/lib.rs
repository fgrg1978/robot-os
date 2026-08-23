//! Host-side runner for `crates/ipc/src/fast_ipc.rs` tests.
//!
//! The kernel `robot_os_ipc` crate cannot be compiled for the host (it depends
//! on RV64-only crates). `fast_ipc.rs` names two of them itself —
//! `robot_os_sync::SpinLock` and `robot_os_sched` — but only under
//! `cfg(not(test))`; under `cfg(test)` it uses the host substitutes defined in
//! its own `host_seam` module. So, exactly like `crates/cap-tests`, we pull the
//! file in via `#[path]` and let its embedded `#[cfg(test)] mod tests` run.
//!
//! Run with:  cd crates/ipc-fast-tests && cargo test

#[path = "../../ipc/src/fast_ipc.rs"]
pub mod fast_ipc;
