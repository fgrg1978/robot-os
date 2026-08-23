//! IO-wait and wake primitives (AQ0).
//!
//! Provides `task_block(reason)` to put the current task to sleep and
//! `wake_*()` functions to unblock tasks when their wait condition is met.

use crate::task::{WaitReason, MAX_TASKS};
use crate::smp::current_cpu_id;
use crate::scheduler;

/// Block the current task until `reason` is satisfied.
///
/// The task is removed from the ready queue and marked `Blocked`.
/// Another task is scheduled immediately. This function "returns"
/// when the task is woken up by a matching `wake_*()` call.
pub fn task_block(reason: WaitReason) {
    let cpu = current_cpu_id();
    scheduler::block_current(cpu, reason);
}

/// Wake all tasks blocked on a specific IRQ.
pub fn wake_by_irq(irq: u32) {
    wake_matching(|r| matches!(r, WaitReason::Irq(i) if *i == irq));
}

/// Wake all tasks blocked on a specific channel.
pub fn wake_by_channel(handle: u32) {
    wake_matching(|r| matches!(r, WaitReason::Channel(h) if *h == handle));
}

/// Wake all tasks blocked on a specific ring buffer.
pub fn wake_by_ring(ring_id: u32) {
    wake_matching(|r| matches!(r, WaitReason::Ring(id) if *id == ring_id));
}

/// Wake all tasks blocked on a specific port.
pub fn wake_by_port(port_id: u32) {
    wake_matching(|r| matches!(r, WaitReason::Port(id) if *id == port_id));
}

/// Wake all tasks whose timer deadline has expired.
pub fn wake_expired_timers(now_ticks: u64) {
    wake_matching(|r| matches!(r, WaitReason::Timer(deadline) if now_ticks >= *deadline));
}

// ── K-C10: the wake decision, isolated as pure logic ────────────────────────

/// What a TID-directed wake must do with one task-pool slot.
///
/// This is the whole of K-C10's policy, split out from
/// `scheduler::wake_task_by_tid` so it can be exercised on the host — the
/// scheduler itself cannot be (static `TASKS`, RISC-V CSRs, assembly context
/// switch). See `crates/sched-wake-tests/`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WakeAction {
    /// Not the addressee. Keep scanning; touch nothing.
    Skip,
    /// Addressee found, but it has **not** committed to `Blocked` yet.
    /// Stamp `wake_pending` (K-C9) so its imminent `block_current()` consumes
    /// the wake instead of sleeping through it. Stop scanning.
    StampPending,
    /// Addressee found and `Blocked`, but on a reason the caller did not
    /// expect. Genuine mismatch: stop scanning, and leave `wake_pending`
    /// alone — this is not our task.
    Mismatch,
    /// Addressee found, `Blocked`, reason matches: transition Blocked → Ready
    /// and enqueue it. Stop scanning.
    Dispatch,
}

/// Decide what to do with one task-pool slot on behalf of a TID-directed wake.
///
/// `reason_matches` is the caller's `WaitReason` predicate evaluated against
/// the slot; it is only meaningful when `is_blocked` is true (a task that has
/// not blocked yet has `wait_reason == WaitReason::None`, which no targeted
/// predicate accepts — that is precisely why selection must be by TID).
///
/// # Truth table (the complete contract; `-` = input not consulted)
///
/// | addressee | blocked | reason matches | prev `wake_pending` | action        | `wake_pending` after |
/// |-----------|---------|----------------|---------------------|---------------|----------------------|
/// | no        | -       | -              | -                   | `Skip`        | unchanged            |
/// | yes       | no      | -              | false               | `StampPending`| **true**             |
/// | yes       | no      | -              | true                | `StampPending`| true (idempotent)    |
/// | yes       | yes     | no             | false               | `Mismatch`    | false (untouched)    |
/// | yes       | yes     | yes            | false               | `Dispatch`    | false                |
///
/// **`Blocked` with the wake stamp already set does not occur under
/// `saved = true`**, which is why those two rows are absent rather than
/// merely uninteresting. Since K-C19 this is enforced by construction, not by
/// call-site discipline: state and stamp share one atomic word
/// (`task::sched_word`), committing to `Blocked` is a CAS that requires the
/// stamp clear, and stamping is a CAS that requires `state != Blocked`.
/// `sched-wake-tests` asserts exactly that. **K-C24 made the state
/// representable** for an UNSAVED target (`wake_transition`'s `!saved` arm
/// stamps a committed-but-still-executing task); a stamp that then survives
/// the switch-away sweep parks as `Blocked + stamp + saved`, and the K-C25
/// reaper (`sched_word::reap_orphaned_stamp`, driven from the timer tick) is
/// that state's designated consumer — see the measured wedge documented
/// there.
///
/// Two rows deserve comment:
///
/// * **`Mismatch` never stamps.** Stamping there would make a task blocked on
///   something unrelated (a `Timer`, say) skip its *next* block. Between
///   becoming wake-able and calling `task_block()`, our addressees run a
///   straight-line stretch of their own syscall: they can be preempted to
///   `Ready`, but cannot become `Blocked` on a different reason. So
///   `Blocked` + non-matching reason means the TID no longer designates the
///   task we mean (exit + TID reuse, stale slot index, confused replier).
///   Same rule, same reasoning, as `scheduler::wq_wake_by_tid`.
/// * **`StampPending` ignores the previous value.** The stamp is an
///   idempotent bit (a CAS that ORs it in), not a counter: two wakes landing
///   in the window skip one block, not two. That is correct for every caller
///   here, because each has exactly one thing to wait for. If K-C10 ever has
///   to count wakes, this is the function that changes first.
///
/// `Mismatch` does not touch the word at all, so a stamp left by an earlier
/// wake survives it — deliberately: clearing it would re-open the lost-wakeup
/// window this finding closes. (`Dispatch` cannot meet a stamp: Blocked with
/// the stamp set is unrepresentable, per the invariant above.)
#[inline]
pub const fn wake_action(is_addressee: bool, is_blocked: bool, reason_matches: bool) -> WakeAction {
    if !is_addressee { return WakeAction::Skip; }
    if !is_blocked { return WakeAction::StampPending; }
    if !reason_matches { return WakeAction::Mismatch; }
    WakeAction::Dispatch
}

// ── K-C10: targeted wakes go through scheduler::wake_task_by_tid ────────────
//
// The three wakes below all address ONE task that is known by TID at the call
// site. Routing them through `wake_matching` (i.e. `try_wake_task`) lost the
// wake whenever the addressee had made itself wake-able but had not yet
// reached `task_block()` — a real SMP window, because the waker runs on
// another hart. `scheduler::wake_task_by_tid` selects by TID, so it can stamp
// `wake_pending` (K-C9) in that window and `block_current()` consumes it.
//
// Do NOT "unify" the broadcast wakes above into this: they have no addressee
// TID, and a not-yet-blocked task has `wait_reason == None`, so a sweep-based
// waker cannot tell the addressee from any other task about to sleep. See the
// `wake_task_by_tid` doc comment for the full argument.

/// Wake a task blocked on an RPC reply (matched by caller TID).
///
/// K-C10: `SYS_IPC_CALL` publishes the request with `channel_send()` *before*
/// `rpc_register()` and before `task_block(WaitReason::Rpc(tid))`, so a server
/// on another hart can receive, reply and land here while the caller is still
/// running. Same lost-wakeup shape as fast IPC; same fix.
///
/// The TID and the predicate select the same task by construction:
/// `WaitReason::Rpc(t)` always carries the blocked caller's own TID.
pub fn wake_by_rpc(caller_tid: u32) {
    scheduler::wake_task_by_tid(
        caller_tid,
        &|r| matches!(r, WaitReason::Rpc(tid) if *tid == caller_tid),
    );
}

/// Wake the server task blocked waiting for a fast IPC call (by server TID).
///
/// K-C10: converted to the TID-directed path. `WaitReason::FastIpcServer(t)`
/// carries the server's own TID, so TID and predicate agree by construction.
/// Also used by the lease paths (`SYS_IPC_LEASE_RETURN` and the timer-ISR
/// lease-expiry sweep) to wake a lessor addressed by TID — same property.
pub fn wake_fast_ipc_server(server_tid: u32) {
    scheduler::wake_task_by_tid(
        server_tid,
        &|r| matches!(r, WaitReason::FastIpcServer(tid) if *tid == server_tid),
    );
}

/// Wake the client task blocked waiting for a fast IPC reply, addressed by
/// the client's TID (K-C10 — preferred over [`wake_fast_ipc_client`]).
///
/// `fast_ipc_reply()` returns the `caller_tid` that owns the exchange, so the
/// addressee is known at the call site and this can close the window where the
/// client has reserved its slot and woken the server but has not yet executed
/// `task_block(WaitReason::FastIpcClient(handle))`.
///
/// The predicate matches on the generation-tagged `handle` (client and server
/// handles for one exchange are the same value — the generation only advances
/// on free): if the client is already blocked it must be blocked on *this*
/// exchange, and a mismatch means the TID no longer designates that client
/// (exit + TID reuse, or a stale handle) — in which case `wake_task_by_tid`
/// deliberately leaves the wake stamp untouched.
pub fn wake_fast_ipc_client_tid(caller_tid: u32, handle: u64) {
    scheduler::wake_task_by_tid(
        caller_tid,
        &|r| matches!(r, WaitReason::FastIpcClient(h) if *h == handle),
    );
}

/// Wake the client task blocked waiting for a fast IPC reply (by exchange
/// handle, sweep-based).
///
/// K-C10: **superseded by [`wake_fast_ipc_client_tid`]** for the reply path;
/// still the right shape for `fast_ipc_release_all`'s orphan wake, whose
/// addressee is unknown (the dying server never learns the client's TID).
/// The handle match means a re-let seat's new client can never be woken by a
/// dead exchange's orphan sweep. A sweep still cannot address a client that
/// has not reached `task_block()` yet — acceptable for the orphan path,
/// whose client has been blocked for the whole exchange by construction.
pub fn wake_fast_ipc_client(handle: u64) {
    wake_matching(|r| matches!(r, WaitReason::FastIpcClient(h) if *h == handle));
}

/// Internal: scan all tasks and wake those matching the predicate.
fn wake_matching(pred: impl Fn(&WaitReason) -> bool) {
    for i in 0..MAX_TASKS {
        scheduler::try_wake_task(i, &pred);
    }
}
