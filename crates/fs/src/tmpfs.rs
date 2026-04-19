//! TmpFS — bounded in-RAM temporary filesystem (F20).
//!
//! Provides a lightweight `/tmp` with a hard capacity cap and FIFO eviction.
//! Entries are allocated from the kernel heap; the entry table itself is a
//! fixed-size static array protected by a spinlock.
//!
//! ## Design goals
//! - **Bounded**: `TMPFS_MAX_BYTES` cap prevents unbounded RAM growth.
//! - **Fast**: no inode tree walk; direct hash-by-name lookup in `O(n)`.
//! - **Eviction**: when full, the oldest (lowest `seq`) entry is removed to
//!   make room for the new one.  Callers that need durability use FAT32.
//! - **VFS-agnostic API**: kernel subsystems (logger, sensor recorder, etc.)
//!   use the direct `tmpfs_*` API; syscall wrappers adapt via the VFS.
//!
//! ## Limits
//! | Constant           | Value     | Meaning                          |
//! |--------------------|-----------|----------------------------------|
//! | `TMPFS_MAX_FILES`  | 64        | Max simultaneous entries         |
//! | `TMPFS_MAX_BYTES`  | 2 MiB     | Total cap on data bytes          |
//! | `TMPFS_NAME_LEN`   | 64        | Max filename length (bytes)      |

extern crate alloc;

use alloc::alloc::{alloc, dealloc, Layout};
use core::ptr;
use robot_os_sync::SpinLock;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of files that can exist in tmpfs simultaneously.
pub const TMPFS_MAX_FILES: usize = 64;
/// Total data capacity (bytes).  Writes that would exceed this limit trigger
/// FIFO eviction of the oldest entry.
pub const TMPFS_MAX_BYTES: usize = 2 * 1024 * 1024; // 2 MiB
/// Maximum filename length including NUL terminator.
pub const TMPFS_NAME_LEN:  usize = 64;

// ── Entry ─────────────────────────────────────────────────────────────────────

/// One tmpfs file entry.
struct TmpEntry {
    /// Filename (NUL-padded, not NUL-terminated if exactly `TMPFS_NAME_LEN`).
    name:     [u8; TMPFS_NAME_LEN],
    /// Heap-allocated data buffer.
    data:     *mut u8,
    /// Allocated capacity in bytes.
    capacity: usize,
    /// Logical size (written bytes).
    size:     usize,
    /// Monotonic sequence number — lower = older.
    seq:      u32,
    /// Slot in use.
    active:   bool,
}

// SAFETY: TmpEntry contains a raw pointer but it is only accessed under the
// SpinLock, which prevents concurrent access.
unsafe impl Send for TmpEntry {}

const EMPTY_ENTRY: TmpEntry = TmpEntry {
    name: [0; TMPFS_NAME_LEN],
    data: ptr::null_mut(),
    capacity: 0,
    size: 0,
    seq: 0,
    active: false,
};

// ── Global table ──────────────────────────────────────────────────────────────

struct TmpfsState {
    entries:    [TmpEntry; TMPFS_MAX_FILES],
    used_bytes: usize,
    next_seq:   u32,
}

impl TmpfsState {
    const fn new() -> Self {
        TmpfsState {
            entries:    [EMPTY_ENTRY; TMPFS_MAX_FILES],
            used_bytes: 0,
            next_seq:   1,
        }
    }
}

static TMPFS: SpinLock<TmpfsState> = SpinLock::new(TmpfsState::new());

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by tmpfs operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmpfsError {
    /// No free slot and eviction failed (all entries pinned or all removed
    /// but still not enough space for the new entry alone).
    OutOfSpace,
    /// Filename exceeds `TMPFS_NAME_LEN` bytes.
    NameTooLong,
    /// File not found.
    NotFound,
    /// Internal heap allocation failed.
    AllocFailed,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn name_fits(name: &[u8]) -> bool {
    name.len() < TMPFS_NAME_LEN
}

fn names_equal(a: &[u8; TMPFS_NAME_LEN], b: &[u8]) -> bool {
    let len = b.len().min(TMPFS_NAME_LEN);
    &a[..len] == b && (len >= TMPFS_NAME_LEN || a[len] == 0)
}

/// Find the index of the oldest (lowest seq) active entry.
fn oldest_index(state: &TmpfsState) -> Option<usize> {
    let mut min_seq = u32::MAX;
    let mut idx = None;
    for (i, e) in state.entries.iter().enumerate() {
        if e.active && e.seq < min_seq {
            min_seq = e.seq;
            idx = Some(i);
        }
    }
    idx
}

/// Free a single entry's heap buffer and mark it inactive.
unsafe fn free_entry(state: &mut TmpfsState, idx: usize) {
    let e = &mut state.entries[idx];
    if !e.data.is_null() {
        dealloc(e.data, Layout::from_size_align_unchecked(e.capacity, 1));
    }
    state.used_bytes -= e.size;
    state.entries[idx] = EMPTY_ENTRY;
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Write (create or overwrite) a file in tmpfs.
///
/// If the capacity cap would be exceeded, the oldest entry is evicted
/// repeatedly until there is room.  Returns `Err(OutOfSpace)` only if the
/// new data alone exceeds `TMPFS_MAX_BYTES`.
pub fn tmpfs_write(name: &[u8], data: &[u8]) -> Result<(), TmpfsError> {
    if !name_fits(name) { return Err(TmpfsError::NameTooLong); }
    if data.len() > TMPFS_MAX_BYTES { return Err(TmpfsError::OutOfSpace); }

    let mut state = TMPFS.lock();

    // Remove existing entry with the same name (overwrite).
    for i in 0..TMPFS_MAX_FILES {
        if state.entries[i].active && names_equal(&state.entries[i].name, name) {
            unsafe { free_entry(&mut state, i); }
            break;
        }
    }

    // Evict oldest until we have enough headroom.
    while state.used_bytes + data.len() > TMPFS_MAX_BYTES {
        if let Some(old) = oldest_index(&state) {
            unsafe { free_entry(&mut state, old); }
        } else {
            break;
        }
    }

    // Find a free slot.
    let slot = state.entries.iter().position(|e| !e.active)
        .ok_or(TmpfsError::OutOfSpace)?;

    // Allocate heap buffer.
    let buf = if data.is_empty() {
        ptr::null_mut()
    } else {
        let layout = unsafe { Layout::from_size_align_unchecked(data.len(), 1) };
        let p = unsafe { alloc(layout) };
        if p.is_null() { return Err(TmpfsError::AllocFailed); }
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), p, data.len()); }
        p
    };

    let seq = state.next_seq;
    state.next_seq = state.next_seq.wrapping_add(1);
    state.used_bytes += data.len();

    let e = &mut state.entries[slot];
    e.name[..name.len()].copy_from_slice(name);
    e.data     = buf;
    e.capacity = data.len();
    e.size     = data.len();
    e.seq      = seq;
    e.active   = true;

    Ok(())
}

/// Read a file from tmpfs into `buf`.
///
/// Returns the number of bytes actually copied (may be less than the file
/// size if `buf` is shorter).  Returns `Err(NotFound)` if the file does not
/// exist.
pub fn tmpfs_read(name: &[u8], buf: &mut [u8]) -> Result<usize, TmpfsError> {
    if !name_fits(name) { return Err(TmpfsError::NameTooLong); }
    let state = TMPFS.lock();
    for e in state.entries.iter() {
        if e.active && names_equal(&e.name, name) {
            let n = buf.len().min(e.size);
            unsafe { ptr::copy_nonoverlapping(e.data, buf.as_mut_ptr(), n); }
            return Ok(n);
        }
    }
    Err(TmpfsError::NotFound)
}

/// Get the size of a tmpfs file without reading its data.
pub fn tmpfs_size(name: &[u8]) -> Option<usize> {
    if !name_fits(name) { return None; }
    let state = TMPFS.lock();
    for e in state.entries.iter() {
        if e.active && names_equal(&e.name, name) {
            return Some(e.size);
        }
    }
    None
}

/// Remove a file from tmpfs.
///
/// Returns `Ok(())` if deleted, `Err(NotFound)` if it did not exist.
pub fn tmpfs_unlink(name: &[u8]) -> Result<(), TmpfsError> {
    if !name_fits(name) { return Err(TmpfsError::NameTooLong); }
    let mut state = TMPFS.lock();
    for i in 0..TMPFS_MAX_FILES {
        if state.entries[i].active && names_equal(&state.entries[i].name, name) {
            unsafe { free_entry(&mut state, i); }
            return Ok(());
        }
    }
    Err(TmpfsError::NotFound)
}

/// List all active entries.  Calls `cb(name_bytes, size)` for each file.
/// The `name_bytes` slice is the raw name without trailing NUL bytes.
pub fn tmpfs_ls(mut cb: impl FnMut(&[u8], usize)) {
    let state = TMPFS.lock();
    for e in state.entries.iter() {
        if !e.active { continue; }
        let name_len = e.name.iter().position(|&b| b == 0).unwrap_or(TMPFS_NAME_LEN);
        cb(&e.name[..name_len], e.size);
    }
}

/// Return `(files_active, used_bytes, max_bytes)`.
pub fn tmpfs_stats() -> (usize, usize, usize) {
    let state = TMPFS.lock();
    let active = state.entries.iter().filter(|e| e.active).count();
    (active, state.used_bytes, TMPFS_MAX_BYTES)
}
