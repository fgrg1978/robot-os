//! Per-task capability tables, indexed by **task pool slot**.
//!
//! Each task gets its own [`crate::cap::CapTable`]; the kernel reaches
//! it through this module's accessors. The table is **dense**: a fixed
//! `[SpinLock<CapTable>; MAX_TASKS]` lives in BSS and survives the
//! lifetime of the kernel.
//!
//! Memory cost: `MAX_TASKS × sizeof(CapTable)` ≈ 64 × ~2 KiB = 128 KiB
//! on default builds; configs that lower `MAX_TASKS` scale down linearly.
//!
//! # Why slot-indexed and not TID-indexed (W3-F4)
//!
//! These tables used to be indexed by TID, guarded by
//! `is_valid_tid(tid) = (tid as usize) < MAX_TASKS`. TIDs are **monotone**:
//! `scheduler.rs` does `NEXT_TID = NEXT_TID.wrapping_add(1)` and never
//! reissues a low value until it has wrapped 2^32. `MAX_TASKS` is 64. So
//! from the 64th task creation onward — trivially reached, `fork()` is
//! unprivileged — every `grant` / `get` / `with_table` returned `None` and
//! every typed-cap syscall answered `EINVAL`. Fail-closed, so not an
//! escalation; but it silently *disabled the better mechanism*: the typed
//! `Cap<T>` path that carries the Kani proofs became unreachable on any
//! long-running robot, and everything fell back to the legacy global handle
//! table with its guessable indices.
//!
//! The pool slot index (`0..MAX_TASKS`) is the quantity that is actually
//! bounded by `MAX_TASKS`, so that is what indexes the array. The cost is a
//! `robot_os_sched::idx_for_tid` lookup — an O(64) unsynchronised scan of
//! `TASK_VALID`/`TASKS`, the same scan the APS dispatch path already does —
//! on every capability operation. No locks and no interrupt toggling, unlike
//! the legacy handle-table scan it replaces.
//!
//! # Slot reuse
//!
//! Slot indices, unlike TIDs, *are* recycled. [`OWNER`] records which TID a
//! slot's table currently belongs to and every accessor lazily wipes the
//! table when it finds a mismatch — so a new task can never inherit the
//! previous occupant's caps, even on a path that skips [`reset`]. This is
//! deliberate belt-and-braces: `crates/sched`'s task-creation path cannot be
//! modified from here, so correctness must not depend on it calling us.
//!
//! # Lifecycle
//!
//! - Every pool slot has an always-present, initially-empty `CapTable`
//!   from boot.
//! - `task_exit` calls [`crate::task_release_all`], which calls [`reset`]
//!   — see that function's doc for the ordering constraint that makes it
//!   land on the right slot.
//! - Any accessor that observes a slot whose recorded owner differs from
//!   the TID being looked up wipes it first (see above).
//!
//! # Concurrency
//!
//! One spinlock per slot. Different tasks acquiring different slots
//! never contend. The same task's syscall path is single-threaded
//! per CPU, so contention on a single slot is rare.

use core::sync::atomic::{AtomicU32, Ordering};

use robot_os_sched::task::MAX_TASKS;
use robot_os_sync::spinlock::SpinLock;

use crate::cap::{Cap, CapError, CapHandle, CapPerms, CapTable, CapTarget};

/// Static per-slot capability tables.
const FRESH_TABLE: SpinLock<CapTable> = SpinLock::new(CapTable::empty());
static CAP_TABLES: [SpinLock<CapTable>; MAX_TASKS] = [FRESH_TABLE; MAX_TASKS];

/// TID currently owning each slot's table. `NO_OWNER` = never used.
///
/// TID 0 is the "no current task" sentinel returned by
/// `current_task_tid()`, and `NEXT_TID` starts at 1 and skips 0 on wrap, so
/// 0 can never be a live task's TID and is safe as the vacant marker.
const NO_OWNER: u32 = 0;
const FRESH_OWNER: AtomicU32 = AtomicU32::new(NO_OWNER);
static OWNER: [AtomicU32; MAX_TASKS] = [FRESH_OWNER; MAX_TASKS];

/// Resolve `tid` to its task-pool slot index, wiping the slot's table first
/// if it still belongs to a previous occupant.
///
/// Returns `None` for a TID with no live task — including TID 0 (idle /
/// "no current task"). That is fail-closed and correct: nothing in
/// `kernel/src` grants typed caps before the first task exists.
///
/// **On the unsynchronised scan.** `idx_for_tid` reads `TASK_VALID`/`TASKS`
/// without `PoolGuard`, by the same convention the APS dispatch path and
/// `wake_by_rpc` already use — this change puts that read on every
/// typed-cap syscall, so the reasoning deserves to be written down.
/// `alloc_slot` sets `TASK_VALID[i] = true` *before* `task.tid` is assigned,
/// so a concurrent scan can see a claimed slot still carrying its previous
/// occupant's TID. That is harmless here only because TIDs are **monotone**:
/// a slot is freed by `do_schedule` after the exiting task is left for good,
/// so a stale slot value is always a dead TID, and `slot_for` is only ever
/// called with a live one (`current_task_tid()`, or the exiting TID at hook
/// time while its slot is still valid). Correctness rests on that
/// monotonicity, not on the read being atomic — so the 2^32 TID wrap that
/// `scheduler.rs` already documents as accepted is the one case where a scan
/// could match the wrong slot.
fn slot_for(tid: u32) -> Option<usize> {
    let idx = resolve_only(tid)?;
    claim_slot(idx, tid);
    Some(idx)
}

/// The lookup half of [`slot_for`], with **no side effects**.
///
/// Separate from [`claim_slot`] because [`delegate`] has to cross-check two
/// resolutions against each other before either of them is allowed to wipe a
/// table. Merging the two, as this module did originally, means a resolution
/// that is about to be rejected has already destroyed something.
fn resolve_only(tid: u32) -> Option<usize> {
    if tid == NO_OWNER {
        return None;
    }
    let idx = robot_os_sched::idx_for_tid(tid)?;
    if idx >= MAX_TASKS {
        return None; // defensive; idx_for_tid already bounds this
    }
    Some(idx)
}

/// Register `tid` as the owner of `idx`, wiping the table if it belonged to a
/// previous occupant.
///
/// Split out of [`slot_for`] so [`resolve_only_untrusted`] can run its
/// confirmation pass **before** any mutation happens. Order matters: the wipe
/// is destructive, so a resolution that turns out to be wrong must be
/// discarded before this is called, not after.
fn claim_slot(idx: usize, tid: u32) {
    // Lazy reset on slot reuse. `swap` makes the claim atomic against
    // another hart resolving the same slot concurrently: exactly one caller
    // observes the stale owner and performs the wipe.
    let prev = OWNER[idx].swap(tid, Ordering::AcqRel);
    if prev != tid {
        *CAP_TABLES[idx].lock() = CapTable::empty();
        // Fresh occupant, fresh delegation quota — the counter tracks one
        // table lifetime (see the quota block below).
        INBOUND_DELEGATIONS[idx].store(0, Ordering::Relaxed);
    }
}

/// Look up a TID that came from **ring 3**, not from `current_task_tid()`.
/// Side-effect-free, like [`resolve_only`]; the caller claims the slot.
///
/// **WHY this exists, and why the hot path does not use it (W3-F10).**
/// [`slot_for`]'s safety argument is "the caller always names a *live* TID".
/// It holds for every caller that exists today: every `cap_store` entry
/// reachable from a syscall passes `robot_os_sched::current_task_tid()`, and
/// `reset` gets the exiting TID while its slot is still valid. Under that
/// premise the unsynchronised `idx_for_tid` scan can only return the right
/// slot or `None`, because the stale value a mid-allocation slot still
/// carries is the *previous* occupant's TID, and TIDs are monotone — a dead
/// TID never equals a live one.
///
/// `delegate` breaks that premise: it is the first path where the TID being
/// resolved is an integer chosen by the caller. A caller that names a
/// recently-dead TID can hit the window in `robot_os_sched::alloc_slot`,
/// which publishes `TASK_VALID[i] = true` *before* the creating task writes
/// `TASKS[i].tid`. During those few instructions a scan matches slot `i` on
/// the dead TID — and slot `i` now belongs to a brand-new, live task. The
/// consequence in this module is not capability theft (the [`OWNER`]
/// mismatch wipes the table before anything can be read out of it) but
/// capability *destruction*: an operation attributed to a dead TID silently
/// empties a live task's cap table.
///
/// The confirmation pass below re-runs the scan and refuses to proceed
/// unless both passes agree. Originally that only **narrowed** the window;
/// the root fix has since landed in `crates/sched` (2026-08-23): the tid
/// sentinel protocol in `alloc_slot`/the free sites publishes `tid = 0`
/// before `TASK_VALID` (Release-fenced, mirrored on free), and
/// `idx_for_tid` revalidates a match behind an Acquire fence — a dead TID
/// can no longer match a mid-allocation slot at all. The double scan here
/// STAYS, deliberately: it is cheap, it is the only layer this module owns,
/// and it keeps containing the damage if the sched-side protocol is ever
/// weakened by an edit that doesn't know about it.
///
/// The mitigation depends on the two scans surviving as two scans, which is
/// not something the language promises: `idx_for_tid` reads `static mut`
/// non-atomically, so a compiler that inlines it is formally entitled to
/// keep the loads. Checked, not assumed — disassembling `delegate` out of
/// `librobot_os_ipc.rlib` at the project's release profile (`opt-level = 2`,
/// `lto = false`) shows `idx_for_tid` inlined as **three** separate loops
/// over `TASK_VALID`/`TASKS`: one for the grantor and two for the target.
/// Turning LTO on is the change that could invalidate that, so re-check
/// there before trusting this comment.
///
/// Cost: one extra O(`MAX_TASKS`) scan, ~190 ns measured for a full sweep,
/// paid only by `delegate` — never by `get`/`grant`/`with_table` on the
/// typed-cap fast path.
fn resolve_only_untrusted(tid: u32) -> Option<usize> {
    let first = resolve_only(tid)?;
    // Confirmation pass. Deliberately re-reads through `idx_for_tid` rather
    // than a slot→TID lookup: this module must not add a symbol the host
    // test shims do not already provide.
    if resolve_only(tid)? != first {
        return None;
    }
    Some(first)
}

/// Returns `true` iff `tid` currently maps to a live task-pool slot.
///
/// Kept under the historical name so existing callers compile, but the
/// meaning changed with W3-F4: it is no longer "the integer is small
/// enough", it is "this TID names a live task".
#[inline]
pub fn is_valid_tid(tid: u32) -> bool {
    slot_for(tid).is_some()
}

/// Look up a capability for the named task.
///
/// Wraps [`CapTable::get`]; returns `Err(CapError::Stale)` if `tid`
/// does not name a live task.
pub fn get<T: CapTarget>(
    tid: u32,
    cap: Cap<T>,
    need: CapPerms,
) -> Result<u32, CapError> {
    let idx = match slot_for(tid) {
        Some(i) => i,
        None => return Err(CapError::Stale),
    };
    let table = CAP_TABLES[idx].lock();
    table.get(cap, need)
}

/// Mint a new typed capability into `tid`'s table.
///
/// Returns `None` if the slot table is full or `tid` names no live task.
pub fn grant<T: CapTarget>(
    tid: u32,
    perms: CapPerms,
    resource: u32,
) -> Option<Cap<T>> {
    let idx = slot_for(tid)?;
    let mut table = CAP_TABLES[idx].lock();
    table.grant(perms, resource)
}

/// Revoke a single cap.
pub fn revoke<T: CapTarget>(tid: u32, cap: Cap<T>) {
    let idx = match slot_for(tid) {
        Some(i) => i,
        None => return,
    };
    let mut table = CAP_TABLES[idx].lock();
    table.revoke(cap);
}

/// Wipe the whole table for a task — called from task exit via
/// [`crate::task_release_all`].
///
/// **Ordering constraint (W3-F7):** this resolves `tid` through
/// `idx_for_tid`, which only succeeds while the task's pool slot is still
/// `TASK_VALID`. `scheduler::task_exit` fires the exit hook *before* marking
/// the task `Zombie`, and `do_schedule` is what actually frees the slot —
/// so the lookup succeeds and the wipe lands on the right slot. If the hook
/// is ever moved after the slot is freed, this becomes a silent no-op and
/// typed caps stop being revoked on exit. The [`OWNER`] lazy-reset above is
/// the backstop for exactly that failure, but do not rely on it: a slot that
/// is never reused would keep a dead task's caps live indefinitely.
pub fn reset(tid: u32) {
    let idx = match slot_for(tid) {
        Some(i) => i,
        None => return,
    };
    let mut table = CAP_TABLES[idx].lock();
    *table = CapTable::empty();
    // Table emptied → its inbound-delegation quota resets with it.
    INBOUND_DELEGATIONS[idx].store(0, Ordering::Relaxed);
    // Release the slot claim so the next occupant re-registers cleanly.
    OWNER[idx].store(NO_OWNER, Ordering::Release);
}

/// Borrow the cap-table for the given task and run a closure on it.
///
/// Used by syscall handlers that need direct access (e.g. to compute
/// multiple cap_table.get() calls atomically without re-locking).
pub fn with_table<R>(tid: u32, f: impl FnOnce(&mut CapTable) -> R) -> Option<R> {
    let idx = slot_for(tid)?;
    let mut table = CAP_TABLES[idx].lock();
    Some(f(&mut *table))
}

// ──────────────────────────────────────────────────────────────────────────
// Delegation — the kernel side of SYS_CAP_GRANT (W3-F10)
// ──────────────────────────────────────────────────────────────────────────
//
// Until now ring 3 could not mint a capability at all: the only producer was
// `kernel_grant_channel_cap` off the boot seed. That is why the typed `Cap<T>`
// path could not replace the legacy `HANDLES` table — a task that receives a
// channel has no way to hand the sender an authenticated endpoint, so
// `channel_send` stayed on guessable global indices forever.
//
// `delegate` is the minimum that unblocks it: a task passes on authority it
// already holds, attenuated. It is not a general "grant" — nothing here can
// create authority that did not already exist in the grantor's own table.
//
// **Inbound-delegation quota (CLOSED 2026-08-23).** Any task may delegate
// into any live task's table, so any task holding one delegable cap could
// spend 256 delegations filling a victim's table and make the victim's next
// `grant` return `EMFILE` — an availability attack, not an escalation (the
// victim keeps every cap it already had, and cannot be made to *use* a
// planted one, since using a cap requires knowing its handle value).
//
// Bounded by [`INBOUND_DELEGATIONS`]: a per-target counter of CROSS-task
// delegations accepted over the table's lifetime, capped at
// [`MAX_INBOUND_DELEGATIONS`]. Design points, each load-bearing:
//
//  * **Cross-task only.** Self-delegation (attenuating your own cap) spends
//    your own slots; counting it would let a task DoS itself into losing a
//    feature, for no protection gained.
//  * **Monotone per table lifetime, not per live count.** Decrementing on
//    revoke would let an attacker fill-revoke-fill forever, turning the
//    bound into a rate limit that resets for free. The counter clears only
//    where the table itself is emptied: [`reset`] (task exit) and the
//    owner-mismatch wipe in [`claim_slot`] (slot reuse — which is also the
//    path `delegate` itself claims through, the third point named by the
//    original TODO).
//  * **Checked and bumped under the TARGET's table lock**, which `delegate`
//    already holds across the mint — so check and increment cannot race
//    another delegation to the same target. The clears don't take the lock
//    (they pair with table-emptying sites that do their own locking); a
//    theoretical clear-vs-bump interleave costs one quota unit of accuracy
//    on a table that was being destroyed anyway, never unsoundness.
//  * **64 = 256/4.** A quarter of the table can be inbound; the victim
//    always keeps ≥192 slots for its own grants. No legitimate pattern in
//    the tree delegates more than a handful of caps to one task.
//
// The principled endgame is unchanged and still written down: require the
// grantor to hold a `Cap<Task>` for the target, making "who may delegate
// into me" an authority question instead of a counter. Nothing mints
// `Cap<Task>` today, which is why it is not the shipped rule.

/// Hard cap on cross-task delegations one target table accepts per lifetime.
pub const MAX_INBOUND_DELEGATIONS: u16 = 64;

const FRESH_INBOUND: core::sync::atomic::AtomicU16 =
    core::sync::atomic::AtomicU16::new(0);
/// Cross-task delegations accepted into each slot's table since it was last
/// emptied. See the quota block above for the full contract.
static INBOUND_DELEGATIONS: [core::sync::atomic::AtomicU16; MAX_TASKS] =
    [FRESH_INBOUND; MAX_TASKS];

/// Why a delegation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DelegateError {
    /// The grantor's own table does not validate the named handle. Carries
    /// the underlying [`CapError`] so the syscall layer can keep the
    /// `ECAPSTALE` / `ECAPKIND` distinction the typed path already reports.
    ///
    /// **This is also the non-owner rejection.** A task that names another
    /// task's handle bits is looking those bits up in *its own* table, where
    /// they are empty or carry a different generation.
    Source(CapError),
    /// The source cap does not carry [`CapPerms::DUP`], so it may not be
    /// passed on.
    NotDelegable,
    /// The requested permissions are not a subset of the source's.
    Amplify,
    /// `want_perms` is empty — an inert cap that would still burn a slot in
    /// the target's table.
    EmptyPerms,
    /// The grantor TID names no live task.
    NoGrantor,
    /// The target TID names no live task, or failed the confirmation pass in
    /// [`resolve_only_untrusted`].
    NoTarget,
    /// Two different TIDs resolved to the same task-pool slot — the
    /// unsynchronised-scan failure this module documents. Fail closed.
    SlotAlias,
    /// The target's cap table is full.
    TargetFull,
    /// The target has already accepted [`MAX_INBOUND_DELEGATIONS`] cross-task
    /// delegations in this table lifetime — the anti-fill bound, see the
    /// quota block above. Self-delegations never hit this.
    QuotaExhausted,
}

impl From<CapError> for DelegateError {
    fn from(e: CapError) -> Self {
        Self::Source(e)
    }
}

/// Delegate a capability the grantor already holds to another task.
///
/// Returns the wire handle **as it will be seen in the target's table**. The
/// grantor's own cap is untouched.
///
/// # The four rules, and why each one is there
///
/// **1. Never amplify.** `want_perms` must be a subset of what the source
/// slot holds, or the call fails with [`DelegateError::Amplify`]. This is the
/// same invariant `handle_dup` in [`crate::handle`] states as "permissions
/// are copied verbatim, never widened" — but expressed as an explicit
/// refusal rather than a verbatim copy, because verbatim is exactly what
/// makes `handle_dup` unable to attenuate: every dup of a `duplicate`-flagged
/// handle is itself infinitely re-dupable. Refusing (rather than silently
/// intersecting) matters because a caller that asked for `RW` and got `R`
/// would go on to use it as `RW` and fail later, somewhere with less context.
///
/// **2. Re-delegation is opt-in, not inherited.** The source must carry
/// [`CapPerms::DUP`]; the *copy* carries DUP only if the grantor explicitly
/// asked for it in `want_perms`. So the default outcome of a delegation is a
/// **leaf** capability — usable, not passable — and a chain only exists where
/// someone deliberately built one. Without this the blast radius of handing a
/// cap to one task is every task it can ever talk to. The bit already exists
/// in the ABI (`CapPerms::DUP`, bit 3) and was unused; this is its first
/// consumer.
///
/// **3. Revocation does not propagate.** The delegated cap lands in a fresh
/// slot of the target's table with its own generation. If the grantor later
/// revokes its own cap, or is revoked, the delegate keeps working until the
/// target exits (`task_release_all` → [`reset`]) or revokes it itself.
/// Delegation is therefore a **transfer of authority, not a lease** — the
/// codebase already has a mechanism for time-bounded authority
/// (`crates/ipc/src/lease.rs`) and this is deliberately not a second one.
/// The alternative, a seL4-style derivation tree, costs a parent link per
/// slot (`MAX_TASKS × MAX_CAPS_PER_TASK` = 16 384 slots × 4 B = 64 KiB more
/// BSS, on top of the 128 KiB these tables already take) and turns revoke
/// from O(1) into a sweep of every table (64 lock acquisitions, 16 384 slot
/// visits) repeated once per chain level. Rule 2 is what keeps that
/// affordable: leaf-by-default means chains are rare and shallow.
///
/// **4. The target must be a live task.** `handle_dup`'s comment names this
/// exact failure — "a stale-TID plant of the exact shape this kernel's slot
/// reuse is prone to" — and its answer was to forbid cross-task delegation
/// from ring 3 entirely (`new_owner_tid == caller_tid`). That answer is not
/// available here: cross-task delegation *is* the feature. So instead the
/// target goes through [`resolve_only_untrusted`], and a grantor/target pair that
/// resolves to the same pool slot while naming different TIDs is refused
/// outright ([`DelegateError::SlotAlias`]) rather than trusted. Read that
/// function's doc for what this still does not close.
///
/// # Locking
///
/// Two tables, two spinlocks, and `SpinLock` is not reentrant — the trap
/// `handle_dup` fell into and had to work around by dropping its guard before
/// re-locking, which left it with a TOCTOU window where the source could be
/// revoked between the read and the mint. Here both slots are resolved first
/// (resolution itself may take a table lock, to wipe a reused slot), then the
/// two locks are taken in **ascending slot order** — deadlock-free against
/// any other pair doing the same — and held across the whole read-check-mint.
/// Self-delegation collapses to a single lock.
pub fn delegate(
    grantor_tid: u32,
    target_tid: u32,
    cap_raw: CapHandle,
    want_perms: CapPerms,
) -> Result<CapHandle, DelegateError> {
    // An all-zero mask mints a cap that grants nothing and can never be
    // upgraded — a pure waste of one of the target's 256 slots, so it is
    // refused as malformed input. To be clear about what this is NOT: it is
    // hygiene, not a defence against table exhaustion. An attacker who can
    // delegate at all can delegate `READ` 256 times and fill any table it can
    // name. See the module TODO on inbound-delegation quotas.
    if want_perms.bits() == 0 {
        return Err(DelegateError::EmptyPerms);
    }

    // Resolve BOTH tids before either claim. `claim_slot` wipes on owner
    // mismatch, and a resolution that is about to be rejected as an alias
    // must not have destroyed a table on its way to being rejected.
    //
    // The grantor comes from `current_task_tid()` in the syscall layer, so it
    // is live by construction — the plain lookup is correct and cheaper.
    let idx_g = resolve_only(grantor_tid).ok_or(DelegateError::NoGrantor)?;
    // The target is an integer chosen by ring 3. Confirmed lookup.
    let idx_t = resolve_only_untrusted(target_tid).ok_or(DelegateError::NoTarget)?;

    // Two distinct TIDs must never resolve to one slot. If they do, the
    // unsynchronised scan lied to us and one of the two tables about to be
    // written is not the one the caller named.
    if grantor_tid != target_tid && idx_g == idx_t {
        return Err(DelegateError::SlotAlias);
    }

    // Now that the pair is coherent, register ownership (and wipe a slot that
    // still belongs to a previous occupant).
    claim_slot(idx_g, grantor_tid);
    if idx_t != idx_g {
        claim_slot(idx_t, target_tid);
    }

    if idx_g == idx_t {
        // Self-delegation: attenuating your own cap into a weaker one, e.g.
        // to hold an RO copy you can hand out later. One lock — taking it
        // twice would deadlock a spinlock that is not reentrant.
        let mut table = CAP_TABLES[idx_g].lock();
        let (kind, perms, resource) = table.inspect_raw(cap_raw)?;
        check_delegable(perms, want_perms)?;
        return table
            .grant_raw(kind, want_perms, resource)
            .ok_or(DelegateError::TargetFull);
    }

    // Ascending-slot-order acquisition: any two harts delegating between the
    // same pair of tasks take the same two locks in the same order.
    let (lo, hi) = if idx_g < idx_t { (idx_g, idx_t) } else { (idx_t, idx_g) };
    let mut lo_guard = CAP_TABLES[lo].lock();
    let mut hi_guard = CAP_TABLES[hi].lock();
    let (src, dst) = if idx_g == lo {
        (&mut *lo_guard, &mut *hi_guard)
    } else {
        (&mut *hi_guard, &mut *lo_guard)
    };

    let (kind, perms, resource) = src.inspect_raw(cap_raw)?;
    check_delegable(perms, want_perms)?;
    // Inbound quota — checked and bumped while holding the TARGET's table
    // lock (this section holds both), so two grantors racing the same
    // victim serialize here and the bound is exact. Checked AFTER the
    // source-side refusals so a rejected delegation never charges the
    // target, and bumped only when the mint below succeeds.
    if INBOUND_DELEGATIONS[idx_t].load(Ordering::Relaxed) >= MAX_INBOUND_DELEGATIONS {
        return Err(DelegateError::QuotaExhausted);
    }
    let handle = dst
        .grant_raw(kind, want_perms, resource)
        .ok_or(DelegateError::TargetFull)?;
    INBOUND_DELEGATIONS[idx_t].fetch_add(1, Ordering::Relaxed);
    Ok(handle)
}

/// The two permission rules, shared by both locking paths so they cannot
/// drift apart: the source must be delegable at all, and the request must be
/// an attenuation of it.
#[inline]
fn check_delegable(held: CapPerms, want: CapPerms) -> Result<(), DelegateError> {
    if !held.contains(CapPerms::DUP) {
        return Err(DelegateError::NotDelegable);
    }
    // `contains` is `held & want == want`, i.e. `want ⊆ held`. Checked
    // against the *slot's* permissions, never against the bits in the wire
    // handle — those are attacker-controlled and `inspect_raw` only uses
    // them for the generation/kind match.
    if !held.contains(want) {
        return Err(DelegateError::Amplify);
    }
    Ok(())
}

/// Number of occupied slots for the named task.
pub fn occupied(tid: u32) -> usize {
    match slot_for(tid) {
        Some(idx) => CAP_TABLES[idx].lock().occupied(),
        None => 0,
    }
}
