//! Host-side runner for `crates/ipc/src/{channel,pipe,signal}.rs`.
//!
//! The kernel `robot_os_ipc` crate cannot be compiled for the host (it pulls
//! in `robot_os_drivers` / `robot_os_sched`, which are RV64-only). The three
//! modules audited in this lane are pure pool manipulation once two things
//! are stood in for:
//!
//!  * **`robot_os_sync::SpinLock`** — the real one calls `robot_os_arch::csr`
//!    (`crates/sync/src/spinlock.rs:15`) to save/restore `sstatus.SIE`, which
//!    does not exist off RISC-V. `shims/sync` provides the same API
//!    (`const fn new`, `lock`, `lock_irqsave`, `Deref`/`DerefMut` guard) over
//!    a `std::sync::Mutex` under the same *library* name, so the modules
//!    compile unmodified. Mutual exclusion semantics match; only the IRQ
//!    discipline is absent, and nothing under test depends on it.
//!  * **Caller identity** — `current_task_tid()` / `current_user_pt()` live in
//!    `robot_os_sched`. Each module has a `#[cfg(test)] mod test_ctx` shim
//!    (compiled *only* here, never into the kernel) that drives the identity
//!    from atomics, so a test can say "now I am ring-3 task 7".
//!
//! Everything else — the pools, the ownership fields, the bounds checks — is
//! the real kernel source, byte for byte.
//!
//! Run with:  `cd crates/ipc-chan-tests && cargo test`

// `cap.rs` carries `#[cfg(kani)]` proof harnesses for the model checker; that
// cfg is unknown to plain cargo and would otherwise warn on every build.
#![allow(unexpected_cfgs)]

// ---------------------------------------------------------------------------
// The modules under test
// ---------------------------------------------------------------------------

// `channel.rs` references `crate::cap` for the typed `Cap<Channel>` path, so
// cap.rs comes along. Its own embedded suite (also run by `crates/cap-tests`)
// therefore executes here too; those tests are not part of this lane's count.
#[path = "../../ipc/src/cap.rs"]
pub mod cap;

#[path = "../../ipc/src/channel.rs"]
pub mod channel;

#[path = "../../ipc/src/pipe.rs"]
pub mod pipe;

#[path = "../../ipc/src/signal.rs"]
pub mod signal;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

#[cfg(test)]
mod harness {
    use std::sync::Mutex;

    /// All three modules keep their state in `static` pools, and
    /// `cargo test` runs tests on parallel threads. Every test takes this
    /// lock and starts from a wiped pool, so one test can never observe
    /// another's channels/pipes/signal entries. The tree's alternative
    /// (`__pipeline_reset_for_tests` in `zerocopy.rs`) resets but does not
    /// serialize; with a shared global pool that is not enough here.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    pub struct Guard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    /// Serialize, wipe all three pools, and start as "kernel task, tid 0".
    pub fn begin() -> Guard {
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::channel::__channel_reset_for_tests();
        crate::pipe::__pipe_reset_for_tests();
        crate::signal::__signal_reset_for_tests();
        as_kernel();
        Guard(g)
    }

    /// Become ring-3 task `tid`.
    pub fn as_user(tid: u32) {
        crate::channel::test_ctx::set(tid, false);
        crate::pipe::test_ctx::set(tid, false);
        crate::signal::test_ctx::set(tid, false);
    }

    /// Become a kernel task (`user_pt == 0` ⇒ privileged bypass).
    pub fn as_kernel() {
        crate::channel::test_ctx::set(0, true);
        crate::pipe::test_ctx::set(0, true);
        crate::signal::test_ctx::set(0, true);
    }
}

// ---------------------------------------------------------------------------
// channel.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod channel_tests {
    use crate::channel::*;
    use crate::harness::{as_kernel, as_user, begin};

    const OWNER: u32 = 11;
    const STRANGER: u32 = 22;

    /// Create `n` channels as ring-3 task `tid`.
    fn create_as(tid: u32, n: usize) -> Vec<usize> {
        as_user(tid);
        (0..n)
            .map(|i| channel_create().unwrap_or_else(|| panic!("create #{i} failed")))
            .collect()
    }

    // ── Ownership: the decision ──────────────────────────────────────────

    #[test]
    fn create_records_the_calling_tid_as_owner() {
        let _g = begin();
        for ch in create_as(OWNER, 4) {
            assert_eq!(channel_owner(ch), Some(OWNER), "ch {ch}");
        }
    }

    #[test]
    fn owner_of_a_free_slot_is_none() {
        let _g = begin();
        assert_eq!(channel_owner(0), None);
        assert_eq!(channel_owner(MAX_CHANNELS - 1), None);
    }

    // ── Ownership: the action (both halves, over several ids) ────────────

    #[test]
    fn owner_receives_what_was_sent() {
        let _g = begin();
        for ch in create_as(OWNER, 4) {
            as_user(OWNER);
            assert_eq!(channel_send(ch, b"ping"), 0);
            let mut buf = [0u8; 16];
            assert_eq!(channel_recv(ch, &mut buf), 4, "ch {ch}");
            assert_eq!(&buf[..4], b"ping");
        }
    }

    #[test]
    fn stranger_cannot_receive_on_any_id() {
        let _g = begin();
        let chans = create_as(OWNER, 4);
        for &ch in &chans {
            as_user(OWNER);
            assert_eq!(channel_send(ch, b"secret"), 0);
        }
        // A third party sweeps every id it can think of, not just the ones
        // it happens to know about.
        as_user(STRANGER);
        for ch in 0..MAX_CHANNELS {
            let mut buf = [0u8; 16];
            assert_eq!(channel_recv(ch, &mut buf), -1, "stranger drained ch {ch}");
            assert_eq!(buf, [0u8; 16], "stranger read bytes out of ch {ch}");
        }
        // ...and the messages are all still there for the rightful owner.
        as_user(OWNER);
        for &ch in &chans {
            let mut buf = [0u8; 16];
            assert_eq!(channel_recv(ch, &mut buf), 6, "ch {ch} lost its message");
            assert_eq!(&buf[..6], b"secret");
        }
    }

    #[test]
    fn kernel_bypasses_the_owner_check() {
        let _g = begin();
        let ch = create_as(OWNER, 1)[0];
        as_user(OWNER);
        assert_eq!(channel_send(ch, b"kmsg"), 0);
        // House convention: current_user_pt() == 0 ⇒ privileged.
        as_kernel();
        let mut buf = [0u8; 16];
        assert_eq!(channel_recv(ch, &mut buf), 4);
        assert_eq!(&buf[..4], b"kmsg");
    }

    /// Pins the `owner == 0` sentinel reasoning documented on
    /// `Channel::owner`: a channel created before any task exists (or by the
    /// idle context) records owner 0, and `current_task_tid()` never returns
    /// 0 for a live task — so ring 3 is denied by construction rather than by
    /// an explicit "is it zero" branch.
    #[test]
    fn kernel_created_channel_denies_ring3_by_sentinel() {
        let _g = begin();
        as_kernel(); // tid 0 — the "no current task" value
        let ch = channel_create().unwrap();
        assert_eq!(channel_owner(ch), Some(0));
        assert_eq!(channel_send(ch, b"boot"), 0);

        for tid in [1u32, 22, u32::MAX] {
            as_user(tid);
            let mut buf = [0u8; 8];
            assert_eq!(channel_recv(ch, &mut buf), -1, "tid {tid} drained it");
            assert_eq!(channel_destroy(ch), -1, "tid {tid} destroyed it");
        }
        as_kernel();
        let mut buf = [0u8; 8];
        assert_eq!(channel_recv(ch, &mut buf), 4);
    }

    #[test]
    fn stranger_cannot_destroy_but_owner_and_kernel_can() {
        let _g = begin();
        let chans = create_as(OWNER, 3);

        as_user(STRANGER);
        for &ch in &chans {
            assert_eq!(channel_destroy(ch), -1, "stranger destroyed ch {ch}");
            assert_eq!(channel_owner(ch), Some(OWNER), "ch {ch} survived?");
        }

        as_user(OWNER);
        assert_eq!(channel_destroy(chans[0]), 0);
        assert_eq!(channel_owner(chans[0]), None);

        as_kernel();
        assert_eq!(channel_destroy(chans[1]), 0);
        assert_eq!(channel_owner(chans[1]), None);
    }

    #[test]
    fn recycled_slot_denies_the_previous_owner() {
        let _g = begin();
        // Owner creates, then gives the channel back.
        let ch = create_as(OWNER, 1)[0];
        as_user(OWNER);
        assert_eq!(channel_destroy(ch), 0);

        // A different task grabs the freed slot.
        as_user(STRANGER);
        let ch2 = channel_create().unwrap();
        assert_eq!(ch2, ch, "expected the freed slot to be reused");

        // The stale id in the previous owner's hand is now inert: the owner
        // field doubles as a generation counter, so there is no ABA.
        as_user(OWNER);
        let mut buf = [0u8; 8];
        assert_eq!(channel_recv(ch, &mut buf), -1);
        assert_eq!(channel_destroy(ch), -1);
    }

    /// PINNED CURRENT BEHAVIOUR, NOT A GUARANTEE.
    ///
    /// `channel_send` is deliberately ungated: `SYS_IPC_CALL`
    /// (`dispatch.rs:232`) has the RPC *client* send on the *server's*
    /// channel. Closing this needs a grantable send right — `SYS_CAP_GRANT`
    /// does not exist yet, and `handle_owned_by` costs a 256-entry locked
    /// sweep on the hot path. **Invert this assertion** the day a send right
    /// lands; until then it documents a known-open hole rather than
    /// pretending there is a gate.
    #[test]
    fn send_is_open_to_non_owners_by_design() {
        let _g = begin();
        let ch = create_as(OWNER, 1)[0];
        as_user(STRANGER);
        assert_eq!(channel_send(ch, b"rpc-request"), 0);
        as_user(OWNER);
        let mut buf = [0u8; 32];
        assert_eq!(channel_recv(ch, &mut buf), 11);
    }

    /// The typed path must NOT be subject to the legacy owner gate: a cap is
    /// minted by the kernel for a grantee who is by construction not the
    /// creator (`kernel_grant_channel_cap`, `handlers.rs:1459`).
    #[test]
    fn typed_cap_recv_bypasses_the_owner_gate() {
        use crate::cap::{targets::Channel, Cap, CapPerms, CapTable};
        let _g = begin();
        let ch = create_as(OWNER, 1)[0];
        as_user(OWNER);
        assert_eq!(channel_send(ch, b"typed"), 0);

        let mut table = CapTable::empty();
        let cap: Cap<Channel> = table.grant(CapPerms::RW, ch as u32).unwrap();

        // Grantee is a completely different task.
        as_user(STRANGER);
        let mut buf = [0u8; 16];
        let n = channel_recv_cap(&table, cap, &mut buf).expect("cap recv denied");
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"typed");
    }

    // ── Bounds and limits: no panic is the requirement ───────────────────

    #[test]
    fn out_of_range_ids_never_panic() {
        let _g = begin();
        let bad = [MAX_CHANNELS, MAX_CHANNELS + 1, usize::MAX, usize::MAX / 2];
        for who in [true, false] {
            if who { as_kernel() } else { as_user(STRANGER) }
            for &ch in &bad {
                let mut buf = [0u8; 8];
                assert_eq!(channel_send(ch, b"x"), -1, "send {ch}");
                assert_eq!(channel_recv(ch, &mut buf), -1, "recv {ch}");
                assert_eq!(channel_destroy(ch), -1, "destroy {ch}");
                assert_eq!(channel_owner(ch), None, "owner {ch}");
            }
        }
    }

    #[test]
    fn inactive_channel_is_rejected_not_read() {
        let _g = begin();
        as_kernel();
        let mut buf = [0u8; 8];
        for ch in 0..MAX_CHANNELS {
            assert_eq!(channel_recv(ch, &mut buf), -1, "ch {ch}");
            assert_eq!(channel_send(ch, b"x"), -1, "ch {ch}");
        }
    }

    #[test]
    fn payload_at_and_over_the_limit() {
        let _g = begin();
        let ch = create_as(OWNER, 1)[0];
        as_user(OWNER);
        let exact = [0xABu8; MSG_MAX_LEN];
        assert_eq!(channel_send(ch, &exact), 0, "exactly MSG_MAX_LEN must fit");
        let over = [0xCDu8; MSG_MAX_LEN + 1];
        assert_eq!(channel_send(ch, &over), -1, "MSG_MAX_LEN+1 must be refused");
        // Zero-length is legal and round-trips as zero bytes.
        assert_eq!(channel_send(ch, &[]), 0);

        let mut buf = [0u8; MSG_MAX_LEN];
        assert_eq!(channel_recv(ch, &mut buf), MSG_MAX_LEN as i32);
        assert_eq!(buf, exact);
        assert_eq!(channel_recv(ch, &mut buf), 0, "zero-length message");
    }

    #[test]
    fn ring_fills_and_refuses_without_panicking() {
        let _g = begin();
        let ch = create_as(OWNER, 1)[0];
        as_user(OWNER);
        // The ring keeps one slot empty to distinguish full from empty.
        for i in 0..RING_CAP - 1 {
            assert_eq!(channel_send(ch, &[i as u8]), 0, "send #{i}");
        }
        for i in 0..8 {
            assert_eq!(channel_send(ch, b"overflow"), -1, "extra send #{i}");
        }
        let mut buf = [0u8; 4];
        for i in 0..RING_CAP - 1 {
            assert_eq!(channel_recv(ch, &mut buf), 1);
            assert_eq!(buf[0], i as u8, "FIFO order broken");
        }
        assert_eq!(channel_recv(ch, &mut buf), 0, "ring should now be empty");
    }

    #[test]
    fn recv_into_short_or_empty_buffer_truncates() {
        let _g = begin();
        let ch = create_as(OWNER, 1)[0];
        as_user(OWNER);
        assert_eq!(channel_send(ch, b"0123456789"), 0);
        let mut small = [0u8; 4];
        assert_eq!(channel_recv(ch, &mut small), 4);
        assert_eq!(&small, b"0123");

        assert_eq!(channel_send(ch, b"abc"), 0);
        let mut empty: [u8; 0] = [];
        assert_eq!(channel_recv(ch, &mut empty), 0, "empty dst copies 0 bytes");
    }

    #[test]
    fn pool_exhaustion_returns_none_not_panic() {
        let _g = begin();
        as_user(OWNER);
        for i in 0..MAX_CHANNELS {
            assert!(channel_create().is_some(), "create #{i}");
        }
        for _ in 0..4 {
            assert!(channel_create().is_none(), "pool must refuse past capacity");
        }
    }

    #[test]
    fn wrap_around_preserves_order() {
        let _g = begin();
        let ch = create_as(OWNER, 1)[0];
        as_user(OWNER);
        let mut buf = [0u8; 4];
        // Push/pop far past RING_CAP so head and tail wrap several times.
        for round in 0..(RING_CAP as u8 * 5) {
            assert_eq!(channel_send(ch, &[round]), 0, "round {round}");
            assert_eq!(channel_recv(ch, &mut buf), 1, "round {round}");
            assert_eq!(buf[0], round);
        }
    }
}

// ---------------------------------------------------------------------------
// pipe.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod pipe_tests {
    use crate::harness::{as_kernel, as_user, begin};
    use crate::pipe::*;

    const OWNER: u32 = 33;
    const STRANGER: u32 = 44;

    fn create_as(tid: u32, n: usize) -> Vec<usize> {
        as_user(tid);
        (0..n)
            .map(|i| pipe_create().unwrap_or_else(|| panic!("create #{i} failed")).0)
            .collect()
    }

    // ── Ownership ────────────────────────────────────────────────────────

    #[test]
    fn create_records_owner_and_round_trips() {
        let _g = begin();
        for idx in create_as(OWNER, 4) {
            assert_eq!(pipe_owner(idx), Some(OWNER), "pipe {idx}");
            as_user(OWNER);
            assert_eq!(pipe_write_buf(idx, b"hello"), 5);
            assert_eq!(pipe_available(idx), 5);
            let mut buf = [0u8; 16];
            assert_eq!(pipe_read_buf(idx, &mut buf), 5);
            assert_eq!(&buf[..5], b"hello");
        }
    }

    #[test]
    fn stranger_cannot_read_write_or_close_any_index() {
        let _g = begin();
        let pipes = create_as(OWNER, 4);
        for &idx in &pipes {
            as_user(OWNER);
            assert_eq!(pipe_write_buf(idx, b"private"), 7);
        }

        as_user(STRANGER);
        for idx in 0..MAX_PIPES {
            let mut buf = [0u8; 16];
            assert_eq!(pipe_read_buf(idx, &mut buf), -1, "read {idx}");
            assert_eq!(buf, [0u8; 16], "stranger got bytes out of pipe {idx}");
            assert_eq!(pipe_write_buf(idx, b"poison"), -1, "write {idx}");
            assert_eq!(pipe_close_read(idx), -1, "close_read {idx}");
            assert_eq!(pipe_close_write(idx), -1, "close_write {idx}");
            assert_eq!(pipe_available(idx), 0, "available {idx} leaked occupancy");
            assert_eq!(pipe_space(idx), 0, "space {idx} leaked occupancy");
        }

        // Nothing the stranger did took effect.
        as_user(OWNER);
        for &idx in &pipes {
            assert_eq!(pipe_available(idx), 7, "pipe {idx} was tampered with");
            let mut buf = [0u8; 16];
            assert_eq!(pipe_read_buf(idx, &mut buf), 7);
            assert_eq!(&buf[..7], b"private");
        }
    }

    #[test]
    fn kernel_bypasses_the_owner_check() {
        let _g = begin();
        let idx = create_as(OWNER, 1)[0];
        as_kernel();
        assert_eq!(pipe_write_buf(idx, b"kernel"), 6);
        let mut buf = [0u8; 16];
        assert_eq!(pipe_read_buf(idx, &mut buf), 6);
        assert!(pipe_space(idx) > 0);
        assert_eq!(pipe_close_write(idx), 0);
        assert_eq!(pipe_close_read(idx), 0);
    }

    #[test]
    fn owner_can_close_each_end_once() {
        let _g = begin();
        let idx = create_as(OWNER, 1)[0];
        as_user(OWNER);
        assert_eq!(pipe_write_buf(idx, b"last"), 4);
        assert_eq!(pipe_close_write(idx), 0);
        // Data written before the close is still readable...
        let mut buf = [0u8; 8];
        assert_eq!(pipe_read_buf(idx, &mut buf), 4);
        // ...then EOF, not EAGAIN, because the writer is gone.
        assert_eq!(pipe_read_buf(idx, &mut buf), 0);
        // And writing after the read end closes is EPIPE.
        assert_eq!(pipe_close_read(idx), 0);
        assert_eq!(pipe_write_buf(idx, b"x"), -1);
    }

    #[test]
    fn empty_pipe_with_live_writer_is_eagain() {
        let _g = begin();
        let idx = create_as(OWNER, 1)[0];
        as_user(OWNER);
        let mut buf = [0u8; 8];
        assert_eq!(pipe_read_buf(idx, &mut buf), -2, "EAGAIN while writer alive");
    }

    // ── Raw pointers and bounds: no panic, no over-read ──────────────────

    #[test]
    fn null_pointers_are_refused() {
        let _g = begin();
        let idx = create_as(OWNER, 1)[0];
        as_user(OWNER);
        assert_eq!(pipe_read(idx, core::ptr::null_mut(), 16), -1);
        assert_eq!(pipe_write(idx, core::ptr::null(), 16), -1);
        // ...and with count 0 as well, since 0 is the other way a caller
        // says "no buffer".
        assert_eq!(pipe_read(idx, core::ptr::null_mut(), 0), -1);
        assert_eq!(pipe_write(idx, core::ptr::null(), 0), -1);
    }

    #[test]
    fn out_of_range_indices_never_panic() {
        let _g = begin();
        let bad = [MAX_PIPES, MAX_PIPES + 1, usize::MAX, usize::MAX / 2];
        let mut buf = [0u8; 8];
        for who in [true, false] {
            if who { as_kernel() } else { as_user(STRANGER) }
            for &idx in &bad {
                assert_eq!(pipe_read_buf(idx, &mut buf), -1, "read {idx}");
                assert_eq!(pipe_write_buf(idx, b"x"), -1, "write {idx}");
                assert_eq!(pipe_close_read(idx), -1, "close_read {idx}");
                assert_eq!(pipe_close_write(idx), -1, "close_write {idx}");
                assert_eq!(pipe_available(idx), 0, "available {idx}");
                assert_eq!(pipe_space(idx), 0, "space {idx}");
                assert_eq!(pipe_owner(idx), None, "owner {idx}");
            }
        }
    }

    #[test]
    fn free_slot_is_rejected() {
        let _g = begin();
        as_kernel();
        let mut buf = [0u8; 8];
        for idx in 0..MAX_PIPES {
            assert_eq!(pipe_read_buf(idx, &mut buf), -1, "read free {idx}");
            assert_eq!(pipe_write_buf(idx, b"x"), -1, "write free {idx}");
            assert_eq!(pipe_close_read(idx), -1, "close free {idx}");
            assert_eq!(pipe_owner(idx), None);
        }
    }

    #[test]
    fn empty_slices_are_a_no_op() {
        let _g = begin();
        let idx = create_as(OWNER, 1)[0];
        as_user(OWNER);
        let mut empty: [u8; 0] = [];
        assert_eq!(pipe_write_buf(idx, &[]), 0);
        assert_eq!(pipe_read_buf(idx, &mut empty), 0);
        assert_eq!(pipe_available(idx), 0);
    }

    /// `count` larger than the buffer is the caller's bug, but the pipe must
    /// still clamp to its own free space and never walk off its ring. The
    /// buffer here is a real `PIPE_BUF_SIZE` array, so the clamped copy stays
    /// inside it — which is exactly the safety contract documented on
    /// `pipe_write`.
    #[test]
    fn oversized_count_clamps_to_ring_capacity() {
        let _g = begin();
        let idx = create_as(OWNER, 1)[0];
        as_user(OWNER);
        let src = [0x5Au8; PIPE_BUF_SIZE];
        // Ask for twice the ring; the ring keeps one byte free as the
        // full/empty discriminator.
        let n = pipe_write(idx, src.as_ptr(), PIPE_BUF_SIZE * 2);
        assert_eq!(n, (PIPE_BUF_SIZE - 1) as i32);
        assert_eq!(pipe_space(idx), 0, "ring should now be full");
        // A second write finds no space and writes nothing.
        assert_eq!(pipe_write(idx, src.as_ptr(), PIPE_BUF_SIZE), 0);

        let mut dst = [0u8; PIPE_BUF_SIZE];
        let r = pipe_read(idx, dst.as_mut_ptr(), PIPE_BUF_SIZE * 2);
        assert_eq!(r, (PIPE_BUF_SIZE - 1) as i32);
        assert!(dst[..PIPE_BUF_SIZE - 1].iter().all(|&b| b == 0x5A));
    }

    #[test]
    fn ring_wraps_without_losing_bytes() {
        let _g = begin();
        let idx = create_as(OWNER, 1)[0];
        as_user(OWNER);
        let chunk = [0u8; 512];
        let mut out = [0u8; 512];
        // 20 × 512 = 10240 bytes through a 4096-byte ring ⇒ several wraps.
        for round in 0..20u8 {
            let payload: Vec<u8> = chunk.iter().map(|_| round).collect();
            assert_eq!(pipe_write_buf(idx, &payload), 512, "round {round}");
            assert_eq!(pipe_read_buf(idx, &mut out), 512, "round {round}");
            assert!(out.iter().all(|&b| b == round), "round {round} corrupted");
        }
    }

    #[test]
    fn pool_exhaustion_returns_none_not_panic() {
        let _g = begin();
        as_user(OWNER);
        for i in 0..MAX_PIPES {
            assert!(pipe_create().is_some(), "create #{i}");
        }
        for _ in 0..4 {
            assert!(pipe_create().is_none(), "pool must refuse past capacity");
        }
    }
}

// ---------------------------------------------------------------------------
// signal.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod signal_tests {
    use crate::harness::{as_kernel, as_user, begin};
    use crate::signal::*;

    const SELF_TID: u32 = 55;
    const VICTIM: u32 = 66;

    // ── Policy: ring 3 signals only itself ───────────────────────────────

    #[test]
    fn ring3_may_signal_itself() {
        let _g = begin();
        as_user(SELF_TID);
        assert_eq!(signal_send(SELF_TID, SIGUSR1), 0);
        assert_eq!(signal_pending() & (1 << SIGUSR1), 1 << SIGUSR1);
    }

    #[test]
    fn ring3_may_not_signal_another_task_and_leaves_no_trace() {
        let _g = begin();
        // The victim exists in the table with a clean slate.
        as_user(VICTIM);
        assert_eq!(signal_send(VICTIM, SIGUSR2), 0);
        let before = signal_table_len();

        as_user(SELF_TID);
        for tid in [VICTIM, 0, 1, 7, 4242, u32::MAX] {
            assert_eq!(signal_send(tid, SIGKILL), -1, "cross-task send to {tid}");
        }
        // Crucially: the refused sends allocated nothing. This is what stops
        // 64 `kill()` calls with invented TIDs from filling the table and
        // aliasing every task onto slot 0.
        assert_eq!(signal_table_len(), before, "denied send still grew the table");

        as_user(VICTIM);
        assert_eq!(signal_pending() & (1 << SIGKILL), 0, "victim got signalled");
    }

    #[test]
    fn kernel_may_signal_any_task() {
        let _g = begin();
        as_kernel();
        for tid in [VICTIM, 1, 2, 3] {
            assert_eq!(signal_send(tid, SIGTERM), 0, "kernel send to {tid}");
        }
        as_user(VICTIM);
        assert_eq!(signal_pending() & (1 << SIGTERM), 1 << SIGTERM);
    }

    // ── Signal numbers: the shift that must never overflow ───────────────

    #[test]
    fn invalid_signal_numbers_are_refused_without_panicking() {
        let _g = begin();
        // `overflow-checks = true` + `panic = "abort"`: a `1u32 << 32` here
        // would reset the board, so this is a safety test, not hygiene.
        let bad = [0u32, NSIG, NSIG + 1, 32, 63, 64, 1000, u32::MAX, u32::MAX - 1];
        for who in [true, false] {
            if who { as_kernel() } else { as_user(SELF_TID) }
            for &s in &bad {
                assert_eq!(signal_send(SELF_TID, s), -1, "signum {s}");
                assert_eq!(signal_set_handler(s, 0xDEAD), SIG_DFL, "handler {s}");
                assert!(!signal_valid(s), "signal_valid({s})");
            }
        }
    }

    #[test]
    fn every_valid_signal_number_is_accepted() {
        let _g = begin();
        as_user(SELF_TID);
        for s in 1..NSIG {
            assert_eq!(signal_send(SELF_TID, s), 0, "signum {s}");
        }
        // All 31 bits set, none of them bit 0.
        assert_eq!(signal_pending(), !1u32);
    }

    // ── Table exhaustion: the index-0 aliasing bug ───────────────────────

    #[test]
    fn full_table_fails_closed_instead_of_aliasing_onto_slot_zero() {
        let _g = begin();
        as_kernel();

        // Task 1 registers first, so under the old code it owned slot 0 —
        // the slot every overflowing caller used to be handed.
        const FIRST: u32 = 1;
        assert_eq!(signal_send(FIRST, SIGUSR1), 0);
        as_user(FIRST);
        assert_eq!(signal_set_mask(0), 0);
        let first_pending_before = signal_pending();
        assert_eq!(first_pending_before, 1 << SIGUSR1);

        // Fill the rest of the table (kernel privilege lets us target any
        // TID; a ring-3 task could do this too before the self-only rule).
        as_kernel();
        let mut filled = 1usize;
        let mut tid = FIRST + 1;
        while signal_send(tid, SIGUSR2) == 0 {
            filled += 1;
            tid += 1;
            assert!(tid < 10_000, "table never filled — is it unbounded?");
        }
        assert_eq!(signal_table_len(), filled, "count disagrees with reality");

        // The overflowing task now fails closed...
        let overflow_tid = tid;
        assert_eq!(signal_send(overflow_tid, SIGKILL), -1);
        as_user(overflow_tid);
        assert_eq!(signal_set_mask(0xFFFF_FFFF), -1, "set_mask must fail closed");
        assert_eq!(
            signal_set_handler(SIGUSR1, 0xBAD),
            SIG_DFL,
            "set_handler must fail closed"
        );

        // ...and, the whole point: task 1's state is untouched. Under the old
        // `get_or_create` these three calls all landed on slot 0.
        as_user(FIRST);
        assert_eq!(signal_pending(), first_pending_before, "slot 0 was clobbered");
        assert_eq!(signal_get_mask(), 0, "slot 0 mask was clobbered");
    }

    #[test]
    fn release_frees_a_slot_and_keeps_the_rest_findable() {
        let _g = begin();
        as_kernel();
        for tid in 1..=5u32 {
            assert_eq!(signal_send(tid, SIGUSR1), 0);
        }
        assert_eq!(signal_table_len(), 5);

        // Drop one from the middle: the compaction moves the last entry into
        // the hole, so `find`'s `0..count` scan must still see it.
        assert!(signal_release(3));
        assert_eq!(signal_table_len(), 4);
        assert!(!signal_release(3), "double release must be a no-op");
        assert!(!signal_release(999), "releasing an unknown tid must be a no-op");

        for tid in [1u32, 2, 4, 5] {
            as_user(tid);
            assert_eq!(
                signal_pending() & (1 << SIGUSR1),
                1 << SIGUSR1,
                "tid {tid} lost its state after compaction"
            );
        }
        as_user(3);
        assert_eq!(signal_pending(), 0, "released tid should have no state");
    }

    #[test]
    fn release_makes_room_again() {
        let _g = begin();
        as_kernel();
        let mut tid = 1u32;
        while signal_send(tid, SIGUSR1) == 0 {
            tid += 1;
            assert!(tid < 10_000);
        }
        let overflow_tid = tid;
        assert_eq!(signal_send(overflow_tid, SIGUSR1), -1);
        assert!(signal_release(1));
        assert_eq!(
            signal_send(overflow_tid, SIGUSR1),
            0,
            "a freed slot must be reusable — this is what task_release_all buys"
        );
    }

    // ── Handlers and masks ───────────────────────────────────────────────

    #[test]
    fn handler_round_trips_but_kill_and_stop_stay_default() {
        let _g = begin();
        as_user(SELF_TID);
        assert_eq!(signal_set_handler(SIGUSR1, 0x1234), SIG_DFL);
        assert_eq!(signal_set_handler(SIGUSR1, 0x5678), 0x1234);
        assert_eq!(signal_set_handler(SIGUSR1, SIG_IGN), 0x5678);

        // Uncatchable signals: the write is silently dropped, the read
        // still reports SIG_DFL.
        for s in [SIGKILL, SIGSTOP] {
            assert_eq!(signal_set_handler(s, 0xDEADBEEF), SIG_DFL, "sig {s}");
            assert_eq!(signal_set_handler(s, 0), SIG_DFL, "sig {s} was stored");
            assert!(!signal_catchable(s));
        }
    }

    #[test]
    fn handlers_are_per_task_not_shared() {
        let _g = begin();
        as_user(SELF_TID);
        assert_eq!(signal_set_handler(SIGUSR1, 0xAAAA), SIG_DFL);
        as_user(VICTIM);
        assert_eq!(
            signal_set_handler(SIGUSR1, 0xBBBB),
            SIG_DFL,
            "second task saw the first task's handler"
        );
        as_user(SELF_TID);
        assert_eq!(signal_set_handler(SIGUSR1, 0), 0xAAAA);
    }

    #[test]
    fn mask_hides_pending_but_never_kill_or_stop() {
        let _g = begin();
        as_user(SELF_TID);
        assert_eq!(signal_send(SELF_TID, SIGUSR1), 0);
        assert_eq!(signal_send(SELF_TID, SIGKILL), 0);
        assert_eq!(signal_send(SELF_TID, SIGSTOP), 0);

        assert_eq!(signal_set_mask(0xFFFF_FFFF), 0);
        let m = signal_get_mask();
        assert_eq!(m & (1 << SIGKILL), 0, "SIGKILL must not be maskable");
        assert_eq!(m & (1 << SIGSTOP), 0, "SIGSTOP must not be maskable");

        let p = signal_pending();
        assert_eq!(p & (1 << SIGUSR1), 0, "SIGUSR1 should be masked out");
        assert_eq!(p & (1 << SIGKILL), 1 << SIGKILL);
        assert_eq!(p & (1 << SIGSTOP), 1 << SIGSTOP);

        assert_eq!(signal_set_mask(0), 0);
        assert_eq!(signal_pending() & (1 << SIGUSR1), 1 << SIGUSR1);
    }

    #[test]
    fn unknown_task_has_no_state_and_no_panic() {
        let _g = begin();
        as_user(4242);
        assert_eq!(signal_pending(), 0);
        assert_eq!(signal_get_mask(), 0);
        assert_eq!(signal_table_len(), 0, "a pure read must not allocate");
    }

    #[test]
    fn default_actions_are_defined_for_every_signal_number() {
        let _g = begin();
        // `signal_default_action` has a catch-all arm; walk the whole u8
        // space plus the edges to prove no arithmetic in there can trap.
        for s in 0..=300u32 {
            let _ = signal_default_action(s);
        }
        let _ = signal_default_action(u32::MAX);
        assert!(matches!(
            signal_default_action(SIGKILL),
            SigDefaultAction::Term
        ));
        assert!(matches!(
            signal_default_action(SIGCONT),
            SigDefaultAction::Cont
        ));
    }
}
