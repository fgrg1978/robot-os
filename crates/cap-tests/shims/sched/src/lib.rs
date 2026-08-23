//! Host stand-in for `robot_os_sched`, reduced to the one thing
//! `crates/ipc/src/cap_store.rs` actually calls: the TID → task-pool-slot
//! lookup.
//!
//! **WHY this exists.** The real `robot_os_sched` is RV64-only (context
//! switch asm, CSRs, PLIC), so the `#[path]` host-test trick cannot pull it
//! in. But the whole point of these tests is `cap_store`'s behaviour *around*
//! that lookup, so the shim exposes a control surface the real scheduler has
//! no reason to expose:
//!
//!   * [`shim_bind`] / [`shim_kill`] — make a TID live or dead at a chosen
//!     slot, so "delegate to a TID that does not exist" is testable.
//!   * [`shim_bind`] called twice with the same slot — make two live TIDs
//!     alias one slot, which is precisely the wrong-slot outcome the real
//!     `idx_for_tid` can produce under a race with `alloc_slot` and which no
//!     amount of single-threaded testing against the real scheduler could
//!     reproduce.
//!   * [`shim_stale_once`] — answer the *next* lookup for a TID with a slot
//!     that is not its own, then answer honestly. That is exactly the shape
//!     of the unsynchronised-scan race: `TASK_VALID[i]` is published before
//!     `TASKS[i].tid`, so one scan can match a slot the next scan will not.
//!     It exists to prove `cap_store::slot_for_untrusted`'s confirmation
//!     pass actually refuses, rather than trusting the comment that says so.
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
struct FakePool {
    /// Live `(tid, slot)` bindings. A `Vec` and not a `[u32; MAX_TASKS]`
    /// because the interesting failure is two TIDs on one slot, which an
    /// array keyed by slot cannot represent.
    live: Vec<(u32, usize)>,
    /// One-shot lie: `(tid, slot)` returned by the next matching lookup.
    stale_once: Option<(u32, usize)>,
}

static POOL: Mutex<Option<FakePool>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut FakePool) -> R) -> R {
    let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(FakePool::default))
}

// ── Test-only control surface ──────────────────────────────────────────────

/// Forget every binding. Call at the start of every test.
pub fn shim_reset() {
    let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(FakePool::default());
}

/// Make `tid` resolve to `slot`. Two TIDs may share a slot on purpose.
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

/// Drop an unconsumed [`shim_stale_once`]. Called at the top of every test so
/// a refusal in one test cannot leak a pending lie into the next.
pub fn shim_clear_stale() {
    with(|p| p.stale_once = None);
}

// ── The `robot_os_sched` surface `cap_store` calls ─────────────────────────

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
