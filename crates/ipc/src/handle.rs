//! Handle-based capability system (AQ6).
//!
//! Inspired by Zircon and seL4. Every resource access is via a handle —
//! an opaque per-process integer. A process can only use resources it has
//! been explicitly granted handles for.
//!
//! Handle types: Sensor, Gpio, I2c, Pwm, Motor, Irq, MmioRegion.

use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum handle entries system-wide.
pub const MAX_HANDLES_GLOBAL: usize = 256;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What kind of resource a handle refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandleKind {
    None,
    Sensor(u8),          // sensor type ID
    Gpio(u32),           // pin number
    I2c(u8, u8),         // bus, address
    Pwm(u8),             // channel
    Motor(u32),          // motor ID
    Irq(u32),            // PLIC IRQ number
    MmioRegion(usize, usize), // phys base, size
}

/// Permission flags for a handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HandlePerms {
    pub read: bool,
    pub write: bool,
    pub execute: bool,   // for MMIO: can map in user PT
    pub duplicate: bool, // can dup to child process
}

impl HandlePerms {
    pub const RO: Self = Self { read: true, write: false, execute: false, duplicate: false };
    pub const RW: Self = Self { read: true, write: true, execute: false, duplicate: false };
    pub const ALL: Self = Self { read: true, write: true, execute: true, duplicate: true };
}

/// A handle entry in the global table.
#[derive(Clone, Copy)]
pub struct HandleEntry {
    pub kind: HandleKind,
    pub perms: HandlePerms,
    pub owner_task: u32,   // TID of owning process
    pub valid: bool,
}

impl HandleEntry {
    pub const fn empty() -> Self {
        Self {
            kind: HandleKind::None,
            perms: HandlePerms::RO,
            owner_task: 0,
            valid: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Global handle table
// ---------------------------------------------------------------------------

/// Global handle table.
///
/// Protected by a single `SpinLock` (same shape as `port.rs`'s `PORTS`).
/// Was a bare `static mut` wired directly to syscalls (`SYS_HANDLE_GRANT`/
/// `_REVOKE`/`_DUP`), reachable concurrently from any hart with zero
/// synchronization — two harts granting/revoking at once could tear an
/// entry or claim the same free slot. `lock_irqsave()` throughout, same
/// discipline as `PORTS` / `SHM_REGIONS` / `IRQ_BINDINGS`.
const EMPTY_HANDLE: HandleEntry = HandleEntry::empty();
static HANDLES: SpinLock<[HandleEntry; MAX_HANDLES_GLOBAL]> =
    SpinLock::new([EMPTY_HANDLE; MAX_HANDLES_GLOBAL]);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Grant a handle to a task. Returns handle ID or None.
pub fn handle_grant(owner_tid: u32, kind: HandleKind, perms: HandlePerms) -> Option<u32> {
    let mut handles = HANDLES.lock_irqsave();
    for i in 0..MAX_HANDLES_GLOBAL {
        if !handles[i].valid {
            handles[i] = HandleEntry {
                kind,
                perms,
                owner_task: owner_tid,
                valid: true,
            };
            return Some(i as u32);
        }
    }
    None
}

/// Revoke a handle.
///
/// **Authority (W2-C3 fix):** the `HANDLES` table is *global* (indices
/// `0..MAX_HANDLES_GLOBAL` are shared across every task) and `owner_task`
/// is just a field, so a raw index is trivially guessable. A non-privileged
/// (userspace) caller may therefore revoke **only a handle it owns**; without
/// this check any task could revoke another task's driver handles by index —
/// a cross-task integrity/availability attack. Privileged (kernel, `user_pt
/// == 0`) callers bypass the ownership check, matching the kernel-full-access
/// convention used by `cap_check` and `handle_grant`.
///
/// Returns `true` iff a valid, authorized entry was cleared.
pub fn handle_revoke(handle_id: u32, caller_tid: u32, privileged: bool) -> bool {
    if handle_id as usize >= MAX_HANDLES_GLOBAL { return false; }
    let mut handles = HANDLES.lock_irqsave();
    let entry = &handles[handle_id as usize];
    if !entry.valid { return false; }
    // Non-privileged callers may revoke only their own handles.
    if !privileged && entry.owner_task != caller_tid { return false; }
    handles[handle_id as usize] = HandleEntry::empty();
    true
}

/// Duplicate a handle for another task (if dup permission set).
///
/// **Authority (W2-C3 fix):** previously this checked only that the *source*
/// entry was valid and carried the `duplicate` permission — it never checked
/// that the **caller** owned the source, nor constrained `new_owner_tid`. A
/// userspace task could thus mint itself (or plant onto an arbitrary,
/// possibly not-yet-existent TID) a copy of any `duplicate`-flagged handle it
/// did not hold — capability theft, and a stale-TID plant of the exact shape
/// this kernel's slot reuse is prone to. Now, for a non-privileged caller:
///
///   1. the caller must own the source handle (`owner_task == caller_tid`);
///   2. the copy may only be minted into the caller's own table
///      (`new_owner_tid == caller_tid`) — no cross-task planting.
///
/// Privileged (kernel, `user_pt == 0`) callers retain unrestricted delegation
/// for provisioning. Permissions are copied verbatim, never widened, so there
/// is no amplification path either way.
pub fn handle_dup(
    handle_id: u32,
    caller_tid: u32,
    new_owner_tid: u32,
    privileged: bool,
) -> Option<u32> {
    if handle_id as usize >= MAX_HANDLES_GLOBAL { return None; }
    // A non-privileged caller may only re-target the dup at itself.
    if !privileged && new_owner_tid != caller_tid { return None; }
    // Copy the fields out and drop the guard before calling handle_grant()
    // below — it takes the same lock, and SpinLock is not reentrant. The
    // ownership check lives INSIDE this block so its early return drops the
    // guard (no TOCTOU window, no self-deadlock).
    let (kind, perms) = {
        let handles = HANDLES.lock_irqsave();
        let entry = &handles[handle_id as usize];
        if !entry.valid || !entry.perms.duplicate { return None; }
        if !privileged && entry.owner_task != caller_tid { return None; }
        (entry.kind, entry.perms)
    };
    handle_grant(new_owner_tid, kind, perms)
}

/// Does `owner_tid` hold a handle of exactly `kind` with the permissions
/// asked for? One locked pass over the table.
///
/// This exists because `cap_check` in `crates/syscall/src/handlers.rs` used
/// to answer the same question by calling `handle_check` (since removed —
/// it had no other callers) once per index —
/// so a lookup took up to `MAX_HANDLES_GLOBAL` (256) separate
/// `lock_irqsave()`/unlock pairs. Measured from ring 3 with `latbench` on
/// QEMU virt, BEFORE this function existed (historical numbers — the
/// motivation, not the current state):
///
///   getpid()            4585 ns   (syscall floor, no capability check)
///   motor_speed(0,0)    7072 ns   (+2.5 us — its handle sits at index 10)
///   gpio_read(0)      102436 ns   (+97.8 us — no match, so all 256 steps)
///
/// WITH the single locked pass (re-measured 2026-08-22, same bench, floor
/// 1879 ns): a hit at index 0 costs +352 ns, a hit at 10 +169 ns, and a
/// full 256-entry MISS +192 ns — the scan is no longer the outlier
/// anywhere; `write` to the UART is.
///
/// The wall-clock cost was the smaller problem. Acquiring an IRQ-saving lock
/// 256 times per syscall means disabling and re-enabling interrupts 256
/// times, which lengthens interrupt latency for *every* task on the hart,
/// not just the caller — on a robot that is a control-loop jitter source.
///
/// This is containment, not the cure. The table is globally indexed with the
/// owner as a field, so any lookup is inherently a scan; the structural fix
/// is per-task handle tables (as the typed `Cap<T>` path in `cap_store`
/// already does), which makes it O(1) and makes cross-task index games
/// impossible by construction.
pub fn handle_owned_by(owner_tid: u32, kind: HandleKind, need_write: bool) -> bool {
    let handles = HANDLES.lock_irqsave();
    for i in 0..MAX_HANDLES_GLOBAL {
        let e = &handles[i];
        if !e.valid { continue; }
        if e.owner_task != owner_tid { continue; }
        if need_write && !e.perms.write { continue; }
        if e.kind == kind { return true; }
    }
    false
}

/// Revoke all handles owned by a task (called on task_exit).
pub fn handle_revoke_all(owner_tid: u32) {
    let mut handles = HANDLES.lock_irqsave();
    for i in 0..MAX_HANDLES_GLOBAL {
        if handles[i].valid && handles[i].owner_task == owner_tid {
            handles[i] = HandleEntry::empty();
        }
    }
}
