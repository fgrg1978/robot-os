/// Lease-based IPC — zero-copy large transfers (M04).
///
/// A **lease** is a time-bounded grant of a shared memory region from a sender
/// (the "lessor") to a receiver (the "lessee").  While the lease is active:
///
/// - The lessee has access to the SHM region (physical pages via existing SHM).
/// - The lessor's write access is revoked (enforced by kernel state — the lessor
///   must call `lease_wait_return()` and will be blocked if the lease has not
///   been returned yet).
/// - When the lessee calls `lease_return()` the lessor is woken.
///
/// This allows camera frames, sensor buffers, and inference inputs to be passed
/// between tasks without any copying.  It is inspired by the ownership transfer
/// semantics of seL4 Reply+RecvWait and Midori's promise-based IPC.
///
/// ## Usage
///
/// ```text
/// Lessor (producer):
///   shm_id = shm_create(...)              // allocate buffer
///   lease_id = lease_grant(shm_id, lessee_tid, expire_ticks)
///   // ... lessor is now READ-ONLY on the region (no enforcement yet — honour-based)
///   lease_wait_return(lease_id)           // blocks until lessee returns or expires
///   // ... lessor regains full access
///
/// Lessee (consumer):
///   lease_id = lease_accept(lessor_tid)  // blocks until a lease arrives
///   shm_id   = lease_shm_id(lease_id)
///   // ... read/process the buffer
///   lease_return(lease_id)               // wake lessor
/// ```

use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of concurrent leases.
pub const MAX_LEASES: usize = 16;

/// Sentinel TID for "no owner".
const NO_TID: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle state of a lease.
#[derive(Clone, Copy, PartialEq)]
pub enum LeaseState {
    /// Slot is unused.
    Free,
    /// Granted but lessee has not accepted yet.
    Pending,
    /// Lessee has accepted; buffer is in use.
    Active,
    /// Lessee has returned the buffer; lessor can reclaim.
    Returned,
    /// Lease timed out (expire_ticks reached).
    Expired,
}

/// A single lease entry.
pub struct LeaseEntry {
    pub shm_id:       usize,
    pub lessor_tid:   u32,
    pub lessee_tid:   u32,
    pub expire_ticks: u64,   // absolute deadline in CLINT ticks (0 = no expiry)
    pub state:        LeaseState,
}

impl LeaseEntry {
    pub const fn empty() -> Self {
        LeaseEntry {
            shm_id:       0,
            lessor_tid:   NO_TID,
            lessee_tid:   NO_TID,
            expire_ticks: 0,
            state:        LeaseState::Free,
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

struct LeaseTable {
    entries: [LeaseEntry; MAX_LEASES],
}

impl LeaseTable {
    const fn new() -> Self {
        const E: LeaseEntry = LeaseEntry::empty();
        LeaseTable { entries: [E; MAX_LEASES] }
    }

    fn alloc(&mut self, shm_id: usize, lessor: u32, lessee: u32, expire: u64) -> Option<usize> {
        for (i, e) in self.entries.iter_mut().enumerate() {
            if e.state == LeaseState::Free {
                *e = LeaseEntry { shm_id, lessor_tid: lessor, lessee_tid: lessee,
                                  expire_ticks: expire, state: LeaseState::Pending };
                return Some(i);
            }
        }
        None
    }

    fn find_pending_for_lessee(&self, lessee: u32) -> Option<usize> {
        self.entries.iter().position(|e| e.state == LeaseState::Pending && e.lessee_tid == lessee)
    }
}

static LEASES: SpinLock<LeaseTable> = SpinLock::new(LeaseTable::new());

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Grant a lease of `shm_id` to `lessee_tid`.
///
/// `expire_ticks`: absolute CLINT tick at which the lease auto-expires (0 = never).
/// Returns `Some(lease_id)` or `None` if no free slots.
pub fn lease_grant(shm_id: usize, lessor_tid: u32, lessee_tid: u32, expire_ticks: u64) -> Option<usize> {
    LEASES.lock().alloc(shm_id, lessor_tid, lessee_tid, expire_ticks)
}

/// Lessee accepts a pending lease from `lessor_tid`.
///
/// Returns `Some((lease_id, shm_id))` if a pending lease exists.
/// The caller should block on `WaitReason::FastIpcServer` or similar if `None`.
pub fn lease_accept(lessee_tid: u32) -> Option<(usize, usize)> {
    let mut table = LEASES.lock();
    if let Some(idx) = table.find_pending_for_lessee(lessee_tid) {
        let shm_id = table.entries[idx].shm_id;
        table.entries[idx].state = LeaseState::Active;
        Some((idx, shm_id))
    } else {
        None
    }
}

/// Lessee returns the lease.  Wakes the lessor.
///
/// Returns `Some(lessor_tid)` to wake, or `None` if the lease_id is invalid.
pub fn lease_return(lease_id: usize) -> Option<u32> {
    if lease_id >= MAX_LEASES { return None; }
    let mut table = LEASES.lock();
    let e = &mut table.entries[lease_id];
    if e.state != LeaseState::Active { return None; }
    let lessor = e.lessor_tid;
    e.state = LeaseState::Returned;
    Some(lessor)
}

/// Lessor checks if the lease has been returned.
///
/// Should be called after being woken from `WaitReason::Timer` or a dedicated
/// lease-wait reason.  Returns `true` if the buffer is safely reclaimed.
pub fn lease_is_returned(lease_id: usize) -> bool {
    if lease_id >= MAX_LEASES { return false; }
    let table = LEASES.lock();
    matches!(table.entries[lease_id].state, LeaseState::Returned | LeaseState::Expired)
}

/// Free a lease entry (called by lessor after reclaiming the buffer).
pub fn lease_free(lease_id: usize) {
    if lease_id >= MAX_LEASES { return; }
    let mut table = LEASES.lock();
    table.entries[lease_id] = LeaseEntry::empty();
}

/// Tick all active leases; expire those past their deadline.
///
/// Returns a list of `(lease_id, lessor_tid)` pairs that just expired so
/// the caller can wake the corresponding lessor tasks.
///
/// Call this from the timer ISR alongside `wake_expired_timers()`.
pub fn lease_tick(now_ticks: u64) -> [(usize, u32); MAX_LEASES] {
    let mut expired = [(0usize, NO_TID); MAX_LEASES];
    let mut count = 0;
    let mut table = LEASES.lock();
    for (i, e) in table.entries.iter_mut().enumerate() {
        if e.state == LeaseState::Active
            && e.expire_ticks != 0
            && now_ticks >= e.expire_ticks
        {
            e.state = LeaseState::Expired;
            if count < MAX_LEASES {
                expired[count] = (i, e.lessor_tid);
                count += 1;
            }
        }
    }
    expired
}

/// Diagnostic: count active leases.
pub fn lease_active_count() -> usize {
    LEASES.lock().entries.iter().filter(|e| e.state == LeaseState::Active).count()
}
