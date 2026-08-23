//! Host stand-in for `robot_os_sched`, reduced to the one thing
//! `crates/ipc/src/cap_store.rs` actually calls: the TID → task-pool-slot
//! lookup.
//!
//! Identical in shape to `crates/cap-tests/shims/sched` (copied, not
//! path-shared — see that crate's copy for the full rationale on why the
//! real scheduler cannot be used here). This crate's tests only need the
//! "make a TID live at a slot" half of the control surface
//! ([`shim_bind`]); the stale/alias-race helpers are carried along so a
//! future addition to this suite does not need to touch the shim again.

use std::sync::Mutex;

/// `cap_store.rs` uses `robot_os_sched::task::MAX_TASKS`. Taken from the
/// real generated constant so the shim cannot drift from `.config`.
pub mod task {
    pub use robot_os_limits::MAX_TASKS;
}

#[derive(Default)]
struct FakePool {
    live: Vec<(u32, usize)>,
    stale_once: Option<(u32, usize)>,
}

static POOL: Mutex<Option<FakePool>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut FakePool) -> R) -> R {
    let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(FakePool::default))
}

/// Forget every binding.
pub fn shim_reset() {
    let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(FakePool::default());
}

/// Make `tid` resolve to `slot`.
pub fn shim_bind(tid: u32, slot: usize) {
    with(|p| {
        p.live.retain(|(t, _)| *t != tid);
        p.live.push((tid, slot));
    });
}

/// Make `tid` resolve to nothing — a dead task.
pub fn shim_kill(tid: u32) {
    with(|p| p.live.retain(|(t, _)| *t != tid));
}

/// Answer the *next* lookup of `tid` with `slot`, then go back to the truth.
pub fn shim_stale_once(tid: u32, slot: usize) {
    with(|p| p.stale_once = Some((tid, slot)));
}

/// Drop an unconsumed [`shim_stale_once`].
pub fn shim_clear_stale() {
    with(|p| p.stale_once = None);
}

/// Translate a TID to a task-pool slot index.
pub fn idx_for_tid(tid: u32) -> Option<usize> {
    with(|p| {
        if let Some((t, slot)) = p.stale_once {
            if t == tid {
                p.stale_once = None;
                return Some(slot);
            }
        }
        p.live.iter().find(|(t, _)| *t == tid).map(|(_, s)| *s)
    })
}
