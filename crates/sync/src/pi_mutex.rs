/// Priority Inheritance Mutex — prevents priority inversion.
///
/// When a high-priority task blocks on a PiMutex held by a lower-priority
/// task, the owner's priority is temporarily boosted to the waiter's level.
/// This prevents unbounded priority inversion in the RT pipeline.
///
/// If no priority callbacks are registered (kernel init hasn't run yet),
/// PiMutex degrades gracefully to a plain spinlock.
///
/// # Priority convention
///
/// Throughout the scheduler, **a lower number is a higher priority**
/// (`PerCpu::ready_bitmap.trailing_zeros()` picks the winner, and
/// `sched::pi_boost_task` only ever writes `new_prio` when
/// `new_prio < task.priority`). Every comparison below follows that.
///
/// # Donation protocol (K-A14)
///
/// Edge-triggered and counted. A waiter donates **at most once** per
/// acquisition; the mutex counts donations in `donations`, and `release()`
/// issues exactly one restore per donation recorded.
///
///   * A waiter reads the current owner and calls the boost callback for it
///     while holding [`PiMutex::pi_state`], then increments `donations`.
///   * The owner's `release()` takes the same `pi_state`, drains `donations`,
///     and de-boosts that many times.
///
/// Holding `pi_state` across the owner read + boost is what makes the
/// protocol safe: without it a waiter can read owner `O`, `O` can release
/// and de-boost itself, and only then does the waiter's boost land — leaving
/// `O` permanently elevated with nobody left to restore it.
///
/// Because the donation is owned by the mutex and undone by the owner, a
/// waiter that is killed or preempted forever mid-wait leaks nothing.
///
/// # Waiting yields, it does not spin to completion
///
/// A contended waiter spins briefly and then calls the registered yield
/// callback. This is not a performance tweak — spinning to completion made
/// inheritance useless whenever two contenders shared a hart: the
/// higher-priority waiter never released the CPU, so the owner it had just
/// boosted could not run, and the contention hung no matter how correct the
/// donation was. Yielding is what lets the boost do its work; owner and
/// waiter then sit at the same priority and share the hart until the critical
/// section ends.
///
/// Yielding rather than blocking on a `WaitQueue` is deliberate. Blocking
/// would need the release-and-sleep to be atomic against the owner's
/// wake — `WaitQueue::wait()` cannot be split that way, so the owner can
/// release and wake an empty queue just before the waiter enqueues, leaving it
/// asleep forever with the mutex free. Yielding re-checks every pass and has
/// no such window.
///
/// # Known limitations
///
///   * Donation is not transitive: `pi_boost_task` writes `Task::priority`
///     but not [`CURRENT_PRIO`], so a waiter that is itself carrying a boost
///     donates its stale base priority. Fixing that needs an
///     effective-priority read callback (a `pi_set_callbacks` signature
///     change).
///   * Not recursive: re-acquiring a PiMutex this task already owns loops
///     forever yielding, with no diagnostic — `donate()` skips
///     `owner == my_tid`, so not even a self-boost is attempted. The hart is
///     no longer monopolised (other tasks still run), which makes this a live
///     lock rather than a hard hang, but it is still a bug in the caller.
///   * **WARNING:** do not take a PiMutex from an interrupt handler (same
///     hazard as `SpinLock::lock`). Once the scheduler is running,
///     [`CURRENT_TID`] names the *interrupted* task, so a handler contending
///     for a mutex that task holds sees `owner == my_tid` and declines to
///     donate — and yielding from an interrupt context is not valid either.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::spinlock::SpinLock;

/// Function pointer for boosting a task's priority: fn(tid, new_priority).
/// Stored as AtomicUsize (pointer-sized).
pub static PI_BOOST_FN:  AtomicUsize = AtomicUsize::new(0);

/// Function pointer for restoring a task's priority: fn(tid, original_priority).
pub static PI_RESTORE_FN: AtomicUsize = AtomicUsize::new(0);

/// Register priority boost/restore callbacks.
/// Must be called once during kernel init, before any PiMutex contention.
pub fn pi_set_callbacks(
    boost:   fn(u32, u32),
    restore: fn(u32, u32),
    yield_fn: fn(),
) {
    PI_BOOST_FN.store(boost as usize, Ordering::Release);
    PI_RESTORE_FN.store(restore as usize, Ordering::Release);
    PI_YIELD_FN.store(yield_fn as usize, Ordering::Release);
}

/// Yield callback. Registered together with boost/restore; absent before the
/// scheduler exists, in which case the acquire loop degrades to a plain spin
/// (correct, because with no scheduler there is nothing to yield to).
static PI_YIELD_FN: AtomicUsize = AtomicUsize::new(0);

fn yield_callback() -> Option<fn()> {
    let p = PI_YIELD_FN.load(Ordering::Acquire);
    if p == 0 { None } else { Some(unsafe { core::mem::transmute::<usize, fn()>(p) }) }
}

/// Load the boost callback, or `None` if kernel init hasn't registered one.
#[inline]
fn boost_callback() -> Option<fn(u32, u32)> {
    let p = PI_BOOST_FN.load(Ordering::Acquire);
    if p == 0 { None } else { Some(unsafe { core::mem::transmute::<usize, fn(u32, u32)>(p) }) }
}

/// Load the restore callback, or `None` if kernel init hasn't registered one.
#[inline]
fn restore_callback() -> Option<fn(u32, u32)> {
    let p = PI_RESTORE_FN.load(Ordering::Acquire);
    if p == 0 { None } else { Some(unsafe { core::mem::transmute::<usize, fn(u32, u32)>(p) }) }
}

/// A no-owner sentinel for `owner_tid`.
const NO_OWNER: u32 = u32::MAX;

/// Plain-load spins between yields while waiting.
///
/// Purely a backoff knob now, not a correctness bound: the acquire loop no
/// longer re-asserts inheritance, so nothing depends on coming back around
/// within any particular window. Small, because the point of the loop is to
/// reach the yield promptly and let the (boosted) owner run.
const SPINS_PER_YIELD: u32 = 64;

/// Priority Inheritance Mutex protecting data of type `T`.
///
/// Owner bookkeeping stays in separate atomics (rather than inside the
/// `SpinLock`) so the spin loop can read `owner_tid` cheaply; `pi_state`
/// exists only to order the read-modify-write sequences that must not
/// interleave.
pub struct PiMutex<T> {
    data:               UnsafeCell<T>,
    locked:             AtomicBool,
    owner_tid:          AtomicU32,
    /// Priority snapshot taken at acquire time.
    ///
    /// CAVEAT: this is [`CURRENT_PRIO`], which the scheduler writes on
    /// context switch from `next.priority` — a value that may *already*
    /// carry an inherited boost. It is therefore not reliably the owner's
    /// base priority. Harmless today only because `sched::pi_restore_task`
    /// ignores its second argument and restores `Task::base_priority`. If
    /// that ever changes, this field must be replaced by a real
    /// base-priority read callback.
    owner_orig_priority: AtomicU32,
    /// How many donations the current owner carries *through this mutex*.
    ///
    /// A count, not a flag: with several waiters each donating once, the
    /// owner's release must undo exactly as many boosts as were made, or the
    /// scheduler's per-task donation counter drifts and the task never returns
    /// to its base priority. Reset to 0 by `record_owner`, incremented under
    /// `pi_state` by `donate`, drained under `pi_state` by `release`.
    donations:          AtomicU32,
    /// Serialises {read owner → boost} in `lock()` against
    /// {read owner → clear → de-boost} in `release()`.
    pi_state:           SpinLock<()>,
}

// Safety: PiMutex provides exclusive access; data is only reachable through
// the guard, which requires acquiring the lock.
unsafe impl<T: Send> Send for PiMutex<T> {}
unsafe impl<T: Send> Sync for PiMutex<T> {}

impl<T> PiMutex<T> {
    /// Create a new unlocked PiMutex.
    pub const fn new(data: T) -> Self {
        PiMutex {
            data:               UnsafeCell::new(data),
            locked:             AtomicBool::new(false),
            owner_tid:          AtomicU32::new(NO_OWNER),
            owner_orig_priority: AtomicU32::new(0),
            donations:          AtomicU32::new(0),
            pi_state:           SpinLock::new(()),
        }
    }

    /// Acquire the mutex, spinning until available.
    ///
    /// While spinning, if a priority boost callback is registered, the
    /// current task's priority is propagated to the lock owner to prevent
    /// priority inversion. The donation is re-asserted periodically so it
    /// survives an owner hand-off and an unrelated restore.
    pub fn lock(&self) -> PiMutexGuard<'_, T> {
        // Fast path: try to acquire immediately
        if self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.record_owner();
            return PiMutexGuard { mutex: self };
        }

        // Slow path: contention — apply priority inheritance.
        //
        // Identity is sampled once: this task cannot migrate or change base
        // priority underneath itself while it is the one executing here.
        let my_tid  = current_task_tid();
        let my_prio = current_task_priority();

        // Donate at most once per acquisition. The old loop re-asserted the
        // boost periodically, which forced `pi_boost_task` to stay idempotent
        // and therefore uncounted — and uncounted donations are exactly why two
        // contended PiMutexes could not compose. Donating once makes the
        // protocol edge-triggered and countable; `release()` undoes precisely
        // as many boosts as were made.
        let mut donated = false;

        loop {
            // (1) Donate to whoever owns the lock, the first time we manage to
            //     observe an owner. Missing the window between a winning CAS
            //     and its `record_owner()` just means we look again next pass.
            if !donated {
                donated = self.donate(my_tid, my_prio);
            }

            // (2) Brief spin, then YIELD. Spinning to completion was the
            //     fundamental flaw: with two contenders on one hart the
            //     higher-priority waiter never released the CPU, so the owner
            //     it had just boosted could not run and the contention hung
            //     regardless of inheritance. Yielding is what lets the boost
            //     do its job — owner and waiter now sit at the same priority
            //     and share the hart until the critical section ends.
            let mut spins: u32 = 0;
            while self.locked.load(Ordering::Relaxed) && spins < SPINS_PER_YIELD {
                core::hint::spin_loop();
                spins += 1;
            }
            if self.locked.load(Ordering::Relaxed) {
                match yield_callback() {
                    // No scheduler yet: nothing to yield to, so a plain spin is
                    // both the only option and the correct one.
                    None    => core::hint::spin_loop(),
                    Some(y) => y(),
                }
            }

            // (3) Try to acquire.
            if self.locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                self.record_owner();
                return PiMutexGuard { mutex: self };
            }
        }
    }

    /// Donate `my_prio` to the current owner, if there is one.
    ///
    /// Runs entirely under `pi_state` so that `release()` cannot clear the
    /// owner and de-boost in between our read of `owner_tid` and the boost
    /// call — that interleaving is what leaks a permanent boost onto a task
    /// that no longer holds the lock.
    ///
    /// No filtering on `my_prio` here: `sched::pi_boost_task` already applies
    /// the `new_prio < owner.priority` test, and priority 0 (the top RT band)
    /// is precisely the level that most needs to donate.
    fn donate(&self, my_tid: u32, my_prio: u32) -> bool {
        // A context with no scheduled task (early boot, IRQ before the
        // scheduler starts) has no priority to give away.
        if my_tid == NO_OWNER {
            return false;
        }
        // Cheap unlocked pre-check to keep the uncontended-owner case off the
        // state lock; re-validated below under the lock.
        if self.owner_tid.load(Ordering::Acquire) == NO_OWNER {
            return false;
        }
        let boost = match boost_callback() {
            Some(f) => f,
            None    => return false, // no scheduler callbacks — plain spinlock
        };

        // IRQ-safe: an interrupt handler contending for the same PiMutex
        // would otherwise deadlock against this hart's own state lock.
        let _st = self.pi_state.lock_irqsave();
        let owner = self.owner_tid.load(Ordering::Acquire);
        if owner != NO_OWNER && owner != my_tid {
            boost(owner, my_prio);
            // Counted under `pi_state`, so the matching `release()` — which
            // drains the counter under the same lock — sees every donation.
            self.donations.fetch_add(1, Ordering::AcqRel);
            return true;
        }
        false
    }

    /// Try to acquire the mutex without spinning.
    /// Returns `None` if already held.
    ///
    /// Never donates: a caller that does not wait suffers no inversion.
    pub fn try_lock(&self) -> Option<PiMutexGuard<'_, T>> {
        if self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.record_owner();
            Some(PiMutexGuard { mutex: self })
        } else {
            None
        }
    }

    /// Store the current task's TID and priority as the lock owner.
    ///
    /// Taken under `pi_state` so `owner_tid` and `owner_orig_priority` are
    /// never observed as a torn pair by a donating waiter.
    fn record_owner(&self) {
        let tid  = current_task_tid();
        let prio = current_task_priority();

        let _st = self.pi_state.lock_irqsave();
        self.owner_tid.store(tid, Ordering::Release);
        self.owner_orig_priority.store(prio, Ordering::Release);
        // A fresh owner starts with no donations. Already zero in practice —
        // `release()` drains the counter and `owner_tid` is NO_OWNER for the
        // whole unlock→record window, so no waiter can add to it — but keep
        // the invariant explicit rather than inferred.
        self.donations.store(0, Ordering::Release);
    }

    /// Release the lock and undo any priority donated through this mutex.
    fn release(&self) {
        // Clear the owner and consume the donation flag atomically with
        // respect to `donate()`. After this section a waiter observes
        // NO_OWNER and will not boost us, so no donation can land after our
        // de-boost.
        let (tid, orig_prio, boosted) = {
            let _st = self.pi_state.lock_irqsave();
            let tid       = self.owner_tid.load(Ordering::Acquire);
            let orig_prio = self.owner_orig_priority.load(Ordering::Acquire);
            let boosted   = self.donations.swap(0, Ordering::AcqRel);
            self.owner_tid.store(NO_OWNER, Ordering::Release);
            self.owner_orig_priority.store(0, Ordering::Release);
            (tid, orig_prio, boosted)
        };

        // Drop the mutex BEFORE de-boosting. De-boosting first would leave us
        // running at base priority while still holding the lock, so a
        // mid-priority task could preempt us in that window and reopen the
        // very inversion the donation paid to avoid. Running one extra
        // instant at the inherited priority after unlocking is bounded and
        // harmless by comparison.
        self.locked.store(false, Ordering::Release);

        // Undo exactly as many boosts as were donated through *this* mutex —
        // no more, no fewer. One restore per donation is what keeps the
        // scheduler's per-task donation counter balanced, and that balance is
        // what lets a second donation source (an outer PiMutex, the lease PI)
        // stay intact across our unlock: the count only reaches zero, and the
        // task only returns to base priority, when the LAST donor leaves.
        if tid != NO_OWNER && boosted > 0 {
            if let Some(restore) = restore_callback() {
                for _ in 0..boosted {
                    restore(tid, orig_prio);
                }
            }
        }
    }
}

/// RAII guard that releases the PiMutex and restores priority on drop.
pub struct PiMutexGuard<'a, T> {
    mutex: &'a PiMutex<T>,
}

impl<T> core::ops::Deref for PiMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> core::ops::DerefMut for PiMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for PiMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.release();
    }
}

// ── Stubs for task identity / priority ───────────────────────────────────────
// These read from atomics that the scheduler sets.  If no scheduler is running
// (early boot), they return safe defaults (tid=MAX, prio=0) which cause the
// PI logic to gracefully no-op.

/// Atomic holding the current task's TID (set by the scheduler on context switch).
/// Defaults to NO_OWNER so PI is a no-op before the scheduler starts.
pub static CURRENT_TID:  AtomicU32 = AtomicU32::new(NO_OWNER);
/// Atomic holding the current task's priority.
pub static CURRENT_PRIO: AtomicU32 = AtomicU32::new(0);

fn current_task_tid() -> u32 {
    CURRENT_TID.load(Ordering::Relaxed)
}

fn current_task_priority() -> u32 {
    CURRENT_PRIO.load(Ordering::Relaxed)
}
