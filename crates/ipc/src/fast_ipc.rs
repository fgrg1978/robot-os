/// Fast-path IPC — seL4-style register-passing (M02).
///
/// Transfers up to 32 bytes (4 × u64) between two tasks without touching
/// user-space memory or allocating any kernel buffer.  Data lives in a
/// per-task FastIpcSlot (64 bytes) that is written by the sender and read
/// by the receiver after a minimal scheduler wakeup.
///
/// ## Protocol
///
/// Caller (client):
///   SYS_IPC_FAST_CALL(server_tid, d0, d1, d2, d3)
///     → places {d0..d3} in server's pending slot
///     → blocks self (WaitReason::FastIpc)
///     → wakes when server calls FAST_REPLY
///     → returns: d0..d3 from server's reply
///
/// Server:
///   SYS_IPC_FAST_ACCEPT()
///     → blocks until a client calls FAST_CALL targeting this task
///     → returns: (caller_tid, d0, d1, d2, d3)
///
///   SYS_IPC_FAST_REPLY(caller_tid, d0, d1, d2, d3)
///     → places {d0..d3} in caller's slot
///     → wakes caller
///     → returns immediately (non-blocking)
///
/// ## Guarantees
/// - No heap allocation, no copy_from_user, no ring buffer.
/// - Maximum 32 bytes per message.
/// - Single-writer: only the designated sender fills a slot.
/// - The slot is NOT thread-safe for concurrent senders to the same server;
///   callers must coordinate at a higher level (or use channels instead).

use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of concurrent fast IPC slots (one per potential caller).
pub const FAST_IPC_MAX_SLOTS: usize = 64;

/// Maximum number of 64-bit words in a fast IPC message.
pub const FAST_IPC_MAX_WORDS: usize = 4;

/// Sentinel TID value meaning "slot is free".
const FAST_IPC_SLOT_FREE: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// State of a fast IPC slot.
#[derive(Clone, Copy, PartialEq)]
enum SlotState {
    /// Slot is unused.
    Free,
    /// Caller has deposited data; waiting for server to accept.
    Pending,
    /// Server has deposited reply; waiting for caller to collect.
    Replied,
}

/// One pending fast IPC exchange.
#[derive(Clone, Copy)]
struct FastIpcSlot {
    /// TID of the caller (client).
    caller_tid: u32,
    /// TID of the server this call is targeting.
    server_tid: u32,
    /// Message data (up to 4 × u64 = 32 bytes).
    words: [u64; FAST_IPC_MAX_WORDS],
    /// Slot lifecycle state.
    state: SlotState,
}

impl FastIpcSlot {
    const fn empty() -> Self {
        FastIpcSlot {
            caller_tid: FAST_IPC_SLOT_FREE,
            server_tid: FAST_IPC_SLOT_FREE,
            words: [0u64; FAST_IPC_MAX_WORDS],
            state: SlotState::Free,
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

struct FastIpcState {
    slots: [FastIpcSlot; FAST_IPC_MAX_SLOTS],
    /// Number of slots currently in use (for quick-reject).
    used: u32,
}

impl FastIpcState {
    const fn new() -> Self {
        FastIpcState {
            slots: [FastIpcSlot::empty(); FAST_IPC_MAX_SLOTS],
            used: 0,
        }
    }

    fn alloc_slot(&mut self, caller: u32, server: u32, words: [u64; FAST_IPC_MAX_WORDS]) -> Option<usize> {
        for (i, s) in self.slots.iter_mut().enumerate() {
            if s.state == SlotState::Free {
                *s = FastIpcSlot { caller_tid: caller, server_tid: server, words, state: SlotState::Pending };
                self.used += 1;
                return Some(i);
            }
        }
        None
    }

    fn find_pending_for_server(&self, server_tid: u32) -> Option<usize> {
        // Early-exit when the table is empty — `used` is updated on every
        // alloc/free so this is O(1) for the common idle case. Without
        // this every fast-IPC poll did a 64-slot linear scan even when
        // nothing was pending.
        if self.used == 0 { return None; }
        self.slots.iter().position(|s| s.state == SlotState::Pending && s.server_tid == server_tid)
    }

    fn find_reply_for_caller(&self, caller_tid: u32) -> Option<usize> {
        if self.used == 0 { return None; }
        self.slots.iter().position(|s| s.state == SlotState::Replied && s.caller_tid == caller_tid)
    }

    fn free_slot(&mut self, idx: usize) {
        self.slots[idx] = FastIpcSlot::empty();
        self.used = self.used.saturating_sub(1);
    }
}

static FAST_IPC: SpinLock<FastIpcState> = SpinLock::new(FastIpcState::new());

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Called when a client issues SYS_IPC_FAST_CALL.
///
/// Deposits the message into a slot targeting `server_tid`.
/// Returns Some(slot_idx) if the slot was allocated (caller should then block).
/// Returns None if no free slots (caller should fall back to channel IPC).
pub fn fast_ipc_call(
    caller_tid: u32,
    server_tid: u32,
    words: [u64; FAST_IPC_MAX_WORDS],
) -> Option<usize> {
    let mut state = FAST_IPC.lock();
    state.alloc_slot(caller_tid, server_tid, words)
}

/// Called when a server issues SYS_IPC_FAST_ACCEPT.
///
/// Returns `Some((slot_idx, caller_tid, words))` if a pending call exists for
/// this server; `None` if no call is pending (server should block).
pub fn fast_ipc_accept(server_tid: u32) -> Option<(usize, u32, [u64; FAST_IPC_MAX_WORDS])> {
    let mut state = FAST_IPC.lock();
    if let Some(idx) = state.find_pending_for_server(server_tid) {
        let caller = state.slots[idx].caller_tid;
        let words  = state.slots[idx].words;
        // Keep slot alive — server needs to reply to it.
        // Change state so another accept won't steal it.
        state.slots[idx].state = SlotState::Replied; // temporarily reuse state to mark "accepted"
        Some((idx, caller, words))
    } else {
        None
    }
}

/// Called when a server issues SYS_IPC_FAST_REPLY.
///
/// Deposits the reply into the slot and transitions state so the caller can
/// collect it.  Returns the caller TID to wake (or None if slot not found).
pub fn fast_ipc_reply(
    slot_idx: usize,
    words: [u64; FAST_IPC_MAX_WORDS],
) -> Option<u32> {
    if slot_idx >= FAST_IPC_MAX_SLOTS { return None; }
    let mut state = FAST_IPC.lock();
    let slot = &mut state.slots[slot_idx];
    if slot.state == SlotState::Free { return None; }
    let caller = slot.caller_tid;
    slot.words  = words;
    slot.state  = SlotState::Replied;
    Some(caller)
}

/// Called after the caller wakes from blocking.
///
/// Returns the reply words and frees the slot.
pub fn fast_ipc_collect(caller_tid: u32) -> Option<[u64; FAST_IPC_MAX_WORDS]> {
    let mut state = FAST_IPC.lock();
    if let Some(idx) = state.find_reply_for_caller(caller_tid) {
        let words = state.slots[idx].words;
        state.free_slot(idx);
        Some(words)
    } else {
        None
    }
}

/// Count currently active fast IPC slots (diagnostic).
pub fn fast_ipc_active() -> u32 {
    FAST_IPC.lock().used
}
