//! Host-side runner for `crates/ipc/src/cap.rs` and the delegation half of
//! `crates/ipc/src/cap_store.rs`.
//!
//! The kernel `robot_os_ipc` crate cannot be compiled for the host
//! (it depends on RV64-only crates). But `cap.rs` itself only depends
//! on `robot_os_abi`, which is host-friendly. We pull `cap.rs` in
//! directly via `#[path]` and let the embedded `#[cfg(test)] mod tests`
//! run on the host.
//!
//! `cap_store.rs` needs two more crates — `robot_os_sync` (RV64 CSR asm in
//! `SpinLock`) and `robot_os_sched` (the whole scheduler) — so those are
//! replaced by the host shims under `shims/` via a Cargo dependency rename.
//! The kernel build never sees them.
//!
//! **The delegation tests live here, not in `cap_store.rs`.**
//! `crates/ipc-lease-tests/src/lib.rs` also pulls `cap_store.rs` in with
//! `#[path]`, so a `#[cfg(test)] mod tests` inside that file would compile
//! and run against *that* crate's scheduler shim, which has a fixed identity
//! TID→slot map and no way to make a TID dead. These tests need exactly that
//! control, and `ipc-lease-tests` is not ours to change.

#[path = "../../ipc/src/cap.rs"]
pub mod cap;

#[path = "../../ipc/src/cap_store.rs"]
pub mod cap_store;

// ──────────────────────────────────────────────────────────────────────────
// SYS_CAP_GRANT — delegation tests (W3-F10)
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod delegate_tests {
    use crate::cap::targets::{Channel, Gpio, Motor};
    use crate::cap::{Cap, CapError, CapHandle, CapKind, CapPerms};
    use crate::cap_store::{self, DelegateError};
    use robot_os_sched::{shim_bind, shim_clear_stale, shim_kill, shim_stale_once};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, MutexGuard};

    /// `cap_store`'s tables, its `OWNER` array and the scheduler shim are all
    /// process-global, and `cargo test` runs test functions in parallel. Every
    /// test in this module takes this lock for its whole body.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Hands out task identities that no other test has used. Slots are never
    /// recycled between tests, so one test can never observe another's
    /// `OWNER` registration or leftover cap slots — which matters here more
    /// than usual, since a stale `OWNER` is one of the things under test.
    static NEXT_ID: AtomicU32 = AtomicU32::new(1);

    struct Task {
        tid: u32,
        slot: usize,
    }

    fn fresh_task() -> Task {
        let n = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let slot = n as usize;
        assert!(
            slot < robot_os_sched::task::MAX_TASKS,
            "this module has outgrown the task pool ({} slots): give tests \
             back their slots or raise MAX_TASKS",
            robot_os_sched::task::MAX_TASKS
        );
        // TID 0 is the "no current task" sentinel; the allocator starts at 1.
        let tid = n;
        shim_bind(tid, slot);
        Task { tid, slot }
    }

    /// Take the module lock. Deliberately recovers from poisoning: one failing
    /// test should not cascade into 16 more.
    fn guard() -> MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Bindings made by the previous test stay harmless (its TIDs and
        // slots are never reused), but an unconsumed one-shot lie must not
        // leak into this one.
        shim_clear_stale();
        g
    }

    // ── The legitimate half ────────────────────────────────────────────────

    #[test]
    fn legitimate_delegation_transfers_attenuated_authority() {
        let _g = guard();
        let a = fresh_task();
        let b = fresh_task();

        // A holds a read/write channel cap it is allowed to pass on.
        let src: Cap<Channel> =
            cap_store::grant(a.tid, CapPerms::RW_DUP, 0xC0FFEE).unwrap();

        // A gives B read-only access to the same channel.
        let delegated =
            cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::READ).unwrap();

        // B can read through it and lands on the same resource.
        assert_eq!(
            cap_store::get(b.tid, Cap::<Channel>::from_raw(delegated), CapPerms::READ),
            Ok(0xC0FFEE)
        );
        // …and cannot write, because that bit was never delegated.
        assert_eq!(
            cap_store::get(b.tid, Cap::<Channel>::from_raw(delegated), CapPerms::WRITE),
            Err(CapError::MissingPerms)
        );
        // A keeps its own cap intact, at full strength.
        assert_eq!(cap_store::get(a.tid, src, CapPerms::WRITE), Ok(0xC0FFEE));
        // Exactly one slot was consumed in B's table.
        assert_eq!(cap_store::occupied(b.tid), 1);
    }

    #[test]
    fn self_delegation_attenuates_and_does_not_deadlock() {
        let _g = guard();
        // Grantor == target collapses to a single table. `SpinLock` is not
        // reentrant, so a naive two-lock implementation hangs here forever —
        // this test is the regression guard for that, and a hang is a failure
        // as surely as a wrong value.
        let a = fresh_task();
        let src: Cap<Gpio> = cap_store::grant(a.tid, CapPerms::RW_DUP, 17).unwrap();

        let weaker =
            cap_store::delegate(a.tid, a.tid, src.raw(), CapPerms::READ).unwrap();

        assert_ne!(weaker.slot(), src.raw().slot(), "must be a new slot");
        assert_eq!(
            cap_store::get(a.tid, Cap::<Gpio>::from_raw(weaker), CapPerms::READ),
            Ok(17)
        );
        assert_eq!(
            cap_store::get(a.tid, Cap::<Gpio>::from_raw(weaker), CapPerms::WRITE),
            Err(CapError::MissingPerms)
        );
    }

    #[test]
    fn dup_can_be_passed_on_explicitly() {
        let _g = guard();
        // Re-delegation is opt-in: A → B carrying DUP, so B → C works.
        let a = fresh_task();
        let b = fresh_task();
        let c = fresh_task();

        let src: Cap<Channel> =
            cap_store::grant(a.tid, CapPerms::RW_DUP, 5).unwrap();
        let to_b = cap_store::delegate(
            a.tid,
            b.tid,
            src.raw(),
            CapPerms::READ.union(CapPerms::DUP),
        )
        .unwrap();
        let to_c =
            cap_store::delegate(b.tid, c.tid, to_b, CapPerms::READ).unwrap();

        assert_eq!(
            cap_store::get(c.tid, Cap::<Channel>::from_raw(to_c), CapPerms::READ),
            Ok(5)
        );
    }

    #[test]
    fn kind_survives_delegation_unchanged() {
        let _g = guard();
        // The delegated copy must carry the source slot's kind, not anything
        // the caller encoded in the handle it passed in.
        let a = fresh_task();
        let b = fresh_task();
        let src: Cap<Motor> = cap_store::grant(a.tid, CapPerms::RW_DUP, 0).unwrap();
        let out = cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::WRITE).unwrap();
        assert_eq!(out.kind(), CapKind::Motor as u8);
        // And it is genuinely a Motor cap in B's table: asking for it as a
        // Channel is refused on kind.
        assert_eq!(
            cap_store::get(b.tid, Cap::<Channel>::from_raw(out), CapPerms::WRITE),
            Err(CapError::WrongKind)
        );
    }

    // ── The rejecting half ─────────────────────────────────────────────────

    #[test]
    fn delegation_cannot_amplify_permissions() {
        let _g = guard();
        let a = fresh_task();
        let b = fresh_task();

        // A holds READ (+DUP) only.
        let src: Cap<Channel> = cap_store::grant(
            a.tid,
            CapPerms::READ.union(CapPerms::DUP),
            99,
        )
        .unwrap();

        // Asking for WRITE is a refusal, not a silent narrowing: a caller
        // that got a quietly weaker cap would discover it much later, in a
        // driver, with none of this context.
        assert_eq!(
            cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::RW),
            Err(DelegateError::Amplify)
        );
        // EXEC it never held either.
        assert_eq!(
            cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::EXEC),
            Err(DelegateError::Amplify)
        );
        // Nothing landed in B's table.
        assert_eq!(cap_store::occupied(b.tid), 0);
    }

    #[test]
    fn delegation_requires_the_dup_bit() {
        let _g = guard();
        let a = fresh_task();
        let b = fresh_task();

        // Full read/write authority, but not the right to pass it on.
        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW, 7).unwrap();

        assert_eq!(
            cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::READ),
            Err(DelegateError::NotDelegable)
        );
        assert_eq!(cap_store::occupied(b.tid), 0);
    }

    #[test]
    fn delegated_cap_is_a_leaf_by_default() {
        let _g = guard();
        // A → B without DUP, so B cannot start a chain. This is the whole
        // difference from `handle_dup`, whose verbatim copy makes every dup
        // of a duplicate-flagged handle infinitely re-dupable.
        let a = fresh_task();
        let b = fresh_task();
        let c = fresh_task();

        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 3).unwrap();
        let to_b = cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::RW).unwrap();

        assert_eq!(
            cap_store::delegate(b.tid, c.tid, to_b, CapPerms::READ),
            Err(DelegateError::NotDelegable)
        );
        assert_eq!(cap_store::occupied(c.tid), 0);
    }

    #[test]
    fn a_non_owner_cannot_delegate_someone_elses_cap() {
        let _g = guard();
        let a = fresh_task();
        let b = fresh_task();
        let c = fresh_task();

        let src: Cap<Channel> =
            cap_store::grant(a.tid, CapPerms::RW_DUP, 0xAAAA).unwrap();

        // B has an empty table, so A's handle bits name nothing in it.
        assert_eq!(
            cap_store::delegate(b.tid, c.tid, src.raw(), CapPerms::READ),
            Err(DelegateError::Source(CapError::Stale))
        );

        // The sharper case. B mints its own first cap, which lands in the
        // same slot with the same generation — so the two handles are
        // **bit-identical**. This is where a globally-indexed table
        // (`crates/ipc/src/handle.rs`) would hand B a copy of A's authority.
        // Per-task tables make the index meaningless across tasks: B's
        // "replay" of A's handle delegates B's OWN resource.
        let b_own: Cap<Channel> =
            cap_store::grant(b.tid, CapPerms::RW_DUP, 0xBBBB).unwrap();
        assert_eq!(
            b_own.raw().as_raw(),
            src.raw().as_raw(),
            "test premise: the two handles must be bit-identical"
        );
        let out = cap_store::delegate(b.tid, c.tid, src.raw(), CapPerms::READ).unwrap();
        assert_eq!(
            cap_store::get(c.tid, Cap::<Channel>::from_raw(out), CapPerms::READ),
            Ok(0xBBBB),
            "replaying another task's handle bits must resolve in the \
             caller's own table, never the victim's"
        );
    }

    #[test]
    fn delegation_to_a_tid_that_never_existed_is_refused() {
        let _g = guard();
        let a = fresh_task();
        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();

        // `handle_dup`'s documented failure: planting a capability onto a TID
        // that does not exist. Nothing was ever bound for this one.
        let ghost = 0xDEAD_0000u32;
        assert_eq!(
            cap_store::delegate(a.tid, ghost, src.raw(), CapPerms::READ),
            Err(DelegateError::NoTarget)
        );
        // TID 0 is the "no current task" sentinel and must never resolve.
        assert_eq!(
            cap_store::delegate(a.tid, 0, src.raw(), CapPerms::READ),
            Err(DelegateError::NoTarget)
        );
        assert_eq!(
            cap_store::delegate(0, a.tid, src.raw(), CapPerms::READ),
            Err(DelegateError::NoGrantor)
        );
    }

    #[test]
    fn delegation_to_a_dead_tid_is_refused() {
        let _g = guard();
        let a = fresh_task();
        let b = fresh_task();
        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();

        // B existed a moment ago and its slot is now free. The recycled-slot
        // plant is the one `handle_dup` calls out by name.
        shim_kill(b.tid);
        assert_eq!(
            cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::READ),
            Err(DelegateError::NoTarget)
        );
    }

    #[test]
    fn two_tids_on_one_slot_are_refused() {
        let _g = guard();
        // The wrong-slot outcome of the unsynchronised `idx_for_tid` scan,
        // staged directly: two live TIDs resolving to one task-pool slot.
        // Whatever produced it, writing the target's cap into the grantor's
        // own table is not what the caller asked for.
        let a = fresh_task();
        let victim_slot = a.slot;
        let b_tid = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        shim_bind(b_tid, victim_slot);

        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();
        assert_eq!(
            cap_store::delegate(a.tid, b_tid, src.raw(), CapPerms::READ),
            Err(DelegateError::SlotAlias)
        );

        // And the refusal must be free of damage. Resolving a TID registers
        // it as the slot's owner and wipes the table if the previous owner
        // differed, so an implementation that claimed both slots before
        // noticing the alias would answer `SlotAlias` *and* destroy the
        // grantor's caps on the way out.
        assert_eq!(
            cap_store::get(a.tid, src, CapPerms::WRITE),
            Ok(1),
            "a refused delegation must not have wiped the grantor's table"
        );
    }

    #[test]
    fn a_stale_scan_is_caught_by_the_confirmation_pass() {
        let _g = guard();
        // Reproduces the real race: `alloc_slot` publishes `TASK_VALID[i]`
        // before the TID is written, so one scan can match a slot that the
        // next scan will not. `slot_for_untrusted` runs the scan twice and
        // refuses when the two disagree.
        let a = fresh_task();
        let b = fresh_task();
        let ghost = 0xBEEF_0000u32; // dead TID, resolves to nothing normally

        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();
        // First lookup of `ghost` lies and points at B's slot; the second
        // tells the truth (nothing).
        shim_stale_once(ghost, b.slot);
        assert_eq!(
            cap_store::delegate(a.tid, ghost, src.raw(), CapPerms::READ),
            Err(DelegateError::NoTarget)
        );
        // And B's table was not touched on the way through.
        assert_eq!(cap_store::occupied(b.tid), 0);
    }

    #[test]
    fn empty_permissions_are_refused() {
        let _g = guard();
        // An all-zero mask makes an inert cap that still burns one of the
        // target's 256 slots — a quiet way to fill someone else's table.
        let a = fresh_task();
        let b = fresh_task();
        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();
        assert_eq!(
            cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::NONE),
            Err(DelegateError::EmptyPerms)
        );
        assert_eq!(cap_store::occupied(b.tid), 0);
    }

    #[test]
    fn garbage_handles_are_refused_without_panicking() {
        let _g = guard();
        // Everything here comes straight off a syscall register. With
        // `panic = "abort"` and `overflow-checks = true`, one unchecked index
        // is a board reset, so the bounds cases matter as much as the
        // authorization ones.
        let a = fresh_task();
        let b = fresh_task();
        let _seed: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();

        let cases = [
            CapHandle::from_raw(0),           // CAP_NULL
            CapHandle::from_raw(u32::MAX),    // all bits set
            CapHandle::from_raw(0xFFFF),      // slot 65535, kind Null
            CapHandle::pack(CapKind::Channel, CapPerms::ALL, 255, 65_535), // slot OOB
            CapHandle::pack(CapKind::Channel, CapPerms::ALL, 255, 256), // one past the end
            CapHandle::pack(CapKind::Channel, CapPerms::ALL, 0, 0), // generation 0
            CapHandle::pack(CapKind::Channel, CapPerms::ALL, 200, 0), // wrong generation
            CapHandle::pack(CapKind::Gpio, CapPerms::ALL, 1, 0), // right slot, wrong kind
        ];
        for (i, raw) in cases.into_iter().enumerate() {
            let got = cap_store::delegate(a.tid, b.tid, raw, CapPerms::READ);
            assert!(
                matches!(got, Err(DelegateError::Source(_))),
                "case {i} ({raw:?}) should have been refused as a bad source, got {got:?}"
            );
        }
        assert_eq!(cap_store::occupied(b.tid), 0);
    }

    #[test]
    fn a_full_target_table_is_refused_without_panicking() {
        let _g = guard();
        let a = fresh_task();
        let b = fresh_task();
        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();

        // Fill B's table to the brim.
        for i in 0..crate::cap::MAX_CAPS_PER_TASK {
            let c: Option<Cap<Gpio>> = cap_store::grant(b.tid, CapPerms::READ, i as u32);
            assert!(c.is_some(), "grant {i} should have fit");
        }
        assert_eq!(cap_store::occupied(b.tid), crate::cap::MAX_CAPS_PER_TASK);

        assert_eq!(
            cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::READ),
            Err(DelegateError::TargetFull)
        );
        // The grantor's cap is untouched by the failed attempt.
        assert_eq!(cap_store::get(a.tid, src, CapPerms::WRITE), Ok(1));
    }

    // ── Semantics this design deliberately chose ───────────────────────────

    #[test]
    fn revoking_the_grantors_cap_leaves_the_delegate_alive() {
        let _g = guard();
        // Delegation is a transfer of authority, not a lease. Pinned as a
        // test because it is the surprising half of the design: if this ever
        // has to change, this is the test that says so out loud.
        let a = fresh_task();
        let b = fresh_task();

        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 42).unwrap();
        let to_b = cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::READ).unwrap();

        cap_store::revoke(a.tid, src);
        assert_eq!(cap_store::get(a.tid, src, CapPerms::READ), Err(CapError::Stale));
        assert_eq!(
            cap_store::get(b.tid, Cap::<Channel>::from_raw(to_b), CapPerms::READ),
            Ok(42),
            "the delegate outlives the grantor's revoke — by design; \
             see cap_store::delegate rule 3"
        );

        // What *does* reclaim it: the holder exiting.
        cap_store::reset(b.tid);
        assert_eq!(
            cap_store::get(b.tid, Cap::<Channel>::from_raw(to_b), CapPerms::READ),
            Err(CapError::Stale)
        );
    }

    #[test]
    fn a_reused_slot_wipes_the_previous_owners_table() {
        let _g = guard();
        // The `OWNER` backstop, stated as behaviour rather than as a comment.
        // It converts "task B inherits task A's caps" into "task A's caps are
        // destroyed" — fail-closed, and the reason a wrong-slot resolution in
        // this module is a denial-of-service and not capability theft.
        let a = fresh_task();
        let slot = a.slot;
        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();
        assert_eq!(cap_store::occupied(a.tid), 1);

        // A dies; a new task draws the same pool slot.
        shim_kill(a.tid);
        let heir_tid = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        shim_bind(heir_tid, slot);

        // The heir sees an empty table, not A's cap.
        assert_eq!(cap_store::occupied(heir_tid), 0);
        assert_eq!(
            cap_store::get(heir_tid, src, CapPerms::READ),
            Err(CapError::Stale)
        );

        // And the flip side, which is the finding worth naming: an operation
        // attributed to the DEAD TID on that slot wipes the live heir's
        // table. Nothing is stolen; something is destroyed.
        let heir_cap: Cap<Channel> =
            cap_store::grant(heir_tid, CapPerms::RW, 2).unwrap();
        assert_eq!(cap_store::get(heir_tid, heir_cap, CapPerms::READ), Ok(2));
        shim_bind(a.tid, slot); // the stale scan matching A again
        assert_eq!(cap_store::occupied(a.tid), 0);
        assert_eq!(
            cap_store::get(heir_tid, heir_cap, CapPerms::READ),
            Err(CapError::Stale),
            "OWNER's wipe-on-mismatch is itself the damage: a live task's \
             capability table is emptied by an operation naming a dead TID"
        );
        shim_kill(a.tid);
    }

    #[test]
    fn delegation_does_not_disturb_unrelated_tasks() {
        let _g = guard();
        // Cheap guard against the "wrong table" class: C is a bystander with
        // caps of its own, and an A → B delegation must not touch it.
        let a = fresh_task();
        let b = fresh_task();
        let c = fresh_task();

        let c_cap: Cap<Gpio> = cap_store::grant(c.tid, CapPerms::RW, 11).unwrap();
        let src: Cap<Channel> = cap_store::grant(a.tid, CapPerms::RW_DUP, 1).unwrap();
        let _ = cap_store::delegate(a.tid, b.tid, src.raw(), CapPerms::READ).unwrap();

        assert_eq!(cap_store::get(c.tid, c_cap, CapPerms::WRITE), Ok(11));
        assert_eq!(cap_store::occupied(c.tid), 1);
    }

    // ── Inbound-delegation quota (the fill-attack bound) ──────────────────

    #[test]
    fn the_inbound_quota_stops_the_fill_attack_at_the_bound() {
        let _g = guard();
        let attacker = fresh_task();
        let victim = fresh_task();

        let src: Cap<Channel> =
            cap_store::grant(attacker.tid, CapPerms::RW_DUP, 0xF111).unwrap();

        // The attack: one delegable cap, delegated to the victim in a loop.
        // Exactly MAX_INBOUND_DELEGATIONS land...
        for i in 0..cap_store::MAX_INBOUND_DELEGATIONS {
            cap_store::delegate(attacker.tid, victim.tid, src.raw(), CapPerms::READ)
                .unwrap_or_else(|e| panic!("delegation {i} refused early: {e:?}"));
        }
        // ...and the next one is refused, with most of the victim's table
        // still free for its own grants.
        assert_eq!(
            cap_store::delegate(attacker.tid, victim.tid, src.raw(), CapPerms::READ),
            Err(DelegateError::QuotaExhausted)
        );
        assert_eq!(
            cap_store::occupied(victim.tid) as u16,
            cap_store::MAX_INBOUND_DELEGATIONS
        );
        let own: Cap<Gpio> = cap_store::grant(victim.tid, CapPerms::RW, 7)
            .expect("the victim must still be able to mint its own caps");
        assert_eq!(cap_store::get(victim.tid, own, CapPerms::READ), Ok(7));
    }

    #[test]
    fn self_delegation_never_charges_the_quota() {
        let _g = guard();
        let a = fresh_task();
        let src: Cap<Channel> =
            cap_store::grant(a.tid, CapPerms::RW_DUP, 0x5E1F).unwrap();

        // Attenuating your own cap spends your own slots, not the inbound
        // budget — well past the quota, every one must succeed.
        for i in 0..(cap_store::MAX_INBOUND_DELEGATIONS + 8) {
            cap_store::delegate(a.tid, a.tid, src.raw(), CapPerms::READ)
                .unwrap_or_else(|e| panic!("self-delegation {i} refused: {e:?}"));
        }
    }

    #[test]
    fn the_quota_is_a_table_lifetime_not_a_permanent_mark() {
        let _g = guard();
        let grantor = fresh_task();
        let target = fresh_task();
        let src: Cap<Channel> =
            cap_store::grant(grantor.tid, CapPerms::RW_DUP, 0xBEEF).unwrap();

        for _ in 0..cap_store::MAX_INBOUND_DELEGATIONS {
            cap_store::delegate(grantor.tid, target.tid, src.raw(), CapPerms::READ)
                .unwrap();
        }
        assert_eq!(
            cap_store::delegate(grantor.tid, target.tid, src.raw(), CapPerms::READ),
            Err(DelegateError::QuotaExhausted)
        );

        // Task exit empties the table — the quota dies with it, so the
        // slot's NEXT occupant (or the same TID after a clean reset, as
        // here) starts with a full budget instead of inheriting a dead
        // task's exhaustion.
        cap_store::reset(target.tid);
        cap_store::delegate(grantor.tid, target.tid, src.raw(), CapPerms::READ)
            .expect("a fresh table lifetime must start with a fresh quota");
    }

    #[test]
    fn refused_delegations_do_not_charge_the_quota() {
        let _g = guard();
        let grantor = fresh_task();
        let target = fresh_task();
        // No DUP: every attempt is refused as NotDelegable...
        let src: Cap<Channel> =
            cap_store::grant(grantor.tid, CapPerms::RW, 0xA11).unwrap();
        for _ in 0..cap_store::MAX_INBOUND_DELEGATIONS {
            assert_eq!(
                cap_store::delegate(grantor.tid, target.tid, src.raw(), CapPerms::READ),
                Err(DelegateError::NotDelegable)
            );
        }
        // ...and none of them burned the target's budget.
        let dup: Cap<Channel> =
            cap_store::grant(grantor.tid, CapPerms::RW_DUP, 0xA12).unwrap();
        cap_store::delegate(grantor.tid, target.tid, dup.raw(), CapPerms::READ)
            .expect("a rejected delegation must not charge the target");
    }
}
