/// Pipe — port of kernel/core/pipe.c + kernel/include/pipe.h
///
/// # Reachability from ring 3 (audited 2026-08-22)
///
/// Only [`pipe_create`] is reachable from userspace, via `SYS_PIPE`
/// (`crates/syscall/src/dispatch.rs:139` → `sys_pipe`,
/// `crates/syscall/src/handlers.rs:550`). There is **no** `SYS_PIPE_READ` /
/// `SYS_PIPE_WRITE`, and `vfs_read` / `vfs_write`
/// (`crates/fs/src/vfs.rs:974` and `:1016`) have no pipe branch — they
/// resolve an fd through the inode table and reject anything that is not
/// `INODE_FILE` or `INODE_DEVICE`. So the "fds" `sys_pipe` hands back are
/// pool indices that no `read()`/`write()` can act on. Every live call to
/// [`pipe_read`] / [`pipe_write`] comes from kernel context with kernel
/// buffers (`kernel/src/main.rs:2062,2066` and `crates/bench/src/ipc.rs:87,88`).
///
/// That is why the raw-pointer signatures below are not, today, an arbitrary
/// kernel read/write primitive — but they are a **loaded gun on the table**:
/// the moment anyone wires a syscall to them, an unvalidated ring-3 pointer
/// becomes exactly that. Anything that reaches these from a syscall MUST go
/// through `robot_os_sched::copy_from_user` / `copy_to_user`
/// (`crates/sched/src/process.rs:452` / `:492`), which walk
/// `vmm::translate_user` and enforce VALID+USER+READ (+WRITE on the store
/// side) at every leaf. The safe [`pipe_read_buf`] / [`pipe_write_buf`]
/// wrappers below exist so that a future syscall never has to touch the raw
/// form at all.

use robot_os_sync::SpinLock;
pub use robot_os_limits::MAX_PIPES;

// ── Caller attribution ───────────────────────────────────────────────────────
//
// **WHY (Carril D / pipe ownership).** `pipe_read`, `pipe_write`,
// `pipe_close_read` and `pipe_close_write` took a raw pool index with no
// notion of who was calling. `pipe_create` returns `(idx, idx)` — both ends
// are the *same* slot — so an index is a full read+write right over the pipe,
// and `MAX_PIPES` is small enough to enumerate exhaustively. The moment a
// read/write syscall lands, an unowned index means any task drains or
// poisons any other task's pipe, and `pipe_close_*` is already a
// cross-task denial of service on its own.
//
// Same shape as `channel.rs`: the identity is read here instead of being
// passed in, so the arity of functions called from `crates/bench` and
// `kernel/src/main.rs` (files outside this lane) does not change. Both of
// those callers are kernel tasks, so they take the privileged bypass.

/// Returns `(caller_tid, privileged)`; see `channel.rs` for the rationale.
#[cfg(not(test))]
#[inline(always)]
fn caller_ctx() -> (u32, bool) {
    (
        robot_os_sched::current_task_tid(),
        robot_os_sched::current_user_pt() == 0,
    )
}

/// Host-test stand-in for [`caller_ctx`]; never compiled into the kernel.
#[cfg(test)]
pub mod test_ctx {
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    pub static TID: AtomicU32 = AtomicU32::new(0);
    pub static PRIVILEGED: AtomicBool = AtomicBool::new(true);

    pub fn set(tid: u32, privileged: bool) {
        TID.store(tid, Ordering::SeqCst);
        PRIVILEGED.store(privileged, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[inline(always)]
fn caller_ctx() -> (u32, bool) {
    use core::sync::atomic::Ordering;
    (
        test_ctx::TID.load(Ordering::SeqCst),
        test_ctx::PRIVILEGED.load(Ordering::SeqCst),
    )
}

/// May the current caller touch pipe slot `idx`? Assumes the pool lock is
/// held by the caller (it takes no lock of its own).
#[inline(always)]
fn access_ok(pipe: &Pipe) -> bool {
    let (caller, privileged) = caller_ctx();
    privileged || pipe.owner == caller
}

pub const PIPE_BUF_SIZE: usize = 4096;

#[derive(Copy, Clone, PartialEq)]
pub enum PipeState {
    Free         = 0,
    Active       = 1,
    ReadClosed   = 2,
    WriteClosed  = 3,
    Closed       = 4,
}

#[derive(Copy, Clone)]
pub struct Pipe {
    pub buffer:          [u8; PIPE_BUF_SIZE],
    pub read_pos:        u32,
    pub write_pos:       u32,
    pub state:           PipeState,
    pub readers:         u32,
    pub writers:         u32,
    pub waiting_readers: u32,
    pub waiting_writers: u32,
    pub id:              u32,
    /// TID of the task that called [`pipe_create`].
    ///
    /// `0` is the vacant marker — `current_task_tid()` returns 0 only when
    /// no task is running and `NEXT_TID` never issues it, so a free slot
    /// denies every ring-3 caller by construction. Rewritten on every
    /// `pipe_create`, so it also invalidates stale indices across slot
    /// reuse.
    pub owner:           u32,
}

impl Pipe {
    pub const fn zeroed() -> Self {
        Pipe {
            buffer:          [0u8; PIPE_BUF_SIZE],
            read_pos:        0,
            write_pos:       0,
            state:           PipeState::Free,
            readers:         0,
            writers:         0,
            waiting_readers: 0,
            waiting_writers: 0,
            id:              0,
            owner:           0,
        }
    }

    pub fn is_empty(&self) -> bool { self.read_pos == self.write_pos }

    pub fn is_full(&self) -> bool {
        ((self.write_pos + 1) as usize % PIPE_BUF_SIZE) == self.read_pos as usize
    }

    pub fn available(&self) -> usize {
        let w = self.write_pos as usize;
        let r = self.read_pos as usize;
        if w >= r { w - r } else { PIPE_BUF_SIZE - r + w }
    }

    pub fn space(&self) -> usize {
        PIPE_BUF_SIZE - 1 - self.available()
    }
}

// ── Global pipe pool ──────────────────────────────────────────────────────────

struct PipePool {
    pipes:   [Pipe; MAX_PIPES],
    next_id: u32,
}

impl PipePool {
    const fn new() -> Self {
        PipePool {
            pipes:   [Pipe::zeroed(); MAX_PIPES],
            next_id: 1,
        }
    }

    fn alloc(&mut self) -> Option<usize> {
        for i in 0..MAX_PIPES {
            if self.pipes[i].state == PipeState::Free {
                self.pipes[i] = Pipe::zeroed();
                self.pipes[i].state = PipeState::Active;
                self.pipes[i].id    = self.next_id;
                self.next_id       += 1;
                return Some(i);
            }
        }
        None
    }
}

static PIPES: SpinLock<PipePool> = SpinLock::new(PipePool::new());

// ── Public API ────────────────────────────────────────────────────────────────

pub fn pipe_init() {
    let mut pool = PIPES.lock();
    for i in 0..MAX_PIPES {
        pool.pipes[i].state = PipeState::Free;
        pool.pipes[i].id    = 0;
    }
}

/// Create a pipe; returns (read_idx, write_idx) on success.
///
/// The calling task becomes the pipe's owner. Because both ends are the
/// same slot, that is the only model this data structure can express today:
/// there is nothing in the `Pipe` struct that distinguishes a read end from
/// a write end, so a "reader TID / writer TID" pair would be a lie. Handing
/// an end to another task requires splitting the slot in two first — an ABI
/// change, flagged in the report rather than improvised here.
pub fn pipe_create() -> Option<(usize, usize)> {
    let (owner, _privileged) = caller_ctx();
    let mut pool = PIPES.lock();
    let idx = pool.alloc()?;
    pool.pipes[idx].readers = 1;
    pool.pipes[idx].writers = 1;
    pool.pipes[idx].owner   = owner;
    Some((idx, idx))  // Same slot — read/write ends distinguished by caller
}

/// TID that owns pipe `idx`, or `None` for an out-of-range or free slot.
pub fn pipe_owner(idx: usize) -> Option<u32> {
    if idx >= MAX_PIPES { return None; }
    let pool = PIPES.lock();
    if pool.pipes[idx].state == PipeState::Free { None } else { Some(pool.pipes[idx].owner) }
}

/// Read up to `count` bytes.  Returns bytes read, 0 on EOF, -1 on error.
///
/// # Safety contract (not enforceable here)
///
/// `buf` must be a valid kernel-writable pointer for at least `count` bytes.
/// This function dereferences it directly; it has no page table to validate
/// against and no way to know whose address space `buf` belongs to. A ring-3
/// pointer must be translated by the *caller* — see the module header. The
/// null case is rejected below because it is the one invalid pointer that can
/// be recognised without an address space, and because `sys_pipe`-shaped
/// callers pass `0` for "no buffer"; everything else is the caller's contract.
/// Prefer [`pipe_read_buf`].
pub fn pipe_read(idx: usize, buf: *mut u8, count: usize) -> i32 {
    if idx >= MAX_PIPES { return -1; }
    if buf.is_null() { return -1; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];

    if pipe.state == PipeState::Free { return -1; }
    if !access_ok(pipe) { return -1; }

    // No data available
    if pipe.is_empty() {
        return if pipe.state == PipeState::WriteClosed || pipe.state == PipeState::Closed {
            0   // EOF: write end closed, no more data coming
        } else {
            -2  // EAGAIN: would block, writer still alive
        };
    }

    let avail = pipe.available();
    let to_read = count.min(avail);
    for i in 0..to_read {
        unsafe {
            *buf.add(i) = pipe.buffer[pipe.read_pos as usize % PIPE_BUF_SIZE];
        }
        pipe.read_pos = (pipe.read_pos + 1) % PIPE_BUF_SIZE as u32;
    }
    to_read as i32
}

/// Write up to `count` bytes.  Returns bytes written, -1 on error.
///
/// # Safety contract (not enforceable here)
///
/// Same as [`pipe_read`], mirrored: `buf` must be a valid kernel-readable
/// pointer for at least `count` bytes. Prefer [`pipe_write_buf`].
pub fn pipe_write(idx: usize, buf: *const u8, count: usize) -> i32 {
    if idx >= MAX_PIPES { return -1; }
    if buf.is_null() { return -1; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];

    if pipe.state == PipeState::Free
        || pipe.state == PipeState::ReadClosed
        || pipe.state == PipeState::Closed
    {
        return -1;  // EPIPE
    }
    if !access_ok(pipe) { return -1; }

    let space = pipe.space();
    let to_write = count.min(space);
    for i in 0..to_write {
        pipe.buffer[pipe.write_pos as usize % PIPE_BUF_SIZE] = unsafe { *buf.add(i) };
        pipe.write_pos = (pipe.write_pos + 1) % PIPE_BUF_SIZE as u32;
    }
    to_write as i32
}

/// Safe wrapper over [`pipe_read`]. This is the form a syscall should use:
/// the kernel-side buffer is a real slice, so the pointer and the length can
/// never disagree, and the ring-3 copy-out stays in the syscall layer where
/// `copy_to_user` lives.
pub fn pipe_read_buf(idx: usize, buf: &mut [u8]) -> i32 {
    if buf.is_empty() { return 0; }
    pipe_read(idx, buf.as_mut_ptr(), buf.len())
}

/// Safe wrapper over [`pipe_write`]; see [`pipe_read_buf`].
pub fn pipe_write_buf(idx: usize, buf: &[u8]) -> i32 {
    if buf.is_empty() { return 0; }
    pipe_write(idx, buf.as_ptr(), buf.len())
}

/// Returns 0 on success, -1 if the index is out of range or the caller does
/// not own the pipe.
///
/// **WHY the check:** closing an end you do not own is a pure denial of
/// service — one call turns another task's live pipe into `ReadClosed`, and
/// every subsequent `pipe_write` on it returns EPIPE forever. Signature
/// changed from `()` to `i32` so a syscall can report `E_PERM`.
pub fn pipe_close_read(idx: usize) -> i32 {
    if idx >= MAX_PIPES { return -1; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];
    if pipe.state == PipeState::Free { return -1; }
    if !access_ok(pipe) { return -1; }
    if pipe.readers > 0 { pipe.readers -= 1; }
    if pipe.readers == 0 {
        pipe.state = if pipe.writers == 0 { PipeState::Closed } else { PipeState::ReadClosed };
    }
    0
}

/// Counterpart of [`pipe_close_read`]; same authorization, same rationale.
pub fn pipe_close_write(idx: usize) -> i32 {
    if idx >= MAX_PIPES { return -1; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];
    if pipe.state == PipeState::Free { return -1; }
    if !access_ok(pipe) { return -1; }
    if pipe.writers > 0 { pipe.writers -= 1; }
    if pipe.writers == 0 {
        pipe.state = if pipe.readers == 0 { PipeState::Closed } else { PipeState::WriteClosed };
    }
    0
}

/// Bytes currently queued. Returns 0 for an index the caller does not own —
/// occupancy is a side channel onto another task's traffic pattern, and 0 is
/// indistinguishable from "empty", so denial leaks nothing.
pub fn pipe_available(idx: usize) -> usize {
    if idx >= MAX_PIPES { return 0; }
    let pool = PIPES.lock();
    if !access_ok(&pool.pipes[idx]) { return 0; }
    pool.pipes[idx].available()
}

/// Free space. Denied callers get 0 ("full"), which is the fail-closed
/// answer: it discourages a write rather than inviting one.
pub fn pipe_space(idx: usize) -> usize {
    if idx >= MAX_PIPES { return 0; }
    let pool = PIPES.lock();
    if !access_ok(&pool.pipes[idx]) { return 0; }
    pool.pipes[idx].space()
}

/// Wipe the whole pool. Host-test hygiene only — see the equivalent note in
/// `channel.rs`. Never built into the kernel.
#[cfg(test)]
pub fn __pipe_reset_for_tests() {
    let mut pool = PIPES.lock();
    for i in 0..MAX_PIPES {
        pool.pipes[i] = Pipe::zeroed();
    }
    pool.next_id = 1;
}
