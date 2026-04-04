/// WaitQueue — lightweight sleep/wake mechanism for blocking synchronization.
///
/// Tasks that call `wait()` are put to sleep (Blocked state) and removed
/// from the scheduler's ready queue. `wake_one()` / `wake_all()` move
/// them back to Ready.
///
/// # Scheduler decoupling
///
/// The sync crate cannot depend on the sched crate (sched depends on sync).
/// Instead, block/wake operations go through function pointers registered at
/// boot via `wq_set_callbacks()`. Before registration, `wait()` degrades to
/// a spinloop (safe for early boot).
///
/// # Protocol
///
/// The WaitQueue holds a fixed-size array of waiting task IDs. When a task
/// calls `wait()`:
///   1. Its TID is added to the waiters array.
///   2. The scheduler callback blocks the task (Blocked state).
///   3. On `wake_one()` / `wake_all()`, the wake callback is called for
///      each waiter TID, moving them back to Ready.
///
/// A SpinLock protects the waiters array to handle concurrent wait/wake
/// from different harts.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Maximum number of tasks that can wait on a single WaitQueue simultaneously.
const WAITQUEUE_CAPACITY: usize = 16;

/// Callback: block the current task. fn() — puts calling task to Blocked.
pub static WQ_BLOCK_FN: AtomicUsize = AtomicUsize::new(0);

/// Callback: wake a specific task by TID. fn(tid: u32).
pub static WQ_WAKE_FN: AtomicUsize = AtomicUsize::new(0);

/// Register scheduler callbacks for WaitQueue block/wake.
/// Must be called once during kernel init, before any WaitQueue usage.
pub fn wq_set_callbacks(block_fn: fn(), wake_fn: fn(u32)) {
    WQ_BLOCK_FN.store(block_fn as usize, Ordering::Release);
    WQ_WAKE_FN.store(wake_fn as usize, Ordering::Release);
}

/// A queue of tasks waiting for a condition to become true.
pub struct WaitQueue {
    lock:    AtomicBool,
    waiters: [u32; WAITQUEUE_CAPACITY],
    count:   usize,
}

// Safety: WaitQueue uses internal spinlock for synchronization.
unsafe impl Send for WaitQueue {}
unsafe impl Sync for WaitQueue {}

impl WaitQueue {
    /// Create a new empty WaitQueue. Usable as `static`.
    pub const fn new() -> Self {
        Self {
            lock:    AtomicBool::new(false),
            waiters: [0; WAITQUEUE_CAPACITY],
            count:   0,
        }
    }

    /// Acquire the internal spinlock.
    #[inline(always)]
    fn spin_lock(&self) {
        while self.lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.lock.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
    }

    /// Release the internal spinlock.
    #[inline(always)]
    fn spin_unlock(&self) {
        self.lock.store(false, Ordering::Release);
    }

    /// Sleep the current task on this WaitQueue.
    ///
    /// The task is blocked until another task calls `wake_one()` or
    /// `wake_all()` on this queue.
    ///
    /// If scheduler callbacks are not yet registered (early boot), this
    /// degrades to a no-op spin — the caller must handle the fallback.
    pub fn wait(&mut self) {
        let tid = current_task_tid();
        if tid == u32::MAX {
            // No scheduler running — spin fallback.
            core::hint::spin_loop();
            return;
        }

        // Add ourselves to the waiters list.
        self.spin_lock();
        if self.count < WAITQUEUE_CAPACITY {
            self.waiters[self.count] = tid;
            self.count += 1;
        }
        // Must unlock BEFORE blocking — otherwise the wake path can't acquire.
        // Safety: the internal spinlock is separate from any data lock.
        self.spin_unlock();

        // Block via scheduler callback.
        let block_ptr = WQ_BLOCK_FN.load(Ordering::Acquire);
        if block_ptr != 0 {
            let block_fn: fn() = unsafe { core::mem::transmute(block_ptr) };
            block_fn();
            // Returns here when woken.
        }
    }

    /// Wake one waiting task (FIFO order).
    /// Returns `true` if a task was woken, `false` if the queue was empty.
    pub fn wake_one(&mut self) -> bool {
        self.spin_lock();
        if self.count == 0 {
            self.spin_unlock();
            return false;
        }
        let tid = self.waiters[0];
        // Shift remaining waiters forward.
        for i in 1..self.count {
            self.waiters[i - 1] = self.waiters[i];
        }
        self.count -= 1;
        self.spin_unlock();

        do_wake(tid);
        true
    }

    /// Wake all waiting tasks.
    /// Returns the number of tasks woken.
    pub fn wake_all(&mut self) -> usize {
        self.spin_lock();
        let n = self.count;
        // Copy TIDs out before releasing the lock.
        let mut tids = [0u32; WAITQUEUE_CAPACITY];
        tids[..n].copy_from_slice(&self.waiters[..n]);
        self.count = 0;
        self.spin_unlock();

        for i in 0..n {
            do_wake(tids[i]);
        }
        n
    }

    /// Returns the number of tasks currently waiting.
    pub fn len(&self) -> usize {
        self.spin_lock();
        let n = self.count;
        self.spin_unlock();
        n
    }

    /// Returns `true` if no tasks are waiting.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Call the scheduler's wake callback for a single TID.
fn do_wake(tid: u32) {
    let wake_ptr = WQ_WAKE_FN.load(Ordering::Acquire);
    if wake_ptr != 0 {
        let wake_fn: fn(u32) = unsafe { core::mem::transmute(wake_ptr) };
        wake_fn(tid);
    }
}

/// Read current task TID from the PI mutex identity atomics.
/// Returns u32::MAX if no scheduler is running.
fn current_task_tid() -> u32 {
    crate::pi_mutex::CURRENT_TID.load(Ordering::Relaxed)
}
