//! Host stand-in for `robot_os_sched`.
//!
//! **WHY this exists.** `crates/ipc/src/lease.rs` calls into the scheduler to
//! wake a blocked lessor and to apply lease priority inheritance, and
//! `crates/ipc/src/cap_store.rs` resolves TIDs to task-pool slots. The real
//! `robot_os_sched` is RV64-only (context switch asm, CSRs, PLIC), so the
//! `#[path]` host-test trick cannot pull it in. This crate provides the same
//! surface, plus **observability the real scheduler has no reason to expose**:
//! every wake is recorded so a test can assert that the lessor was actually
//! woken, not merely that a state bit flipped. That distinction is the whole
//! point of these tests — the project's recurring failure mode has been
//! validating the decision and never the actuation.
//!
//! Pulled in under the name `robot_os_sched` via a Cargo dependency rename.
//! The kernel never sees it.

use std::sync::Mutex;

/// `cap_store.rs` uses `robot_os_sched::task::MAX_TASKS`. Taken from the real
/// generated constant so the shim cannot drift from `.config`.
pub mod task {
    pub use robot_os_limits::MAX_TASKS;
}

#[derive(Default)]
struct FakeSched {
    current_tid: u32,
    user_pt: usize,
    /// TIDs passed to `wq_wake_by_tid`, in order.
    wq_wakes: Vec<u32>,
    /// TIDs passed to `wake_fast_ipc_server`, in order.
    fast_ipc_wakes: Vec<u32>,
    /// TIDs passed to `wake_by_rpc`, in order. Recorded separately from the
    /// other two because `rpc.rs` must wake an orphaned client through this
    /// path specifically: `SYS_IPC_CALL` parks it on `WaitReason::Rpc(tid)`,
    /// and a wake aimed at any other reason is a silent no-op that a test
    /// asserting "some wake happened" would not catch.
    rpc_wakes: Vec<u32>,
    /// `(tid, prio)` pairs passed to `boost_ready_task`.
    boosts: Vec<(u32, u32)>,
    /// TIDs passed to `restore_ready_task`.
    restores: Vec<u32>,
    /// Priority reported by `task_priority`, per TID. Absent = task gone.
    priorities: Vec<(u32, u32)>,
}

static SCHED: Mutex<Option<FakeSched>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut FakeSched) -> R) -> R {
    let mut g = SCHED.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(FakeSched::default))
}

// ── Test-only control surface ──────────────────────────────────────────────

/// Wipe all recorded state. Call at the start of every test.
pub fn shim_reset() {
    let mut g = SCHED.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(FakeSched::default());
}

/// Pretend the given task is running. `user_pt == 0` means "kernel task",
/// which is the house convention for a privileged caller.
pub fn shim_set_current(tid: u32, user_pt: usize) {
    with(|s| {
        s.current_tid = tid;
        s.user_pt = user_pt;
    });
}

pub fn shim_set_priority(tid: u32, prio: u32) {
    with(|s| {
        s.priorities.retain(|(t, _)| *t != tid);
        s.priorities.push((tid, prio));
    });
}

pub fn shim_wq_wakes() -> Vec<u32> {
    with(|s| s.wq_wakes.clone())
}

pub fn shim_fast_ipc_wakes() -> Vec<u32> {
    with(|s| s.fast_ipc_wakes.clone())
}

pub fn shim_rpc_wakes() -> Vec<u32> {
    with(|s| s.rpc_wakes.clone())
}

pub fn shim_boosts() -> Vec<(u32, u32)> {
    with(|s| s.boosts.clone())
}

pub fn shim_restores() -> Vec<u32> {
    with(|s| s.restores.clone())
}

/// Every TID woken by either path, deduplicated order-insensitively.
pub fn shim_was_woken(tid: u32) -> bool {
    with(|s| s.wq_wakes.contains(&tid) || s.fast_ipc_wakes.contains(&tid))
}

// ── The `robot_os_sched` surface the ipc modules actually call ─────────────

pub fn current_task_tid() -> u32 {
    with(|s| s.current_tid)
}

pub fn current_user_pt() -> usize {
    with(|s| s.user_pt)
}

pub fn task_priority(tid: u32) -> Option<u32> {
    with(|s| s.priorities.iter().find(|(t, _)| *t == tid).map(|(_, p)| *p))
}

pub fn boost_ready_task(tid: u32, new_prio: u32) {
    with(|s| s.boosts.push((tid, new_prio)));
}

pub fn restore_ready_task(tid: u32) {
    with(|s| s.restores.push(tid));
}

pub fn wq_wake_by_tid(tid: u32) {
    with(|s| s.wq_wakes.push(tid));
}

pub fn wake_fast_ipc_server(tid: u32) {
    with(|s| s.fast_ipc_wakes.push(tid));
}

/// Wake a task blocked on `WaitReason::Rpc(tid)`. Used by `rpc_cancel_all` to
/// release the clients of a server that has just exited.
pub fn wake_by_rpc(tid: u32) {
    with(|s| s.rpc_wakes.push(tid));
}

/// Never called by the tests — `lease_wait_return` blocks, and a host stand-in
/// has nothing to block on. Present so the module compiles.
pub fn wq_block_current() {
    panic!(
        "wq_block_current() reached in a host test: lease_wait_return() would \
         spin forever here. Test the guard paths, not the blocking loop."
    );
}

/// `cap_store::slot_for` maps TID → task-pool slot. Identity-ish mapping is
/// enough for the port tests: distinct TIDs get distinct slots.
pub fn idx_for_tid(tid: u32) -> Option<usize> {
    if tid == 0 || tid as usize >= task::MAX_TASKS {
        return None;
    }
    Some(tid as usize)
}
