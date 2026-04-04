/// Completion — one-shot event signaling primitive.
///
/// A task calls `wait()` to sleep until another task (or IRQ handler)
/// calls `complete()`. Unlike a spinlock, the waiter releases the CPU
/// entirely while waiting.
///
/// Typical uses:
/// - Wait for driver init to finish before reading sensors.
/// - Wait for a DMA or SPI transfer to complete.
/// - Wait for a response from the brain server.
///
/// A Completion can be `complete()`-ed before anyone `wait()`-s — the
/// next `wait()` returns immediately without blocking.
///
/// # Reuse
///
/// Call `reset()` to reuse a Completion for another event cycle.

use core::sync::atomic::{AtomicBool, Ordering};
use crate::waitqueue::WaitQueue;

/// A one-shot completion event.
pub struct Completion {
    done: AtomicBool,
    wq:   WaitQueue,
}

// Safety: Completion uses AtomicBool + WaitQueue (both internally synced).
unsafe impl Send for Completion {}
unsafe impl Sync for Completion {}

impl Completion {
    /// Create a new incomplete Completion. Usable as `static`.
    pub const fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            wq:   WaitQueue::new(),
        }
    }

    /// Wait for the completion to be signaled.
    ///
    /// If `complete()` was already called, returns immediately.
    /// Otherwise, blocks the current task until `complete()` is called.
    pub fn wait(&mut self) {
        // Fast path: already completed.
        if self.done.load(Ordering::Acquire) {
            return;
        }

        // Slow path: sleep on the WaitQueue.
        // Loop handles spurious wakeups (though our WaitQueue doesn't
        // produce them, defensive coding for future changes).
        while !self.done.load(Ordering::Acquire) {
            self.wq.wait();
        }
    }

    /// Signal the completion, waking all waiters.
    ///
    /// Safe to call from any context including IRQ handlers
    /// (wake_all only touches atomics + scheduler wake callback).
    pub fn complete(&mut self) {
        self.done.store(true, Ordering::Release);
        self.wq.wake_all();
    }

    /// Returns `true` if the completion has been signaled.
    pub fn is_complete(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Reset the completion for reuse.
    ///
    /// Must only be called when no tasks are waiting (i.e., after all
    /// waiters have observed `complete()` and resumed).
    pub fn reset(&mut self) {
        self.done.store(false, Ordering::Release);
    }
}
