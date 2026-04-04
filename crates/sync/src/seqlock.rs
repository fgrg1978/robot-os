/// SeqLock — single-writer, multi-reader lock-free synchronization.
///
/// The writer never blocks. Readers detect concurrent writes via an
/// atomic sequence counter and retry. Ideal for data published at high
/// frequency (sensor samples, timestamps) read by many consumers.
///
/// # Protocol
///
/// Writer:
///   1. Increment sequence to odd  (signals "write in progress")
///   2. Write data
///   3. Increment sequence to even (signals "write complete")
///
/// Reader:
///   1. Read sequence (retry if odd — write in progress)
///   2. Copy data
///   3. Re-read sequence — if changed, data may be torn → retry
///
/// # Memory ordering
///
/// - Writer uses `Release` after data write so readers see updated data.
/// - Reader uses `Acquire` before data read so it observes the writer's stores.
/// - Inner spin on odd uses `Relaxed` (TTAS pattern).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

/// A SeqLock protecting data of type `T`.
///
/// `T` must be `Copy` because readers may observe partially-written data
/// and must be able to discard and retry without running destructors.
pub struct SeqLock<T: Copy> {
    seq:  AtomicU32,
    data: UnsafeCell<T>,
}

// Safety: SeqLock provides its own synchronization protocol.
// T: Send + Copy is sufficient — writer has exclusive mutable access,
// readers only get copies.
unsafe impl<T: Copy + Send> Send for SeqLock<T> {}
unsafe impl<T: Copy + Send> Sync for SeqLock<T> {}

impl<T: Copy> SeqLock<T> {
    /// Create a new SeqLock with initial data.
    pub const fn new(data: T) -> Self {
        Self {
            seq:  AtomicU32::new(0),
            data: UnsafeCell::new(data),
        }
    }

    /// Read the protected data, retrying if a write is in progress.
    ///
    /// This never blocks the writer. If contention is high the reader
    /// spins, but each iteration is just two atomic loads + a memcpy.
    #[inline]
    pub fn read(&self) -> T {
        loop {
            // Step 1: wait for even sequence (no write in progress).
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 != 0 {
                // Writer active — spin until it finishes.
                core::hint::spin_loop();
                continue;
            }

            // Step 2: copy data.
            // Safety: we only read; torn reads are detected by the seq check below.
            let value = unsafe { *self.data.get() };

            // Step 3: verify sequence didn't change during our read.
            let s2 = self.seq.load(Ordering::Acquire);
            if s1 == s2 {
                return value;
            }
            // Sequence changed — data may be torn, retry.
            core::hint::spin_loop();
        }
    }

    /// Begin a write. Returns a guard that must be used to complete the write.
    ///
    /// # Safety contract
    /// Only ONE writer may call this at a time. If multiple writers are
    /// possible, the caller must serialize them externally (e.g. with a
    /// SpinLock around the write call, or by design — single producer).
    #[inline]
    pub fn write(&self) -> SeqLockWriteGuard<'_, T> {
        // Odd sequence = write in progress.
        self.seq.fetch_add(1, Ordering::Acquire);
        SeqLockWriteGuard { lock: self }
    }
}

/// RAII guard for a SeqLock write operation.
///
/// Dereferences to `&mut T` for writing. Completes the write (bumps
/// sequence to even) on drop.
pub struct SeqLockWriteGuard<'a, T: Copy> {
    lock: &'a SeqLock<T>,
}

impl<T: Copy> core::ops::Deref for SeqLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: Copy> core::ops::DerefMut for SeqLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: Copy> Drop for SeqLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // Even sequence = write complete. Release ensures data stores
        // are visible before the sequence update.
        self.lock.seq.fetch_add(1, Ordering::Release);
    }
}
