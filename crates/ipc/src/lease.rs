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

use core::sync::atomic::{AtomicUsize, Ordering};
use robot_os_sync::SpinLock;
pub use robot_os_limits::MAX_LEASES;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

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

// IRQ-safe: `lease_tick()` runs from the timer ISR (kernel `handle_interrupt`),
// while every other accessor runs in task/syscall context on the same hart with
// interrupts enabled. A plain `lock()` in task context would let a timer tick
// re-enter `lease_tick()` → `lock()` → same-hart deadlock. Every accessor below
// therefore uses `lock_irqsave()`, never plain `lock()` (see `crates/ipc/port.rs`
// for the same pattern).
static LEASES: SpinLock<LeaseTable> = SpinLock::new(LeaseTable::new());

/// Number of table entries `lease_tick` could possibly act on: state is
/// `Pending | Active` **and** `expire_ticks != 0`.
///
/// **WHY (audit: "the common case pays 157 instructions to do nothing").**
/// `lease_tick` runs inside the timer ISR. On this robot the normal table
/// state is "no lease with a deadline", and the old shape paid the full
/// 157-instruction cost anyway: 61 fixed (of which 32 were the unconditional
/// stores that materialise the 256-byte return array) plus 16 loop iterations
/// that all fall through. This counter is what makes an early exit possible
/// *before* the lock and before any buffer exists.
///
/// Measured on the RV64 artifact, common case, callee + caller drain loop:
/// **242 → 32 instructions** (157 + 85 before, 6 + 26 after). The 157 figure
/// was re-derived here from a byte-for-byte copy of the old body compiled in
/// the same invocation as the new one, not taken from the audit on trust —
/// it came out identical.
static DEADLINE_LEASES: AtomicUsize = AtomicUsize::new(0);

/// Is this entry in the set [`DEADLINE_LEASES`] counts?
#[inline]
fn has_deadline(e: &LeaseEntry) -> bool {
    matches!(e.state, LeaseState::Pending | LeaseState::Active) && e.expire_ticks != 0
}

/// Recompute [`DEADLINE_LEASES`] from the table. **Call at the end of every
/// operation that writes `state` or `expire_ticks`, with the lock still
/// held.**
///
/// **WHY a full recount rather than incremental +1/−1 deltas.** Six call
/// sites mutate lease state (`lease_grant`, `lease_accept`, `lease_return`,
/// `lease_free`, `lease_release_all`, `lease_tick`), some of them with two
/// branches. A single missed decrement costs only wasted ISR work, but a
/// single missed *increment* means a lease with a deadline that the ISR never
/// looks at again — `lease_wait_return` sleeps forever on the exact bound
/// `expire_ticks` exists to provide. Exactness by construction is worth more
/// than the arithmetic: every one of those sites is cold (a grant, a return,
/// a task exit — each already ending in a cross-task wake that costs
/// thousands of nanoseconds), and this is 16 iterations under a lock the
/// caller already holds. Nothing is added to the ISR path.
#[inline]
fn refresh_deadline_count(table: &LeaseTable) {
    let mut n = 0usize;
    for e in table.entries.iter() {
        // `wrapping_add` and not `+`: the kernel builds with
        // `overflow-checks = true`, and a plain increment makes rustc emit a
        // branch to `panic::add_overflow` that `lease_tick` would carry
        // *inside the timer ISR* (verified on the RV64 artifact). `n` is
        // bounded by `MAX_LEASES`, so the two are equivalent here.
        if has_deadline(e) { n = n.wrapping_add(1); }
    }
    // `Release` pairs with nothing in particular — correctness comes from
    // `LEASES`, which every reader of the table takes. See `lease_tick` for
    // why the load side is `Relaxed`.
    DEADLINE_LEASES.store(n, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Grant a lease of `shm_id` to `lessee_tid`.
///
/// `expire_ticks`: absolute CLINT tick at which the lease auto-expires (0 = never).
/// Returns `Some(lease_id)` or `None` if no free slots.
///
/// **REVIEWED, NOT CHANGED (IPC-6 sweep).** `SYS_IPC_LEASE_GRANT` passes `a0`
/// as a raw `shm_id` from ring 3 and this function never checks that
/// `lessor_tid` owns that region — a task can advertise a lease over shared
/// memory belonging to somebody else. That is deliberately left alone here
/// because it grants **no authority the caller did not already have**:
/// `shm::shm_acquire(tid, shm_id)` itself performs no ownership or capability
/// check whatsoever (`crates/ipc/src/shm.rs`), so any task can already attach
/// to any active region by id without going near a lease. Adding
/// `shm_owner(shm_id) == lessor_tid` here would look like a fix while leaving
/// the actual door open, and would change the behaviour of the kernel lease
/// bench that this lane cannot run in QEMU. The real fix belongs in
/// `shm_acquire`, which is not this lane's file — see the report.
pub fn lease_grant(shm_id: usize, lessor_tid: u32, lessee_tid: u32, expire_ticks: u64) -> Option<usize> {
    let mut table = LEASES.lock_irqsave();
    let id = table.alloc(shm_id, lessor_tid, lessee_tid, expire_ticks);
    // A grant is the only way a deadline enters the table, so skipping this
    // is the one mistake that would make `lease_tick`'s early exit unsound.
    refresh_deadline_count(&table);
    id
}

/// Lessee accepts a pending lease from `lessor_tid`.
///
/// Returns `Some((lease_id, shm_id))` if a pending lease exists.
/// The caller should block on `WaitReason::FastIpcServer` or similar if `None`.
pub fn lease_accept(lessee_tid: u32) -> Option<(usize, usize)> {
    let mut table = LEASES.lock_irqsave();
    if let Some(idx) = table.find_pending_for_lessee(lessee_tid) {
        let shm_id = table.entries[idx].shm_id;
        table.entries[idx].state = LeaseState::Active;
        // Pending → Active cannot change the count (both are counted), but the
        // invariant is "every writer of `state` refreshes", and an invariant
        // with an exception is an invariant nobody can check by inspection.
        refresh_deadline_count(&table);
        Some((idx, shm_id))
    } else {
        None
    }
}

/// Lessee returns the lease.  Wakes the lessor.
///
/// `caller_tid` is the TID of the task asking for the return; `privileged` is
/// `true` for kernel callers (`current_user_pt() == 0`), which bypass the
/// ownership check — the same convention as `cap_store`'s typed callers and
/// `dispatch::port_access_ok`.
///
/// Returns `Some(lessor_tid)` to wake, or `None` if the lease_id is invalid,
/// not Active, or does not belong to `caller_tid`.
///
/// **WHY the ownership check exists (IPC-6):** this used to take a bare
/// `lease_id` from `a0` of `SYS_IPC_LEASE_RETURN` and validate nothing but
/// `< MAX_LEASES` — and `MAX_LEASES` is 16, small and dense, so there is
/// nothing to guess. Any ring-3 task could therefore "return" a lease
/// belonging to two *other* tasks. That is not a nuisance: the lessor is
/// woken believing the buffer is back and resumes writing it, while the
/// legitimate lessee still has the very same SHM pages mapped and is still
/// reading them. A ring-3-triggerable data race on shared memory, on the path
/// this kernel uses to hand camera frames and sensor buffers around. Only the
/// **lessee** may return: it is the party that holds the buffer, and the
/// return is the act of giving it back.
pub fn lease_return(lease_id: usize, caller_tid: u32, privileged: bool) -> Option<u32> {
    if lease_id >= MAX_LEASES { return None; }
    let lessor = {
        let mut table = LEASES.lock_irqsave();
        let e = &mut table.entries[lease_id];
        if e.state != LeaseState::Active { return None; }
        // Two integer comparisons inside a lock we already hold: ~2 ns against
        // a measured 1879 ns/op syscall floor, and this is the *slow* half of
        // the lease path (it ends with a cross-task wake).
        if !privileged && e.lessee_tid != caller_tid { return None; }
        e.state = LeaseState::Returned;
        let lessor = e.lessor_tid;
        refresh_deadline_count(&table);
        lessor
    };
    // Wake a lessor blocked in `lease_wait_return` (WaitQueue). Released the
    // lock first — never wake while holding it. Harmless if the lessor used a
    // different wait path (wq_wake_by_tid is a no-op unless WaitQueue-blocked).
    if lessor != NO_TID {
        robot_os_sched::wq_wake_by_tid(lessor);
    }
    Some(lessor)
}

/// Lessor: block until the lessee returns the lease (or it expires), applying
/// **lease priority inheritance** (RFC-0031, experiment I3) when enabled.
///
/// While the (typically high-priority) lessor is blocked on a lease held by a
/// lower-priority lessee, the lessee inherits the lessor's priority — and is
/// re-positioned in the ready queue (`boost_ready_task`, since the legacy
/// bitmap scheduler buckets by priority at enqueue time) — so it is scheduled
/// ahead of mid-priority work and can return the buffer promptly instead of
/// being starved (unbounded inversion for a non-expiring lease). The boost is
/// undone on wake (return, exit, or expiry).
///
/// Gated by `robot_os_limits::LEASE_PRIORITY_INHERITANCE` (const-eliminated
/// when off → a plain block-until-returned loop). Wakeups come via
/// `wq_wake_by_tid(lessor)` from `lease_return` callers and the expiry path.
///
/// Must NOT be called holding any lock (it blocks).
///
/// **Authorization (IPC-6, same class as `lease_return` / `lease_free`):**
/// only the **lessor** of `lease_id` may wait on it. Waiting on a stranger's
/// lease donates *this* task's priority to that lease's lessee
/// (`boost_ready_task`) — an unauthenticated priority boost of an arbitrary
/// task, driven by an integer in `0..16` — and parks the caller on a wake
/// it has no claim to. Unlike its two siblings this one derives the caller
/// itself rather than taking `caller_tid`/`privileged` parameters: it has
/// **no syscall arm today** (`SYS_IPC_LEASE_WAIT` does not exist; the only
/// caller in the tree is the kernel lease bench in `kernel/src/main.rs`), so
/// this is insurance against the arm being added later, not a live hole, and
/// deriving the identity here avoids changing a signature no ring-3 path
/// reaches. If an arm is ever wired, switch to explicit parameters to match
/// `lease_return`. Free either way: this function blocks, so nothing here is
/// on a hot path.
pub fn lease_wait_return(lease_id: usize) {
    if lease_id >= MAX_LEASES {
        return;
    }
    let privileged = robot_os_sched::current_user_pt() == 0;
    let me = robot_os_sched::current_task_tid();
    // Snapshot the lessee under the lock, then release before blocking. Wait
    // for any in-flight lease (Pending = granted-not-yet-accepted, or Active);
    // only short-circuit if it is already finished or the slot is free.
    let lessee = {
        let table = LEASES.lock_irqsave();
        let e = &table.entries[lease_id];
        if !privileged && e.lessor_tid != me {
            return;
        }
        match e.state {
            LeaseState::Pending | LeaseState::Active => e.lessee_tid,
            _ => return, // Free / Returned / Expired — nothing to wait for.
        }
    };

    // Priority inheritance: boost the lessee to our priority if ours is higher
    // (lower number). We do NOT remember the lessee's observed priority to
    // restore later — that was the bug. Two lessors donating to the same lessee
    // each captured whatever they happened to observe, and since each blocks on
    // its own lease id and leases can be returned in any order, an out-of-LIFO
    // restore both dropped the lessee below a still-active donor and left it
    // boosted at a stale value forever. The scheduler now counts donations and
    // decides when the base priority comes back.
    let mut donated = false;
    if robot_os_limits::LEASE_PRIORITY_INHERITANCE && lessee != NO_TID {
        if let (Some(my_prio), Some(lessee_prio)) =
            (robot_os_sched::task_priority(me), robot_os_sched::task_priority(lessee))
        {
            if my_prio < lessee_prio {
                robot_os_sched::boost_ready_task(lessee, my_prio);
                donated = true;
            }
        }
    }

    // Block until the buffer is back (returned or expired). Woken by
    // wq_wake_by_tid from the returner / expiry path.
    while !lease_is_returned(lease_id) {
        robot_os_sched::wq_block_current();
    }

    // Undo the inherited boost. No-op if the lessee already exited. Must be
    // called exactly once per successful boost — the scheduler's donation
    // counter only returns the task to its base priority when it hits zero.
    if donated {
        robot_os_sched::restore_ready_task(lessee);
    }
}

/// Lessor checks if the lease has been returned.
///
/// Should be called after being woken from `WaitReason::Timer` or a dedicated
/// lease-wait reason.  Returns `true` if the buffer is safely reclaimed.
pub fn lease_is_returned(lease_id: usize) -> bool {
    if lease_id >= MAX_LEASES { return false; }
    let table = LEASES.lock_irqsave();
    matches!(table.entries[lease_id].state, LeaseState::Returned | LeaseState::Expired)
}

/// Free a lease entry (called by the lessor after reclaiming the buffer).
///
/// Returns `true` if the slot was freed, `false` if `lease_id` is out of
/// range, already free, or does not belong to `caller_tid`.
///
/// **WHY the ownership check exists (IPC-6):** `SYS_IPC_LEASE_FREE` passed
/// `a0` straight through and this function validated only `< MAX_LEASES`, so
/// any ring-3 task could destroy an in-progress SHM cession between two other
/// tasks — the lessor then blocks in `lease_wait_return` on a slot that has
/// gone `Free` (it returns early, so the lessor "reclaims" a buffer the
/// lessee is still using), and the id is immediately re-issued by
/// `lease_grant` to somebody else. Only the **lessor** may free: the entry is
/// the lessor's bookkeeping of its own buffer, and `lease_grant` is what
/// allocated it.
///
/// **WHY freeing an Active lease is still allowed:** the buffer belongs to
/// the lessor, `MAX_LEASES` is 16, and refusing would let a lessee that
/// simply never returns (grant with `expire_ticks == 0`) pin a slot for the
/// life of the board — a 16-deep table is trivially exhausted that way. The
/// recycled id is safe *because* of the ownership checks added here: the
/// abandoned lessee's stale `lease_id` no longer matches the new entry's
/// `lessee_tid`, so its `lease_return` is rejected instead of corrupting the
/// next pair of tasks. Remove either check and slot recycling becomes a
/// confused-deputy primitive again.
///
/// `privileged` (kernel, `current_user_pt() == 0`) bypasses the check, per
/// the house convention shared with `cap_store`'s typed callers.
pub fn lease_free(lease_id: usize, caller_tid: u32, privileged: bool) -> bool {
    if lease_id >= MAX_LEASES { return false; }
    let mut table = LEASES.lock_irqsave();
    let e = &mut table.entries[lease_id];
    if e.state == LeaseState::Free { return false; }
    if !privileged && e.lessor_tid != caller_tid { return false; }
    *e = LeaseEntry::empty();
    refresh_deadline_count(&table);
    true
}

/// Reclaim every lease `tid` participates in — task-exit hook (IPC-3).
///
/// **WHY this exists (IPC-3):** `LEASES` is a fixed 16-entry BSS table and
/// nothing reclaimed it. `task_release_all` called only `handle_revoke_all`,
/// `cap_store::reset` and `shm_release_all`, so every task that died holding
/// a lease burned a slot permanently; sixteen such deaths and `lease_grant`
/// returns `None` forever, with no diagnostic. Worse than the leak is the
/// blocked peer, which is why the two roles are treated differently:
///
///  * **The lessee dies** (this task held the buffer). The lessor may be
///    parked in `lease_wait_return`, which loops on `lease_is_returned` and
///    only exits for `Returned | Expired`. Nobody will ever return the
///    buffer, so the lessor sleeps forever — on a robot that is a control
///    task that stops actuating, not a hung shell. We mark the lease
///    `Expired` (**not** `Returned`: the buffer was never handed back, and
///    `Expired` is the state `lease_tick` already uses for exactly this
///    "the lessee did not give it back" outcome, so the lessor's post-wake
///    code cannot mistake an abandoned buffer for a clean handover) and wake
///    the lessor. The entry is deliberately **left allocated** so the lessor
///    frees it through the normal `lease_free` path, identical to the timer
///    expiry flow; if the lessor later dies too, the lessor branch below
///    reclaims the slot, so nothing leaks either way.
///  * **The lessor dies** (this task owns the buffer). Free the slot
///    outright. The lessee may still have the region mapped, but that is
///    governed by the SHM refcount (`shm_release_all` gives back the dead
///    lessor's reference; the region survives while the lessee holds one),
///    not by this table. Keeping the entry alive would buy nothing — there
///    is no lessor left to wake or to free it — and would leak the slot.
///    A lease `Pending` at that moment also strands a lessee blocked in
///    `SYS_IPC_LEASE_ACCEPT`, so that lessee is woken too; it re-runs
///    `lease_accept`, finds nothing, and gets an error instead of sleeping
///    for the life of the board.
///
/// A self-lease (`lessor == lessee == tid`) hits the lessor branch and is
/// freed; that is why the lessor test comes first.
///
/// **Wakes happen after the guard is dropped.** Waking under `LEASES` would
/// invert the lock order against the scheduler's task pool, and `lease_return`
/// already documents the rule. TIDs are buffered on the stack first
/// (2 × 16 × 4 B).
///
/// **The two roles get different wakes, on purpose.** A stranded *lessor* is
/// woken through both paths, mirroring the timer expiry path in
/// `kernel/src/main.rs`: it may be parked in `lease_wait_return` (WaitQueue →
/// `wq_wake_by_tid`) or in the legacy `SYS_IPC_LEASE_*` arm
/// (`WaitReason::FastIpcServer` → `wake_fast_ipc_server`), and waking only one
/// is a silent lost wakeup for half the callers. A stranded *lessee* is woken
/// **only** through `wake_fast_ipc_server`, because `SYS_IPC_LEASE_ACCEPT` is
/// its only wait path and it never uses the WaitQueue. That restraint matters:
/// `wq_wake_by_tid` on a task that is *not* currently blocked latches
/// `wake_pending` (the K-C9 lost-wakeup fix), which the task then consumes to
/// skip its next block — a spurious non-block injected into a task that was
/// never waiting on us.
///
/// Cost: exit path only, bounded by `MAX_LEASES` (16). Nothing is added to
/// the grant/accept/return hot path.
pub fn lease_release_all(tid: u32) {
    // Lessors: parked in `lease_wait_return` (WaitQueue) or in the legacy
    // accept/return arm (FastIpcServer). Woken through both.
    let mut wake_lessor: [u32; MAX_LEASES] = [NO_TID; MAX_LEASES];
    let mut n_lessor = 0usize;
    // Lessees: only ever parked in `SYS_IPC_LEASE_ACCEPT` (FastIpcServer).
    let mut wake_lessee: [u32; MAX_LEASES] = [NO_TID; MAX_LEASES];
    let mut n_lessee = 0usize;

    {
        let mut table = LEASES.lock_irqsave();
        for e in table.entries.iter_mut() {
            if e.state == LeaseState::Free { continue; }

            // Lessor first: a self-lease must be freed, not expired.
            if e.lessor_tid == tid {
                let stranded = if e.state == LeaseState::Pending { e.lessee_tid } else { NO_TID };
                *e = LeaseEntry::empty();
                if stranded != NO_TID && stranded != tid && n_lessee < MAX_LEASES {
                    wake_lessee[n_lessee] = stranded;
                    n_lessee += 1;
                }
                continue;
            }

            if e.lessee_tid == tid
                && matches!(e.state, LeaseState::Pending | LeaseState::Active)
            {
                // `Pending` counts as lessee-held: `lease_wait_return` blocks
                // on `Pending | Active`, so a lessee that dies before ever
                // accepting strands the lessor just as thoroughly.
                e.state = LeaseState::Expired;
                if e.lessor_tid != NO_TID && n_lessor < MAX_LEASES {
                    wake_lessor[n_lessor] = e.lessor_tid;
                    n_lessor += 1;
                }
            }
        }
        refresh_deadline_count(&table);
    } // guard dropped — never wake while holding LEASES.

    for i in 0..n_lessor {
        robot_os_sched::wq_wake_by_tid(wake_lessor[i]);
        robot_os_sched::wake_fast_ipc_server(wake_lessor[i]);
    }
    for i in 0..n_lessee {
        robot_os_sched::wake_fast_ipc_server(wake_lessee[i]);
    }
}

/// Expire every lease past its deadline; write the affected lessors' TIDs
/// into `expired_lessors` and return how many were written.
///
/// Call this from the timer ISR alongside `wake_expired_timers()`. The caller
/// wakes the first `n` entries; this function does no wakes of its own,
/// because waking under `LEASES` inverts the lock order against the
/// scheduler's task pool.
///
/// The intended drain loop, and the one the numbers below were measured
/// against:
///
/// ```ignore
/// let mut expired = [0u32; robot_os_ipc::MAX_LEASES];
/// let n = robot_os_ipc::lease_tick(now, &mut expired);
/// for &lessor_tid in expired.iter().take(n) {
///     robot_os_sched::wq_wake_by_tid(lessor_tid);
///     robot_os_sched::wake_fast_ipc_server(lessor_tid);
/// }
/// ```
///
/// `iter().take(n)` and **not** `&expired[..n]`: `n` comes from an opaque
/// cross-crate call (`lto = false`), so LLVM cannot discharge the slice-range
/// check and emits a call to `panic` — inside the timer ISR, under
/// `panic = "abort"`. Verified on the artifact: the `take` form emits no
/// panic path and costs the same 26 instructions.
///
/// # WHY the signature changed (audit: 157 instructions to do nothing)
///
/// The old shape returned `[(usize, u32); MAX_LEASES]` by value. Measured on
/// the RV64 artifact, the common case on this robot — *no lease with a
/// deadline* — cost **157 instructions**, 61 of them fixed, and **32 of those
/// 61 were unconditional stores** that zero-fill the 256-byte return array in
/// the prologue, fully unrolled.
///
/// That is why a bare early exit would have saved nothing: the array is
/// materialised through the caller's `sret` pointer *before* any test can
/// skip it. The array had to leave the return position for the exit to have
/// anything to skip.
///
/// Counted honestly — callee **plus** the caller's own drain loop, because
/// the caller's cost is forced by this signature and `lto = false` means
/// nothing sinks it:
///
/// | case | callee | caller | total |
/// |---|---:|---:|---:|
/// | before | 157 | 85 | **242** |
/// | after (no deadline armed) | 6 | 26 | **32** |
///
/// The worst case moved too, and in the right direction: 16 leases expiring
/// on the same tick costs **291** instructions, against **349** before.
///
/// Two further decisions behind this exact signature:
///
///  * **Lessor TIDs only, not `(lease_id, lessor_tid)` pairs.** The one
///    caller in the tree — the drain loop in `kernel/src/main.rs` — ends with
///    `let _ = lease_id;`: it never used the id. Dropping it halves the
///    buffer the caller must materialise every tick, from 256 B (32 stores)
///    to 64 B (8), and the caller's buffer is real cost — the kernel builds
///    with `lto = false`, so nothing sinks it past this call.
///  * **`&mut [u32; MAX_LEASES]`, not a slice.** A slice would need a
///    capacity check per write and could silently drop expiries on a short
///    buffer. A fixed array of exactly the table's size cannot.
///
/// # WHY `Pending` expires too
///
/// (Unchanged from the previous revision.) This used to test `state ==
/// Active` only, but `lease_wait_return` blocks on `Pending | Active`. A
/// lessor that granted with a deadline and whose lessee never called
/// `lease_accept` therefore had its own deadline silently not apply — it
/// slept forever on a lease the ISR refused to expire, the exact failure mode
/// `expire_ticks` exists to bound.
pub fn lease_tick(now_ticks: u64, expired_lessors: &mut [u32; MAX_LEASES]) -> usize {
    // The early exit. `Relaxed` on purpose: this load carries no
    // synchronisation duty — every reader and writer of the table itself goes
    // through `LEASES`, and the authoritative deadline test is inside the
    // loop below. The only consequence of reading a stale value is a
    // *one-tick* lag on a deadline armed concurrently on another hart, which
    // is smaller than the granularity `expire_ticks` already has: nothing in
    // the tree arms a timer at grant time, so a deadline has never been
    // observable before the next tick anyway. A stale value can never be
    // stale forever — the store is a plain atomic write, not a cached local.
    if DEADLINE_LEASES.load(Ordering::Relaxed) == 0 {
        return 0;
    }

    let mut count = 0usize;
    let mut table = LEASES.lock_irqsave();
    for e in table.entries.iter_mut() {
        if has_deadline(e) && now_ticks >= e.expire_ticks {
            e.state = LeaseState::Expired;
            // The `count < MAX_LEASES` guard is redundant — the loop runs at
            // most `MAX_LEASES` times and writes at most once per iteration —
            // but it is **not** free to drop. Verified on the RV64 artifact:
            // without it LLVM cannot prove the index in range and emits a
            // call to `panic_bounds_check` on the expiry path. Under
            // `panic = "abort"` that is a board reset sitting in the timer
            // ISR, reachable only through a compiler bug or a future edit,
            // and it costs an extra instruction per expiry anyway. The guard
            // makes the range provable and the panic path disappears.
            if count < MAX_LEASES {
                expired_lessors[count] = e.lessor_tid;
                count += 1;
            }
        }
    }
    // **The one place that adjusts the counter by a delta instead of
    // recounting, and why it is safe here specifically.** The argument that
    // rules out deltas everywhere else — six mutation sites, a missed
    // increment is a lease that never expires — does not apply to this one:
    // the delta is not inferred from a state machine, it is `count`, and
    // every entry counted in it was `has_deadline` on the way in and is
    // `Expired` on the way out, so the decrement is exact by construction and
    // provable in three lines.
    //
    // It is worth an exception because this is the ISR. Measured on the RV64
    // artifact: a full recount here costs **101 instructions** every time the
    // early exit does not fire, and it is what would have pushed the worst
    // case (16 leases expiring on one tick) from 349 instructions to 390.
    // With the delta the worst case is 289 — *below* the pre-change ceiling.
    if count != 0 {
        DEADLINE_LEASES.fetch_sub(count, Ordering::Release);
    }
    count
}

/// Diagnostic: count active leases.
pub fn lease_active_count() -> usize {
    LEASES.lock_irqsave().entries.iter().filter(|e| e.state == LeaseState::Active).count()
}

/// Diagnostic: the value of [`lease_tick`]'s early-exit counter — the number
/// of leases that are `Pending | Active` with a non-zero deadline.
///
/// Lock-free by design: this is exactly what the ISR reads. Exposed so a test
/// can assert the invariant `DEADLINE_LEASES == |{counted entries}|` directly
/// instead of inferring it from behaviour — a counter that is stale *upwards*
/// still ticks correctly and would hide behind a purely behavioural test,
/// while silently restoring the 157-instruction cost this counter removed.
pub fn lease_deadline_count() -> usize {
    DEADLINE_LEASES.load(Ordering::Relaxed)
}

/// Wipe the whole lease table. Host-test hygiene only — the suite shares one
/// static `LEASES`, so each test must start from a known state. Never built
/// into the kernel: a reachable "cancel every lease on the board" entry point
/// is exactly the cross-task teardown the ownership checks above close.
#[cfg(test)]
pub fn __lease_reset_for_tests() {
    let mut table = LEASES.lock_irqsave();
    for e in table.entries.iter_mut() {
        *e = LeaseEntry::empty();
    }
    refresh_deadline_count(&table);
}

/// Read a lease's state without going through the public API (host tests).
#[cfg(test)]
pub fn __lease_state_for_tests(lease_id: usize) -> LeaseState {
    if lease_id >= MAX_LEASES { return LeaseState::Free; }
    LEASES.lock_irqsave().entries[lease_id].state
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The lease table is one process-wide static, so the tests must not run
    /// concurrently. Same reason `__lease_reset_for_tests` exists.
    static SERIAL: Mutex<()> = Mutex::new(());

    const LESSOR: u32 = 1;
    const LESSEE: u32 = 2;
    const STRANGER: u32 = 3;

    /// Ring-3 identities are the default; `privileged` is passed explicitly.
    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        __lease_reset_for_tests();
        robot_os_sched::shim_reset();
        // Non-zero user page table = ring 3.
        robot_os_sched::shim_set_current(STRANGER, 0x1000);
        g
    }

    fn grant_and_accept(lessor: u32, lessee: u32) -> usize {
        let id = lease_grant(0, lessor, lessee, 0).expect("free lease slot");
        let (accepted, _shm) = lease_accept(lessee).expect("pending lease");
        assert_eq!(accepted, id);
        id
    }

    // ── IPC-6: lease_return is the lessee's ────────────────────────────────

    #[test]
    fn return_accepted_from_lessee_denied_from_anyone_else() {
        let _g = setup();
        // Walk several ids so a pass cannot be luck with slot 0.
        for shift in 0..4u32 {
            __lease_reset_for_tests();
            let lessor = LESSOR + shift * 10;
            let lessee = LESSEE + shift * 10;
            // Burn `shift` slots first so the id under test moves.
            for _ in 0..shift {
                lease_grant(0, 900, 901, 0).unwrap();
            }
            let id = grant_and_accept(lessor, lessee);
            assert_eq!(id, shift as usize);

            // A third party may not return it.
            assert!(lease_return(id, STRANGER, false).is_none());
            // Neither may the lessor: returning is the act of giving the
            // buffer back, and the lessor never had it.
            assert!(lease_return(id, lessor, false).is_none());
            // The lease is untouched — this is the half that matters. A
            // rejected call that still flipped the state would wake the
            // lessor while the real lessee still holds the pages.
            assert!(__lease_state_for_tests(id) == LeaseState::Active);
            assert!(!lease_is_returned(id));
            assert!(!robot_os_sched::shim_was_woken(lessor));

            // The legitimate lessee can.
            assert_eq!(lease_return(id, lessee, false), Some(lessor));
            assert!(__lease_state_for_tests(id) == LeaseState::Returned);
            assert!(lease_is_returned(id));
            assert!(robot_os_sched::shim_was_woken(lessor));
        }
    }

    #[test]
    fn return_is_bypassed_for_kernel_callers() {
        let _g = setup();
        let id = grant_and_accept(LESSOR, LESSEE);
        // House convention: kernel (`current_user_pt() == 0`) skips the check.
        assert_eq!(lease_return(id, STRANGER, true), Some(LESSOR));
        assert!(lease_is_returned(id));
    }

    #[test]
    fn return_rejects_non_active_states() {
        let _g = setup();
        // Pending: the lessee never accepted, so it never held the buffer.
        let pending = lease_grant(0, LESSOR, LESSEE, 0).unwrap();
        assert!(lease_return(pending, LESSEE, false).is_none());
        assert!(__lease_state_for_tests(pending) == LeaseState::Pending);

        // Free slot.
        assert!(lease_return(MAX_LEASES - 1, LESSEE, false).is_none());

        // A different lessee, because `lease_accept` takes the *first*
        // pending lease for a TID and slot 0 above is still outstanding.
        let id = grant_and_accept(LESSOR, LESSEE + 100);
        assert!(lease_return(id, LESSEE + 100, false).is_some());
        // Double-return must not re-wake the lessor a second time.
        assert!(lease_return(id, LESSEE + 100, false).is_none());
        assert_eq!(robot_os_sched::shim_wq_wakes().len(), 1);
    }

    // ── IPC-6: lease_free is the lessor's ──────────────────────────────────

    #[test]
    fn free_accepted_from_lessor_denied_from_anyone_else() {
        let _g = setup();
        for shift in 0..4u32 {
            __lease_reset_for_tests();
            let lessor = LESSOR + shift * 10;
            let lessee = LESSEE + shift * 10;
            for _ in 0..shift {
                lease_grant(0, 900, 901, 0).unwrap();
            }
            let id = grant_and_accept(lessor, lessee);

            assert!(!lease_free(id, STRANGER, false));
            assert!(!lease_free(id, lessee, false));
            // Denied means *nothing happened* — the slot is still the pair's.
            assert!(__lease_state_for_tests(id) == LeaseState::Active);

            assert!(lease_free(id, lessor, false));
            assert!(__lease_state_for_tests(id) == LeaseState::Free);
            // Freeing twice is not success.
            assert!(!lease_free(id, lessor, false));
        }
    }

    #[test]
    fn free_is_bypassed_for_kernel_callers() {
        let _g = setup();
        let id = grant_and_accept(LESSOR, LESSEE);
        assert!(lease_free(id, STRANGER, true));
        assert!(__lease_state_for_tests(id) == LeaseState::Free);
    }

    // ── IPC-6: lease_wait_return only for the lessor ───────────────────────

    #[test]
    fn wait_return_from_a_stranger_returns_without_blocking_or_boosting() {
        let _g = setup();
        let id = grant_and_accept(LESSOR, LESSEE);
        robot_os_sched::shim_set_priority(STRANGER, 1);
        robot_os_sched::shim_set_priority(LESSEE, 9);
        robot_os_sched::shim_set_current(STRANGER, 0x1000);
        // If the guard were missing this would donate STRANGER's priority to
        // LESSEE and then park on `wq_block_current`, which the shim panics
        // on — so reaching the assertions at all is part of the assertion.
        lease_wait_return(id);
        assert!(robot_os_sched::shim_boosts().is_empty());
        assert!(__lease_state_for_tests(id) == LeaseState::Active);
    }

    // ── Bounds: no reachable panic (panic = "abort" resets the board) ──────

    #[test]
    fn out_of_range_and_boundary_ids_never_panic() {
        let _g = setup();
        for id in [MAX_LEASES, MAX_LEASES + 1, usize::MAX, usize::MAX - 1] {
            assert!(lease_return(id, LESSEE, false).is_none());
            assert!(lease_return(id, LESSEE, true).is_none());
            assert!(!lease_free(id, LESSOR, false));
            assert!(!lease_free(id, LESSOR, true));
            assert!(!lease_is_returned(id));
            lease_wait_return(id); // must return, not block
        }
        // Last valid index must still behave, and a free slot must not block
        // `lease_wait_return` either.
        let last = MAX_LEASES - 1;
        assert!(!lease_is_returned(last));
        assert!(!lease_free(last, LESSOR, false));
        lease_wait_return(last);
    }

    // ── IPC-3: the lessee dies ─────────────────────────────────────────────

    #[test]
    fn lessee_death_expires_the_lease_and_wakes_the_lessor() {
        let _g = setup();
        let id = grant_and_accept(LESSOR, LESSEE);

        lease_release_all(LESSEE);

        // THE DECISION, PINNED: an abandoned buffer becomes `Expired`, never
        // `Returned`. `Returned` would tell the lessor the lessee handed the
        // pages back cleanly; it did not, it died holding them.
        assert!(__lease_state_for_tests(id) == LeaseState::Expired);
        // And the lessor's wait must actually terminate — a lessor asleep
        // forever is a control task that stops actuating.
        assert!(lease_is_returned(id));
        assert!(robot_os_sched::shim_wq_wakes().contains(&LESSOR));
        assert!(robot_os_sched::shim_fast_ipc_wakes().contains(&LESSOR));
        // The slot stays allocated so the lessor frees it through the normal
        // path, exactly like a timer expiry.
        assert!(lease_free(id, LESSOR, false));
    }

    #[test]
    fn lessee_death_while_still_pending_also_expires_and_wakes() {
        let _g = setup();
        // Never accepted: `lease_wait_return` blocks on Pending too, so this
        // strands the lessor just as thoroughly as the Active case.
        let id = lease_grant(0, LESSOR, LESSEE, 0).unwrap();
        lease_release_all(LESSEE);
        assert!(__lease_state_for_tests(id) == LeaseState::Expired);
        assert!(robot_os_sched::shim_was_woken(LESSOR));
    }

    // ── IPC-3: the lessor dies ─────────────────────────────────────────────

    #[test]
    fn lessor_death_frees_the_slot_and_wakes_a_pending_lessee() {
        let _g = setup();
        let id = lease_grant(0, LESSOR, LESSEE, 0).unwrap(); // Pending

        lease_release_all(LESSOR);

        // THE DECISION, PINNED: no lessor left to wake or to free the entry,
        // so the slot goes back to the table immediately.
        assert!(__lease_state_for_tests(id) == LeaseState::Free);
        // The would-be lessee is parked in SYS_IPC_LEASE_ACCEPT; wake it so it
        // gets an error instead of sleeping for the life of the board.
        assert!(robot_os_sched::shim_was_woken(LESSEE));
    }

    #[test]
    fn lessor_death_on_an_active_lease_frees_the_slot_without_waking() {
        let _g = setup();
        let id = grant_and_accept(LESSOR, LESSEE);
        lease_release_all(LESSOR);
        assert!(__lease_state_for_tests(id) == LeaseState::Free);
        // The lessee is not blocked on anything here — it holds the buffer.
        assert!(!robot_os_sched::shim_was_woken(LESSEE));
        // Its stale lease_id is now harmless: the slot may be re-granted to a
        // different pair, and the ownership check rejects the old lessee.
        let reused = lease_grant(0, 40, 41, 0).unwrap();
        assert_eq!(reused, id);
        let (_a, _s) = lease_accept(41).unwrap();
        assert!(lease_return(reused, LESSEE, false).is_none());
        assert!(__lease_state_for_tests(reused) == LeaseState::Active);
    }

    #[test]
    fn self_lease_is_freed_not_expired() {
        let _g = setup();
        let id = grant_and_accept(7, 7);
        lease_release_all(7);
        assert!(__lease_state_for_tests(id) == LeaseState::Free);
    }

    #[test]
    fn release_all_ignores_uninvolved_tasks_and_free_slots() {
        let _g = setup();
        let id = grant_and_accept(LESSOR, LESSEE);
        lease_release_all(999);
        assert!(__lease_state_for_tests(id) == LeaseState::Active);
        assert!(robot_os_sched::shim_wq_wakes().is_empty());
        // NO_TID must never be treated as an owner of the free slots.
        lease_release_all(NO_TID);
        assert!(__lease_state_for_tests(id) == LeaseState::Active);
        for i in 1..MAX_LEASES {
            assert!(__lease_state_for_tests(i) == LeaseState::Free);
        }
    }

    // ── IPC-3: the leak this closes ────────────────────────────────────────

    #[test]
    fn exhausting_the_table_then_killing_the_owner_makes_slots_grantable_again() {
        let _g = setup();
        for i in 0..MAX_LEASES {
            assert!(lease_grant(0, LESSOR, LESSEE, 0).is_some(), "slot {i}");
        }
        // Table full — this is the state a board reached permanently before
        // IPC-3, after MAX_LEASES tasks died holding a lease.
        assert!(lease_grant(0, LESSOR, LESSEE, 0).is_none());

        lease_release_all(LESSOR);

        assert!(lease_grant(0, 50, 51, 0).is_some());
        assert_eq!(lease_active_count(), 0);
    }

    #[test]
    fn exhausting_via_dead_lessees_is_reclaimed_by_the_lessor_freeing() {
        let _g = setup();
        for _ in 0..MAX_LEASES {
            let id = lease_grant(0, LESSOR, LESSEE, 0).unwrap();
            lease_accept(LESSEE).unwrap();
            let _ = id;
        }
        assert!(lease_grant(0, LESSOR, LESSEE, 0).is_none());
        // Lessee dies: entries become Expired but stay allocated by design.
        lease_release_all(LESSEE);
        assert!(lease_grant(0, LESSOR, LESSEE, 0).is_none());
        // The lessor then dies (or frees) and the table comes back.
        lease_release_all(LESSOR);
        assert!(lease_grant(0, 60, 61, 0).is_some());
    }

    // ── lease_tick ─────────────────────────────────────────────────────────

    /// Scratch buffer for the out-param, plus the ISR's own drain shape.
    fn tick(now: u64) -> Vec<u32> {
        let mut out = [NO_TID; MAX_LEASES];
        let n = lease_tick(now, &mut out);
        assert!(n <= MAX_LEASES);
        out[..n].to_vec()
    }

    #[test]
    fn tick_expires_active_and_pending_leases_past_their_deadline() {
        let _g = setup();
        // Accepted, with a deadline. Granted through the public API rather
        // than by poking `expire_ticks` under the lock: a direct write would
        // bypass `refresh_deadline_count` and leave the early-exit counter
        // stale — the test would then be exercising a state the kernel can
        // never reach.
        let active = lease_grant(0, LESSOR, LESSEE, 100).unwrap();
        assert_eq!(lease_accept(LESSEE).unwrap().0, active);
        let pending = lease_grant(0, 20, 21, 100).unwrap();
        let never = lease_grant(0, 30, 31, 0).unwrap(); // 0 = no expiry

        let expired = tick(200);

        assert!(__lease_state_for_tests(active) == LeaseState::Expired);
        // Pending used to be skipped, so a lessor that granted with a deadline
        // to a lessee that never accepted slept past its own deadline.
        assert!(__lease_state_for_tests(pending) == LeaseState::Expired);
        assert!(__lease_state_for_tests(never) == LeaseState::Pending);
        assert!(expired.contains(&LESSOR));
        assert!(expired.contains(&20));
        assert!(!expired.contains(&30), "a lease with no deadline was expired");
        assert_eq!(expired.len(), 2);
    }

    #[test]
    fn tick_before_the_deadline_changes_nothing() {
        let _g = setup();
        let id = lease_grant(0, LESSOR, LESSEE, 500).unwrap();
        assert!(tick(499).is_empty());
        assert!(__lease_state_for_tests(id) == LeaseState::Pending);
    }

    // ── The early-exit counter (the 157-instruction fix) ───────────────────
    //
    // The counter is what lets `lease_tick` return before it touches the
    // lock. If it can ever read low while a deadline is live, a lease stops
    // expiring and `lease_wait_return` sleeps past the bound `expire_ticks`
    // exists to provide — so the invariant is asserted directly, on every
    // path that writes lease state, and not merely inferred from behaviour.

    /// Ground truth, recomputed from the table by a route that shares no code
    /// with `refresh_deadline_count`.
    fn counted_by_hand() -> usize {
        let t = LEASES.lock_irqsave();
        t.entries
            .iter()
            .filter(|e| {
                (e.state == LeaseState::Pending || e.state == LeaseState::Active)
                    && e.expire_ticks != 0
            })
            .count()
    }

    fn assert_counter_agrees(what: &str) {
        assert_eq!(
            lease_deadline_count(),
            counted_by_hand(),
            "DEADLINE_LEASES drifted from the table after {what}"
        );
    }

    #[test]
    fn the_early_exit_counter_tracks_every_state_transition() {
        let _g = setup();
        assert_eq!(lease_deadline_count(), 0);

        let a = lease_grant(0, LESSOR, LESSEE, 500).unwrap();
        assert_counter_agrees("grant with a deadline");
        assert_eq!(lease_deadline_count(), 1);

        // A lease with no deadline is invisible to the ISR and must not arm it.
        let b = lease_grant(0, 40, 41, 0).unwrap();
        assert_counter_agrees("grant without a deadline");
        assert_eq!(lease_deadline_count(), 1);

        // Pending → Active keeps the deadline live.
        assert_eq!(lease_accept(LESSEE).unwrap().0, a);
        assert_counter_agrees("accept");
        assert_eq!(lease_deadline_count(), 1);

        // Active → Returned retires it.
        assert_eq!(lease_return(a, LESSEE, false), Some(LESSOR));
        assert_counter_agrees("return");
        assert_eq!(lease_deadline_count(), 0);

        // ...and with the counter at zero the ISR really does nothing.
        assert!(tick(u64::MAX).is_empty());

        // Free of a deadline-less lease leaves it at zero.
        assert!(lease_free(b, 40, false));
        assert_counter_agrees("free");

        // Expiry through the tick itself retires the deadline.
        lease_grant(0, 50, 51, 10).unwrap();
        assert_eq!(lease_deadline_count(), 1);
        assert_eq!(tick(10), vec![50]);
        assert_counter_agrees("tick expiry");
        assert_eq!(lease_deadline_count(), 0);

        // Free of a still-armed lease retires it too.
        let d = lease_grant(0, 60, 61, 900).unwrap();
        assert_eq!(lease_deadline_count(), 1);
        assert!(lease_free(d, 60, false));
        assert_counter_agrees("free of an armed lease");
        assert_eq!(lease_deadline_count(), 0);

        // And both branches of the task-exit sweep.
        let e = lease_grant(0, 70, 71, 900).unwrap();
        let f = lease_grant(0, 80, 81, 900).unwrap();
        assert_eq!(lease_deadline_count(), 2);
        lease_release_all(71); // lessee dies → Expired
        assert_counter_agrees("lessee exit");
        assert_eq!(lease_deadline_count(), 1);
        lease_release_all(80); // lessor dies → slot freed
        assert_counter_agrees("lessor exit");
        assert_eq!(lease_deadline_count(), 0);
        let _ = (e, f);
    }

    /// A full table of armed leases: the counter saturates at `MAX_LEASES`,
    /// the tick writes exactly `MAX_LEASES` TIDs, and nothing overruns the
    /// caller's buffer.
    #[test]
    fn a_full_table_expiring_at_once_fills_the_buffer_exactly() {
        let _g = setup();
        for i in 0..MAX_LEASES {
            lease_grant(0, 100 + i as u32, 200 + i as u32, 5).unwrap();
        }
        assert_eq!(lease_deadline_count(), MAX_LEASES);

        let mut out = [NO_TID; MAX_LEASES];
        let n = lease_tick(5, &mut out);
        assert_eq!(n, MAX_LEASES);
        for i in 0..MAX_LEASES {
            assert_eq!(out[i], 100 + i as u32);
        }
        assert_eq!(lease_deadline_count(), 0);
        // Idempotent: a second tick expires nothing and takes the early exit.
        assert!(tick(u64::MAX).is_empty());
    }

    /// The early exit must never skip a deadline that is genuinely live: with
    /// the counter armed, every `now` from before to after the deadline
    /// behaves exactly as the unconditional loop would have.
    #[test]
    fn the_early_exit_never_skips_a_live_deadline() {
        let _g = setup();
        for deadline in [1u64, 2, 1000, u64::MAX] {
            __lease_reset_for_tests();
            let id = lease_grant(0, LESSOR, LESSEE, deadline).unwrap();
            assert_eq!(lease_deadline_count(), 1);
            // Every tick strictly before the deadline leaves it armed.
            for now in [0u64, deadline.saturating_sub(1)] {
                if now < deadline {
                    assert!(tick(now).is_empty(), "deadline {deadline} fired early at {now}");
                    assert_eq!(lease_deadline_count(), 1);
                }
            }
            // The tick *at* the deadline fires it (`now >= expire`).
            assert_eq!(tick(deadline), vec![LESSOR]);
            assert!(__lease_state_for_tests(id) == LeaseState::Expired);
            assert_eq!(lease_deadline_count(), 0);
        }
    }
}
