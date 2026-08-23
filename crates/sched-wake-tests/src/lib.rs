//! Host-side runner for the K-C10 wake logic in `crates/sched/src/wait.rs`.
//!
//! # What is real here and what is not — read this before trusting a green run
//!
//! `crates/sched/src/scheduler.rs` cannot be compiled for the host: static
//! `TASKS`/`PER_CPU` arrays, RISC-V CSR reads (`read_sstatus`), an assembly
//! context switch, and `current_cpu_id()` reading `tp`. Refactoring it to be
//! host-buildable would mean rewriting the core of a production scheduler,
//! which is far more dangerous than the race K-C10 closes. So it is **not**
//! compiled here.
//!
//! What *is* compiled from the kernel tree, unmodified, via `#[path]`:
//!
//!   * `crates/sched/src/task.rs` — the real `WaitReason`, `TaskState`,
//!     `Task`, the real `MAX_TASKS`, and since K-C19 the real `sched_word`
//!     transition protocol (state + wake stamp in one atomic word). It has
//!     no dependency beyond `robot_os_limits`.
//!   * `crates/sched/src/wait.rs` — the real `wake_action()` truth table and
//!     the real wake entry points with their real `WaitReason` predicates.
//!
//! What is stubbed (`smp`, `scheduler` below): only the *mechanism* —
//! scanning the task pool, flipping `state`, and `cpu_enqueue_locked`. The
//! stub records every call so the tests can assert which primitive each wake
//! routed to and exactly which `WaitReason` values its predicate accepts.
//!
//! So these tests prove: (1) the K-C10 decision table is what the kernel
//! executes, because `wake_task_by_tid` matches on `wait::wake_action()`;
//! (2) each targeted wake addresses the right TID with the right predicate;
//! (3) the broadcast wakes still use the sweep and were not swept into the
//! TID path. They do **not** prove the SMP interleaving itself — that needs
//! the ring-3 scenario described at the bottom of this file.

// The real task definitions — no stubs, no copies.
#[path = "../../sched/src/task.rs"]
pub mod task;

/// Stub for `crate::smp`. `wait::task_block()` only needs a CPU id; the real
/// one reads the RISC-V `tp` register.
pub mod smp {
    pub fn current_cpu_id() -> usize { 0 }
}

/// Stub for `crate::scheduler`, recording instead of scheduling.
///
/// Every call is logged together with the result of applying the caller's
/// predicate to a fixed probe set (`probes()`), which is how the tests get at
/// the closures `wait.rs` builds internally.
pub mod scheduler {
    use crate::task::WaitReason;
    use std::sync::Mutex;

    /// Fixed probe set. Index positions are stable and referenced by the
    /// tests via `probe_index()`.
    pub fn probes() -> Vec<WaitReason> {
        vec![
            WaitReason::None,
            WaitReason::WaitQueue,
            WaitReason::Timer(1234),
            WaitReason::Irq(5),
            WaitReason::Channel(5),
            WaitReason::Ring(5),
            WaitReason::Port(5),
            WaitReason::Rpc(7),
            WaitReason::Rpc(8),
            WaitReason::FastIpcServer(7),
            WaitReason::FastIpcServer(8),
            WaitReason::FastIpcClient(3),
            WaitReason::FastIpcClient(4),
        ]
    }

    pub fn probe_index(r: WaitReason) -> usize {
        probes().iter().position(|p| *p == r).expect("probe not in set")
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Call {
        /// `try_wake_task(idx, pred)` — the sweep path.
        Sweep { idx: usize, accepts: Vec<WaitReason> },
        /// `wake_task_by_tid(tid, pred)` — the K-C10 targeted path.
        ByTid { tid: u32, accepts: Vec<WaitReason> },
        /// `block_current(cpu, reason)`.
        Block { cpu: usize, reason: WaitReason },
    }

    pub static LOG: Mutex<Vec<Call>> = Mutex::new(Vec::new());

    pub fn reset() { LOG.lock().unwrap().clear(); }
    pub fn log() -> Vec<Call> { LOG.lock().unwrap().clone() }

    fn accepted(pred: &dyn Fn(&WaitReason) -> bool) -> Vec<WaitReason> {
        probes().into_iter().filter(|r| pred(r)).collect()
    }

    pub fn block_current(cpu: usize, reason: WaitReason) {
        LOG.lock().unwrap().push(Call::Block { cpu, reason });
    }

    pub fn try_wake_task(idx: usize, pred: &dyn Fn(&WaitReason) -> bool) {
        let accepts = accepted(pred);
        LOG.lock().unwrap().push(Call::Sweep { idx, accepts });
    }

    pub fn wake_task_by_tid(tid: u32, pred: &dyn Fn(&WaitReason) -> bool) -> bool {
        let accepts = accepted(pred);
        LOG.lock().unwrap().push(Call::ByTid { tid, accepts });
        false
    }
}

// The real wake logic, compiled against the real `task` and the stubs above.
#[path = "../../sched/src/wait.rs"]
pub mod wait;

#[cfg(test)]
mod tests {
    use super::scheduler::{self, Call};
    use super::task::{TaskState, WaitReason, MAX_TASKS};
    use super::wait::{self, WakeAction};
    use core::sync::atomic::Ordering;

    // The stub log is a process-wide singleton and `cargo test` runs tests in
    // threads, so every test that inspects it must hold this lock.
    static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_log<R>(f: impl FnOnce() -> R) -> R {
        let _g = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        scheduler::reset();
        f()
    }

    // ── 1. The K-C10 truth table ────────────────────────────────────────────

    #[test]
    fn wake_action_covers_the_full_truth_table() {
        // Not the addressee: nothing else is consulted.
        for &blocked in &[false, true] {
            for &matches in &[false, true] {
                assert_eq!(
                    wait::wake_action(false, blocked, matches),
                    WakeAction::Skip,
                    "non-addressee must always Skip (blocked={blocked}, matches={matches})"
                );
            }
        }
        // Addressee, not yet Blocked: this is the lost-wakeup window.
        assert_eq!(wait::wake_action(true, false, false), WakeAction::StampPending);
        assert_eq!(wait::wake_action(true, false, true), WakeAction::StampPending);
        // Addressee, Blocked on something else: genuine mismatch.
        assert_eq!(wait::wake_action(true, true, false), WakeAction::Mismatch);
        // Addressee, Blocked, reason matches: the ordinary wake.
        assert_eq!(wait::wake_action(true, true, true), WakeAction::Dispatch);
    }

    #[test]
    fn wake_action_is_a_total_function_of_three_bools() {
        // Guards against someone adding a fourth outcome without a rule:
        // all eight input combinations must land in the four known actions.
        let mut seen = [false; 4];
        for &a in &[false, true] {
            for &b in &[false, true] {
                for &c in &[false, true] {
                    match wait::wake_action(a, b, c) {
                        WakeAction::Skip => seen[0] = true,
                        WakeAction::StampPending => seen[1] = true,
                        WakeAction::Mismatch => seen[2] = true,
                        WakeAction::Dispatch => seen[3] = true,
                    }
                }
            }
        }
        assert_eq!(seen, [true; 4], "every action must be reachable");
    }

    #[test]
    fn only_stamp_pending_is_reachable_when_not_blocked() {
        // The property that makes K-C10 correct: a task that has not blocked
        // yet can never be Dispatched (its wait_reason is None, so no
        // targeted predicate accepts it) and must never be judged a Mismatch
        // (that would drop the wake — the original bug).
        for &matches in &[false, true] {
            let a = wait::wake_action(true, false, matches);
            assert_ne!(a, WakeAction::Dispatch);
            assert_ne!(a, WakeAction::Mismatch);
        }
    }

    // ── 2. The K-C9/K-C19 handshake — the REAL protocol, not a model ────────
    //
    // Until K-C19 this section *modelled* block/wake over the removed
    // `wake_pending: AtomicBool`, replicating scheduler.rs by hand — exactly
    // the "test that replicates the logic it guards" trap. The protocol now
    // lives in `task::sched_word` as free functions over an `AtomicU32`, so
    // these tests execute the kernel's own transition code.

    use super::task::sched_word::{
        self, commit_blocked_or_consume_wake, reap_orphaned_stamp,
        wake_transition, WakeTransition, STATE_MASK, WAKE_STAMP,
    };
    use core::sync::atomic::AtomicU32;

    fn word(s: TaskState) -> AtomicU32 { AtomicU32::new(sched_word::pack(s)) }
    fn state_now(w: &AtomicU32) -> TaskState {
        sched_word::state_of(w.load(Ordering::Acquire))
    }
    fn stamped(w: &AtomicU32) -> bool {
        w.load(Ordering::Acquire) & WAKE_STAMP != 0
    }
    /// A targeted wake with a matching reason, as `wake_task_by_tid` issues
    /// it against a PARKED target (context saved — the common case).
    fn wake(w: &AtomicU32, reason_matches: bool) -> WakeTransition {
        wake_transition(w, || reason_matches, true, true)
    }

    /// The same wake against an UNSAVED target (K-C24: Blocked but still
    /// executing past an unswitched block).
    fn wake_unsaved(w: &AtomicU32, reason_matches: bool) -> WakeTransition {
        wake_transition(w, || reason_matches, true, false)
    }

    #[test]
    fn wake_before_block_does_not_sleep() {
        // The exact SYS_IPC_FAST_CALL race: server replies between the
        // client's wake_fast_ipc_server() and its task_block().
        let w = word(TaskState::Running);

        assert_eq!(wake(&w, true), WakeTransition::Stamped,
                   "nothing to dispatch — client is still Running");
        assert!(stamped(&w), "the wake must have been stamped");

        assert!(!commit_blocked_or_consume_wake(&w),
                "K-C9: the client must skip blocking, not sleep forever");
        assert_eq!(state_now(&w), TaskState::Running);
        assert!(!stamped(&w), "the stamp must be consumed exactly once");
    }

    #[test]
    fn wake_after_block_dispatches_normally() {
        let w = word(TaskState::Running);

        assert!(commit_blocked_or_consume_wake(&w));
        assert_eq!(state_now(&w), TaskState::Blocked);

        assert_eq!(wake(&w, true), WakeTransition::Dispatched);
        assert_eq!(state_now(&w), TaskState::Ready);
        assert!(!stamped(&w), "an ordinary wake must not stamp");
    }

    #[test]
    fn stamp_is_consumed_once_not_latched() {
        // A latched stamp would make every subsequent block a no-op — the
        // task would spin instead of sleeping.
        let w = word(TaskState::Running);
        wake(&w, true);

        assert!(!commit_blocked_or_consume_wake(&w), "first block consumes the stamp");
        assert!(commit_blocked_or_consume_wake(&w), "second block must actually sleep");
        assert_eq!(state_now(&w), TaskState::Blocked);
    }

    #[test]
    fn two_wakes_before_block_still_consume_once() {
        // The stamp is an idempotent bit (see the truth table): two early
        // wakes skip one block, not two.
        let w = word(TaskState::Running);
        assert_eq!(wake(&w, true), WakeTransition::Stamped);
        assert_eq!(wake(&w, true), WakeTransition::Stamped);

        assert!(!commit_blocked_or_consume_wake(&w));
        assert!(commit_blocked_or_consume_wake(&w), "only one block is skipped");
    }

    #[test]
    fn mismatch_while_blocked_touches_nothing() {
        // The rule that stops K-C10 turning a hang into cross-task
        // corruption: a task blocked on an unrelated reason must not be
        // marked, or it would skip its own next, unrelated wait.
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w));

        assert_eq!(wake(&w, /* reason_matches */ false), WakeTransition::Mismatch);
        assert_eq!(state_now(&w), TaskState::Blocked, "must stay asleep");
        assert!(!stamped(&w), "mismatch must never stamp");
    }

    #[test]
    fn broadcast_wake_never_stamps() {
        // `try_wake_task` (the sweep path) passes `stamp_if_unblocked =
        // false`: a sweep cannot tell its addressee from any other task about
        // to sleep, so stamping there would hand random tasks a phantom wake.
        let w = word(TaskState::Running);
        assert_eq!(wake_transition(&w, || true, false, true), WakeTransition::NotBlocked);
        assert!(!stamped(&w));
        assert_eq!(state_now(&w), TaskState::Running);
    }

    // ── 2b. K-C19: the invariant is structural now ──────────────────────────

    #[test]
    fn a_stamped_word_cannot_commit_to_blocked() {
        // The blocker half of K-C19: committing past a pending wake is not a
        // race that discipline avoids — the CAS refuses it.
        let w = word(TaskState::Running);
        wake(&w, true);
        for _ in 0..3 {
            assert!(!commit_blocked_or_consume_wake(&w) || state_now(&w) == TaskState::Blocked);
            if state_now(&w) == TaskState::Blocked { break; }
        }
        // After the first (consuming) call the state must still be Running —
        // never Blocked-with-a-wake-outstanding.
        assert_ne!(w.load(Ordering::Acquire),
                   sched_word::pack(TaskState::Blocked) | WAKE_STAMP,
                   "Blocked with the stamp set must be unrepresentable");
    }

    #[test]
    fn a_committed_word_cannot_be_stamped() {
        // The waker half of K-C19: once the task is Blocked, a targeted wake
        // must dispatch (or mismatch) — it can never leave a stamp behind for
        // nobody to consume. This was the measured ~1-in-3 permanent sleep.
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w));

        assert_eq!(wake(&w, true), WakeTransition::Dispatched,
                   "a wake against a Blocked task must dispatch, never stamp");
        assert!(!stamped(&w));
    }

    #[test]
    fn blocked_with_stamp_is_unreachable_from_every_interleaving() {
        // Exhaustive over every serialized order of one blocker step and up
        // to two wakes: the two rows deliberately absent from the K-C10
        // truth table must be unrepresentable outcomes, whatever the order.
        type Step = u8; // 0 = commit, 1 = matching wake, 2 = mismatched wake
        fn run(steps: &[Step]) -> u32 {
            let w = word(TaskState::Running);
            for s in steps {
                match s {
                    0 => { let _ = commit_blocked_or_consume_wake(&w); }
                    1 => { let _ = wake(&w, true); }
                    _ => { let _ = wake(&w, false); }
                }
            }
            w.load(Ordering::Acquire)
        }
        for a in 0..3u8 {
            for b in 0..3u8 {
                for c in 0..3u8 {
                    let end = run(&[a, b, c]);
                    let is_blocked =
                        end & STATE_MASK == sched_word::pack(TaskState::Blocked);
                    let has_stamp = end & WAKE_STAMP != 0;
                    assert!(!(is_blocked && has_stamp),
                            "steps {a},{b},{c} produced Blocked+stamp");
                }
            }
        }
    }

    // ── 2c. K-C24: Blocked does not mean parked ─────────────────────────────
    //
    // `block_current`'s do_schedule can find nothing to run and RETURN: the
    // task keeps executing with `state == Blocked` and its context unsaved.
    // Dispatching it then enqueues a RUNNING task — measured as the phase-A
    // server sitting `Ready` in no queue forever. The `saved` gate turns
    // those wakes into stamps.

    #[test]
    fn an_unsaved_blocked_target_is_stamped_never_dispatched() {
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w)); // Blocked, still running
        assert_eq!(wake_unsaved(&w, true), WakeTransition::Stamped,
                   "dispatching an unswitched block enqueues a running task");
        assert_eq!(state_now(&w), TaskState::Blocked, "state must not move");
        assert!(stamped(&w));
        // The task loops back into its next block attempt: the commit
        // consumes the stamp AND normalizes to Running in one CAS — leaving
        // Blocked behind on the skip path would re-expose the bug.
        assert!(!commit_blocked_or_consume_wake(&w), "stamp must be consumed");
        assert_eq!(state_now(&w), TaskState::Running,
                   "skip path must normalize the word to the truth");
        assert!(!stamped(&w));
    }

    #[test]
    fn a_mismatched_unsaved_target_is_left_alone() {
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w));
        assert_eq!(wake_unsaved(&w, false), WakeTransition::Mismatch);
        assert_eq!(state_now(&w), TaskState::Blocked);
        assert!(!stamped(&w), "a mismatch must not stamp, saved or not");
    }

    #[test]
    fn a_parked_stamp_is_swept_by_the_dispatch_cas() {
        // If the task DOES get parked with a K-C24 stamp set (do_schedule's
        // Blocked-arm conversion races are re-covered kernel-side), a later
        // targeted wake against the now-saved task must deliver: the
        // dispatch CAS replaces the whole word, stamp included.
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w));
        assert_eq!(wake_unsaved(&w, true), WakeTransition::Stamped);
        // ...task gets switched out; context saved; a fresh wake arrives:
        assert_eq!(wake(&w, true), WakeTransition::Dispatched);
        assert_eq!(state_now(&w), TaskState::Ready);
        assert!(!stamped(&w), "dispatch must sweep the stamp with the state");
    }

    // ── 2d. K-C25: an orphaned parked stamp needs a reaper ──────────────────
    //
    // The leg 2c's sweep test cannot cover: the stamp lands AFTER
    // do_schedule's switch-away sweep checked and the task parks with it set.
    // A later wake WOULD sweep it (test above) — but the one-shot wakes
    // (fast-IPC reply, RPC) fire exactly once, and at the measured wedge
    // every possible waker was already asleep. `reap_orphaned_stamp`, driven
    // by the timer tick, is that state's designated consumer.

    #[test]
    fn an_orphaned_parked_stamp_is_reaped() {
        // The measured 2026-08-24 wedge, step by step: client committed to
        // Blocked; the reply's one-shot wake stamped it (unsaved — mid
        // switch-out, past the sweep's check); the context save completed;
        // no further wake will ever fire.
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w));
        assert_eq!(wake_unsaved(&w, true), WakeTransition::Stamped);
        // ...context_switch.S finishes the save; the task is parked
        // Blocked+stamp+saved. The tick's reaper must deliver the wake:
        assert!(reap_orphaned_stamp(&w), "the reaper must recover the wake");
        assert_eq!(state_now(&w), TaskState::Ready);
        assert!(!stamped(&w), "recovery must consume the stamp");
        // Idempotence: a second reap finds nothing to do.
        assert!(!reap_orphaned_stamp(&w));
    }

    #[test]
    fn reap_refuses_a_stampless_sleeper() {
        // An ordinary parked task is not the reaper's business — waking it
        // with no delivered wake would be a phantom wake.
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w));
        assert!(!reap_orphaned_stamp(&w));
        assert_eq!(state_now(&w), TaskState::Blocked, "must stay asleep");
    }

    #[test]
    fn reap_refuses_a_running_stamp() {
        // A stamp on a not-yet-blocked task belongs to block_current's
        // consume path (K-C9); the reaper stealing it would turn the
        // imminent block into a permanent sleep.
        let w = word(TaskState::Running);
        assert_eq!(wake(&w, true), WakeTransition::Stamped);
        assert!(!reap_orphaned_stamp(&w), "Running+stamp is not orphaned");
        assert!(stamped(&w), "the stamp must survive for the commit");
        assert!(!commit_blocked_or_consume_wake(&w), "commit consumes it");
    }

    #[test]
    fn reap_and_a_late_wake_deliver_exactly_once() {
        // If a second wake DOES exist, it races the reaper over the same
        // Blocked+stamp word. Exactly one may win — a double delivery would
        // enqueue the task twice.
        use std::sync::{Arc, Barrier};
        const ROUNDS: usize = 20_000;
        let w = Arc::new(word(TaskState::Running));
        let start = Arc::new(Barrier::new(2));
        let (w2, start2) = (w.clone(), start.clone());

        let reaper = std::thread::spawn(move || {
            let mut wins = 0usize;
            for _ in 0..ROUNDS {
                start2.wait();
                if reap_orphaned_stamp(&w2) { wins += 1; }
                start2.wait();
            }
            wins
        });
        let mut wake_wins = 0usize;
        for _ in 0..ROUNDS {
            assert!(commit_blocked_or_consume_wake(&w));
            assert_eq!(wake_unsaved(&w, true), WakeTransition::Stamped);
            start.wait();
            if wake(&w, true) == WakeTransition::Dispatched { wake_wins += 1; }
            start.wait();
            assert_eq!(state_now(&w), TaskState::Ready,
                       "someone must have delivered the wake");
            // NOTE: the word MAY carry a fresh stamp here — when the reaper
            // wins, the late wake finds Ready and stamps (StampPending),
            // which the task's next block would consume. That is a delivered
            // second wake, not a double delivery of the first.
            // Reset the seat for the next round.
            w.store(sched_word::pack(TaskState::Running), Ordering::Release);
        }
        let reap_wins = reaper.join().unwrap();
        assert_eq!(reap_wins + wake_wins, ROUNDS,
                   "every round must deliver exactly once");
    }

    #[test]
    fn reentry_commit_with_a_standing_blocked_word_proceeds() {
        // Unswitched block loops back in with the word still Blocked and no
        // stamp: the commit must stand (return true) so the task keeps
        // trying to yield the hart — not spin forever failing to re-commit.
        let w = word(TaskState::Running);
        assert!(commit_blocked_or_consume_wake(&w));
        assert!(commit_blocked_or_consume_wake(&w),
                "a standing commit is still a commit");
        assert_eq!(state_now(&w), TaskState::Blocked);
    }

    #[test]
    fn kc19_stress_no_wake_is_ever_lost() {
        // The race itself, run on real threads against the real protocol: a
        // blocker committing while a waker delivers exactly one wake. In
        // every round, either the blocker consumed the stamp (skip) or the
        // waker dispatched (Blocked → Ready). A round ending Blocked is a
        // lost wake — the K-C19 hang. The reverted double-check handshake
        // failed the mirror property (dispatching a task that never
        // committed); both are asserted.
        use std::sync::{Arc, Barrier};
        const ROUNDS: usize = 20_000;

        let w = Arc::new(word(TaskState::Running));
        let start = Arc::new(Barrier::new(2));
        let end = Arc::new(Barrier::new(2));

        let waker = {
            let w = Arc::clone(&w);
            let (start, end) = (Arc::clone(&start), Arc::clone(&end));
            std::thread::spawn(move || {
                for _ in 0..ROUNDS {
                    start.wait();
                    let wt = wake_transition(&w, || true, true, true);
                    assert_ne!(wt, WakeTransition::Mismatch);
                    assert_ne!(wt, WakeTransition::NotBlocked);
                    end.wait();
                }
            })
        };

        for i in 0..ROUNDS {
            start.wait();
            let committed = commit_blocked_or_consume_wake(&w);
            end.wait(); // both sides done; the word is now quiescent
            let now = w.load(Ordering::Acquire);
            if committed {
                // We really blocked, so the waker must have dispatched us.
                assert_eq!(now, sched_word::pack(TaskState::Ready),
                           "round {i}: committed but never dispatched — lost wake (K-C19)");
            } else {
                // We consumed the stamp and skipped the block.
                assert_eq!(now, sched_word::pack(TaskState::Running),
                           "round {i}: skipped the block but the word moved");
            }
            // Reset for the next round, as the scheduler would (dispatch
            // path: dequeued and set Running; skip path: already Running).
            w.store(sched_word::pack(TaskState::Running), Ordering::Release);
        }
        waker.join().unwrap();
    }

    // ── 3. Routing: which wake uses which primitive ─────────────────────────

    fn only_by_tid(log: &[Call]) -> (u32, Vec<WaitReason>) {
        assert_eq!(log.len(), 1, "a targeted wake must make exactly one call: {log:?}");
        match &log[0] {
            Call::ByTid { tid, accepts } => (*tid, accepts.clone()),
            other => panic!("expected the TID-directed path, got {other:?}"),
        }
    }

    #[test]
    fn wake_by_rpc_is_tid_directed() {
        let log = with_log(|| { wait::wake_by_rpc(7); scheduler::log() });
        let (tid, accepts) = only_by_tid(&log);
        assert_eq!(tid, 7);
        assert_eq!(accepts, vec![WaitReason::Rpc(7)],
                   "must accept Rpc(7) and nothing else");
    }

    #[test]
    fn wake_fast_ipc_server_is_tid_directed() {
        let log = with_log(|| { wait::wake_fast_ipc_server(7); scheduler::log() });
        let (tid, accepts) = only_by_tid(&log);
        assert_eq!(tid, 7);
        assert_eq!(accepts, vec![WaitReason::FastIpcServer(7)]);
    }

    #[test]
    fn wake_fast_ipc_client_tid_addresses_the_client_and_matches_its_slot() {
        // The point of the new variant: the addressee is the *caller* TID,
        // while the predicate still keys on the slot.
        let log = with_log(|| { wait::wake_fast_ipc_client_tid(9, 3); scheduler::log() });
        let (tid, accepts) = only_by_tid(&log);
        assert_eq!(tid, 9, "must address the client TID, not the slot");
        assert_eq!(accepts, vec![WaitReason::FastIpcClient(3)],
                   "must not accept FastIpcClient(4) — wrong slot is a mismatch");
    }

    #[test]
    fn targeted_predicates_reject_every_unrelated_reason() {
        // A targeted predicate that accepted WaitReason::None would make the
        // Mismatch arm unreachable and re-open the corruption hazard.
        for (label, log) in [
            ("rpc", with_log(|| { wait::wake_by_rpc(7); scheduler::log() })),
            ("server", with_log(|| { wait::wake_fast_ipc_server(7); scheduler::log() })),
            ("client", with_log(|| { wait::wake_fast_ipc_client_tid(9, 3); scheduler::log() })),
        ] {
            let (_, accepts) = only_by_tid(&log);
            assert_eq!(accepts.len(), 1, "{label}: predicate must be exact");
            assert!(!accepts.contains(&WaitReason::None),
                    "{label}: must never accept WaitReason::None");
            assert!(!accepts.contains(&WaitReason::WaitQueue),
                    "{label}: must never accept WaitQueue");
        }
    }

    #[test]
    fn broadcast_wakes_still_use_the_sweep() {
        // These have no addressee TID. Routing them through the TID path
        // would be the trap K-C10 exists to avoid, so pin them here.
        for (label, log) in [
            ("irq", with_log(|| { wait::wake_by_irq(5); scheduler::log() })),
            ("channel", with_log(|| { wait::wake_by_channel(5); scheduler::log() })),
            ("ring", with_log(|| { wait::wake_by_ring(5); scheduler::log() })),
            ("port", with_log(|| { wait::wake_by_port(5); scheduler::log() })),
            ("timers", with_log(|| { wait::wake_expired_timers(9999); scheduler::log() })),
            // Legacy slot-keyed client wake — still a sweep, still lossy.
            ("client_by_slot", with_log(|| { wait::wake_fast_ipc_client(3); scheduler::log() })),
        ] {
            assert_eq!(log.len(), MAX_TASKS, "{label}: must sweep the whole pool");
            for c in &log {
                assert!(matches!(c, Call::Sweep { .. }),
                        "{label}: must not use the TID-directed path: {c:?}");
            }
        }
    }

    #[test]
    fn legacy_slot_keyed_client_wake_still_matches_the_slot() {
        // Kept only for the un-migrated SYS_IPC_FAST_REPLY call site; its
        // predicate must stay identical to the TID variant's.
        let log = with_log(|| { wait::wake_fast_ipc_client(3); scheduler::log() });
        match &log[0] {
            Call::Sweep { accepts, .. } => {
                assert_eq!(*accepts, vec![WaitReason::FastIpcClient(3)]);
            }
            other => panic!("expected a sweep, got {other:?}"),
        }
    }

    #[test]
    fn task_block_forwards_the_reason_unchanged() {
        let log = with_log(|| {
            wait::task_block(WaitReason::FastIpcClient(3));
            scheduler::log()
        });
        assert_eq!(log, vec![Call::Block { cpu: 0, reason: WaitReason::FastIpcClient(3) }]);
    }

    // ── K-C12: CPU placement policy ─────────────────────────────────────
    //
    // These exercise `task::pick_cpu_by_load`, the real decision the kernel
    // makes in `scheduler::find_best_cpu`. What the kernel does around it —
    // walking `TASKS[]` to build the per-CPU `CpuLoad` — cannot be compiled
    // for the host (static `TASKS`, `context.tp`, CSRs), so the *sampling* is
    // not covered here. That half is covered from ring 3 by
    // `userspace/ipctest`, which is where the defect was found in the first
    // place. Stated plainly so a green run here is not mistaken for proof
    // that placement as a whole is correct.

    use crate::task::{CpuLoad, EnqueueOutcome, enqueue_decision, pick_cpu_by_load};

    fn load(rt_blocking: u32, blocking: u32, total: u32) -> CpuLoad {
        CpuLoad { rt_blocking, blocking, total }
    }

    #[test]
    fn placement_avoids_a_hart_with_higher_priority_residents() {
        // The K-C12 shape, taken from the measured boot layout: hart 0 hosts
        // rt-motor + flight-ctrl (priority 8) and looks *emptiest* by queued
        // count; hart 2 hosts only same-or-lower priority work. A newcomer at
        // DEFAULT_PRIORITY must go to hart 2 even though hart 0 carries fewer
        // tasks in total, because on hart 0 it would never be dispatched.
        let loads = [
            load(2, 2, 2), // hart 0 — two RT residents, fewest tasks
            load(1, 1, 5), // hart 1
            load(0, 0, 4), // hart 2 — no one outranks the newcomer
            load(1, 1, 5), // hart 3
        ];
        assert_eq!(pick_cpu_by_load(&loads), 2);
    }

    #[test]
    fn placement_ranks_real_time_blockers_ahead_of_everything() {
        // The regression that made the first version of this fix incomplete,
        // taken from the probe: every hart ends up with two outranking
        // residents, so `blocking` ties — and hart 0 has the *fewest* tasks
        // overall because the earlier children all went to hart 2. Ranking on
        // (blocking, total) alone sends the newcomer to hart 0, whose two
        // blockers are `rt-motor` and `flight-ctrl` at priority 8 and which
        // therefore never dispatches it. The real-time count has to win.
        let loads = [
            load(2, 2, 6),  // hart 0 — two RT blockers, lightest overall
            load(1, 2, 9),
            load(0, 2, 20), // hart 2 — no RT blockers, heaviest by far
            load(1, 2, 9),
        ];
        assert_eq!(pick_cpu_by_load(&loads), 2);
    }

    #[test]
    fn placement_still_uses_the_plain_blocker_count_below_the_rt_key() {
        // Non-RT blockers are not harmless — dispatch is strict priority, so
        // a p13 task that never sleeps starves p16 just as thoroughly. With
        // the RT key tied, fewer blockers must still win over fewer tasks.
        let loads = [load(1, 4, 1), load(1, 2, 30)];
        assert_eq!(pick_cpu_by_load(&loads), 1);
    }

    #[test]
    fn placement_falls_back_to_total_only_among_equally_live_harts() {
        // Balance still matters — but only once liveness is settled.
        let loads = [load(0, 0, 9), load(0, 0, 3), load(0, 0, 7)];
        assert_eq!(pick_cpu_by_load(&loads), 1);
    }

    #[test]
    fn placement_never_trades_liveness_for_balance() {
        // A hart with one blocker loses to a hart with none no matter how
        // lopsided the totals are. This is the whole point of the ordering:
        // an unbalanced-but-running task beats a balanced-and-starved one.
        let loads = [load(1, 1, 0), load(0, 0, u32::MAX)];
        assert_eq!(pick_cpu_by_load(&loads), 1);
    }

    #[test]
    fn placement_is_deterministic_on_ties_and_total_ordering() {
        // Ties resolve to the lowest index, so the choice is reproducible.
        assert_eq!(pick_cpu_by_load(&[load(1, 1, 1), load(1, 1, 1), load(1, 1, 1)]), 0);
        // And every non-empty input yields an in-range index.
        for a in 0..3u32 {
            for b in 0..3u32 {
                for c in 0..3u32 {
                    let got = pick_cpu_by_load(&[load(a, a, c), load(b, b, a), load(c, c, b)]);
                    assert!(got < 3, "out-of-range CPU {got}");
                }
            }
        }
    }

    #[test]
    fn placement_handles_a_single_cpu_and_an_empty_list() {
        assert_eq!(pick_cpu_by_load(&[load(9, 9, 9)]), 0);
        assert_eq!(pick_cpu_by_load(&[]), 0);
    }

    // ── K-C12: ready-queue admission ────────────────────────────────────
    //
    // `cpu_enqueue`'s guard used to be a `debug_assert!`, compiled out of the
    // release profile the kernel ships. These pin the replacement policy.
    // The ring manipulation itself lives on `static mut PER_CPU` and is not
    // host-compilable; only the decision is.

    #[test]
    fn enqueue_refuses_a_task_that_is_already_queued() {
        // The duplicate case is safe to refuse precisely because an entry
        // already exists — refusing loses nothing.
        assert_eq!(
            enqueue_decision(true, 0, 64),
            EnqueueOutcome::AlreadyQueued,
            "a queued task must never be appended twice"
        );
        // Even on an empty ring, and even on a full one: being queued wins.
        assert_eq!(enqueue_decision(true, 64, 64), EnqueueOutcome::AlreadyQueued);
    }

    #[test]
    fn enqueue_refuses_instead_of_overwriting_a_full_ring() {
        // The bug: `count == capacity` used to fall through to
        // `q.buf[q.tail] = idx`, clobbering a live entry and pushing `count`
        // past the ring. Refusal is the only non-corrupting answer that is
        // also not a panic (= board reset under `panic = "abort"`).
        assert_eq!(enqueue_decision(false, 64, 64), EnqueueOutcome::Full);
        // And a count that somehow ran past capacity must still refuse, not
        // wrap back into "there is room".
        assert_eq!(enqueue_decision(false, 65, 64), EnqueueOutcome::Full);
        assert_eq!(enqueue_decision(false, usize::MAX, 64), EnqueueOutcome::Full);
    }

    #[test]
    fn enqueue_appends_only_with_room_and_no_duplicate() {
        assert_eq!(enqueue_decision(false, 0, 64), EnqueueOutcome::Append);
        assert_eq!(enqueue_decision(false, 63, 64), EnqueueOutcome::Append);
    }

    #[test]
    fn enqueue_never_appends_past_capacity_for_any_input() {
        // Exhaustive over the ring: Append implies there was a free slot.
        for count in 0..=70usize {
            for &already in &[false, true] {
                if enqueue_decision(already, count, 64) == EnqueueOutcome::Append {
                    assert!(!already && count < 64, "appended with count={count} already={already}");
                }
            }
        }
    }

    #[test]
    fn a_full_ring_of_distinct_tasks_is_unreachable_while_queued_holds() {
        // The safety argument for `Task::queued`, stated as a test: with at
        // most MAX_TASKS tasks and at most one queue entry each, one CPU's
        // queues can never hold more than MAX_TASKS entries — so a
        // MAX_TASKS-deep ring cannot be asked to hold one more.
        let capacity = crate::task::MAX_TASKS;
        for queued_elsewhere in 0..capacity {
            let room_here = capacity - queued_elsewhere;
            assert_eq!(
                enqueue_decision(false, room_here.saturating_sub(1), capacity),
                EnqueueOutcome::Append,
                "a distinct task must always fit while the invariant holds"
            );
        }
    }
}

// ── Ring-3 scenario that would actually demonstrate the race ────────────────
//
// The tests above cannot observe an SMP interleaving; only the kernel can.
// The scenario below is what `userspace/ipctest/` must do under `-smp 4`.
// It is written to be implementable by another lane without reading this
// crate.
//
// Shape:
//   * Parent registers as a fast-IPC server and `fork()`s N ≥ 8 children.
//   * Each child immediately issues SYS_IPC_FAST_CALL(parent_tid, [seq,0,0,0])
//     in a loop of M ≥ 200 iterations.
//   * The parent loops SYS_IPC_FAST_ACCEPT → SYS_IPC_FAST_REPLY(slot,
//     [seq ^ MAGIC, ...]) with NO delay between accept and reply. The tight
//     accept/reply loop is what makes the reply land inside the client's
//     window between wake_fast_ipc_server() and task_block(); adding a sleep
//     there hides the bug.
//   * Children must be pinned across harts (or left unpinned) so client and
//     server genuinely run on different harts — on `-smp 1` the race cannot
//     occur and the test proves nothing. Assert the hart count at startup.
//
// What to assert:
//   1. Every one of the N×M calls returns, and returns `seq ^ MAGIC` for its
//      own `seq`. A wrong value means a reply was collected by the wrong
//      client (slot aliasing), which is a *different* bug from ours.
//   2. Each child prints `IPCTEST child=<i> done=<M>` and the parent prints
//      `IPCTEST all=<N*M> OK` only after all children exited. The harness
//      greps for `IPCTEST all=`.
//   3. A per-child watchdog: before each call the child records `seq` in a
//      known location (a global it also prints on a timer, or simply
//      printing `IPCTEST child=<i> seq=<k>` every 32 iterations). This is
//      what separates the two failure modes.
//
// How to tell "hung on the lost wakeup" from "hung on something else" — this
// is the part that matters, because a bare timeout proves nothing:
//   * Lost wakeup (K-C10) looks like: the run stops making progress, the
//     LAST line for one or more children is a `seq=` line, and the *parent*
//     keeps going or itself blocks in FAST_ACCEPT with no pending call. The
//     signature is that the stuck child is `Blocked` on
//     `WaitReason::FastIpcClient(slot)` while its slot is already replied to
//     — i.e. the reply data is present but nobody collected it. Expose this
//     from the shell: a `ps`-style dump showing state + wait_reason per task
//     plus the fast-IPC slot table (owner TID, replied flag). If a slot is
//     `replied=true` with its owner `Blocked on FastIpcClient(that slot)`,
//     that is K-C10 and nothing else.
//   * Slot exhaustion looks different: FAST_CALL returns -1 immediately
//     rather than hanging. Children must print `IPCTEST child=<i> ENOSLOT`
//     and continue, so exhaustion never masquerades as a hang.
//   * A `-1` from `fast_ipc_collect` after a *successful* wake is the
//     spurious-`wake_pending` signature (see the K-C10 report): the call
//     returns, but with -1 and no reply. Children must count those
//     separately and print `IPCTEST child=<i> spurious=<n>`; a non-zero
//     count is a real finding even if the run completes.
//   * Anything else (fork failure, scheduler deadlock, page fault) shows up
//     as a missing `IPCTEST child=<i> done=` line with no preceding `seq=`
//     progress at all, or as a board reset — distinguishable because the
//     panic handler prints first.
//
// Expected result today: with the K-C10 fix in `wake_fast_ipc_server` the
// FAST_ACCEPT side is closed, but the client side is only closed once
// `SYS_IPC_FAST_REPLY` switches from `wake_fast_ipc_client(slot)` to
// `wake_fast_ipc_client_tid(caller_tid, slot)`. Until that call-site change
// lands, this scenario should still hang — which makes it a genuine
// regression test rather than a formality.

// ═══════════════════════════════════════════════════════════════════════════
// Carril E — ELF loader bounds and hart-liveness accounting
// ═══════════════════════════════════════════════════════════════════════════
//
// Both modules below are compiled from the kernel tree verbatim, exactly like
// `task.rs`/`wait.rs` above: no copies, no stubs, no dependencies to stub.
// They were split out of `process.rs` / `smp.rs` precisely so that they could
// be — the files they came from cannot leave the RISC-V target (PTE flags and
// the physical allocator in one, `mv tp` and `_secondary_start` in the other).
//
// What this does NOT cover, stated rather than papered over:
//   * The page-table half of `load_elf_into` — `vmm::map`,
//     `vmm::translate_user` reuse, `add_user_leaf_perms` and the W^X refusal
//     of EXEC-onto-WRITE. Those need a page table, so they need the target.
//   * `wake_harts` itself: the SBI call and `current_cpu_id()`. Only the
//     accounting it feeds is here — but the accounting *was* the bug.

#[path = "../../sched/src/elf_bounds.rs"]
pub mod elf_bounds;

#[path = "../../sched/src/hart_set.rs"]
pub mod hart_set;

#[cfg(test)]
mod elf_bounds_tests {
    use super::elf_bounds::{check_pt_load, page_up, SegCheck, SegLimits, SegReject};

    /// The real limits, spelled out because this crate cannot depend on
    /// `robot_os_mm` (RISC-V CSRs) or `robot_os_sched` (ditto).
    ///
    /// `guard_limit` is `vmm::USER_GUARD_LIMIT`, `low_max` is
    /// `process::USER_LOW_MAX`, `page_size` is `mmu::PAGE_SIZE`. If any of
    /// those three moves, `real_images_still_load` below is what should fail
    /// first — it replays the segment tables measured off the actual binaries.
    const LIM: SegLimits = SegLimits {
        guard_limit: 0x1_0000,
        low_max: 0x0200_0000,
        page_size: 4096,
    };

    /// A well-formed segment, to be perturbed one field at a time.
    fn ok(p_vaddr: usize, p_memsz: usize) -> SegCheck {
        check_pt_load(0x1000, p_vaddr, p_memsz, p_memsz, 0x10_0000, 0, LIM)
    }

    fn reject_of(c: SegCheck) -> SegReject {
        match c {
            SegCheck::Reject(r) => r,
            other => panic!("expected a rejection, got {:?}", other),
        }
    }

    fn range_of(c: SegCheck) -> (usize, usize) {
        match c {
            SegCheck::Load(r) => (r.va_start, r.va_end),
            other => panic!("expected the segment to load, got {:?}", other),
        }
    }

    // ── The lower bound itself ──────────────────────────────────────────

    #[test]
    fn page_zero_is_refused() {
        // The whole point of the encargo: a legal ELF can name p_vaddr = 0,
        // and before this the loader mapped it, which made the null guard in
        // handle_demand_fault/handle_cow_fault meaningless for that process.
        assert_eq!(reject_of(ok(0, 0x1000)), SegReject::NullGuard);
    }

    #[test]
    fn just_below_the_guard_limit_is_refused() {
        assert_eq!(reject_of(ok(0xFFFF, 1)), SegReject::NullGuard);
        assert_eq!(reject_of(ok(0xF000, 0x1000)), SegReject::NullGuard);
    }

    #[test]
    fn exactly_the_guard_limit_still_loads() {
        // The single most dangerous off-by-one in this change. 0x10000 is
        // both USER_GUARD_LIMIT and the min PT_LOAD p_vaddr of every one of
        // the 12 binaries in build/. A `<=` here bricks all of userspace,
        // and with QEMU off the table this assertion is the only thing
        // standing between that and a commit.
        assert_eq!(range_of(ok(0x1_0000, 0x1000)), (0x1_0000, 0x1_1000));
    }

    #[test]
    fn just_above_the_guard_limit_still_loads() {
        assert_eq!(range_of(ok(0x1_0001, 1)), (0x1_0000, 0x1_1000));
    }

    #[test]
    fn a_segment_ending_inside_the_guard_cannot_exist() {
        // p_vaddr is the only lower bound needed: memsz only ever grows the
        // range upwards, so no accepted segment can reach below the guard.
        for va in [0x1_0000usize, 0x1_0001, 0x2_0000] {
            let (start, _) = range_of(ok(va, 1));
            assert!(start >= LIM.guard_limit, "va_start {:#x} below the guard", start);
        }
    }

    // ── Upper bound and overflow ────────────────────────────────────────

    #[test]
    fn the_low_max_ceiling_is_exclusive_for_the_start() {
        assert_eq!(reject_of(ok(LIM.low_max, 1)), SegReject::StartAboveLowMax);
        assert_eq!(reject_of(ok(LIM.low_max + 1, 1)), SegReject::StartAboveLowMax);
    }

    #[test]
    fn the_low_max_ceiling_is_inclusive_for_the_end() {
        // A segment may end exactly at the ceiling.
        let (_, end) = range_of(ok(LIM.low_max - 0x1000, 0x1000));
        assert_eq!(end, LIM.low_max);
        assert_eq!(
            reject_of(ok(LIM.low_max - 0x1000, 0x1001)),
            SegReject::EndOutOfRange
        );
    }

    #[test]
    fn memsz_overflow_rejects_instead_of_panicking() {
        // overflow-checks = true + panic = abort: an unchecked p_vaddr +
        // p_memsz here is a board reset driven by a file on a FAT volume.
        assert_eq!(reject_of(ok(0x1_0000, usize::MAX)), SegReject::EndOutOfRange);
        assert_eq!(
            reject_of(ok(0x1_0000, usize::MAX - 0x1_0000)),
            SegReject::EndOutOfRange
        );
    }

    #[test]
    fn an_absurd_vaddr_is_caught_by_the_ceiling_not_by_arithmetic() {
        assert_eq!(reject_of(ok(usize::MAX, 1)), SegReject::StartAboveLowMax);
        assert_eq!(reject_of(ok(usize::MAX, usize::MAX)), SegReject::StartAboveLowMax);
    }

    // ── File-range bounds ───────────────────────────────────────────────

    #[test]
    fn filesz_over_memsz_is_refused() {
        let c = check_pt_load(0x1000, 0x1_0000, 0x2000, 0x1000, 0x10_0000, 0, LIM);
        assert_eq!(reject_of(c), SegReject::FileSizeOverMemSize);
    }

    #[test]
    fn a_file_range_past_the_blob_is_refused() {
        let c = check_pt_load(0xF000, 0x1_0000, 0x2000, 0x2000, 0x10_000, 0, LIM);
        assert_eq!(reject_of(c), SegReject::FileRangeOutOfBlob);
    }

    #[test]
    fn an_offset_overflow_rejects_instead_of_panicking() {
        let c = check_pt_load(usize::MAX, 0x1_0000, 0x1000, 0x1000, 0x10_0000, 0, LIM);
        assert_eq!(reject_of(c), SegReject::FileRangeOutOfBlob);
    }

    // ── Segment ordering (the invariant the reuse branch documented) ─────

    #[test]
    fn a_descending_segment_is_refused() {
        // Header order, not vaddr order, drives the loop. An image whose
        // second PT_LOAD sits below the first used to be loaded anyway, and
        // its file bytes rewrote the first segment's already-mapped page.
        let c = check_pt_load(0x1000, 0x1_0000, 0x100, 0x100, 0x10_0000, 0x1_1000, LIM);
        assert_eq!(reject_of(c), SegReject::Descending);
    }

    #[test]
    fn a_segment_starting_exactly_at_the_previous_end_is_allowed() {
        // Not hypothetical: brain_client, gpio_drv, ipctest, reflex and
        // uhello all have .rodata starting at exactly .text's end byte.
        // A `>` here instead of `>=` would reject 5 of the 12 real images.
        let c = check_pt_load(0x1c72, 0x1_0c72, 0x1b3, 0x1b3, 0x10_0000, 0x1_0c72, LIM);
        assert!(matches!(c, SegCheck::Load(_)), "got {:?}", c);
    }

    #[test]
    fn segments_sharing_a_page_but_not_a_byte_are_allowed() {
        let c = check_pt_load(0x1b48, 0x1_0b48, 0x849, 0x849, 0x10_0000, 0x1_0b44, LIM);
        let (start, end) = range_of(c);
        assert_eq!((start, end), (0x1_0000, 0x1_2000));
    }

    // ── Empty segments ──────────────────────────────────────────────────

    #[test]
    fn a_zero_memsz_segment_is_skipped_not_rejected() {
        // Deliberate, and it is why the null-guard check sits after it: a
        // PT_LOAD with p_memsz = 0 maps nothing at all, so even p_vaddr = 0
        // is inert. Rejecting it would refuse images that are merely odd.
        assert_eq!(check_pt_load(0, 0, 0, 0, 0, 0, LIM), SegCheck::Empty);
        assert_eq!(check_pt_load(0, usize::MAX, 0, 0, 0, 0, LIM), SegCheck::Empty);
    }

    // ── Page rounding ───────────────────────────────────────────────────

    #[test]
    fn page_up_saturates_instead_of_wrapping() {
        assert_eq!(page_up(0, 4096), 0);
        assert_eq!(page_up(1, 4096), 4096);
        assert_eq!(page_up(4096, 4096), 4096);
        assert_eq!(page_up(usize::MAX, 4096), usize::MAX & !4095);
    }

    #[test]
    fn an_unaligned_segment_maps_from_its_page_base() {
        let (start, end) = range_of(ok(0x1_0b48, 0x849));
        assert_eq!((start, end), (0x1_0000, 0x1_2000));
    }

    // ── Nothing in this range may panic ─────────────────────────────────

    #[test]
    fn no_input_anywhere_near_the_threshold_panics() {
        // A panic under this profile is a board reset, and `exec` is
        // reachable from ring 3, so "returns something" is the assertion.
        let interesting = [
            0usize, 1, 0xFFF, 0x1000, 0xFFFF, 0x1_0000, 0x1_0001,
            LIM.low_max - 1, LIM.low_max, LIM.low_max + 1,
            usize::MAX / 2, usize::MAX - 1, usize::MAX,
        ];
        let mut loaded = 0;
        let mut rejected = 0;
        for &va in &interesting {
            for &memsz in &interesting {
                for &filesz in &interesting {
                    for &off in &interesting {
                        match check_pt_load(off, va, filesz, memsz, 0x10_0000, 0, LIM) {
                            SegCheck::Load(_) => loaded += 1,
                            _ => rejected += 1,
                        }
                    }
                }
            }
        }
        assert_eq!(loaded + rejected, interesting.len().pow(4));
        assert!(loaded > 0, "the sweep must exercise the accepting path too");
    }

    #[test]
    fn every_prev_seg_end_is_survivable() {
        for &prev in &[0usize, 0x1_0000, LIM.low_max, usize::MAX] {
            let _ = check_pt_load(0x1000, 0x1_0000, 0x100, 0x100, 0x10_0000, prev, LIM);
        }
    }

    // ── Regression: the real images must still load ─────────────────────

    /// Every PT_LOAD of every ELF in `build/`, read off the files with a
    /// program-header parser (`p_offset, p_vaddr, p_filesz, p_memsz`).
    /// Grouped per image so the ordering check sees the real sequence.
    const REAL_IMAGES: &[(&str, &[(usize, usize, usize, usize)])] = &[
        ("abitest",      &[(0x1000, 0x10000, 0xb44, 0xb44), (0x1b48, 0x10b48, 0x849, 0x849), (0x3000, 0x12000, 0x0, 0x8)]),
        ("brain_client", &[(0x1000, 0x10000, 0xc72, 0xc72), (0x1c72, 0x10c72, 0x1b3, 0x1b3)]),
        ("captest",      &[(0x1000, 0x10000, 0x3dc, 0x3dc), (0x13e0, 0x103e0, 0x1a5, 0x1a5), (0x2000, 0x11000, 0x0, 0x4)]),
        ("gpio_drv",     &[(0x1000, 0x10000, 0x15a, 0x15a), (0x115a, 0x1015a, 0x46, 0x46)]),
        ("hello",        &[(0x1000, 0x10000, 0x35, 0x35)]),
        ("ipctest",      &[(0x1000, 0x10000, 0x1cfa, 0x1cfa), (0x2d00, 0x11d00, 0xa2c, 0xa2c), (0x4000, 0x13000, 0x8, 0xd0)]),
        ("ipctest2",     &[(0x1000, 0x10000, 0x1860, 0x1860), (0x2860, 0x11860, 0x827, 0x827), (0x4000, 0x13000, 0x8, 0xc8)]),
        ("latbench",     &[(0x1000, 0x10000, 0x48c, 0x48c), (0x1490, 0x10490, 0x284, 0x284)]),
        ("reflex",       &[(0x1000, 0x10000, 0x1f0, 0x1f0), (0x11f0, 0x101f0, 0xbb, 0xbb)]),
        ("syscall_test", &[(0x1000, 0x10000, 0xe7, 0xe7)]),
        ("uhello",       &[(0x1000, 0x10000, 0x34, 0x34), (0x1034, 0x10034, 0x24, 0x24)]),
    ];

    #[test]
    fn real_images_still_load() {
        // The bound is only as good as the fact that it costs nothing. If a
        // future edit tightens `guard_limit`, moves `low_max`, or turns the
        // ordering check into `>`, this is what says which binaries died.
        for (name, segs) in REAL_IMAGES {
            let mut prev_seg_end = 0usize;
            for &(off, va, filesz, memsz) in *segs {
                let blob_len = off + filesz + 0x1000; // every real p_offset is in range
                match check_pt_load(off, va, filesz, memsz, blob_len, prev_seg_end, LIM) {
                    SegCheck::Load(r) => prev_seg_end = r.seg_end,
                    other => panic!("{}: segment at {:#x} refused: {:?}", name, va, other),
                }
            }
        }
    }

    #[test]
    fn every_real_image_starts_exactly_at_the_guard_limit() {
        // The measurement the threshold was chosen from, kept as an
        // assertion instead of a claim in a comment.
        for (name, segs) in REAL_IMAGES {
            let min_va = segs.iter().map(|s| s.1).min().unwrap();
            assert_eq!(min_va, LIM.guard_limit, "{} no longer starts at the guard limit", name);
        }
    }
}

#[cfg(test)]
mod hart_set_tests {
    use super::hart_set::{mark_alive, online_prefix, stranded, HART_MASK_BITS};

    /// The accounting `wake_harts` used before this tanda, reproduced so the
    /// tests can show exactly where it diverges — and where it does not.
    fn legacy_online(num_cpus: usize, boot: usize, started: &[bool]) -> usize {
        let mut online = 1; // "boot hart is already running" — i.e. slot 0
        let mut prefix_intact = true;
        for hart in 0..num_cpus {
            if hart == boot {
                continue;
            }
            if started[hart] {
                if prefix_intact {
                    online += 1;
                }
            } else {
                prefix_intact = false;
            }
        }
        online
    }

    /// What `wake_harts` computes now, from the same inputs.
    fn current_online(num_cpus: usize, boot: usize, started: &[bool]) -> usize {
        let mut alive = mark_alive(0, boot);
        for hart in 0..num_cpus {
            if hart == boot {
                continue;
            }
            if started[hart] {
                alive = mark_alive(alive, hart);
            }
        }
        online_prefix(alive, num_cpus)
    }

    /// True when every hart in `0..online` is actually running — the single
    /// property `find_best_cpu` and `rebalance_from_offline_cpus` rely on.
    fn prefix_is_honest(online: usize, boot: usize, started: &[bool]) -> bool {
        (0..online).all(|h| h == boot || started[h])
    }

    fn all_patterns(num_cpus: usize) -> Vec<Vec<bool>> {
        (0..(1u32 << num_cpus))
            .map(|bits| (0..num_cpus).map(|h| bits & (1 << h) != 0).collect())
            .collect()
    }

    // ── The regression, stated as the concrete measured case ────────────

    #[test]
    fn boot_hart_two_with_hart_zero_dead_used_to_claim_hart_zero_was_alive() {
        // Boot hart 2 was measured in QEMU virt (K-C16 evidence). Harts 1
        // and 3 start, hart 0 does not.
        let started = [false, true, false, true]; // index 2 unused: it is the boot hart
        assert_eq!(legacy_online(4, 2, &started), 1);
        assert!(!prefix_is_honest(1, 2, &started), "legacy published a dead hart 0");

        assert_eq!(current_online(4, 2, &started), 0);
        assert!(prefix_is_honest(0, 2, &started));
    }

    #[test]
    fn boot_hart_two_with_a_hole_used_to_publish_the_dead_hart() {
        // Hart 0 starts, hart 1 fails, hart 3 starts, boot hart is 2.
        let started = [true, false, false, true];
        assert_eq!(legacy_online(4, 2, &started), 2); // claims harts 0 and 1
        assert!(!prefix_is_honest(2, 2, &started), "legacy published dead hart 1");

        assert_eq!(current_online(4, 2, &started), 1); // only hart 0
        assert!(prefix_is_honest(1, 2, &started));
    }

    #[test]
    fn boot_hart_two_with_everything_up_was_already_correct() {
        // The honest half of the answer: with no hart_start failure the old
        // seed happened to land on the right number, which is why this never
        // showed up in a normal boot.
        let started = [true, true, false, true];
        assert_eq!(legacy_online(4, 2, &started), 4);
        assert_eq!(current_online(4, 2, &started), 4);
    }

    #[test]
    fn with_boot_hart_zero_nothing_changed_at_all() {
        // Exhaustive over every start/fail pattern for 1..=4 harts: the fix
        // must not move the value on the configuration the kernel has always
        // actually run.
        for num_cpus in 1..=4 {
            for mut started in all_patterns(num_cpus) {
                started[0] = true; // the boot hart is running by definition
                assert_eq!(
                    legacy_online(num_cpus, 0, &started),
                    current_online(num_cpus, 0, &started),
                    "num_cpus={} started={:?}",
                    num_cpus,
                    started
                );
            }
        }
    }

    #[test]
    fn the_published_prefix_is_honest_for_every_boot_hart_and_pattern() {
        // The property, checked exhaustively rather than argued: for any boot
        // hart and any failure pattern, every hart below the published value
        // is running. The legacy accounting violates this (see below), which
        // is what makes it a bug and not a stale comment.
        let mut legacy_violations = 0;
        for num_cpus in 1..=4 {
            for boot in 0..num_cpus {
                for mut started in all_patterns(num_cpus) {
                    started[boot] = true;
                    let now = current_online(num_cpus, boot, &started);
                    assert!(
                        prefix_is_honest(now, boot, &started),
                        "num_cpus={} boot={} started={:?} online={}",
                        num_cpus, boot, started, now
                    );
                    if !prefix_is_honest(legacy_online(num_cpus, boot, &started), boot, &started) {
                        legacy_violations += 1;
                    }
                }
            }
        }
        assert!(
            legacy_violations > 0,
            "if the legacy accounting never lied, this whole change is cosmetic"
        );
    }

    #[test]
    fn every_legacy_violation_needs_both_a_nonzero_boot_hart_and_a_failure() {
        // The precise scope of the bug, so the report does not overstate it.
        for num_cpus in 1..=4 {
            for boot in 0..num_cpus {
                for mut started in all_patterns(num_cpus) {
                    started[boot] = true;
                    let legacy = legacy_online(num_cpus, boot, &started);
                    if !prefix_is_honest(legacy, boot, &started) {
                        assert_ne!(boot, 0, "a boot hart of 0 never produced a dishonest prefix");
                        assert!(
                            started.iter().any(|&s| !s),
                            "a run with no hart_start failure never produced a dishonest prefix"
                        );
                    }
                }
            }
        }
    }

    // ── Prefix / stranded semantics ─────────────────────────────────────

    #[test]
    fn a_live_hart_past_a_hole_is_excluded_and_reported() {
        let alive = 0b1011u64; // harts 0,1,3 up; hart 2 dead
        assert_eq!(online_prefix(alive, 4), 2);
        assert_eq!(stranded(alive, 4), 0b1000);
    }

    #[test]
    fn nothing_is_stranded_when_the_set_is_already_a_prefix() {
        assert_eq!(stranded(0b1111, 4), 0);
        assert_eq!(stranded(0b0011, 4), 0);
        assert_eq!(stranded(0b0000, 4), 0);
    }

    #[test]
    fn harts_past_num_cpus_are_not_counted() {
        // A boot hart id above the DTB's cpu count sets a bit outside the
        // range; it must not inflate the prefix.
        assert_eq!(online_prefix(0b1111_1111, 4), 4);
        assert_eq!(stranded(0b1111_0011, 4), 0);
    }

    #[test]
    fn a_dead_hart_zero_publishes_zero_rather_than_a_comfortable_one() {
        // Clamping to 1 would assert hart 0 is alive when it is not. Both
        // consumers guard the zero case; a lie has no guard.
        assert_eq!(online_prefix(0b1110, 4), 0);
    }

    // ── Nothing here may panic ──────────────────────────────────────────

    #[test]
    fn out_of_range_hart_ids_do_not_shift_overflow() {
        // `1u64 << 64` is an overflow panic under overflow-checks, i.e. a
        // board reset, and num_cpus comes from the DTB.
        assert_eq!(mark_alive(0, HART_MASK_BITS), 0);
        assert_eq!(mark_alive(0, HART_MASK_BITS + 1), 0);
        assert_eq!(mark_alive(0, usize::MAX), 0);
        assert_eq!(mark_alive(0b101, 12345), 0b101);
    }

    #[test]
    fn an_absurd_cpu_count_saturates_at_the_mask_width() {
        assert_eq!(online_prefix(u64::MAX, usize::MAX), HART_MASK_BITS);
        assert_eq!(online_prefix(u64::MAX, HART_MASK_BITS), HART_MASK_BITS);
        assert_eq!(stranded(u64::MAX, usize::MAX), 0);
        assert_eq!(stranded(u64::MAX - 1, usize::MAX), u64::MAX - 1);
    }

    #[test]
    fn the_boundary_bit_is_representable() {
        let alive = mark_alive(0, HART_MASK_BITS - 1);
        assert_eq!(alive, 1u64 << (HART_MASK_BITS - 1));
        assert_eq!(online_prefix(alive, HART_MASK_BITS), 0);
        assert_eq!(stranded(alive, HART_MASK_BITS), alive);
    }
}
