//! Synchronous RPC — IPC_CALL / IPC_REPLY (F00.5).
//!
//! A client sends a message to a server channel and blocks until the server
//! replies. This enables request/response patterns between userspace drivers.
//!
//! Flow:
//!   1. Client: IPC_CALL(server_ch, msg) → message sent to channel, client blocks
//!   2. Server: CHAN_READ(server_ch) → reads message
//!   3. Server: IPC_REPLY(caller_tid, reply) → reply stored, client woken
//!   4. Client: wakes up, retrieves reply data

use core::sync::atomic::{AtomicBool, Ordering};
use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of concurrent pending RPCs.
pub const MAX_PENDING_RPCS: usize = 16;

/// Maximum RPC message/reply size in bytes.
pub const RPC_MSG_MAX_LEN: usize = 64;

/// "No server identified" — the call was sent to a channel with no live
/// owner. Only a privileged (kernel) caller can answer such an entry.
///
/// `u32::MAX` and not `0`: `channel.rs` already uses `0` as its *vacant*
/// owner marker, so a channel whose slot was never claimed reports
/// `Some(0)`. Folding both into one sentinel keeps "nobody owns this" a
/// single, fail-closed value in this table.
const NO_SERVER: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A pending RPC call waiting for a reply.
pub struct RpcPending {
    /// Task ID of the caller (blocked).
    pub caller_tid: u32,
    /// Channel the call was sent to (for correlation/debugging).
    pub server_channel: u32,
    /// TID of the task entitled to answer this call: the **owner of
    /// `server_channel` at the moment the call was registered**, or
    /// [`NO_SERVER`].
    ///
    /// **WHY it is captured at register time rather than resolved at reply
    /// time.** Two independent reasons, and either one alone is sufficient:
    ///
    ///  * **It is strictly stronger.** `channel_destroy` frees the slot and
    ///    `channel_create` hands the same index to whoever asks next. Looking
    ///    the owner up when the reply arrives would let a task that destroyed
    ///    and re-created the channel underneath a live call answer it — the
    ///    exact recycled-id confused deputy `lease_free` documents. A TID
    ///    snapshot pins the identity to the call.
    ///  * **It is the only version that survives the exit path.** The
    ///    server-death sweep in [`rpc_cancel_all`] runs from
    ///    `task_release_all`, by which point the dying task's channels may
    ///    already be gone; `channel_owner` would then answer `None` for every
    ///    entry and the sweep would find nothing to release.
    pub server_tid: u32,
    /// Reply buffer (kernel-side copy).
    pub reply_buf: [u8; RPC_MSG_MAX_LEN],
    /// Reply length (filled by server via IPC_REPLY).
    pub reply_len: u32,
    /// Whether this slot is active (waiting for reply).
    pub active: bool,
    /// Whether the reply has been written (server called IPC_REPLY).
    pub done: AtomicBool,
}

impl RpcPending {
    pub const fn empty() -> Self {
        Self {
            caller_tid: 0,
            server_channel: 0,
            server_tid: NO_SERVER,
            reply_buf: [0u8; RPC_MSG_MAX_LEN],
            reply_len: 0,
            active: false,
            done: AtomicBool::new(false),
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

/// Global pending-RPC table.
///
/// Protected by a single `SpinLock` (same shape as `port.rs`'s `PORTS`).
/// Was a bare `static mut` wired to `SYS_IPC_CALL`/`SYS_IPC_REPLY`, reachable
/// concurrently from any hart with zero synchronization — two harts calling
/// `rpc_register` at once could claim the same free slot, and a `rpc_reply`
/// racing a `rpc_get_reply` on the same entry could read a torn/mid-write
/// `reply_buf`. `lock_irqsave()` throughout, same discipline as the other
/// tables in this crate.
const EMPTY_RPC: RpcPending = RpcPending::empty();
static RPC_PENDING: SpinLock<[RpcPending; MAX_PENDING_RPCS]> =
    SpinLock::new([EMPTY_RPC; MAX_PENDING_RPCS]);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Who is calling, and is it a kernel task?
///
/// Read here rather than taken as a parameter, exactly as `channel.rs` and
/// `lease_wait_return` already do in this crate. `rpc_reply` has a single
/// caller in the whole tree — the `SYS_IPC_REPLY` arm of `dispatch.rs` — so
/// self-attribution denies nothing legitimate, and it makes the check
/// impossible for a future syscall arm to forget to pass. `privileged` is the
/// house convention (`current_user_pt() == 0`), matching `cap_store`'s
/// typed callers, `port_access_ok`, `lease_return` and `channel_destroy`.
#[inline]
fn caller_ctx() -> (u32, bool) {
    (
        robot_os_sched::current_task_tid(),
        robot_os_sched::current_user_pt() == 0,
    )
}

/// Register a pending RPC call. Returns rpc_id or None if no free slots.
/// Called by kernel when processing SYS_IPC_CALL.
///
/// Snapshots the owner of `server_channel` into [`RpcPending::server_tid`];
/// see that field for why the snapshot is taken here and not at reply time.
pub fn rpc_register(caller_tid: u32, server_channel: u32) -> Option<u32> {
    // Resolve the server *before* taking `RPC_PENDING`: `channel_owner` takes
    // the channel pool's own lock, and nesting the two tables would create a
    // lock order that nothing else in this crate observes. Nothing here needs
    // the two views to be consistent — the snapshot is the point.
    let server_tid = match crate::channel::channel_owner(server_channel as usize) {
        // `0` is `channel.rs`'s vacant-owner marker and can never be a live
        // TID, so it folds into "no server".
        Some(0) | None => NO_SERVER,
        Some(t) => t,
    };
    let mut pending = RPC_PENDING.lock_irqsave();
    for i in 0..MAX_PENDING_RPCS {
        if !pending[i].active {
            pending[i] = RpcPending {
                caller_tid,
                server_channel,
                server_tid,
                reply_buf: [0u8; RPC_MSG_MAX_LEN],
                reply_len: 0,
                active: true,
                done: AtomicBool::new(false),
            };
            return Some(i as u32);
        }
    }
    None
}

/// Complete an RPC call with a reply.
/// Called by kernel when processing SYS_IPC_REPLY.
/// Returns the caller_tid that should be woken up, or None if no matching RPC
/// or the calling task is not entitled to answer it.
///
/// # Who may answer an RPC
///
/// **The owner of the channel the call was sent to, and nobody else** (plus
/// kernel tasks, by the house privilege convention).
///
/// That is the only party with a claim. `SYS_IPC_CALL` publishes the request
/// with `channel_send(server_channel, …)` and `channel_recv` already refuses
/// every task but the channel's owner (`channel.rs`, "message theft and
/// cross-task destroy"), so the owner is by construction the only task that
/// can have *read* the request. Letting anyone else answer would mean the
/// reply came from a task that never saw the question.
///
/// **WHAT WAS BROKEN.** This function used to match on `caller_tid` alone —
/// the *client's* TID, an argument taken raw from `a0` of `SYS_IPC_REPLY` and
/// bounded by nothing. `MAX_PENDING_RPCS` is 16 and TIDs are small, so a
/// ring-3 task could sweep the space and answer other tasks' calls with
/// chosen bytes. The client then returns from a blocking `SYS_IPC_CALL`
/// believing the payload came from its server: for a userspace driver that is
/// a sensor reading, a motor acknowledgement or a configuration value
/// fabricated by an unprivileged task. Identical in shape to IPC-1
/// (`fast_ipc_reply`), which was closed with a `server_tid` check on the
/// slot; this is the same fix on the slow path.
///
/// The dispatch arm's existing `a0 != current_tid` guard (no self-replies) is
/// now redundant — a task cannot be its own server *and* be blocked in
/// `SYS_IPC_CALL` — but it is harmless and cheap, so it is left alone.
///
/// Cost: one `u32` compare inside a lock this function already holds.
pub fn rpc_reply(caller_tid: u32, reply_data: &[u8]) -> Option<u32> {
    let (replier, privileged) = caller_ctx();
    let mut pending = RPC_PENDING.lock_irqsave();
    for i in 0..MAX_PENDING_RPCS {
        let rpc = &mut pending[i];
        if rpc.active && rpc.caller_tid == caller_tid && !rpc.done.load(Ordering::Acquire) {
            // Fail closed, in two steps rather than one compare.
            //
            // The obvious single test `rpc.server_tid != replier` has a
            // sentinel collision: `NO_SERVER` *is* `u32::MAX`, so a task
            // whose TID happened to be `u32::MAX` would authenticate as the
            // server of every call whose channel had no owner. Whether
            // `NEXT_TID` can actually issue that value is a property of
            // another file and another lane; making the sentinel unmatchable
            // here costs one predictable branch and removes the dependency.
            // (Caught by `a_call_to_an_ownerless_channel_records_no_server`,
            // which sweeps the extremes of the id space — it failed on the
            // one-compare version.)
            if !privileged && (rpc.server_tid == NO_SERVER || rpc.server_tid != replier) {
                return None;
            }
            let copy_len = reply_data.len().min(RPC_MSG_MAX_LEN);
            rpc.reply_buf[..copy_len].copy_from_slice(&reply_data[..copy_len]);
            rpc.reply_len = copy_len as u32;
            rpc.done.store(true, Ordering::Release);
            return Some(caller_tid);
        }
    }
    None
}

/// Retrieve reply data for a completed RPC and free the slot.
/// Called by the woken-up caller to get the result.
/// Returns (reply_len, reply_buf) or None if not found/not done.
pub fn rpc_get_reply(caller_tid: u32, dst: &mut [u8]) -> Option<u32> {
    let mut pending = RPC_PENDING.lock_irqsave();
    for i in 0..MAX_PENDING_RPCS {
        let rpc = &mut pending[i];
        if rpc.active && rpc.caller_tid == caller_tid && rpc.done.load(Ordering::Acquire) {
            let copy_len = (rpc.reply_len as usize).min(dst.len());
            dst[..copy_len].copy_from_slice(&rpc.reply_buf[..copy_len]);
            let reply_len = rpc.reply_len;
            // Free the slot
            *rpc = RpcPending::empty();
            return Some(reply_len);
        }
    }
    None
}

/// Reclaim every pending RPC `tid` participates in — task-exit hook (IPC-3).
///
/// **WHY this exists.** `RPC_PENDING` is a fixed 16-entry BSS table and
/// nothing reclaimed it: every client that died between `SYS_IPC_CALL` and
/// `SYS_IPC_REPLY` burned a slot permanently, and sixteen such deaths made
/// `rpc_register` return `None` for the life of the board.
///
/// The two roles are treated differently, because their failure modes are:
///
///  * **A dying client.** Free the slot, no wake. The task being reclaimed
///    *is* the blocked party; there is no third party asleep on this entry. A
///    server that later answers the dead TID finds no active slot and gets
///    `None`, which is the correct answer. The match ignores `done`, so a
///    reply that landed moments before the client died is discarded with the
///    slot rather than left `active` forever waiting for a `rpc_get_reply`
///    that will never come.
///
///  * **A dying server** — this is the half that used to be impossible.
///    `SYS_IPC_CALL` parks the client on `WaitReason::Rpc(tid)` with no
///    deadline of any kind: no timeout, no expiry sweep, nothing analogous to
///    `lease_tick`. If the only task that could answer exits, **the client
///    sleeps for the life of the board** — on a robot that is a control task
///    that silently stops actuating, not a hung shell. The entry is freed and
///    the client woken with `wake_by_rpc`; it re-runs `rpc_get_reply`, finds
///    nothing, and its `SYS_IPC_CALL` returns `-1`. A failed RPC is a result
///    the client can act on; an infinite sleep is not.
///
/// **WHY freeing (rather than flagging the entry "server dead") is enough.**
/// The `SYS_IPC_CALL` arm already returns `-1` on `rpc_get_reply(..) == None`,
/// so the error path exists and needs no new state, no extra field, and no
/// change to `rpc_get_reply`. Adding a "failed" flag would only let the
/// client distinguish *why* it failed, which no caller in the tree asks for.
///
/// The audit recorded this as unclosable from here — "`Channel` has no owner
/// field at all, so there is no mapping from a dead TID to the channels it
/// served". `channel.rs` grew `channel_owner` in the meantime, and
/// [`rpc_register`] now snapshots it into `server_tid`, which is that missing
/// key. Note the snapshot is load-bearing *for this function specifically*:
/// by the time `task_release_all` runs, the dead server's channels may
/// already be destroyed, so a live `channel_owner` lookup here would answer
/// `None` for every entry and release nothing.
///
/// **Wakes happen after the guard is dropped**, same rule as `lease_return`
/// and `lease_release_all`: waking under `RPC_PENDING` inverts the lock order
/// against the scheduler's task pool. TIDs are buffered on the stack first
/// (16 × 4 B).
///
/// Cost: exit path only, one pass over 16 slots under the table's own lock.
pub fn rpc_cancel_all(tid: u32) {
    let mut orphans: [u32; MAX_PENDING_RPCS] = [0; MAX_PENDING_RPCS];
    let mut n_orphans = 0usize;

    {
        let mut pending = RPC_PENDING.lock_irqsave();
        for i in 0..MAX_PENDING_RPCS {
            if !pending[i].active {
                continue;
            }
            // Client role first: a task that somehow called itself must be
            // freed silently rather than woken — it is the one exiting.
            if pending[i].caller_tid == tid {
                pending[i] = RpcPending::empty();
                continue;
            }
            if pending[i].server_tid == tid {
                let client = pending[i].caller_tid;
                pending[i] = RpcPending::empty();
                if n_orphans < MAX_PENDING_RPCS {
                    orphans[n_orphans] = client;
                    n_orphans += 1;
                }
            }
        }
    } // guard dropped — never wake while holding RPC_PENDING.

    for i in 0..n_orphans {
        robot_os_sched::wake_by_rpc(orphans[i]);
    }
}

/// Wipe the pending-RPC table. Host-test hygiene only — the suite shares one
/// static `RPC_PENDING`. Never built into the kernel.
#[cfg(test)]
pub fn __rpc_reset_for_tests() {
    let mut pending = RPC_PENDING.lock_irqsave();
    for i in 0..MAX_PENDING_RPCS {
        pending[i] = RpcPending::empty();
    }
}

/// Is slot `id` still occupied? (host tests)
#[cfg(test)]
pub fn __rpc_active_for_tests(id: u32) -> bool {
    if id as usize >= MAX_PENDING_RPCS { return false; }
    RPC_PENDING.lock_irqsave()[id as usize].active
}

/// Server TID snapshotted into slot `id` (host tests).
#[cfg(test)]
pub fn __rpc_server_for_tests(id: u32) -> u32 {
    if id as usize >= MAX_PENDING_RPCS { return NO_SERVER; }
    RPC_PENDING.lock_irqsave()[id as usize].server_tid
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static SERIAL: Mutex<()> = Mutex::new(());

    const SERVER: u32 = 77;
    const CLIENT: u32 = 1;
    const OTHER_CLIENT: u32 = 2;
    const IMPOSTOR: u32 = 99;

    /// Adopt an identity for *both* identity sources this test binary has.
    ///
    /// `channel.rs` reads the caller through its own `#[cfg(test)] test_ctx`
    /// atomics, while `rpc.rs` reads it from the `robot_os_sched` host shim.
    /// Setting only one produces a channel owned by TID 0 (the vacant marker)
    /// and an authorization failure that has nothing to do with the code under
    /// test.
    fn become_task(tid: u32, privileged: bool) {
        crate::channel::test_ctx::set(tid, privileged);
        robot_os_sched::shim_set_current(tid, if privileged { 0 } else { 0x1000 });
    }

    /// A channel owned by `SERVER`, plus a clean RPC table.
    fn setup() -> (std::sync::MutexGuard<'static, ()>, u32) {
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        __rpc_reset_for_tests();
        crate::channel::__channel_reset_for_tests();
        robot_os_sched::shim_reset();
        become_task(SERVER, false);
        let ch = crate::channel::channel_create().expect("channel pool exhausted") as u32;
        become_task(CLIENT, false);
        (g, ch)
    }

    // ── The registration snapshot ──────────────────────────────────────────

    #[test]
    fn register_snapshots_the_channels_owner_as_the_server() {
        let (_g, ch) = setup();
        let id = rpc_register(CLIENT, ch).unwrap();
        assert_eq!(__rpc_server_for_tests(id), SERVER);
    }

    #[test]
    fn a_call_to_an_ownerless_channel_records_no_server() {
        let (_g, _ch) = setup();
        // Never created: `channel_owner` answers `None`.
        let id = rpc_register(CLIENT, 63).unwrap();
        assert_eq!(__rpc_server_for_tests(id), NO_SERVER);
        // And nobody in ring 3 can answer it — including a task whose TID
        // happens to be the sentinel-adjacent extreme.
        for tid in [0u32, 1, SERVER, IMPOSTOR, u32::MAX - 1, u32::MAX] {
            become_task(tid, false);
            assert!(rpc_reply(CLIENT, b"x").is_none(), "tid {tid} answered an ownerless call");
        }
        // The kernel still can, by the house privilege convention.
        become_task(0, true);
        assert_eq!(rpc_reply(CLIENT, b"x"), Some(CLIENT));
    }

    // ── Who may answer (both halves, whole id space) ────────────────────────

    #[test]
    fn the_channel_owner_may_answer_and_nobody_else_may() {
        let (_g, ch) = setup();
        rpc_register(CLIENT, ch).unwrap();

        // The impostor half: every other TID in a wide sweep is refused, and
        // the slot is left untouched so the real server can still answer.
        for tid in (0u32..256).chain([1000, 65_535, u32::MAX - 1, u32::MAX]) {
            if tid == SERVER { continue; }
            become_task(tid, false);
            assert!(
                rpc_reply(CLIENT, b"forged").is_none(),
                "tid {tid} impersonated the server"
            );
        }
        let mut dst = [0u8; 8];
        assert!(
            rpc_get_reply(CLIENT, &mut dst).is_none(),
            "a refused reply still marked the call done"
        );

        // The legitimate half.
        become_task(SERVER, false);
        assert_eq!(rpc_reply(CLIENT, b"real"), Some(CLIENT));
        become_task(CLIENT, false);
        assert_eq!(rpc_get_reply(CLIENT, &mut dst), Some(4));
        assert_eq!(&dst[..4], b"real");
    }

    #[test]
    fn a_kernel_task_may_always_answer() {
        let (_g, ch) = setup();
        rpc_register(CLIENT, ch).unwrap();
        become_task(4242, true); // ring 0, not the channel owner
        assert_eq!(rpc_reply(CLIENT, b"k"), Some(CLIENT));
    }

    /// The recycled-id confused deputy the snapshot exists to stop: the server
    /// destroys its channel and somebody else re-creates the same index.
    #[test]
    fn destroying_and_recreating_the_channel_does_not_transfer_the_right_to_answer() {
        let (_g, ch) = setup();
        rpc_register(CLIENT, ch).unwrap();

        become_task(SERVER, false);
        assert_eq!(crate::channel::channel_destroy(ch as usize), 0);
        become_task(IMPOSTOR, false);
        let ch2 = crate::channel::channel_create().unwrap() as u32;
        assert_eq!(ch2, ch, "the pool did not recycle the index; test is inert");

        assert!(
            rpc_reply(CLIENT, b"forged").is_none(),
            "the new owner of a recycled channel index answered an old call"
        );
        become_task(SERVER, false);
        assert_eq!(rpc_reply(CLIENT, b"real"), Some(CLIENT));
    }

    // ── Task exit: the client half (unchanged behaviour) ────────────────────

    #[test]
    fn cancel_all_frees_only_the_dying_clients_slots() {
        let (_g, ch) = setup();
        let a = rpc_register(CLIENT, ch).unwrap();
        let b = rpc_register(OTHER_CLIENT, ch).unwrap();
        let c = rpc_register(CLIENT, ch).unwrap();

        rpc_cancel_all(CLIENT);

        assert!(!__rpc_active_for_tests(a));
        assert!(!__rpc_active_for_tests(c));
        // The other client's call is untouched — a task exit must not cancel
        // RPCs belonging to tasks that are still running.
        assert!(__rpc_active_for_tests(b));
        become_task(SERVER, false);
        assert_eq!(rpc_reply(OTHER_CLIENT, b"ok"), Some(OTHER_CLIENT));
    }

    #[test]
    fn cancel_all_discards_a_reply_that_landed_just_before_the_client_died() {
        let (_g, ch) = setup();
        let id = rpc_register(CLIENT, ch).unwrap();
        become_task(SERVER, false);
        assert_eq!(rpc_reply(CLIENT, b"late"), Some(CLIENT));
        // `done` is set but nobody will ever call `rpc_get_reply`. Matching on
        // `caller_tid` regardless of `done` is what stops that slot leaking.
        rpc_cancel_all(CLIENT);
        assert!(!__rpc_active_for_tests(id));
        let mut dst = [0u8; 8];
        assert!(rpc_get_reply(CLIENT, &mut dst).is_none());
    }

    #[test]
    fn exhausting_the_table_then_killing_the_client_makes_slots_available_again() {
        let (_g, ch) = setup();
        for _ in 0..MAX_PENDING_RPCS {
            assert!(rpc_register(CLIENT, ch).is_some());
        }
        // The permanent-failure state a board reached before IPC-3 wired this
        // function in: `rpc_cancel_all` had zero callers in the whole tree.
        assert!(rpc_register(CLIENT, ch).is_none());
        assert!(rpc_register(OTHER_CLIENT, ch).is_none());

        rpc_cancel_all(CLIENT);

        assert!(rpc_register(OTHER_CLIENT, ch).is_some());
    }

    #[test]
    fn replying_to_a_cancelled_client_is_a_clean_none() {
        let (_g, ch) = setup();
        rpc_register(CLIENT, ch).unwrap();
        rpc_cancel_all(CLIENT);
        // The server comes back later and answers a dead client.
        become_task(SERVER, false);
        assert!(rpc_reply(CLIENT, b"too late").is_none());
    }

    // ── Task exit: the server half (the gap that is now closed) ─────────────

    /// The audit's known gap, inverted into a property. A dying server used to
    /// leave its clients `active` and parked on `WaitReason::Rpc` with no
    /// timeout anywhere in the kernel — asleep for the life of the board.
    #[test]
    fn a_dying_server_releases_and_wakes_its_clients() {
        let (_g, ch) = setup();
        let a = rpc_register(CLIENT, ch).unwrap();
        let b = rpc_register(OTHER_CLIENT, ch).unwrap();

        rpc_cancel_all(SERVER);

        assert!(!__rpc_active_for_tests(a), "the server's exit left a client's slot allocated");
        assert!(!__rpc_active_for_tests(b));
        // Freeing the slot without waking would be the same infinite sleep
        // with the leak fixed — the actuation is the point.
        let woken = robot_os_sched::shim_rpc_wakes();
        assert!(woken.contains(&CLIENT), "client {CLIENT} was not woken: {woken:?}");
        assert!(woken.contains(&OTHER_CLIENT), "client {OTHER_CLIENT} was not woken: {woken:?}");
        // And the client's `SYS_IPC_CALL` now takes its error path.
        let mut dst = [0u8; 8];
        assert!(rpc_get_reply(CLIENT, &mut dst).is_none());
    }

    #[test]
    fn a_dying_server_does_not_touch_another_servers_clients() {
        let (_g, ch) = setup();
        // A second server with its own channel.
        become_task(IMPOSTOR, false);
        let ch2 = crate::channel::channel_create().unwrap() as u32;
        become_task(CLIENT, false);
        let mine = rpc_register(CLIENT, ch).unwrap();
        let theirs = rpc_register(OTHER_CLIENT, ch2).unwrap();

        rpc_cancel_all(SERVER);

        assert!(!__rpc_active_for_tests(mine));
        assert!(__rpc_active_for_tests(theirs), "an unrelated server's call was cancelled");
        assert_eq!(robot_os_sched::shim_rpc_wakes(), vec![CLIENT]);
    }

    /// A task that is both the client and the server of the same entry (a
    /// self-call) must be freed silently, not woken — it is the one exiting.
    #[test]
    fn a_self_call_is_freed_without_a_wake() {
        let (_g, _ch) = setup();
        become_task(SERVER, false);
        let own = crate::channel::channel_create().unwrap() as u32;
        let id = rpc_register(SERVER, own).unwrap();
        assert_eq!(__rpc_server_for_tests(id), SERVER);

        rpc_cancel_all(SERVER);

        assert!(!__rpc_active_for_tests(id));
        assert!(
            robot_os_sched::shim_rpc_wakes().is_empty(),
            "the exiting task was woken as its own orphaned client"
        );
    }

    /// A full table of one server's clients: every slot is released and every
    /// client woken, and the buffer that carries the TIDs out of the lock is
    /// exactly big enough.
    #[test]
    fn a_dying_server_with_the_table_full_releases_every_slot() {
        let (_g, ch) = setup();
        for i in 0..MAX_PENDING_RPCS {
            assert!(rpc_register(200 + i as u32, ch).is_some());
        }
        rpc_cancel_all(SERVER);
        for i in 0..MAX_PENDING_RPCS {
            assert!(!__rpc_active_for_tests(i as u32));
        }
        let woken = robot_os_sched::shim_rpc_wakes();
        assert_eq!(woken.len(), MAX_PENDING_RPCS);
        for i in 0..MAX_PENDING_RPCS {
            assert!(woken.contains(&(200 + i as u32)));
        }
        // Capacity is fully restored.
        assert!(rpc_register(CLIENT, ch).is_some());
    }

    // ── Limits ─────────────────────────────────────────────────────────────

    #[test]
    fn out_of_range_slot_queries_never_panic() {
        let (_g, _ch) = setup();
        assert!(!__rpc_active_for_tests(MAX_PENDING_RPCS as u32));
        assert!(!__rpc_active_for_tests(u32::MAX));
        let mut dst = [0u8; 4];
        assert!(rpc_get_reply(u32::MAX, &mut dst).is_none());
        rpc_cancel_all(u32::MAX);
        rpc_cancel_all(0);
    }

    /// A channel index far outside the pool must not panic in `rpc_register`;
    /// `channel_owner` bounds it, so the call registers with no server.
    #[test]
    fn an_absurd_channel_index_registers_without_a_server_and_never_panics() {
        let (_g, _ch) = setup();
        for ch in [u32::MAX, u32::MAX - 1, 1 << 31, 64, 65] {
            let id = rpc_register(CLIENT, ch).unwrap();
            assert_eq!(__rpc_server_for_tests(id), NO_SERVER);
            rpc_cancel_all(CLIENT);
        }
    }

    #[test]
    fn reply_longer_than_the_buffer_is_truncated_not_overflowed() {
        let (_g, ch) = setup();
        rpc_register(CLIENT, ch).unwrap();
        let big = [0xAAu8; RPC_MSG_MAX_LEN * 2];
        become_task(SERVER, false);
        assert_eq!(rpc_reply(CLIENT, &big), Some(CLIENT));
        let mut dst = [0u8; RPC_MSG_MAX_LEN];
        assert_eq!(rpc_get_reply(CLIENT, &mut dst), Some(RPC_MSG_MAX_LEN as u32));
        assert!(dst.iter().all(|b| *b == 0xAA));
    }
}
