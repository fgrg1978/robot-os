/// Fast-path IPC — seL4-style register-passing (M02).
///
/// Transfers up to 32 bytes (4 × u64) between two tasks without touching
/// user-space memory or allocating any kernel buffer.  Data lives in a
/// slot drawn from one global pool of `FAST_IPC_MAX_SLOTS` entries (NOT one
/// slot per task — the pool is shared board-wide, which is why
/// `fast_ipc_release_all` matters), written by the sender and read by the
/// receiver after a minimal scheduler wakeup.
///
/// ## Protocol (as actually implemented by `syscall::dispatch`)
///
/// Caller (client):
///   SYS_IPC_FAST_CALL(server_tid, d0, d1, d2, d3)
///     → places {d0..d3} in a slot targeting `server_tid`
///     → blocks self (WaitReason::FastIpcClient(slot_idx))
///     → wakes when the server calls FAST_REPLY
///     → returns: word 0 of the reply, or -1 (no slot / dead server /
///       bad target) meaning "fall back to channel IPC"
///
/// Server:
///   SYS_IPC_FAST_ACCEPT()
///     → blocks until a client calls FAST_CALL targeting this task
///     → **returns a `handle`**, not the caller TID and no longer a bare slot
///       index: the handle is `slot_index | generation << 6` — see
///       [`fast_ipc_make_handle`]. The request words come back in a1..a5.
///
///   SYS_IPC_FAST_REPLY(handle, d0, d1, d2, d3)
///     → `a0` is the **handle** from FAST_ACCEPT, not a TID and not a raw index
///     → the replier's TID and privilege come from the current task, not from
///       user registers — see [`fast_ipc_reply`]
///     → places {d0..d3} in the slot, wakes the caller
///     → returns 0, or a negative code if the reply was rejected (non-blocking
///       either way) — see [`FastIpcReply`]
///
/// ## Guarantees
/// - No heap allocation, no copy_from_user, no ring buffer.
/// - Maximum 32 bytes per message.
/// - Single-writer: only the designated sender fills a slot. This was an
///   aspiration until IPC-1; [`fast_ipc_reply`] now enforces it.
/// - The slot is NOT thread-safe for concurrent senders to the same server;
///   callers must coordinate at a higher level (or use channels instead).

#[cfg(target_os = "none")]
use robot_os_sync::SpinLock;
#[cfg(not(target_os = "none"))]
use self::host_seam::SpinLock;

// ---------------------------------------------------------------------------
// Scheduler seam
// ---------------------------------------------------------------------------
//
// The kernel build uses `sched_seam` below, whose two functions are
// `#[inline(always)]` one-liners over `robot_os_sched`: after inlining the
// seam is not in the binary at all, so it costs nothing on the fast path.
//
// **WHY it exists.** `robot_os_sync` and `robot_os_sched` are RV64-only —
// `robot_os_sync::SpinLock` alone fails to build for the host with
// `unresolved import robot_os_arch::csr`. The house pattern for testing an
// `ipc` module (see `crates/cap-tests`) is to pull the file in with `#[path]`
// and run its embedded `#[cfg(test)] mod tests`, which is impossible while the
// module names those crates unconditionally.
//
// The switch is `target_os = "none"`, **not** `cfg(test)`: `cargo test` builds
// the crate twice — once plain, once with `cfg(test)` — and the plain build
// would still have to resolve `robot_os_sync`. Every kernel target this tree
// builds (`riscv64imac-unknown-none-elf`, the VF2 and K1 variants) is
// `target_os = "none"`, so the host substitutes below are unreachable from any
// build that can run on a board.

#[cfg(target_os = "none")]
mod sched_seam {
    /// See [`super::fast_ipc_call`] for the cost and the race this accepts.
    #[inline(always)]
    pub fn tid_exists(tid: u32) -> bool {
        robot_os_sched::idx_for_tid(tid).is_some()
    }
    #[inline(always)]
    pub fn wake_client(handle: u64) {
        robot_os_sched::wake_fast_ipc_client(handle);
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of concurrent fast IPC slots (one per potential caller).
pub const FAST_IPC_MAX_SLOTS: usize = 64;

/// Maximum number of 64-bit words in a fast IPC message.
pub const FAST_IPC_MAX_WORDS: usize = 4;

/// Sentinel TID value meaning "slot is free".
const FAST_IPC_SLOT_FREE: u32 = u32::MAX;

// ── Server handle encoding (slot ABA) ──────────────────────────────────────
//
// **WHY a handle and not the bare slot index.** The index alone identifies a
// *slot*, never the *exchange* that was occupying it when the server accepted.
// Concretely: server S accepts client A's slot 5; A dies;
// `fast_ipc_release_all` frees slot 5; client B calls S and lands on slot 5; S
// accepts it. If S now replies with the index it was still holding for A, that
// reply passes both surviving checks — the slot is `Accepted` and S really is
// its `server_tid` — and **B collects the answer that was meant for A**. The
// ownership check from IPC-1 confines the damage to that server's own clients;
// it does not remove it.
//
// The handle names the tenancy, not the seat: index in the low bits, a
// per-slot generation counter above it, and the generation is bumped every
// time the slot is freed (`FastIpcState::free_slot`). A handle issued for
// tenancy N therefore stops matching the moment tenancy N ends, whether the
// slot is re-let or left empty.
//
// **The split, and what it costs.** `a0` carries the handle to ring 3 as an
// `i64` whose negative half is already spoken for by the error codes, so 63
// bits are usable. 6 of them index the 64 slots exactly; the remaining 57 are
// generation. Encode and decode are a shift, a mask and an or — no lookup, no
// second lock, nothing added to the critical section that was already being
// taken. That matters: the measured syscall floor here is 1879 ns/op and this
// is the path the kernel exists to make fast.
//
// **When the ABA comes back.** At 2^57 = 144_115_188_075_855_872 reuses *of
// one slot* the generation wraps and an ancient handle can match again. One
// reuse per nanosecond — faster than an instruction retires on this class of
// core — still needs about 4.5 years of doing nothing else. It is documented,
// tested (`generation_wrap_reopens_aba_the_documented_residual`) and accepted,
// not overlooked.

/// Bits of the handle that carry the slot index. 64 slots need exactly 6.
pub const FAST_IPC_SLOT_BITS: u32 = 6;

/// Mask for the slot-index field of a handle.
pub const FAST_IPC_SLOT_MASK: u64 = (1u64 << FAST_IPC_SLOT_BITS) - 1;

/// Mask for the generation field of a handle, **after** shifting it down.
///
/// 57 bits: 63 usable (bit 63 stays clear so every handle is a non-negative
/// `i64`) minus the 6 spent on the index.
pub const FAST_IPC_GEN_MASK: u64 = (1u64 << (63 - FAST_IPC_SLOT_BITS)) - 1;

// If `FAST_IPC_MAX_SLOTS` ever grows past what `FAST_IPC_SLOT_BITS` can
// address, indices would alias into the generation field and two different
// slots would share handles — silently. Fail the build instead.
const _: () = assert!(FAST_IPC_MAX_SLOTS <= (1usize << FAST_IPC_SLOT_BITS));

/// Build the ring-3 handle for `(slot_idx, generation)`.
///
/// Total by construction: both fields are masked, so no input panics and no
/// input can set bit 63. Exposed because `libsys` has to be able to state the
/// same layout, and one shared definition is cheaper than two that drift.
#[inline(always)]
pub const fn fast_ipc_make_handle(slot_idx: usize, generation: u64) -> u64 {
    ((generation & FAST_IPC_GEN_MASK) << FAST_IPC_SLOT_BITS)
        | ((slot_idx as u64) & FAST_IPC_SLOT_MASK)
}

/// Slot index carried by `handle`, or `None` if it does not name a real slot.
///
/// Pure arithmetic over a value ring 3 chose: it reads no table and reveals
/// nothing. It is here so that a caller which wants to *log* or *label* an
/// exchange (`libsys`'s `FastRequest.slot`, the census printers) does not have
/// to re-derive the layout and get it subtly different from this file.
///
/// **Bit 63 must be clear.** Every handle this file issues is a non-negative
/// `i64`, because that is the half of `a0` that is not already spoken for by
/// the error codes. Ignoring the bit instead of rejecting it would make
/// `h` and `h | 1<<63` the same handle, so a server that stored a negative
/// return value and later handed *that* back could land on a live slot
/// (`1<<63` alone decodes to index 0, generation 0). One compare turns that
/// whole class into a refusal.
#[inline(always)]
pub const fn fast_ipc_handle_slot(handle: u64) -> Option<usize> {
    if handle > i64::MAX as u64 {
        return None;
    }
    let idx = (handle & FAST_IPC_SLOT_MASK) as usize;
    if idx < FAST_IPC_MAX_SLOTS { Some(idx) } else { None }
}

/// Generation carried by `handle`. Same reasoning as [`fast_ipc_handle_slot`].
#[inline(always)]
const fn handle_generation(handle: u64) -> u64 {
    (handle >> FAST_IPC_SLOT_BITS) & FAST_IPC_GEN_MASK
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// State of a fast IPC slot.
///
/// **WHY `Accepted` is its own state (IPC-2).** `fast_ipc_accept` used to set
/// `Replied` with the comment "temporarily reuse state to mark accepted",
/// while `words` still held the *request*. `find_reply_for_caller` matches
/// exactly on `Replied`, so any wakeup of the client between accept and reply
/// — a server that accepts and then dies, or any spurious wake — made the
/// client collect its own request and treat it as the server's answer. No
/// attacker required. The three states must stay distinct: `Pending` is
/// "unclaimed work" (only `find_pending_for_server` matches it, so a second
/// accept cannot steal a claimed slot), `Accepted` is "claimed, no answer
/// yet" (matched by nothing), `Replied` is "answer present" (only
/// `find_reply_for_caller` matches it). Collapse any two and the confusion
/// above comes back.
#[derive(Clone, Copy, PartialEq)]
// `Debug` only off-board: it exists so host tests can name the state in an
// assertion failure, and there is no reason to hand the kernel binary a
// formatting impl for it.
#[cfg_attr(not(target_os = "none"), derive(Debug))]
enum SlotState {
    /// Slot is unused.
    Free,
    /// Caller has deposited data; waiting for server to accept.
    Pending,
    /// Server has claimed the call; `words` still holds the *request* and no
    /// reply exists yet. Deliberately matched by neither lookup helper.
    Accepted,
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
    /// Tenancy counter, bumped by [`FastIpcState::free_slot`] and by nothing
    /// else. It is the only field that must **survive** the wipe a free does:
    /// zero it and every handle the previous tenant handed out becomes valid
    /// again, which is exactly the ABA this field exists to close. Held masked
    /// to `FAST_IPC_GEN_MASK` so encoding is lossless.
    generation: u64,
}

impl FastIpcSlot {
    const fn empty() -> Self {
        FastIpcSlot {
            caller_tid: FAST_IPC_SLOT_FREE,
            server_tid: FAST_IPC_SLOT_FREE,
            words: [0u64; FAST_IPC_MAX_WORDS],
            state: SlotState::Free,
            generation: 0,
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
                // Carry the generation across the overwrite. This whole-struct
                // assignment is the easier of the two places to forget it —
                // `free_slot` at least looks like it is doing something to the
                // counter, this one looks like plain initialisation. Dropping
                // it here would reset every freshly allocated slot to
                // generation 0 and reopen the ABA on the very next reuse.
                let generation = s.generation;
                *s = FastIpcSlot {
                    caller_tid: caller,
                    server_tid: server,
                    words,
                    state: SlotState::Pending,
                    generation,
                };
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
        // `get_mut` rather than `[idx]`: with `panic = "abort"` an out-of-range
        // index is a board reset, i.e. a physical-safety event on a robot. Keep
        // the no-panic property local to the access instead of depending on a
        // bounds check made by whichever caller happens to be in fashion.
        if let Some(s) = self.slots.get_mut(idx) {
            // Ending the tenancy is what retires every handle issued for it.
            // Bump *here*, not in `alloc_slot`, so a handle dies the instant
            // its exchange does — even if the slot is never re-let.
            //
            // `wrapping_add` and not `+`: `overflow-checks = true` in this
            // tree, so the plain add would be a panic at the wrap, and
            // `panic = "abort"` makes a panic a board reset. Wrapping is also
            // the *correct* arithmetic — this is a tag, not a count, and the
            // mask keeps it inside the 57 bits the handle can carry.
            let next = s.generation.wrapping_add(1) & FAST_IPC_GEN_MASK;
            *s = FastIpcSlot::empty();
            s.generation = next;
            self.used = self.used.saturating_sub(1);
        }
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
///
/// **WHY the target is validated (IPC-5).** `server_tid` arrives raw in `a0`.
/// A call aimed at a TID that does not exist used to succeed: it burned a slot,
/// woke nobody, and blocked the caller forever. Ring 3 could therefore exhaust
/// all 64 slots with 64 syscalls, after which every `fast_ipc_call` returns
/// `None` for the life of the board and the fast path is dead — silently, since
/// dispatch's documented answer to `None` is "-1, fall back to channel IPC".
///
/// **Cost of the check, stated plainly.** `idx_for_tid` is an O(`MAX_TASKS`)
/// = O(64) linear scan of the `TASKS`/`TASK_VALID` statics with *no*
/// synchronisation, so it can run against another hart creating or destroying
/// a task and its answer is advisory, not authoritative. That is accepted on
/// purpose: the TOCTOU cannot be closed here, because holding `FAST_IPC` does
/// not freeze task creation either, and the only damage a lost race can do is
/// leak one slot — which `fast_ipc_release_all` reclaims when the task dies.
/// The scan runs *before* the lock is taken so the hot critical section stays
/// as short as it was and no lock-ordering question arises at all.
pub fn fast_ipc_call(
    caller_tid: u32,
    server_tid: u32,
    words: [u64; FAST_IPC_MAX_WORDS],
) -> Option<u64> {
    // Self-call is an immediate self-deadlock: the caller blocks waiting for a
    // reply only it could send, and the slot is leaked until it is killed.
    // O(1) to reject, so reject.
    if caller_tid == server_tid {
        return None;
    }
    // Also rejects the `FAST_IPC_SLOT_FREE` sentinel, which is not a live TID.
    if !sched_seam::tid_exists(server_tid) {
        return None;
    }
    let mut state = FAST_IPC.lock();
    let idx = state.alloc_slot(caller_tid, server_tid, words)?;
    // The CLIENT's handle for this exchange — same encoding, and (because the
    // generation only advances on free) the same VALUE the server's accept
    // will mint. The client blocks on `WaitReason::FastIpcClient(handle)`, so
    // a wake for exchange N can never be confused with exchange N+1 in the
    // same seat: this is the client-side half of the slot-ABA closure (the
    // server-side half is `fast_ipc_reply`'s generation check).
    let generation = state.slots.get(idx)?.generation;
    Some(fast_ipc_make_handle(idx, generation))
}

/// Called when a server issues SYS_IPC_FAST_ACCEPT.
///
/// Returns `Some((handle, caller_tid, words))` if a pending call exists for
/// this server; `None` if no call is pending (server should block).
///
/// **`handle`, not `slot_idx`.** It names *this* exchange, not the seat the
/// exchange happens to be sitting in — see the handle-encoding note above for
/// the ABA that a bare index cannot survive. The server hands it straight back
/// to [`fast_ipc_reply`]; it is opaque, and the only supported thing to do
/// with it besides replying is [`fast_ipc_handle_slot`] for logging.
pub fn fast_ipc_accept(server_tid: u32) -> Option<(u64, u32, [u64; FAST_IPC_MAX_WORDS])> {
    let mut state = FAST_IPC.lock();
    let idx = state.find_pending_for_server(server_tid)?;
    let slot = state.slots.get_mut(idx)?;
    let caller = slot.caller_tid;
    let words = slot.words;
    let handle = fast_ipc_make_handle(idx, slot.generation);
    // Keep the slot alive — the server still owes a reply on it. `Accepted`,
    // not `Replied`: `words` still holds the request at this point, and
    // `Replied` is the state `find_reply_for_caller` hands to the client.
    // See `SlotState` for the confusion this separation prevents.
    slot.state = SlotState::Accepted;
    Some((handle, caller, words))
}

/// Outcome of [`fast_ipc_reply`].
///
/// **`Stale` and `Refused` are NOT a partition of "rejected".** Read them as:
/// `Stale` is *sufficient* proof that the exchange the handle named is over
/// and will never be answerable — the server can drop its bookkeeping for it
/// with certainty. `Refused` proves nothing beyond "not now": a handle whose
/// slot has already been re-let to a *different* server, or re-let and not yet
/// accepted, fails an earlier check and reports `Refused` even though it is
/// just as dead. Anyone who inverts this — treating `Refused` as "handle still
/// good, retry later" — is reading a guarantee that was never made.
///
/// **Why the check order is load-bearing.** `Stale` is only reachable *after*
/// the state and ownership checks have passed, i.e. only by the slot's current
/// legitimate server (or a privileged task). Reordered so that generation is
/// tested first, this variant would become a generation oracle: any ring-3
/// task could sweep 64 indices, binary-search the generation of each, and read
/// off how often every slot on the board has turned over — activity it has no
/// part in. That is the same hole `fast_ipc_wait_state` documents and refuses
/// to open, and the reasoning there applies verbatim here.
#[derive(Clone, Copy, PartialEq, Eq)]
// `Debug` only off-board, same reasoning as `SlotState`.
#[cfg_attr(not(target_os = "none"), derive(Debug))]
pub enum FastIpcReply {
    /// Reply deposited. Wake `caller_tid`, which is blocked on
    /// `WaitReason::FastIpcClient(slot_idx)`.
    ///
    /// `slot_idx` is returned rather than left for the caller to decode out of
    /// the handle on purpose: the client-side wait reason is keyed on the raw
    /// index, and dispatch re-deriving it would be a second implementation of
    /// this file's encoding, free to drift from it.
    Woke { caller_tid: u32, slot_idx: usize },
    /// The handle is well-formed and its slot is currently this server's, but
    /// the generation is from an earlier tenancy: the exchange it named ended
    /// (the client died, or it was already completed and collected) and the
    /// slot has since been re-let. **Nothing was written.** This is the ABA the
    /// generation tag exists to catch; before it, this reply would have been
    /// delivered to whoever is in the slot now.
    Stale,
    /// Rejected for any other reason: the handle is not one this file could
    /// have issued (sign bit set, or an index outside the table), the slot is
    /// free, the slot is not `Accepted` (never accepted, or already answered),
    /// or the replier is not the slot's server.
    Refused,
}

/// Called when a server issues SYS_IPC_FAST_REPLY.
///
/// Deposits the reply into the slot and transitions state so the caller can
/// collect it. See [`FastIpcReply`] for the three outcomes and for why
/// `Refused` must not be read as "try again".
///
/// `handle` is the value [`fast_ipc_accept`] returned, taken raw from `a0`.
/// Every one of the 2^64 bit patterns is a legal *input*: decode is a compare
/// and two masks, and anything that does not name a live tenancy is `Refused`
/// or `Stale`, never a panic. With `panic = "abort"` a reachable panic here is
/// a board reset, i.e. a physical-safety event on a robot.
///
/// `replier_tid` is the TID of the task actually making the syscall and
/// `privileged` is true for kernel tasks (`current_user_pt() == 0`), which
/// skip the ownership check — the house convention, same as `cap_store`'s
/// typed callers and `port_access_ok`.
///
/// **WHY the ownership check exists (IPC-1).** The handle arrives raw in `a0`
/// and its index field is only 0..63, so walking it is 64 syscalls. Before this
/// check `server_tid` was written by `alloc_slot` and never read for
/// authorisation: any ring-3 task could reply on a slot it never accepted,
/// impersonating an arbitrary IPC server and waking that server's client with
/// data of the attacker's choosing — the client has no way to tell. The check
/// runs *inside* the lock that was already being taken, so there is no TOCTOU
/// window against a concurrent `accept`/`release` and it costs no extra lock.
///
/// The `Accepted` requirement is the second half of the same fix: replying to
/// a call you never accepted is meaningless, and it also blocks a "reply
/// twice" that would overwrite a reply already deposited but not yet
/// collected.
///
/// **WHY the generation check exists (slot ABA).** `server_tid` proves *who*
/// may answer; it cannot prove *what* is being answered. Server S accepts
/// client A's slot, A dies, `fast_ipc_release_all` frees the slot, client B
/// calls S and lands on the same index, S accepts. A reply from S carrying the
/// index it still held for A passed both checks above — the slot is `Accepted`
/// and S is its server — and B collected the answer meant for A. One extra
/// comparison against a counter S never sees closes it.
///
/// **`privileged` skips the ownership check and NEVER the generation check.**
/// The two are different questions. Ownership is authorisation, and the house
/// convention is that kernel tasks are trusted with it. The generation is not
/// authorisation at all, it is the identity of the exchange: a kernel server
/// holding a handle to an exchange that ended is just as wrong about reality
/// as a user one, and letting it write would corrupt whichever client is in
/// the slot now. "Privileged bypasses the checks" is precisely the sentence a
/// later reader will over-apply — it bypasses one named check, not the concept.
pub fn fast_ipc_reply(
    handle: u64,
    replier_tid: u32,
    privileged: bool,
    words: [u64; FAST_IPC_MAX_WORDS],
) -> FastIpcReply {
    let slot_idx = match fast_ipc_handle_slot(handle) {
        Some(i) => i,
        None => return FastIpcReply::Refused,
    };
    let mut state = FAST_IPC.lock();
    // `get_mut` and not `[slot_idx]`: the decode above already bounds the
    // index, but the no-panic property stays local to the access rather than
    // depending on a check made somewhere else that someone may later move.
    let slot = match state.slots.get_mut(slot_idx) {
        Some(s) => s,
        None => return FastIpcReply::Refused,
    };
    if slot.state != SlotState::Accepted {
        return FastIpcReply::Refused;
    }
    if !privileged && slot.server_tid != replier_tid {
        return FastIpcReply::Refused;
    }
    // Last, and that ordering is part of the design — see `FastIpcReply`.
    if slot.generation != handle_generation(handle) {
        return FastIpcReply::Stale;
    }
    let caller = slot.caller_tid;
    slot.words = words;
    slot.state = SlotState::Replied;
    FastIpcReply::Woke { caller_tid: caller, slot_idx }
}

/// Called after the caller wakes from blocking.
///
/// Returns the reply words and frees the slot.
pub fn fast_ipc_collect(caller_tid: u32) -> Option<[u64; FAST_IPC_MAX_WORDS]> {
    let mut state = FAST_IPC.lock();
    let idx = state.find_reply_for_caller(caller_tid)?;
    let words = state.slots.get(idx)?.words;
    state.free_slot(idx);
    Some(words)
}

/// Answer to "should I block again, or give up?" — see [`fast_ipc_wait_state`].
#[derive(Clone, Copy, PartialEq, Eq)]
// `Debug` only off-board, same reasoning as `SlotState`.
#[cfg_attr(not(target_os = "none"), derive(Debug))]
pub enum FastIpcWait {
    /// The slot is not this caller's any more: free, or re-allocated to a
    /// different client. Blocking again would sleep forever — give up and
    /// return the "-1, fall back to channel IPC" answer.
    Gone,
    /// The slot is still this caller's and the server has not replied yet
    /// (`Pending` or `Accepted`). A wake seen in this state is spurious;
    /// blocking again is correct and will not be lost.
    Waiting,
    /// The reply is deposited. `fast_ipc_collect` will succeed.
    Ready,
}

/// Classify a blocked client's slot: O(1), one lock acquisition, no scan.
///
/// **WHY this exists.** The scheduler's `wake_pending` seal closes the lost
/// wakeup at the cost of admitting spurious ones, so a client can return from
/// `task_block` with no reply waiting. `fast_ipc_collect` alone cannot tell the
/// two survivable outcomes apart — it returns `None` both when the wake was
/// spurious (slot alive, must block again) and when the server died and
/// `fast_ipc_release_all` reclaimed the slot (blocking again sleeps forever).
/// Guessing either way is worse than the false `-1` the retry loop is meant to
/// remove: guess `Waiting` on a dead slot and ring 3 hangs permanently; guess
/// `Gone` on a live one and a perfectly good exchange is thrown away.
///
/// **WHY it takes `caller_tid` and not just the index (IPC-1, again).** An
/// index-only probe would be a state oracle over the whole 64-slot table:
/// walking 0..63 would leak which servers have work in flight and when a reply
/// lands, for slots the prober has no part in. A slot that is alive but owned
/// by somebody else reads as `Gone` — indistinguishable, from the prober's
/// side, from a slot that does not exist. That is the same shape of check
/// `fast_ipc_reply` makes, and it must not be relaxed to "index is in range":
/// doing so reopens IPC-1 through the back door.
///
/// **Interaction with the ABA hazard.** The *server* side of it is closed: the
/// handle `fast_ipc_accept` issues carries a generation and `fast_ipc_reply`
/// rejects a stale one. The **client** side is tagged too since 2026-08-23:
/// `fast_ipc_call` returns the generation-tagged handle, the client blocks on
/// `WaitReason::FastIpcClient(handle)` (now a `u64` in `sched`), and this
/// accessor takes the handle and verifies the generation — so a seat re-let
/// since the client's exchange answers `Gone` even in the one case the old
/// containment could not see through (a slot re-allocated to a *recycled*
/// TID equal to the caller's). The `caller_tid` check stays as the cheaper
/// first filter and as defence in depth.
///
/// Note the asymmetry with `fast_ipc_collect`, which matches by `caller_tid`
/// across the table rather than by index: a client has at most one outstanding
/// fast call (it is blocked for the whole exchange), so the two agree.
pub fn fast_ipc_wait_state(handle: u64, caller_tid: u32) -> FastIpcWait {
    // Decode rejects bit 63 and out-of-range indices — `Gone`, never a panic
    // (`panic = "abort"` makes a bad value from ring 3 a board reset).
    let slot_idx = match fast_ipc_handle_slot(handle) {
        Some(i) => i,
        None => return FastIpcWait::Gone,
    };
    let state = FAST_IPC.lock();
    let slot = match state.slots.get(slot_idx) {
        Some(s) => s,
        None => return FastIpcWait::Gone,
    };
    // Ownership before state: a slot belonging to anyone else must be
    // indistinguishable from an empty one. `Free` slots carry
    // `FAST_IPC_SLOT_FREE` in `caller_tid`, so the state test is what stops a
    // caller whose TID somehow equals the sentinel from reading free slots.
    if slot.state == SlotState::Free || slot.caller_tid != caller_tid {
        return FastIpcWait::Gone;
    }
    // Generation LAST, after ownership — same ordering rule as
    // `fast_ipc_reply`, and here it is not even observable (wrong generation
    // and wrong owner both answer `Gone`), so no oracle either way. A stale
    // generation means the seat was re-let since this client's exchange: the
    // slot in front of us is someone else's, and `Gone` is the answer that
    // sends the retry loop to its clean -1 exit instead of back to sleep on
    // another exchange's future.
    if slot.generation != handle_generation(handle) {
        return FastIpcWait::Gone;
    }
    match slot.state {
        SlotState::Replied => FastIpcWait::Ready,
        // Pending and Accepted are both "server still owes an answer". Free is
        // unreachable here, and mapping it to Gone is the conservative answer.
        SlotState::Pending | SlotState::Accepted => FastIpcWait::Waiting,
        SlotState::Free => FastIpcWait::Gone,
    }
}

/// Release every fast IPC slot in which `tid` is either endpoint — called from
/// the task-exit hook.
///
/// **WHY the exit hook must do this (IPC-3).** Nothing used to reclaim these
/// slots. There are 64 of them and they are global, not per-task, so any task
/// that dies mid-exchange burns one permanently. Once all 64 are gone
/// `fast_ipc_call` returns `None` forever, dispatch answers `-1`, and `-1`'s
/// documented meaning is "fall back to channel IPC" — so the fast path this
/// kernel exists to optimise dies silently, everything still *works*, and no
/// test anywhere goes red. That silence is the defect, more than the leak.
///
/// **Dying client.** Just free the slot. A `Pending` slot removed before the
/// server accepted it merely withdraws work; a server woken for a call that is
/// no longer there gets `None` from `fast_ipc_accept` and returns `-1`.
///
/// **Dying server that already replied** (`Replied`): the slot is left alone.
/// It belongs to the client at that point — see the inline note below for the
/// completed-exchange regression that skipping it prevents.
///
/// **Dying server with a client still blocked** (`Pending` or `Accepted`) is
/// the case that needs a decision: the client is asleep on
/// `WaitReason::FastIpcClient(idx)` waiting for a reply that can never come, so
/// freeing the slot and walking away leaves it asleep for the life of the
/// board. We free the slot and then wake the client by slot index.
///
/// Free-then-wake, with no synthetic error payload, is deliberate. The client
/// resumes inside the `SYS_IPC_FAST_CALL` arm, calls `fast_ipc_collect`, finds
/// no `Replied` slot for its TID, and dispatch returns `-1` — exactly the
/// "no fast path available, use a channel" answer the ABI already documents.
/// Manufacturing a `Replied` slot with an error sentinel would instead invent
/// a second convention *and* hand the client a word pattern it could mistake
/// for data. There is nothing to mistake here.
///
/// The waking is done **after** the lock is dropped. Not for deadlock reasons
/// — `try_wake_task` takes run-queue locks and never `FAST_IPC`, so there is no
/// cycle — but because holding the hottest lock in the IPC path across a
/// run-queue lock would put scheduler contention on every fast IPC operation.
///
/// The old residual here — the wake landing on a *different* client that
/// re-allocated the index between the free and the wake — is CLOSED by the
/// client-side handle (2026-08-23): the orphan wake below carries the
/// pre-free generation, the sleeping client is blocked on exactly that
/// handle, and a new tenant of the seat is blocked on a different one, so
/// the wake matches the dead exchange's owner or nobody. No spurious -1 for
/// bystanders anymore.
///
/// What *has* changed: the free below bumps the slot's generation, so any
/// handle the dying server was still holding for this slot is retired here and
/// `fast_ipc_reply` will answer `FastIpcReply::Stale` for it rather than
/// writing into whatever exchange takes the slot next.
///
/// Returns the number of slots freed (diagnostic; callers may ignore it).
pub fn fast_ipc_release_all(tid: u32) -> usize {
    // Handles (generation-tagged) whose blocked client must be woken.
    // Fixed-size, on the stack, bounded by the table itself — no heap in
    // this kernel.
    let mut orphaned = [0u64; FAST_IPC_MAX_SLOTS];
    let mut orphan_n = 0usize;
    let mut freed = 0usize;

    {
        let mut state = FAST_IPC.lock();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            let (is_caller, is_server, live) = match state.slots.get(idx) {
                Some(s) => (
                    s.caller_tid == tid,
                    s.server_tid == tid,
                    s.state != SlotState::Free,
                ),
                None => continue,
            };
            if !live || !(is_caller || is_server) {
                continue;
            }
            if is_server && !is_caller {
                // A slot already in `Replied` is the *client's* property: the
                // answer is deposited, the client has been woken, and the
                // dying server owes nothing more. Reclaiming it here would
                // turn a completed exchange into a spurious -1 every time a
                // server replies and then exits — the ordinary one-shot
                // service pattern, and deterministic on a single hart because
                // the woken client cannot run until the server yields. The
                // client's own `fast_ipc_collect` frees it; if the client dies
                // first, `fast_ipc_release_all(caller)` does.
                if state.slots.get(idx).map(|s| s.state) == Some(SlotState::Replied) {
                    continue;
                }
                // Otherwise the client is blocked on an answer that will never
                // come, so it must be woken after the slot is freed. Capture
                // the handle with the PRE-free generation — `free_slot` bumps
                // it, and the sleeping client is blocked on the handle of the
                // exchange that just died, not on whatever the seat becomes
                // next.
                if let Some(o) = orphaned.get_mut(orphan_n) {
                    let gen = state.slots.get(idx).map(|s| s.generation).unwrap_or(0);
                    *o = fast_ipc_make_handle(idx, gen);
                    orphan_n += 1;
                }
            }
            state.free_slot(idx);
            freed += 1;
        }
    } // lock dropped before touching the scheduler — see doc above.

    for i in 0..orphan_n {
        if let Some(&handle) = orphaned.get(i) {
            sched_seam::wake_client(handle);
        }
    }

    freed
}

/// Census of the slot table by state, for diagnosing a wedged exchange.
///
/// Returns `(pending, accepted, replied)`.
///
/// **WHY this exists.** When a fast-IPC exchange wedges, the client is blocked
/// on `FastIpcClient(slot)` and the server on `FastIpcServer(tid)` — and those
/// two states look identical in a log whether the slot is `Pending` (the server
/// lost the wake) or `Accepted` (the server took the call and never answered).
/// They are different bugs with different fixes, and nothing else in the tree
/// tells them apart. `ipc-trace` cannot: it is six UART writes per exchange, so
/// it perturbs the timing enough to hide the race entirely — measured, the
/// traced build passes 8/8 where the untraced one wedges.
///
/// Three counters read under one lock, called at most every few seconds from a
/// diagnostic task, is cheap enough not to move the race.
pub fn fast_ipc_census() -> (u32, u32, u32, u32) {
    let state = FAST_IPC.lock();
    let mut pending = 0u32;
    let mut accepted = 0u32;
    let mut replied = 0u32;
    for s in state.slots.iter() {
        match s.state {
            SlotState::Pending  => pending  += 1,
            SlotState::Accepted => accepted += 1,
            SlotState::Replied  => replied  += 1,
            SlotState::Free     => {}
        }
    }
    // `used` is reported alongside the real counts on purpose. Both lookup
    // helpers early-exit on `used == 0`, so a `used` that has drifted below the
    // true occupancy makes live slots **invisible**: `fast_ipc_collect` would
    // answer `None` for a reply that is sitting right there, and the client
    // would go back to sleep on it. `used != pending + accepted + replied` is
    // therefore not a cosmetic discrepancy, it is that exact bug.
    (pending, accepted, replied, state.used)
}

/// Identify every non-free slot: `(idx, state_code, caller_tid, server_tid)`,
/// written into `out`, returning how many were filled.
///
/// State codes: 1 = Pending, 2 = Accepted, 3 = Replied.
///
/// **WHY identities and not just counts.** A census that says "one reply is
/// waiting and one client is asleep" is consistent with two opposite stories:
/// the sleeping client is the one the reply belongs to (a lost wake), or it is
/// a *different* client and the reply belongs to someone who already moved on.
/// Only the identities separate them, and every counter so far has said the
/// wake path is clean — so the premise that they match is the one left to test.
pub fn fast_ipc_slot_ids(out: &mut [(u8, u8, u32, u32)]) -> usize {
    let state = FAST_IPC.lock();
    let mut n = 0usize;
    for (i, s) in state.slots.iter().enumerate() {
        if n >= out.len() { break; }
        let code = match s.state {
            SlotState::Free     => continue,
            SlotState::Pending  => 1u8,
            SlotState::Accepted => 2u8,
            SlotState::Replied  => 3u8,
        };
        out[n] = (i as u8, code, s.caller_tid, s.server_tid);
        n += 1;
    }
    n
}

/// Count currently active fast IPC slots (diagnostic).
pub fn fast_ipc_active() -> u32 {
    FAST_IPC.lock().used
}

// ===========================================================================
// Host-test scaffolding — off-board only, never in the kernel binary.
// ===========================================================================

/// Host substitutes for the RV64-only crates this module names.
///
/// Compiled **only** off-board (`not(target_os = "none")`), so nothing here can
/// reach a board. See the seam note near the top of the file for why the
/// substitution is needed at all.
///
/// `allow(dead_code)`: the inspection helpers are used by `mod tests`, which is
/// `cfg(test)`, so the plain host lib build sees them unused. This allow must
/// stay scoped to this module — the kernel build has no `allow` anywhere near
/// it and warnings are failures there.
#[cfg(not(target_os = "none"))]
#[allow(dead_code)]
mod host_seam {
    use core::cell::UnsafeCell;
    use core::ops::{Deref, DerefMut};
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Stand-in for `robot_os_sync::SpinLock` with the same surface used here
    /// (`new`, `lock`, deref). A real spin lock rather than a no-op: `cargo
    /// test` runs test functions on parallel threads against the same
    /// `static FAST_IPC`, so the lock has to actually work.
    pub struct SpinLock<T> {
        locked: AtomicBool,
        data: UnsafeCell<T>,
    }
    unsafe impl<T: Send> Sync for SpinLock<T> {}
    unsafe impl<T: Send> Send for SpinLock<T> {}

    impl<T> SpinLock<T> {
        pub const fn new(v: T) -> Self {
            SpinLock { locked: AtomicBool::new(false), data: UnsafeCell::new(v) }
        }
        pub fn lock(&self) -> SpinGuard<'_, T> {
            while self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            SpinGuard { lock: self }
        }
    }

    pub struct SpinGuard<'a, T> {
        lock: &'a SpinLock<T>,
    }
    impl<T> Deref for SpinGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.lock.data.get() }
        }
    }
    impl<T> DerefMut for SpinGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.lock.data.get() }
        }
    }
    impl<T> Drop for SpinGuard<'_, T> {
        fn drop(&mut self) {
            self.lock.locked.store(false, Ordering::Release);
        }
    }

    // ── Fake task table + wake log ─────────────────────────────────────────
    //
    // `LIVE_TIDS` is a bitmap of "TIDs that exist" so IPC-5 can be exercised
    // both ways. `WAKE_LOG` is a bitmap of slot indices `wake_client` was
    // called on — that is what lets a test assert the *actuation* (the client
    // was really woken) and not just the decision (the slot was freed).
    pub const HOST_MAX_TID: u32 = 128;
    static LIVE_TIDS: [AtomicBool; HOST_MAX_TID as usize] =
        [const { AtomicBool::new(false) }; HOST_MAX_TID as usize];
    static WAKE_LOG: [AtomicBool; super::FAST_IPC_MAX_SLOTS] =
        [const { AtomicBool::new(false) }; super::FAST_IPC_MAX_SLOTS];
    static WAKE_HANDLES: [core::sync::atomic::AtomicU64; super::FAST_IPC_MAX_SLOTS] =
        [const { core::sync::atomic::AtomicU64::new(0) }; super::FAST_IPC_MAX_SLOTS];
    static WAKE_COUNT: AtomicU32 = AtomicU32::new(0);

    pub fn set_tid_live(tid: u32, live: bool) {
        if let Some(c) = LIVE_TIDS.get(tid as usize) {
            c.store(live, Ordering::SeqCst);
        }
    }
    /// Backs `sched_seam::tid_exists` under `cfg(test)`.
    pub fn live(tid: u32) -> bool {
        LIVE_TIDS.get(tid as usize).map(|c| c.load(Ordering::SeqCst)).unwrap_or(false)
    }
    /// Backs `sched_seam::wake_client` under `cfg(test)`. Takes the
    /// generation-tagged handle the kernel-side seam now carries; the log
    /// stays indexed by slot so existing assertions keep reading naturally,
    /// and the full handle is kept alongside so a test can assert the orphan
    /// wake was minted with the PRE-free generation.
    pub fn record_wake(handle: u64) {
        let slot_idx = (handle & super::FAST_IPC_SLOT_MASK) as usize;
        if let Some(c) = WAKE_LOG.get(slot_idx) {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(h) = WAKE_HANDLES.get(slot_idx) {
            h.store(handle, Ordering::SeqCst);
        }
        WAKE_COUNT.fetch_add(1, Ordering::SeqCst);
    }
    /// Last handle `wake_client` was called with for a given slot.
    pub fn woken_handle(slot_idx: usize) -> Option<u64> {
        WAKE_HANDLES.get(slot_idx).map(|h| h.load(Ordering::SeqCst))
    }
    pub fn clear_all_tids() {
        for c in LIVE_TIDS.iter() {
            c.store(false, Ordering::SeqCst);
        }
    }
    pub fn clear_wake_log() {
        for c in WAKE_LOG.iter() {
            c.store(false, Ordering::SeqCst);
        }
        WAKE_COUNT.store(0, Ordering::SeqCst);
    }
    pub fn was_woken(slot_idx: usize) -> bool {
        WAKE_LOG.get(slot_idx).map(|c| c.load(Ordering::SeqCst)).unwrap_or(false)
    }
    pub fn wake_count() -> u32 {
        WAKE_COUNT.load(Ordering::SeqCst)
    }
}

#[cfg(not(target_os = "none"))]
mod sched_seam {
    pub fn tid_exists(tid: u32) -> bool {
        if tid >= super::host_seam::HOST_MAX_TID {
            return false;
        }
        // Mirrors the real `idx_for_tid`: unknown TID ⇒ false.
        super::host_seam::live(tid)
    }
    pub fn wake_client(handle: u64) {
        super::host_seam::record_wake(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    const SERVER: u32 = 7;
    const OTHER_SERVER: u32 = 8;
    const CLIENT: u32 = 21;
    const IMPOSTOR: u32 = 99;
    const REQ: [u64; FAST_IPC_MAX_WORDS] = [0xAAAA, 0xBBBB, 0xCCCC, 0xDDDD];
    const RSP: [u64; FAST_IPC_MAX_WORDS] = [0x1111, 0x2222, 0x3333, 0x4444];

    /// `FAST_IPC` is one global table and `cargo test` runs tests on parallel
    /// threads, so every test must own it exclusively and start from a known
    /// state. Holding this guard for the body of the test is what makes the
    /// slot-exhaustion test (IPC-3) meaningful at all.
    static SERIAL: Mutex<()> = Mutex::new(());

    struct Env {
        _g: MutexGuard<'static, ()>,
    }

    fn env() -> Env {
        // `into_inner` on poisoning: one failing test must not cascade into
        // every other test reporting a poisoned lock instead of its own result.
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut st = FAST_IPC.lock();
            for s in st.slots.iter_mut() {
                *s = FastIpcSlot::empty();
            }
            st.used = 0;
        }
        host_seam::clear_all_tids();
        host_seam::clear_wake_log();
        // Default population for the common case.
        host_seam::set_tid_live(SERVER, true);
        host_seam::set_tid_live(OTHER_SERVER, true);
        host_seam::set_tid_live(CLIENT, true);
        host_seam::set_tid_live(IMPOSTOR, true);
        Env { _g: g }
    }

    fn slot_state(idx: usize) -> Option<SlotState> {
        FAST_IPC.lock().slots.get(idx).map(|s| s.state)
    }

    fn slot_generation(idx: usize) -> u64 {
        FAST_IPC.lock().slots.get(idx).map(|s| s.generation).unwrap_or(0)
    }

    /// Force a slot's generation. Only a test can do this: reaching the wrap
    /// honestly needs 2^57 reuses, so the alternative to poking the counter is
    /// leaving the wrap untested and taking its behaviour on faith.
    fn set_slot_generation(idx: usize, generation: u64) {
        if let Some(s) = FAST_IPC.lock().slots.get_mut(idx) {
            s.generation = generation & FAST_IPC_GEN_MASK;
        }
    }

    /// The handle ACCEPT would return for `idx` right now. Also the handle
    /// the CLIENT of a live exchange on `idx` holds — the two are the same
    /// value while the exchange lives (the generation only advances on free).
    fn handle_of(idx: usize) -> u64 {
        fast_ipc_make_handle(idx, slot_generation(idx))
    }

    /// [`fast_ipc_call`] reduced to its pre-handle shape — the SEAT index —
    /// because most of these tests reason about slots (`slot_state`,
    /// `reply_now`). Tests that care about the client handle itself call
    /// `fast_ipc_call` directly or reconstruct it with [`handle_of`].
    fn call_idx(
        caller: u32,
        server: u32,
        w: [u64; FAST_IPC_MAX_WORDS],
    ) -> Option<usize> {
        fast_ipc_call(caller, server, w).and_then(fast_ipc_handle_slot)
    }

    /// [`fast_ipc_reply`] reduced to its pre-generation shape — handle in,
    /// `Option<caller_tid>` out — so the tests that predate handles keep
    /// asserting exactly what they always asserted. New tests that care which
    /// *kind* of rejection happened call `fast_ipc_reply` directly.
    fn reply_h(
        handle: u64,
        tid: u32,
        privileged: bool,
        w: [u64; FAST_IPC_MAX_WORDS],
    ) -> Option<u32> {
        match fast_ipc_reply(handle, tid, privileged, w) {
            FastIpcReply::Woke { caller_tid, .. } => Some(caller_tid),
            FastIpcReply::Stale | FastIpcReply::Refused => None,
        }
    }

    /// Same, addressed by slot index at that slot's current generation.
    fn reply_now(
        idx: usize,
        tid: u32,
        privileged: bool,
        w: [u64; FAST_IPC_MAX_WORDS],
    ) -> Option<u32> {
        reply_h(handle_of(idx), tid, privileged, w)
    }

    // ── IPC-5: target validation ───────────────────────────────────────────

    #[test]
    fn call_to_live_server_is_accepted() {
        let _e = env();
        assert!(fast_ipc_call(CLIENT, SERVER, REQ).is_some());
        assert_eq!(fast_ipc_active(), 1);
    }

    #[test]
    fn call_to_nonexistent_tid_is_rejected_and_leaks_no_slot() {
        let _e = env();
        host_seam::set_tid_live(55, false);
        assert!(fast_ipc_call(CLIENT, 55, REQ).is_none());
        // The whole point of IPC-5: the failed call must not burn a slot.
        assert_eq!(fast_ipc_active(), 0);
    }

    #[test]
    fn call_to_free_sentinel_tid_is_rejected() {
        let _e = env();
        assert!(fast_ipc_call(CLIENT, FAST_IPC_SLOT_FREE, REQ).is_none());
        assert_eq!(fast_ipc_active(), 0);
    }

    #[test]
    fn self_call_is_rejected() {
        let _e = env();
        assert!(fast_ipc_call(SERVER, SERVER, REQ).is_none());
        assert_eq!(fast_ipc_active(), 0);
    }

    #[test]
    fn sixty_four_calls_to_a_dead_tid_do_not_exhaust_the_table() {
        // The exact ring-3 denial-of-service IPC-5 closes.
        let _e = env();
        host_seam::set_tid_live(55, false);
        for _ in 0..FAST_IPC_MAX_SLOTS {
            assert!(fast_ipc_call(CLIENT, 55, REQ).is_none());
        }
        assert_eq!(fast_ipc_active(), 0);
        assert!(fast_ipc_call(CLIENT, SERVER, REQ).is_some());
    }

    // ── Happy path ─────────────────────────────────────────────────────────

    #[test]
    fn full_exchange_delivers_reply_and_frees_slot() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let (handle, caller, words) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(fast_ipc_handle_slot(handle), Some(idx));
        assert_eq!(caller, CLIENT);
        assert_eq!(words, REQ);
        assert_eq!(reply_now(idx, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP));
        assert_eq!(fast_ipc_active(), 0);
        assert_eq!(slot_state(idx), Some(SlotState::Free));
    }

    #[test]
    fn accept_with_nothing_pending_returns_none() {
        let _e = env();
        assert!(fast_ipc_accept(SERVER).is_none());
        // And a call aimed elsewhere must not be visible to this server.
        let _ = call_idx(CLIENT, OTHER_SERVER, REQ).expect("slot");
        assert!(fast_ipc_accept(SERVER).is_none());
    }

    // ── IPC-2: Accepted is not Replied ─────────────────────────────────────

    #[test]
    fn collect_between_accept_and_reply_returns_none_not_the_request() {
        // The heart of IPC-2. Before the fix this returned Some(REQ) and the
        // client took its own request for the server's answer.
        let _e = env();
        let _idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(fast_ipc_collect(CLIENT), None);
    }

    #[test]
    fn accept_moves_slot_to_accepted_not_replied() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        assert_eq!(slot_state(idx), Some(SlotState::Pending));
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(slot_state(idx), Some(SlotState::Accepted));
        assert_eq!(reply_now(idx, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(slot_state(idx), Some(SlotState::Replied));
    }

    #[test]
    fn collect_before_accept_returns_none() {
        let _e = env();
        let _idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        assert_eq!(fast_ipc_collect(CLIENT), None);
    }

    #[test]
    fn second_accept_cannot_steal_an_accepted_slot() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        assert!(fast_ipc_accept(SERVER).is_some());
        assert!(fast_ipc_accept(SERVER).is_none());
        assert_eq!(slot_state(idx), Some(SlotState::Accepted));
    }

    // ── IPC-1: only the real server may reply ──────────────────────────────

    /// Fill every slot with a call to `SERVER` and accept them all, so the
    /// impostor test below sweeps the whole 0..63 handle space rather than
    /// proving one lucky index.
    fn fill_and_accept_all() {
        for i in 0..FAST_IPC_MAX_SLOTS {
            let caller = 1000 + i as u32;
            assert!(fast_ipc_call(caller, SERVER, REQ).is_some(), "alloc {i}");
        }
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert!(fast_ipc_accept(SERVER).is_some(), "accept {i}");
        }
    }

    #[test]
    fn impostor_is_rejected_on_every_slot_index() {
        let _e = env();
        fill_and_accept_all();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(
                reply_now(idx, IMPOSTOR, false, RSP),
                None,
                "impostor accepted on slot {idx}"
            );
            // Rejection must not have mutated the slot.
            assert_eq!(slot_state(idx), Some(SlotState::Accepted), "slot {idx} mutated");
        }
        // And no client can collect anything the impostor tried to plant.
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(fast_ipc_collect(1000 + i as u32), None);
        }
    }

    #[test]
    fn real_server_is_accepted_on_every_slot_index() {
        // The other half of the gate: the check must not reject the legitimate
        // server anywhere in the handle space.
        let _e = env();
        fill_and_accept_all();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            let caller = 1000 + idx as u32;
            assert_eq!(
                reply_now(idx, SERVER, false, RSP),
                Some(caller),
                "real server rejected on slot {idx}"
            );
            assert_eq!(fast_ipc_collect(caller), Some(RSP));
        }
        assert_eq!(fast_ipc_active(), 0);
    }

    #[test]
    fn privileged_replier_bypasses_the_ownership_check() {
        // House convention: kernel tasks (current_user_pt() == 0) skip
        // authorisation, same as cap_store's typed callers / port_access_ok.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(reply_now(idx, IMPOSTOR, true, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP));
    }

    #[test]
    fn reply_to_a_slot_that_was_never_accepted_is_rejected() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        // Pending, not Accepted — even the real server may not reply yet.
        assert_eq!(reply_now(idx, SERVER, false, RSP), None);
        assert_eq!(reply_now(idx, SERVER, true, RSP), None);
        assert_eq!(slot_state(idx), Some(SlotState::Pending));
        assert_eq!(fast_ipc_collect(CLIENT), None);
    }

    #[test]
    fn reply_to_a_free_slot_is_rejected() {
        let _e = env();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(reply_now(idx, SERVER, false, RSP), None);
            assert_eq!(reply_now(idx, SERVER, true, RSP), None);
        }
        assert_eq!(fast_ipc_active(), 0);
    }

    #[test]
    fn replying_twice_cannot_overwrite_an_uncollected_reply() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(reply_now(idx, SERVER, false, RSP), Some(CLIENT));
        let second = [0xDEADu64, 0, 0, 0];
        assert_eq!(reply_now(idx, SERVER, false, second), None);
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP));
    }

    // ── Slot ABA: the generation tag on the server handle ──────────────────

    #[test]
    fn handle_encoding_round_trips_over_the_whole_table() {
        // The layout is arithmetic on a value ring 3 supplies, so the two
        // fields must be recoverable for every index and at both ends of the
        // generation range — an aliasing bug here would silently make two
        // different exchanges share a handle.
        let _e = env();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            for g in [0u64, 1, 2, 0x5555_5555, FAST_IPC_GEN_MASK - 1, FAST_IPC_GEN_MASK] {
                let h = fast_ipc_make_handle(idx, g);
                assert_eq!(fast_ipc_handle_slot(h), Some(idx), "idx {idx} gen {g}");
                assert_eq!(handle_generation(h), g, "idx {idx} gen {g}");
                // Bit 63 must stay clear: the handle travels in `a0` as an
                // `i64` whose negative half means "error".
                assert!((h as i64) >= 0, "idx {idx} gen {g} handle is negative");
            }
        }
    }

    #[test]
    fn accept_hands_back_the_slots_current_generation() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let (h, _, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(h, fast_ipc_make_handle(idx, slot_generation(idx)));
        // And that handle is the one that works.
        assert_eq!(reply_h(h, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP));
    }

    #[test]
    fn freeing_a_slot_advances_its_generation_by_exactly_one() {
        let _e = env();
        for round in 0..5u64 {
            let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
            assert_eq!(idx, 0, "test needs the same slot every round");
            assert_eq!(slot_generation(0), round, "round {round}");
            assert_eq!(fast_ipc_release_all(CLIENT), 1);
            assert_eq!(slot_generation(0), round + 1, "round {round}");
        }
    }

    #[test]
    fn a_stale_handle_is_refused_on_a_reassigned_slot() {
        // **This is the defect, turned into a guard.** Server accepts A's
        // slot; A dies and the slot is reclaimed; B calls the same server and
        // lands on the same index; the server accepts. A reply carrying the
        // handle the server was still holding for A used to pass both checks
        // — Accepted, and the server really is the slot's server — and B
        // collected A's answer.
        let _e = env();
        const A: u32 = 30;
        const B: u32 = 31;
        host_seam::set_tid_live(A, true);
        host_seam::set_tid_live(B, true);

        let idx = call_idx(A, SERVER, REQ).expect("slot");
        let (stale, caller, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(caller, A);

        // A dies mid-exchange; the slot comes back to the pool.
        assert_eq!(fast_ipc_release_all(A), 1);
        // B calls and must land on the very index the stale handle names,
        // otherwise the test is not exercising the hazard at all.
        let reused = call_idx(B, SERVER, REQ).expect("slot");
        assert_eq!(reused, idx, "test needs the same index to be reused");
        let (fresh, caller2, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(caller2, B);
        assert_ne!(fresh, stale, "handle did not change across the reuse");

        // The stale handle must be rejected, and rejected *as stale*.
        assert_eq!(fast_ipc_reply(stale, SERVER, false, RSP), FastIpcReply::Stale);
        // Nothing was written: B is still waiting, not holding A's answer.
        assert_eq!(slot_state(idx), Some(SlotState::Accepted));
        assert_eq!(fast_ipc_collect(B), None);
        assert_eq!(fast_ipc_wait_state(handle_of(idx), B), FastIpcWait::Waiting);

        // The fresh handle still works — the gate must not cost the live half.
        assert_eq!(
            fast_ipc_reply(fresh, SERVER, false, RSP),
            FastIpcReply::Woke { caller_tid: B, slot_idx: idx }
        );
        assert_eq!(fast_ipc_collect(B), Some(RSP));
    }

    #[test]
    fn a_stale_handle_is_refused_over_the_whole_table() {
        // One index proves one index. Sweep all 64 so an encoding bug that
        // only bites at some offsets cannot hide.
        let _e = env();
        let mut stale = [0u64; FAST_IPC_MAX_SLOTS];
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert!(fast_ipc_call(1000 + i as u32, SERVER, REQ).is_some());
        }
        for i in 0..FAST_IPC_MAX_SLOTS {
            let (h, _, _) = fast_ipc_accept(SERVER).expect("accept");
            stale[i] = h;
        }
        // Every client dies; every slot turns over.
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(fast_ipc_release_all(1000 + i as u32), 1);
        }
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert!(fast_ipc_call(2000 + i as u32, SERVER, REQ).is_some());
        }
        for _ in 0..FAST_IPC_MAX_SLOTS {
            assert!(fast_ipc_accept(SERVER).is_some());
        }
        for (i, &h) in stale.iter().enumerate() {
            assert_eq!(
                fast_ipc_reply(h, SERVER, false, RSP),
                FastIpcReply::Stale,
                "stale handle {i} was honoured"
            );
        }
        // No new tenant collected anything.
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(fast_ipc_collect(2000 + i as u32), None, "client {i}");
        }
    }

    #[test]
    fn a_handle_from_a_future_generation_is_refused() {
        // The mirror of the stale case. It is not reachable by an honest
        // server, but `a0` is ring 3's to choose, so the comparison must be
        // equality and not "at least".
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        let g = slot_generation(idx);
        for ahead in [1u64, 2, 1000, FAST_IPC_GEN_MASK] {
            let h = fast_ipc_make_handle(idx, g.wrapping_add(ahead) & FAST_IPC_GEN_MASK);
            if handle_generation(h) == g { continue; }
            assert_eq!(fast_ipc_reply(h, SERVER, false, RSP), FastIpcReply::Stale, "+{ahead}");
        }
        assert_eq!(slot_state(idx), Some(SlotState::Accepted));
    }

    #[test]
    fn a_privileged_replier_is_still_bound_by_the_generation() {
        // `privileged` waives *ownership*, which is authorisation. It does not
        // waive the generation, which is the identity of the exchange: a
        // kernel server holding a dead handle is as wrong about reality as a
        // user one, and letting it write would corrupt the slot's new tenant.
        let _e = env();
        const A: u32 = 30;
        const B: u32 = 31;
        host_seam::set_tid_live(A, true);
        host_seam::set_tid_live(B, true);
        let idx = call_idx(A, SERVER, REQ).expect("slot");
        let (stale, _, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(fast_ipc_release_all(A), 1);
        assert_eq!(call_idx(B, SERVER, REQ), Some(idx));
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(fast_ipc_reply(stale, IMPOSTOR, true, RSP), FastIpcReply::Stale);
        assert_eq!(fast_ipc_collect(B), None);
    }

    #[test]
    fn stale_never_leaks_the_generation_to_a_non_owner() {
        // `Stale` is only reachable after the state and ownership checks have
        // passed. Reordered, it would be a generation oracle: a ring-3 task
        // could sweep 64 indices and read off how often each slot has turned
        // over. A non-owner must get the same `Refused` whatever it guesses.
        let _e = env();
        fill_and_accept_all();
        // Turn slot 0 over a few times so its generation is genuinely
        // distinguishable from its neighbours'.
        for _ in 0..3 {
            assert_eq!(fast_ipc_release_all(1000), 1);
            assert!(fast_ipc_call(1000, SERVER, REQ).is_some());
            assert!(fast_ipc_accept(SERVER).is_some());
        }
        for g in 0..8u64 {
            let h = fast_ipc_make_handle(0, g);
            assert_eq!(
                fast_ipc_reply(h, IMPOSTOR, false, RSP),
                FastIpcReply::Refused,
                "impostor learned something at gen {g}"
            );
        }
    }

    #[test]
    fn a_stale_handle_on_a_free_slot_reads_as_refused_not_stale() {
        // `Stale` and `Refused` are not a partition of "rejected", and the
        // doc on `FastIpcReply` says so. A dead handle whose slot happens to
        // be free fails the state check first. `Stale` is *sufficient* proof
        // the exchange is over; `Refused` proves nothing either way.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let (h, _, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(fast_ipc_release_all(CLIENT), 1);
        assert_eq!(slot_state(idx), Some(SlotState::Free));
        assert_eq!(fast_ipc_reply(h, SERVER, false, RSP), FastIpcReply::Refused);
    }

    #[test]
    fn a_stale_handle_on_a_slot_now_owned_by_another_server_is_refused() {
        // Same non-partition, the other way: the slot is live but belongs to
        // somebody else, so ownership rejects before generation is consulted.
        // That is what stops `Stale` being an oracle.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let (h, _, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(fast_ipc_release_all(CLIENT), 1);
        assert_eq!(call_idx(CLIENT, OTHER_SERVER, REQ), Some(idx));
        assert!(fast_ipc_accept(OTHER_SERVER).is_some());
        assert_eq!(fast_ipc_reply(h, SERVER, false, RSP), FastIpcReply::Refused);
        assert_eq!(fast_ipc_collect(CLIENT), None);
    }

    #[test]
    fn a_collected_exchange_retires_its_own_handle() {
        // The ABA does not need a death to set it up: a completed exchange
        // frees the slot too, so a server that replies twice with the same
        // handle across two different clients must be stopped the same way.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let (h1, _, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(reply_h(h1, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP));
        // New client, same index.
        assert_eq!(call_idx(IMPOSTOR, SERVER, REQ), Some(idx));
        assert!(fast_ipc_accept(SERVER).is_some());
        let poison = [0xDEADu64, 0, 0, 0];
        assert_eq!(fast_ipc_reply(h1, SERVER, false, poison), FastIpcReply::Stale);
        assert_eq!(fast_ipc_collect(IMPOSTOR), None);
    }

    #[test]
    fn the_generation_wraps_without_panicking() {
        // `overflow-checks = true` + `panic = "abort"`: a plain `+ 1` at the
        // ceiling would reset the board. The counter is a tag, not a count, so
        // wrapping is also the correct arithmetic.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        set_slot_generation(idx, FAST_IPC_GEN_MASK);
        assert_eq!(slot_generation(idx), FAST_IPC_GEN_MASK);
        assert_eq!(fast_ipc_release_all(CLIENT), 1);
        assert_eq!(slot_generation(idx), 0, "generation did not wrap to 0");
        // And the wrapped value still encodes and decodes cleanly.
        assert_eq!(call_idx(CLIENT, SERVER, REQ), Some(idx));
        let (h, _, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(handle_generation(h), 0);
        assert_eq!(reply_h(h, SERVER, false, RSP), Some(CLIENT));
    }

    #[test]
    fn generation_wrap_reopens_aba_the_documented_residual() {
        // The honest statement of what this fix does NOT cover. At the wrap a
        // handle from an ancient tenancy matches again — that is inherent to a
        // finite tag, and 2^57 = 144_115_188_075_855_872 reuses of one slot is
        // the price. One reuse per nanosecond, faster than an instruction
        // retires, still needs ~4.5 years of doing nothing else.
        //
        // Reached here by poking the counter, because reaching it honestly is
        // the point. If this test ever fails, the encoding changed and the
        // wraparound claim in the report needs recomputing.
        let _e = env();
        const A: u32 = 30;
        const B: u32 = 31;
        host_seam::set_tid_live(A, true);
        host_seam::set_tid_live(B, true);

        let idx = call_idx(A, SERVER, REQ).expect("slot");
        set_slot_generation(idx, 0);
        let (ancient, _, _) = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(handle_generation(ancient), 0);
        assert_eq!(fast_ipc_release_all(A), 1);
        assert_eq!(slot_generation(idx), 1);

        // Simulate 2^57 - 1 further turnovers of this slot.
        set_slot_generation(idx, 0);
        assert_eq!(call_idx(B, SERVER, REQ), Some(idx));
        assert!(fast_ipc_accept(SERVER).is_some());

        // The ancient handle matches again, and B collects A's answer. This is
        // the ABA, back — documented, bounded, accepted.
        assert_eq!(
            fast_ipc_reply(ancient, SERVER, false, RSP),
            FastIpcReply::Woke { caller_tid: B, slot_idx: idx }
        );
        assert_eq!(fast_ipc_collect(B), Some(RSP));
    }

    // ── Bounds: no reachable panic (panic = "abort" resets the board) ───────

    #[test]
    fn arbitrary_handle_bit_patterns_never_panic() {
        // Every 64-bit value is a legal `a0`. With `FAST_IPC_MAX_SLOTS` = 64
        // the index field is saturated, so no handle decodes out of range —
        // but the table must still be swept for a panic, and every one of
        // these must be refused rather than land on a live exchange.
        let _e = env();
        fill_and_accept_all();
        // Every live slot is at generation 0, so a handle is honourable here
        // only if bit 63 is clear *and* its generation field is 0 — i.e. only
        // the plain indices 0..63. None of these qualify: the first two groups
        // have bit 63 set, the rest carry a non-zero generation.
        for h in [
            u64::MAX,
            1u64 << 63,
            (1u64 << 63) | 5,
            (1u64 << 63) | (FAST_IPC_GEN_MASK << FAST_IPC_SLOT_BITS),
            u64::MAX / 2,
            i64::MAX as u64,
            FAST_IPC_GEN_MASK << FAST_IPC_SLOT_BITS,
            0x7EAD_BEEF_DEAD_BEEF,
            1u64 << FAST_IPC_SLOT_BITS,
        ] {
            assert!(
                h > i64::MAX as u64 || handle_generation(h) != 0,
                "test datum {h:#x} is honourable, not a rejection case"
            );
            for (tid, privileged) in
                [(SERVER, false), (SERVER, true), (IMPOSTOR, false), (IMPOSTOR, true)]
            {
                assert_eq!(reply_h(h, tid, privileged, RSP), None, "handle {h:#x}");
            }
        }
        // A handle is invalidated by its sign bit, not repaired by masking it.
        let good = handle_of(0);
        assert!(reply_h(good | (1u64 << 63), SERVER, false, RSP).is_none());
        assert_eq!(fast_ipc_handle_slot(good | (1u64 << 63)), None);
        // Nothing above wrote anywhere in the table.
        for idx in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(slot_state(idx), Some(SlotState::Accepted), "slot {idx} mutated");
        }
    }

    #[test]
    fn out_of_range_slot_index_never_panics() {
        // Kept as a direct index sweep: `fast_ipc_handle_slot` is the only
        // bounds check between ring 3 and `slots[..]`, and an off-by-one there
        // is a board reset under `panic = "abort"`.
        let _e = env();
        for idx in [
            FAST_IPC_MAX_SLOTS,      // 64 — first invalid
            FAST_IPC_MAX_SLOTS + 1,
            1024,
            usize::MAX / 2,
            usize::MAX - 1,
            usize::MAX,
        ] {
            let expect = if idx as u64 > i64::MAX as u64 {
                None // sign bit set: never a handle this file issued
            } else {
                Some(idx & (FAST_IPC_SLOT_MASK as usize))
            };
            assert_eq!(fast_ipc_handle_slot(idx as u64), expect, "idx {idx}");
            // ...and with every slot free, no handle at all may be honoured.
            assert_eq!(reply_h(idx as u64, SERVER, false, RSP), None, "idx {idx}");
            assert_eq!(reply_h(idx as u64, SERVER, true, RSP), None, "idx {idx}");
        }
    }

    #[test]
    fn last_valid_slot_index_works() {
        // 63 must be usable — an off-by-one in the bounds check would make the
        // last slot permanently unrepliable and leak it on every wrap.
        let _e = env();
        fill_and_accept_all();
        let last = FAST_IPC_MAX_SLOTS - 1;
        assert_eq!(reply_now(last, SERVER, false, RSP), Some(1000 + last as u32));
        assert_eq!(fast_ipc_collect(1000 + last as u32), Some(RSP));
    }

    // ── wait-state accessor: the retry loop's oracle ───────────────────────

    #[test]
    fn wait_state_is_waiting_for_the_owner_before_the_reply() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        // Pending — server has not even accepted yet.
        assert_eq!(fast_ipc_wait_state(handle_of(idx), CLIENT), FastIpcWait::Waiting);
        let _ = fast_ipc_accept(SERVER).expect("accept");
        // Accepted — server owes an answer. Still "block again".
        assert_eq!(fast_ipc_wait_state(handle_of(idx), CLIENT), FastIpcWait::Waiting);
    }

    #[test]
    fn wait_state_is_ready_once_the_reply_is_deposited() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        // The client's handle for the exchange, captured while it lives —
        // collect frees the slot and bumps the generation, so the post-collect
        // probe below must use the handle the client actually held.
        let h = handle_of(idx);
        assert_eq!(reply_now(idx, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_wait_state(h, CLIENT), FastIpcWait::Ready);
        // ...and Ready must actually mean collect succeeds.
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP));
        // After collecting, the exchange is over: the client's own handle is
        // dead (state check catches it; the bumped generation would too).
        assert_eq!(fast_ipc_wait_state(h, CLIENT), FastIpcWait::Gone);
    }

    #[test]
    fn wait_state_is_gone_after_the_server_dies() {
        // The case the retry loop must not get wrong: blocking again here is
        // sleeping forever.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let h = handle_of(idx); // the client's view, captured before the free
        assert_eq!(fast_ipc_release_all(SERVER), 1);
        assert_eq!(fast_ipc_wait_state(h, CLIENT), FastIpcWait::Gone);
    }

    #[test]
    fn wait_state_is_gone_on_a_free_slot() {
        let _e = env();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(
                fast_ipc_wait_state(handle_of(idx), CLIENT),
                FastIpcWait::Gone,
                "idx {idx}"
            );
        }
    }

    #[test]
    fn wait_state_hides_live_slots_owned_by_somebody_else() {
        // IPC-1 through the back door: an index-only probe would be a state
        // oracle over the whole table. A live slot owned by another client
        // must be indistinguishable from an empty one.
        let _e = env();
        fill_and_accept_all();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            let owner = 1000 + idx as u32;
            let h = handle_of(idx);
            assert_eq!(fast_ipc_wait_state(h, owner), FastIpcWait::Waiting, "owner {owner}");
            assert_eq!(
                fast_ipc_wait_state(h, IMPOSTOR),
                FastIpcWait::Gone,
                "third party read slot {idx}"
            );
        }
        // Even after a reply lands, the third party learns nothing.
        assert_eq!(reply_now(0, SERVER, false, RSP), Some(1000));
        assert_eq!(fast_ipc_wait_state(handle_of(0), IMPOSTOR), FastIpcWait::Gone);
        assert_eq!(fast_ipc_wait_state(handle_of(0), 1000), FastIpcWait::Ready);
    }

    #[test]
    fn wait_state_hides_slots_from_the_free_sentinel_tid() {
        // Free slots carry FAST_IPC_SLOT_FREE in caller_tid; a probe using it
        // must not match them.
        let _e = env();
        let _ = call_idx(CLIENT, SERVER, REQ).expect("slot");
        for idx in 0..FAST_IPC_MAX_SLOTS {
            assert_eq!(
                fast_ipc_wait_state(handle_of(idx), FAST_IPC_SLOT_FREE),
                FastIpcWait::Gone,
                "idx {idx}"
            );
        }
    }

    #[test]
    fn wait_state_hostile_handles_never_panic_and_read_as_gone() {
        // `a0` is ring 3's to choose. Bit 63 set, out-of-range garbage in the
        // generation field, plausible-but-wrong values — everything must be
        // `Gone`, never a panic (`panic = "abort"` = board reset) and never
        // a peek at someone's slot.
        let _e = env();
        let _ = call_idx(CLIENT, SERVER, REQ).expect("slot");
        for h in [
            1u64 << 63,                 // negative-half handle: decode refuses
            u64::MAX,                   // ditto
            u64::MAX >> 1,              // bit 63 clear, absurd generation: mismatch
            FAST_IPC_MAX_SLOTS as u64,  // decodes as idx 0, generation 1: stale
            fast_ipc_make_handle(1, 7), // free seat, wrong gen: state check
        ] {
            assert_eq!(fast_ipc_wait_state(h, CLIENT), FastIpcWait::Gone, "h {h:#x}");
        }
    }

    #[test]
    fn wait_state_reports_gone_when_the_slot_is_reassigned() {
        // The ABA shape, from the *waiting client's* side — now closed by the
        // generation in the client handle, not merely contained by the TID
        // check. The second half is the case the TID check could never see:
        // the seat re-let to the SAME tid.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let stale = handle_of(idx); // the doomed exchange's handle
        assert_eq!(fast_ipc_wait_state(stale, CLIENT), FastIpcWait::Waiting);
        // CLIENT dies; the slot is recycled by a different client.
        assert_eq!(fast_ipc_release_all(CLIENT), 1);
        let reused = call_idx(IMPOSTOR, SERVER, REQ).expect("slot");
        assert_eq!(reused, idx, "test needs the same index to be reused");
        assert_eq!(fast_ipc_wait_state(stale, CLIENT), FastIpcWait::Gone);
        assert_eq!(fast_ipc_wait_state(handle_of(idx), IMPOSTOR), FastIpcWait::Waiting);

        // Same-tid recycle: IMPOSTOR's first exchange dies, IMPOSTOR calls
        // again and lands on the same seat. Its OLD handle must read Gone —
        // with a bare index this was indistinguishable from the live one.
        let old = handle_of(idx);
        assert_eq!(fast_ipc_release_all(IMPOSTOR), 1);
        let again = call_idx(IMPOSTOR, SERVER, REQ).expect("slot");
        assert_eq!(again, idx, "test needs the same index to be reused");
        assert_eq!(fast_ipc_wait_state(old, IMPOSTOR), FastIpcWait::Gone,
                   "stale-generation handle matched the re-let seat");
        assert_eq!(fast_ipc_wait_state(handle_of(idx), IMPOSTOR), FastIpcWait::Waiting);
    }

    #[test]
    fn wait_state_never_reports_ready_without_a_collectable_reply() {
        // The contract the retry loop leans on, swept over the whole table and
        // every lifecycle stage: Ready ⇒ collect succeeds.
        let _e = env();
        fill_and_accept_all();
        for idx in 0..FAST_IPC_MAX_SLOTS {
            let owner = 1000 + idx as u32;
            assert_ne!(fast_ipc_wait_state(handle_of(idx), owner), FastIpcWait::Ready);
            assert_eq!(fast_ipc_collect(owner), None);
        }
        for idx in 0..FAST_IPC_MAX_SLOTS {
            let owner = 1000 + idx as u32;
            assert_eq!(reply_now(idx, SERVER, false, RSP), Some(owner));
            assert_eq!(fast_ipc_wait_state(handle_of(idx), owner), FastIpcWait::Ready);
            assert_eq!(fast_ipc_collect(owner), Some(RSP));
        }
    }

    #[test]
    fn wait_state_stays_ready_when_the_server_dies_after_replying() {
        // Pairs with release_by_server_tid_preserves_an_already_deposited_reply:
        // the retry loop must still see Ready and hand the reply over.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(reply_now(idx, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_release_all(SERVER), 0);
        assert_eq!(fast_ipc_wait_state(handle_of(idx), CLIENT), FastIpcWait::Ready);
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP));
    }

    // ── IPC-3: reclamation on task death ───────────────────────────────────

    #[test]
    fn exhausting_the_table_kills_the_fast_path_until_release() {
        // This is the test whose absence let the leak kill the fast path in
        // silence: everything still "works", it just stops being fast.
        let _e = env();
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert!(fast_ipc_call(1000 + i as u32, SERVER, REQ).is_some(), "alloc {i}");
        }
        assert_eq!(fast_ipc_active(), FAST_IPC_MAX_SLOTS as u32);
        // Table full — dispatch would answer -1 ("fall back to channel IPC").
        assert!(fast_ipc_call(CLIENT, SERVER, REQ).is_none());

        // The server dies: every slot it owns comes back.
        assert_eq!(fast_ipc_release_all(SERVER), FAST_IPC_MAX_SLOTS);
        assert_eq!(fast_ipc_active(), 0);

        // ...and the fast path is alive again.
        assert!(fast_ipc_call(CLIENT, SERVER, REQ).is_some());
    }

    #[test]
    fn release_by_client_tid_frees_the_slot_and_wakes_nobody() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        assert_eq!(fast_ipc_release_all(CLIENT), 1);
        assert_eq!(fast_ipc_active(), 0);
        assert_eq!(slot_state(idx), Some(SlotState::Free));
        // Nobody to wake: the dead task *is* the client.
        assert_eq!(host_seam::wake_count(), 0);
        // The server now finds nothing to accept, i.e. -1 at the syscall.
        assert!(fast_ipc_accept(SERVER).is_none());
    }

    #[test]
    fn release_by_server_tid_frees_the_slot_and_wakes_the_blocked_client() {
        // Decision under test: free + wake, no synthetic reply. The client
        // wakes, collects nothing, and dispatch answers -1.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        // The handle the sleeping client is blocked on — the orphan wake must
        // be minted with THIS (pre-free) generation, or it can never match
        // the sleeper (and could only match the seat's next tenant).
        let clients_handle = handle_of(idx);
        assert_eq!(fast_ipc_release_all(SERVER), 1);
        assert_eq!(fast_ipc_active(), 0);
        assert!(host_seam::was_woken(idx), "blocked client was not woken");
        assert_eq!(host_seam::wake_count(), 1);
        assert_eq!(
            host_seam::woken_handle(idx),
            Some(clients_handle),
            "orphan wake was minted with the wrong generation"
        );
        assert_eq!(fast_ipc_collect(CLIENT), None);
    }

    #[test]
    fn release_by_server_tid_wakes_client_of_an_accepted_slot_too() {
        // Server accepted and then died — the client is just as stuck as in
        // the Pending case, so it must be woken just the same.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(fast_ipc_release_all(SERVER), 1);
        assert!(host_seam::was_woken(idx));
        assert_eq!(fast_ipc_collect(CLIENT), None);
    }

    #[test]
    fn release_wakes_every_orphaned_client_not_just_the_first() {
        let _e = env();
        for i in 0..FAST_IPC_MAX_SLOTS {
            assert!(fast_ipc_call(1000 + i as u32, SERVER, REQ).is_some());
        }
        assert_eq!(fast_ipc_release_all(SERVER), FAST_IPC_MAX_SLOTS);
        assert_eq!(host_seam::wake_count(), FAST_IPC_MAX_SLOTS as u32);
        for idx in 0..FAST_IPC_MAX_SLOTS {
            assert!(host_seam::was_woken(idx), "slot {idx} not woken");
        }
    }

    #[test]
    fn release_touches_only_the_dead_tids_slots() {
        let _e = env();
        let a = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let b = call_idx(CLIENT, OTHER_SERVER, REQ).expect("slot");
        assert_eq!(fast_ipc_release_all(SERVER), 1);
        assert_eq!(slot_state(a), Some(SlotState::Free));
        assert_eq!(slot_state(b), Some(SlotState::Pending));
        assert_eq!(fast_ipc_active(), 1);
    }

    #[test]
    fn release_of_an_unrelated_or_sentinel_tid_is_a_noop() {
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        assert_eq!(fast_ipc_release_all(IMPOSTOR), 0);
        // u32::MAX is the "slot is free" sentinel: free slots carry it in both
        // TID fields, so a release keyed on it must not scavenge the table.
        assert_eq!(fast_ipc_release_all(FAST_IPC_SLOT_FREE), 0);
        assert_eq!(fast_ipc_active(), 1);
        assert_eq!(slot_state(idx), Some(SlotState::Pending));
        assert_eq!(host_seam::wake_count(), 0);
    }

    #[test]
    fn release_on_an_empty_table_is_a_noop() {
        let _e = env();
        assert_eq!(fast_ipc_release_all(SERVER), 0);
        assert_eq!(fast_ipc_active(), 0);
        assert_eq!(host_seam::wake_count(), 0);
    }

    #[test]
    fn release_by_server_tid_preserves_an_already_deposited_reply() {
        // The one-shot service: reply, then exit. On a single hart the exit
        // hook always runs before the woken client is scheduled, so reclaiming
        // a `Replied` slot here would deterministically turn every completed
        // exchange of that shape into a spurious -1.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(reply_now(idx, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_release_all(SERVER), 0, "reply was reclaimed");
        assert_eq!(fast_ipc_collect(CLIENT), Some(RSP), "reply was lost");
        assert_eq!(fast_ipc_active(), 0);
    }

    #[test]
    fn release_of_a_replied_but_uncollected_slot_frees_it() {
        // Client died after the server replied but before it woke: nothing to
        // wake (the dead task is the client), slot must still come back.
        let _e = env();
        let idx = call_idx(CLIENT, SERVER, REQ).expect("slot");
        let _ = fast_ipc_accept(SERVER).expect("accept");
        assert_eq!(reply_now(idx, SERVER, false, RSP), Some(CLIENT));
        assert_eq!(fast_ipc_release_all(CLIENT), 1);
        assert_eq!(fast_ipc_active(), 0);
        assert_eq!(host_seam::wake_count(), 0);
    }

    #[test]
    fn used_counter_never_desyncs_from_the_table() {
        // `used == 0` is the O(1) early-exit in both lookup helpers: if the
        // counter drifts above the real occupancy the helpers still work, but
        // if it drifts below zero-occupancy they go blind. Exercise a full
        // alloc/free cycle through every public entry point.
        let _e = env();
        for round in 0..3 {
            for i in 0..FAST_IPC_MAX_SLOTS {
                assert!(fast_ipc_call(1000 + i as u32, SERVER, REQ).is_some());
            }
            assert_eq!(fast_ipc_active(), FAST_IPC_MAX_SLOTS as u32, "round {round}");
            if round % 2 == 0 {
                for i in 0..FAST_IPC_MAX_SLOTS {
                    let (handle, caller, _) = fast_ipc_accept(SERVER).expect("accept");
                    assert_eq!(reply_h(handle, SERVER, false, RSP), Some(caller));
                    assert_eq!(fast_ipc_collect(caller), Some(RSP));
                    let _ = i;
                }
            } else {
                assert_eq!(fast_ipc_release_all(SERVER), FAST_IPC_MAX_SLOTS);
            }
            assert_eq!(fast_ipc_active(), 0, "round {round}");
            assert!(fast_ipc_accept(SERVER).is_none());
        }
    }
}
