//! Host-side runner for the IPC-3 / IPC-6 tests that live inside
//! `crates/ipc/src/{lease,rpc,port}.rs`.
//!
//! Same trick as `crates/cap-tests`: the whole `robot_os_ipc` crate cannot be
//! built for the host (RV64-only dependencies), so each module is pulled in
//! directly with `#[path]` and its embedded `#[cfg(test)] mod tests` runs
//! here. The two kernel crates those modules call into (`robot_os_sync`,
//! `robot_os_sched`) are replaced by the host shims under `shims/` via a
//! Cargo dependency rename — the kernel build never sees them.
//!
//! `io_ring.rs` is deliberately absent; see the crate-level note at the
//! bottom of this file.

#[path = "../../ipc/src/lease.rs"]
pub mod lease;

#[path = "../../ipc/src/rpc.rs"]
pub mod rpc;

// `rpc.rs` resolves "who is allowed to answer this call" through
// `channel_owner`, so the channel pool has to be here too. Its own tests live
// in `crates/ipc-chan-tests` and are *not* duplicated by this: `channel.rs`
// carries no `#[cfg(test)] mod tests` of its own, only the `test_ctx`
// identity shim and `__channel_reset_for_tests`.
//
// **Two identity sources in one binary, on purpose.** Under `cfg(test)`,
// `channel.rs` reads the caller from its own `test_ctx` atomics while
// `rpc.rs` reads it from the `robot_os_sched` shim. The rpc suite's
// `become_task()` sets both; setting one alone yields a channel owned by TID
// 0 and an authorization failure unrelated to the code under test.
#[path = "../../ipc/src/channel.rs"]
pub mod channel;

// `port.rs`'s typed `port_*_cap` wrappers reference `crate::cap` and
// `crate::cap_store`, so both must exist under those exact paths.
#[path = "../../ipc/src/cap.rs"]
pub mod cap;

#[path = "../../ipc/src/cap_store.rs"]
pub mod cap_store;

#[path = "../../ipc/src/port.rs"]
pub mod port;

// `io_ring.rs`'s per-opcode capability check goes through `crate::handle`.
// That module needs nothing but `robot_os_sync`.
#[path = "../../ipc/src/handle.rs"]
pub mod handle;

// `io_ring.rs` allocates one physical page per ring; the page allocator is
// stood in for by `shims/mm`. Its `IoRingOps` dispatch table is a struct of
// plain `fn` pointers declared in the module itself — no driver crate is
// involved — so the whole submit path is drivable from the host.
#[path = "../../ipc/src/io_ring.rs"]
pub mod io_ring;

// ── What is NOT covered here, and why ──────────────────────────────────────
//
//  * `lease_wait_return`'s blocking loop. It parks on
//    `robot_os_sched::wq_block_current()`, which on the host has nothing to
//    block on — the shim panics there deliberately rather than spinning
//    forever. The *guard* on that function is tested (a stranger returns
//    immediately and donates no priority); the block/wake handshake itself
//    needs the real scheduler and belongs in QEMU.
//  * `io_ring_worker_poll`. Its claim/release discipline is the same
//    `claim_ring`/`release_ring` pair the orphan test drives through
//    `io_ring_submit`, but the worker's own loop is not exercised here.
//  * The `dispatch_sqe` capability matrix. The orphan test runs one
//    privileged `OP_READ_SENSOR`; the per-opcode `HandleKind` checks are a
//    different lane's concern (W3-F3) and are not re-asserted.
