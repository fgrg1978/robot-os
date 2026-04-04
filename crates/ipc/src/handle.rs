//! Handle-based capability system (AQ6).
//!
//! Inspired by Zircon and seL4. Every resource access is via a handle —
//! an opaque per-process integer. A process can only use resources it has
//! been explicitly granted handles for.
//!
//! Handle types: Sensor, Gpio, I2c, Pwm, Motor, Channel, Ring, Port,
//!               Irq, MmioRegion.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum handles per process.
pub const MAX_HANDLES_PER_TASK: usize = 32;

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
    Channel(u32),        // IPC channel ID
    Ring(u32),           // io_ring ID
    Port(u32),           // event port ID
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

static mut HANDLES: [HandleEntry; MAX_HANDLES_GLOBAL] = {
    const EMPTY: HandleEntry = HandleEntry::empty();
    [EMPTY; MAX_HANDLES_GLOBAL]
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Grant a handle to a task. Returns handle ID or None.
pub fn handle_grant(owner_tid: u32, kind: HandleKind, perms: HandlePerms) -> Option<u32> {
    unsafe {
        for i in 0..MAX_HANDLES_GLOBAL {
            if !HANDLES[i].valid {
                HANDLES[i] = HandleEntry {
                    kind,
                    perms,
                    owner_task: owner_tid,
                    valid: true,
                };
                return Some(i as u32);
            }
        }
    }
    None
}

/// Revoke a handle.
pub fn handle_revoke(handle_id: u32) {
    if handle_id as usize >= MAX_HANDLES_GLOBAL { return; }
    unsafe { HANDLES[handle_id as usize] = HandleEntry::empty(); }
}

/// Duplicate a handle for another task (if dup permission set).
pub fn handle_dup(handle_id: u32, new_owner_tid: u32) -> Option<u32> {
    if handle_id as usize >= MAX_HANDLES_GLOBAL { return None; }
    unsafe {
        let entry = &HANDLES[handle_id as usize];
        if !entry.valid || !entry.perms.duplicate { return None; }
        handle_grant(new_owner_tid, entry.kind, entry.perms)
    }
}

/// Validate that a task owns a handle and has required permissions.
pub fn handle_check(handle_id: u32, owner_tid: u32, need_write: bool) -> Option<HandleKind> {
    if handle_id as usize >= MAX_HANDLES_GLOBAL { return None; }
    unsafe {
        let entry = &HANDLES[handle_id as usize];
        if !entry.valid { return None; }
        if entry.owner_task != owner_tid { return None; }
        if need_write && !entry.perms.write { return None; }
        Some(entry.kind)
    }
}

/// Get handle kind (without ownership check — for kernel internal use).
pub fn handle_kind(handle_id: u32) -> Option<HandleKind> {
    if handle_id as usize >= MAX_HANDLES_GLOBAL { return None; }
    unsafe {
        let entry = &HANDLES[handle_id as usize];
        if entry.valid { Some(entry.kind) } else { None }
    }
}

/// Count handles owned by a task.
pub fn handle_count(owner_tid: u32) -> usize {
    let mut count = 0;
    unsafe {
        for i in 0..MAX_HANDLES_GLOBAL {
            if HANDLES[i].valid && HANDLES[i].owner_task == owner_tid {
                count += 1;
            }
        }
    }
    count
}

/// Revoke all handles owned by a task (called on task_exit).
pub fn handle_revoke_all(owner_tid: u32) {
    unsafe {
        for i in 0..MAX_HANDLES_GLOBAL {
            if HANDLES[i].valid && HANDLES[i].owner_task == owner_tid {
                HANDLES[i] = HandleEntry::empty();
            }
        }
    }
}
