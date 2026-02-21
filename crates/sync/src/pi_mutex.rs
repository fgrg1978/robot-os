/// Priority Inheritance Mutex — prevents priority inversion.
///
/// When a high-priority task blocks on a PiMutex held by a lower-priority
/// task, the owner's priority is temporarily boosted to the waiter's level.
/// This prevents unbounded priority inversion in the RT pipeline.
///
/// If no priority callbacks are registered (kernel init hasn't run yet),
/// PiMutex degrades gracefully to a plain spinlock.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

/// Function pointer for boosting a task's priority: fn(tid, new_priority).
/// Stored as AtomicUsize (pointer-sized) — works on both RV32 and RV64.
pub static PI_BOOST_FN:  AtomicUsize = AtomicUsize::new(0);

/// Function pointer for restoring a task's priority: fn(tid, original_priority).
pub static PI_RESTORE_FN: AtomicUsize = AtomicUsize::new(0);

/// Register priority boost/restore callbacks.
/// Must be called once during kernel init, before any PiMutex contention.
pub fn pi_set_callbacks(
    boost:   fn(u32, u32),
    restore: fn(u32, u32),
) {
    PI_BOOST_FN.store(boost as usize, Ordering::Release);
    PI_RESTORE_FN.store(restore as usize, Ordering::Release);
}

/// A no-owner sentinel for `owner_tid`.
const NO_OWNER: u32 = u32::MAX;

/// Priority Inheritance Mutex protecting data of type `T`.
pub struct PiMutex<T> {
    data:               UnsafeCell<T>,
    locked:             AtomicBool,
    owner_tid:          AtomicU32,
    owner_orig_priority: AtomicU32,
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
        }
    }

    /// Acquire the mutex, spinning until available.
    ///
    /// While spinning, if a priority boost callback is registered, the
    /// current task's priority is propagated to the lock owner to prevent
    /// priority inversion.
    pub fn lock(&self) -> PiMutexGuard<'_, T> {
        // Fast path: try to acquire immediately
        if self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.record_owner();
            return PiMutexGuard { mutex: self };
        }

        // Slow path: contention — apply priority inheritance
        let my_prio = current_task_priority();
        let mut boosted = false;

        loop {
            // Boost owner if we have a callback and know the owner
            if !boosted {
                let owner = self.owner_tid.load(Ordering::Acquire);
                if owner != NO_OWNER && my_prio > 0 {
                    let boost_ptr = PI_BOOST_FN.load(Ordering::Acquire);
                    if boost_ptr != 0 {
                        let boost_fn: fn(u32, u32) =
                            unsafe { core::mem::transmute(boost_ptr) };
                        boost_fn(owner, my_prio);
                        boosted = true;
                    }
                }
            }

            // Spin until the lock looks free
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }

            // Try to acquire
            if self.locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                self.record_owner();
                return PiMutexGuard { mutex: self };
            }
        }
    }

    /// Try to acquire the mutex without spinning.
    /// Returns `None` if already held.
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
    fn record_owner(&self) {
        let tid  = current_task_tid();
        let prio = current_task_priority();
        self.owner_tid.store(tid, Ordering::Release);
        self.owner_orig_priority.store(prio, Ordering::Release);
    }

    /// Restore the owner's original priority and release the lock.
    fn release(&self) {
        let tid       = self.owner_tid.load(Ordering::Acquire);
        let orig_prio = self.owner_orig_priority.load(Ordering::Acquire);

        // Clear owner before unlocking to avoid stale reads
        self.owner_tid.store(NO_OWNER, Ordering::Release);
        self.owner_orig_priority.store(0, Ordering::Release);

        // Restore priority if callback is registered
        if tid != NO_OWNER {
            let restore_ptr = PI_RESTORE_FN.load(Ordering::Acquire);
            if restore_ptr != 0 {
                let restore_fn: fn(u32, u32) =
                    unsafe { core::mem::transmute(restore_ptr as usize) };
                restore_fn(tid, orig_prio);
            }
        }

        // Unlock
        self.locked.store(false, Ordering::Release);
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
