#![no_std]

//! Generic typed channel — Phase H.
//!
//! A `Channel<T>` is a single-slot, multi-producer / multi-consumer IPC
//! primitive for bare-metal systems.  It stores one value of type `T: Copy`,
//! a monotonic sequence number, and a timestamp (caller-provided).
//!
//! # Design
//!
//! - **Zero-alloc**: no heap, no `Vec`, no `Box`.  `T` must be `Copy`.
//! - **Lock**: uses `SpinLock` for exclusive access (minimal critical section).
//! - **Sequence**: monotonic `u64` incremented on every `publish()`.
//!   Readers compare seq to detect new data without inspecting the payload.
//! - **Timestamp**: caller-provided `u64` (typically `clint::get_time()`).
//!   Enables generic watchdog: `channel.age(now) > threshold → stale`.
//!
//! # Example
//!
//! ```ignore
//! use robot_os_channel::Channel;
//!
//! #[derive(Clone, Copy)]
//! struct Cmd { speed: i32 }
//!
//! static CH: Channel<Cmd> = Channel::new(Cmd { speed: 0 });
//!
//! // Publisher
//! CH.publish(Cmd { speed: 50 }, clint::get_time());
//!
//! // Reader
//! let snap = CH.read();
//! if snap.seq > 0 {
//!     use_cmd(snap.val);
//! }
//! ```

use core::sync::atomic::{AtomicU64, Ordering};
use robot_os_sync::SpinLock;

/// A snapshot returned by [`Channel::read`].
#[derive(Clone, Copy)]
pub struct Snapshot<T: Copy> {
    /// The latest published value.
    pub val: T,
    /// Monotonic sequence number (0 = never published).
    pub seq: u64,
    /// Caller-provided timestamp of the last `publish()`.
    pub timestamp: u64,
}

/// A single-slot typed channel.
///
/// Stores one `T`, a sequence counter, and a timestamp.
/// All operations are O(1) with bounded spin time.
pub struct Channel<T: Copy> {
    inner: SpinLock<Inner<T>>,
    /// Sequence counter — readable without locking for fast "has new data?" check.
    seq: AtomicU64,
}

struct Inner<T: Copy> {
    val: T,
    seq: u64,
    timestamp: u64,
}

// Safety: Channel provides exclusive access via SpinLock.
unsafe impl<T: Copy + Send> Send for Channel<T> {}
unsafe impl<T: Copy + Send> Sync for Channel<T> {}

impl<T: Copy> Channel<T> {
    /// Create a new channel with a default value.
    ///
    /// `seq` starts at 0 (never published).
    pub const fn new(default: T) -> Self {
        Channel {
            inner: SpinLock::new(Inner {
                val: default,
                seq: 0,
                timestamp: 0,
            }),
            seq: AtomicU64::new(0),
        }
    }

    /// Publish a new value.
    ///
    /// Increments the sequence number and stores the caller-provided timestamp.
    /// `timestamp` should be `clint::get_time()` or equivalent monotonic clock.
    pub fn publish(&self, val: T, timestamp: u64) {
        let mut g = self.inner.lock();
        g.seq += 1;
        g.val = val;
        g.timestamp = timestamp;
        let s = g.seq;
        drop(g);
        // Update the lock-free seq counter AFTER releasing the lock,
        // so readers that see a new seq will get consistent data.
        self.seq.store(s, Ordering::Release);
    }

    /// Read the current value, sequence number, and timestamp.
    ///
    /// Returns a `Snapshot<T>` (copy-out, no blocking beyond the spin).
    pub fn read(&self) -> Snapshot<T> {
        let g = self.inner.lock();
        Snapshot {
            val: g.val,
            seq: g.seq,
            timestamp: g.timestamp,
        }
    }

    /// Read the sequence counter without locking.
    ///
    /// Useful for fast "has new data?" checks:
    /// ```ignore
    /// if ch.seq() > my_last_seq { let snap = ch.read(); ... }
    /// ```
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Age in ticks since the last publish, given the current time.
    ///
    /// Returns `u64::MAX` if the channel has never been published to (seq == 0).
    pub fn age(&self, now: u64) -> u64 {
        let g = self.inner.lock();
        if g.seq == 0 {
            u64::MAX
        } else {
            now.saturating_sub(g.timestamp)
        }
    }

    /// Returns `true` if the channel has been published to at least once.
    pub fn is_valid(&self) -> bool {
        self.seq.load(Ordering::Acquire) > 0
    }
}
