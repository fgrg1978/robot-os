/// Spinlock using RISC-V atomic operations (TTAS pattern).
///
/// Wraps data in a `SpinLock<T>` to ensure exclusive access.
/// Uses `compare_exchange` (compiles to `amoor.w.aq`) for the lock.
///
/// Two acquisition modes:
/// - `lock()`         — standard spinlock, interrupts unchanged.
/// - `lock_irqsave()` — disables interrupts on the local hart before
///                       acquiring, preventing deadlock when an IRQ handler
///                       contends for the same lock. Returns an `IrqSaveGuard`
///                       that restores the previous interrupt state on drop.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use robot_os_arch::csr;

/// A simple test-and-set spinlock protecting data of type `T`.
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// Safety: SpinLock provides exclusive access via lock/unlock.
unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Create a new unlocked SpinLock.
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    /// Core spin loop — shared by both lock variants.
    #[inline(always)]
    fn acquire_spin(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // TTAS: spin on a plain load to avoid bus contention.
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
    }

    /// Acquire the lock, spinning until it is available.
    /// Returns a guard that releases the lock on drop.
    ///
    /// **WARNING:** If this lock may be taken from an interrupt handler,
    /// use `lock_irqsave()` instead — otherwise the IRQ can preempt the
    /// holder on the same hart and deadlock.
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        self.acquire_spin();
        SpinLockGuard { lock: self }
    }

    /// Acquire the lock with interrupts disabled on the local hart.
    ///
    /// Saves the previous `sstatus.SIE` state, clears it (disables
    /// supervisor interrupts), then spins for the lock. The returned
    /// `IrqSaveGuard` restores the original interrupt state on drop.
    ///
    /// Use this when the lock is (or may be) shared with an IRQ handler.
    pub fn lock_irqsave(&self) -> IrqSaveGuard<'_, T> {
        // Read sstatus and disable SIE in one step via csrrc.
        let prev_sstatus = csr::read_sstatus();
        csr::write_sstatus(prev_sstatus & !csr::SSTATUS_SIE);

        self.acquire_spin();
        IrqSaveGuard { lock: self, prev_sstatus }
    }

    /// Try to acquire the lock without spinning.
    /// Returns `None` if the lock is already held.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinLockGuard { lock: self })
        } else {
            None
        }
    }

    /// Try to acquire the lock with IRQ save, without spinning.
    /// Returns `None` (and restores interrupts) if the lock is already held.
    pub fn try_lock_irqsave(&self) -> Option<IrqSaveGuard<'_, T>> {
        let prev_sstatus = csr::read_sstatus();
        csr::write_sstatus(prev_sstatus & !csr::SSTATUS_SIE);

        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(IrqSaveGuard { lock: self, prev_sstatus })
        } else {
            // Failed — restore interrupts before returning.
            csr::write_sstatus(prev_sstatus);
            None
        }
    }

    /// Unsafe: get a mutable reference without locking.
    /// Only use during single-threaded init before SMP starts.
    ///
    /// Also (intentionally) used after SMP is up by panic-path `*_panic`
    /// helpers (e.g. `motor::motor_stop_panic`, `gpio::gpio_write_panic`,
    /// `pwm::pwm_set_duty_pct_panic`) that must never block on a lock
    /// another hart might be holding at panic time — see those functions'
    /// doc comments for the "torn state is fine, a hung panic handler is
    /// not" rationale. Not a bug if you see it called from there.
    ///
    /// # Safety
    /// Caller must ensure no concurrent access, OR (panic path only)
    /// accept a possible torn read/write racing a concurrent lock holder.
    pub unsafe fn get_mut_unchecked(&self) -> &mut T {
        &mut *self.data.get()
    }

    /// Release the lock (used internally by guards).
    #[inline(always)]
    fn release(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

// ── Standard guard (no IRQ save) ────────────────────────────────────────────

/// RAII guard that releases the spinlock when dropped.
pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> core::ops::Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.release();
    }
}

// ── IRQ-save guard ──────────────────────────────────────────────────────────

/// RAII guard that releases the spinlock AND restores the previous
/// interrupt enable state (`sstatus.SIE`) when dropped.
///
/// Drop order matters: unlock first (Release store), then restore
/// interrupts. This ensures that a pending IRQ that fires immediately
/// after re-enable sees the lock as free.
pub struct IrqSaveGuard<'a, T> {
    lock: &'a SpinLock<T>,
    prev_sstatus: usize,
}

impl<T> core::ops::Deref for IrqSaveGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for IrqSaveGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for IrqSaveGuard<'_, T> {
    fn drop(&mut self) {
        // Release lock first, then restore interrupt state.
        self.lock.release();
        // Restore only the SIE bit from the saved sstatus.
        let current = csr::read_sstatus();
        let restored = (current & !csr::SSTATUS_SIE)
            | (self.prev_sstatus & csr::SSTATUS_SIE);
        csr::write_sstatus(restored);
    }
}
