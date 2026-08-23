//! IO Ring — shared-memory submission/completion queues (AQ1).
//!
//! Inspired by Linux io_uring. Zero-copy, zero-syscall data path for
//! high-frequency sensor data between kernel/drivers and userspace.
//!
//! Layout (fits in one 4 KiB page):
//!   SQ (Submission Queue): userspace writes requests, kernel reads
//!   CQ (Completion Queue): kernel writes results, userspace reads
//!   Data buffer: large results (LiDAR scans, camera frames)

use core::sync::atomic::{AtomicU32, Ordering};
use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of entries in each queue.
pub const RING_SQ_SIZE: usize = 32;
pub const RING_CQ_SIZE: usize = 32;

/// Maximum data per slot in the data buffer.
pub const RING_DATA_SLOT_SIZE: usize = 64;

/// Total data buffer size.
pub const RING_DATA_BUF_SIZE: usize = 2048;

/// Maximum number of io_rings system-wide.
pub const MAX_IO_RINGS: usize = 16;

// ---------------------------------------------------------------------------
// Ring opcodes
// ---------------------------------------------------------------------------

pub const OP_NOP: u16 = 0;
pub const OP_READ_SENSOR: u16 = 1;
pub const OP_WRITE_GPIO: u16 = 2;
pub const OP_READ_GPIO: u16 = 3;
pub const OP_I2C_READ: u16 = 4;
pub const OP_I2C_WRITE: u16 = 5;
pub const OP_PWM_SET: u16 = 6;
pub const OP_MOTOR_SPEED: u16 = 7;
pub const OP_NET_SEND: u16 = 8;
pub const OP_NET_RECV: u16 = 9;
pub const OP_CAMERA_CAPTURE: u16 = 10;
pub const OP_IRQ_WAIT: u16 = 11;

// ---------------------------------------------------------------------------
// Shared structures (mapped in both kernel and userspace)
// ---------------------------------------------------------------------------

/// Submission queue entry — written by userspace, read by kernel.
///
/// # WHY `addr` and `reg` are their own fields (the `param1` split)
///
/// The previous layout had three generic `paramN` words and told I2C to pack
/// *both* the device address and the register into `param1`. Two things went
/// wrong with that, and only one of them was documented:
///
///  * **The capability could not be reconstructed.** `HandleKind::I2c(bus,
///    addr)` — the handle `sys_i2c_read`/`sys_i2c_write` check — needs the
///    address on its own. With addr and reg fused there is no faithful way to
///    rebuild it, so [`dispatch_sqe`] denied both I2C opcodes outright to
///    unprivileged rings rather than enforce something weaker than the
///    syscall path.
///  * **`param1` was already taken.** The I2C arms *also* read `param1` as
///    the offset into `data_buf`, in the same expression that passed it as
///    `addr_reg` to the driver. One field, two mutually exclusive meanings:
///    any ring submitting a working I2C read addressed a device number equal
///    to its own buffer offset. Inert only because `OPS` is never registered.
///
/// Splitting them costs 8 bytes per SQE (24 → 32). The ring still fits one
/// 4 KiB page with room to spare: 4 queue indices (16 B) + 32 × 32 B SQ
/// (1024) + 32 × 16 B CQ (512) + 2048 B data = 3600 B.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SqEntry {
    pub opcode: u16,
    pub flags: u16,
    /// Bus / device selector: I2C bus, GPIO pin, sensor type, PWM channel,
    /// socket fd, left motor speed.
    pub param0: u32,
    /// Offset into `data_buf` for the buffer opcodes; a scalar (GPIO level,
    /// PWM duty, right motor speed) for the register opcodes.
    pub param1: u32,
    /// Length in bytes for the buffer opcodes.
    pub param2: u32,
    /// Device address on the bus named by `param0`. **I2C only.** Kept
    /// separate from `param1` so `dispatch_sqe` can build the exact
    /// `HandleKind::I2c(bus, addr)` the syscall path checks.
    pub addr: u16,
    /// Device register. **`OP_I2C_READ` only** — `OP_I2C_WRITE` takes the
    /// register as the first byte of its payload, exactly as `sys_i2c_write`
    /// defines it, and ignores this field.
    pub reg: u16,
    /// Opaque tag for correlation.
    pub user_data: u64,
}

/// Completion queue entry — written by kernel, read by userspace.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CqEntry {
    pub user_data: u64,    // copied from SqEntry
    pub result: i32,       // bytes read, or negative error
    pub flags: u32,
}

/// The shared ring structure (fits in ~4 KiB page).
#[repr(C)]
pub struct IoRing {
    // Submission queue
    pub sq_head: AtomicU32,          // kernel consumes from here
    pub sq_tail: AtomicU32,          // userspace produces to here
    pub sq_entries: [SqEntry; RING_SQ_SIZE],

    // Completion queue
    pub cq_head: AtomicU32,          // userspace consumes from here
    pub cq_tail: AtomicU32,          // kernel produces to here
    pub cq_entries: [CqEntry; RING_CQ_SIZE],

    // Shared data buffer for large results
    pub data_buf: [u8; RING_DATA_BUF_SIZE],
}

// ---------------------------------------------------------------------------
// Kernel-side ring management
// ---------------------------------------------------------------------------

/// Kernel state for one io_ring instance.
pub struct IoRingState {
    /// Physical address of the shared page (0 = unused slot).
    pub phys_addr: usize,
    /// Owning task TID (as `usize`). Read by [`io_ring_owner`] and by the
    /// per-opcode capability check in [`dispatch_sqe`].
    pub owner_task: usize,
    /// Ring ID (index in global array).
    pub ring_id: u32,
    /// Whether this ring is active.
    pub active: bool,
    /// `true` iff the ring was created by a kernel task (`user_pt == 0`).
    ///
    /// **WHY it is captured at create time (W3-F3):** the async worker
    /// (`io_ring_worker_poll`) drains *every* ring from a kernel task, so a
    /// capability check written against "the current task" would be
    /// unconditionally satisfied there and enforce nothing. Recording the
    /// creator's privilege — and its TID — makes the check a property of the
    /// ring, evaluated identically from the syscall path and the worker.
    pub owner_privileged: bool,
    /// `true` while a submit pass is dereferencing `phys_addr` outside the
    /// table lock.
    ///
    /// **WHY (W3-F3):** `dispatch_sqe` calls into drivers (I2C transfers,
    /// motor writes); holding an IRQ-saving spinlock across that would pin
    /// interrupts off for milliseconds and is a control-loop jitter source in
    /// its own right. So the submit path copies `phys_addr` out and drops the
    /// lock — which reopens `io_ring_destroy` freeing the page under it. This
    /// flag closes that window: destroy refuses while a pass is in flight.
    pub in_flight: bool,
    /// `true` when the owning task died while a submit pass held an in-flight
    /// claim: the slot is already `active = false` (so no *new* claim can
    /// start) but its page is still being dereferenced and must not be freed
    /// until [`release_ring`] closes the claim.
    ///
    /// **WHY (IPC-3):** the task-exit hook must not spin waiting for the
    /// async worker, and it must not free the page under it either — that is
    /// precisely the use-after-free `in_flight` was introduced to prevent.
    /// Deferring the free to the claim's own exit point is the only place
    /// where "the pass is definitely finished" is known, and `release_ring`
    /// already holds the table lock there.
    pub orphaned: bool,
}

impl IoRingState {
    pub const fn empty() -> Self {
        Self {
            phys_addr: 0,
            owner_task: usize::MAX,
            ring_id: 0,
            active: false,
            owner_privileged: false,
            in_flight: false,
            orphaned: false,
        }
    }
}

/// Global array of io_ring instances.
///
/// Protected by a single `SpinLock` covering the whole table, the same shape
/// as `handle.rs`'s `HANDLES`, `port.rs`'s `PORTS`, `shm.rs`'s
/// `SHM_REGIONS`, `lease.rs`'s `LEASES` and `irq_bind.rs`'s `IRQ_BINDINGS`.
///
/// **WHY (W3-F3):** this was the one table in the crate still declared
/// `static mut`, with every accessor touching it in a bare `unsafe` block and
/// no synchronization at all, while `SYS_IO_SETUP` / `SYS_IO_SUBMIT` /
/// `SYS_IO_WAIT` reach it from ring 3 on any hart. Two harts could both
/// observe `!IO_RINGS[i].active` and both claim slot `i` — one page leaked
/// and two tasks sharing a ring id — and `io_ring_destroy` racing
/// `io_ring_submit` freed the page while the other hart was still writing
/// completions into it. `lock_irqsave()` throughout, matching its five
/// siblings, so an IRQ-context caller can be added later without silently
/// reintroducing a same-hart deadlock.
const EMPTY_RING: IoRingState = IoRingState::empty();
static IO_RINGS: SpinLock<[IoRingState; MAX_IO_RINGS]> =
    SpinLock::new([EMPTY_RING; MAX_IO_RINGS]);

/// Allocate a new io_ring. Returns (ring_id, phys_addr) or None.
pub fn io_ring_create(owner_task: usize) -> Option<(u32, usize)> {
    // Allocate a physical page for the shared ring
    let page = robot_os_mm::pmm::alloc_page().ok()?;
    let phys = page.as_usize();

    // Snapshot the creator's privilege *before* taking the table lock —
    // `current_user_pt()` reads scheduler state and we want the critical
    // section to stay short and free of foreign locks.
    let privileged = robot_os_sched::current_user_pt() == 0;

    {
        let mut rings = IO_RINGS.lock_irqsave();
        for i in 0..MAX_IO_RINGS {
            if !rings[i].active {
                rings[i] = IoRingState {
                    phys_addr: phys,
                    owner_task,
                    ring_id: i as u32,
                    active: true,
                    owner_privileged: privileged,
                    in_flight: false,
                    orphaned: false,
                };
                // Zero-init the ring (alloc_page already zeroes, but be explicit).
                // SAFETY: `phys` is a freshly allocated PMM page, exclusively
                // owned by this slot, which we hold the table lock over.
                unsafe {
                    let ring = phys as *mut IoRing;
                    (*ring).sq_head.store(0, Ordering::Relaxed);
                    (*ring).sq_tail.store(0, Ordering::Relaxed);
                    (*ring).cq_head.store(0, Ordering::Relaxed);
                    (*ring).cq_tail.store(0, Ordering::Relaxed);
                }
                return Some((i as u32, phys));
            }
        }
    }
    // No free slots — free the page
    let _ = robot_os_mm::pmm::free_page(page);
    None
}

/// TID of the task that created `ring_id`, or `None` if the slot is inactive.
///
/// **WHY this is public (W3-F3):** `owner_task` was written by
/// `io_ring_create` and read nowhere, so the field advertised an ownership
/// model no code applied. The legacy `SYS_IO_SUBMIT` / `SYS_IO_WAIT` arms take
/// a raw userspace ring id bounded only by `MAX_IO_RINGS` (16), so without an
/// owner check any task could drive or observe any other task's ring.
pub fn io_ring_owner(ring_id: u32) -> Option<usize> {
    if ring_id as usize >= MAX_IO_RINGS { return None; }
    let rings = IO_RINGS.lock_irqsave();
    let state = &rings[ring_id as usize];
    if state.active { Some(state.owner_task) } else { None }
}

/// Destroy an io_ring and free its page.
///
/// Returns `false` if the ring is not active, or if a submit pass is
/// currently in flight on it — freeing the page under a live pass is exactly
/// the use-after-free `in_flight` exists to prevent. Callers that get `false`
/// on a busy ring may retry; the pass is bounded by `RING_SQ_SIZE`.
pub fn io_ring_destroy(ring_id: u32) -> bool {
    if ring_id as usize >= MAX_IO_RINGS { return false; }
    let mut rings = IO_RINGS.lock_irqsave();
    let state = &mut rings[ring_id as usize];
    if !state.active || state.in_flight { return false; }
    let _ = robot_os_mm::pmm::free_page(
        robot_os_mm::addr::PhysAddr::new(state.phys_addr)
    );
    *state = IoRingState::empty();
    true
}

/// Get the number of pending submissions in an io_ring.
pub fn io_ring_pending(ring_id: u32) -> u32 {
    if ring_id as usize >= MAX_IO_RINGS { return 0; }
    // Read the two queue indices with the table lock still held: they are
    // two atomic loads on an already-resident page, so the critical section
    // stays short, and holding the lock is what stops a concurrent
    // `io_ring_destroy` from freeing the page between the `active` test and
    // the dereference.
    let rings = IO_RINGS.lock_irqsave();
    let state = &rings[ring_id as usize];
    if !state.active { return 0; }
    // SAFETY: the slot is active and the table lock is held, so `phys_addr`
    // names a live PMM page owned by this ring for the whole borrow.
    unsafe {
        let ring = state.phys_addr as *const IoRing;
        let head = (*ring).sq_head.load(Ordering::Acquire);
        let tail = (*ring).sq_tail.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }
}

/// Claim a ring for a submit pass: validates it, marks `in_flight`, and
/// returns `(phys_addr, owner_tid, owner_privileged)`.
///
/// Returns `None` when the ring is inactive or already in flight. The caller
/// **must** pair this with [`release_ring`] on every exit path.
fn claim_ring(ring_id: u32) -> Option<(usize, u32, bool)> {
    if ring_id as usize >= MAX_IO_RINGS { return None; }
    let mut rings = IO_RINGS.lock_irqsave();
    let state = &mut rings[ring_id as usize];
    if !state.active || state.in_flight { return None; }
    state.in_flight = true;
    Some((
        state.phys_addr,
        // `owner_task` is a TID stored widened to usize; narrow it back for
        // the handle table, saturating rather than truncating so a sentinel
        // `usize::MAX` can never alias a real TID.
        state.owner_task.min(u32::MAX as usize) as u32,
        state.owner_privileged,
    ))
}

/// Release a claim taken by [`claim_ring`].
///
/// Also completes a **deferred destroy**: if the owning task died mid-pass,
/// [`io_ring_release_all`] left the slot `active = false, orphaned = true`
/// with its page intact, because freeing it there would have pulled the page
/// out from under the very pass this call is ending. This is the exactly-once
/// exit point of every claim, so it is the one place where the page is
/// provably no longer referenced.
///
/// Hot-path cost: one bool read inside a lock this function already takes —
/// no extra acquisition, no extra branch on the submit fast path beyond a
/// predictable not-taken test.
fn release_ring(ring_id: u32) {
    if ring_id as usize >= MAX_IO_RINGS { return; }
    let mut rings = IO_RINGS.lock_irqsave();
    let state = &mut rings[ring_id as usize];
    state.in_flight = false;
    if state.orphaned {
        let phys = state.phys_addr;
        *state = IoRingState::empty();
        let _ = robot_os_mm::pmm::free_page(robot_os_mm::addr::PhysAddr::new(phys));
    }
}

/// Destroy every io_ring owned by `tid` — task-exit hook (IPC-3).
///
/// **WHY this exists (IPC-3):** `IO_RINGS` is a fixed 16-entry BSS table and
/// each live entry pins one 4 KiB PMM page. Nothing reclaimed either:
/// `task_release_all` called only `handle_revoke_all`, `cap_store::reset` and
/// `shm_release_all`, so a task that died holding a ring leaked both the slot
/// and its page permanently. Sixteen deaths and `SYS_IO_SETUP` fails forever.
/// It is also an authorization leak, for the same reason as `port_release_all`:
/// `io_ring_access_ok` authorizes on `io_ring_owner(id) == current_tid`, and
/// TIDs are recycled, so a ring left active is inherited whole — including
/// its `owner_privileged` bit — by the next task drawing that TID.
///
/// **The async-worker race, and why this does not reintroduce it.**
/// `io_ring_submit` and `io_ring_worker_poll` both copy `phys_addr` out and
/// drop the table lock before calling `dispatch_sqe` (they must: it calls
/// into I2C/motor drivers, and holding an IRQ-saving spinlock across that is
/// a control-loop jitter source). `in_flight` is what stops `io_ring_destroy`
/// freeing the page inside that window. A task-exit hook has neither option
/// available to `io_ring_destroy`'s callers: it cannot return `false` and ask
/// to be retried, and it must not block waiting for the worker — the caller
/// is `scheduler::task_exit`, mid-teardown, on its way to `do_schedule`.
///
/// So the free is **deferred, never waited on**: an in-flight ring is marked
/// `active = false, orphaned = true` and its page left mapped. `claim_ring`
/// requires `active`, so no *new* pass can start; the one pass already
/// running keeps dereferencing a page that is still live and valid, and
/// [`release_ring`] frees it when that pass ends. The window is unobservable
/// from every other accessor — `io_ring_owner`, `io_ring_pending` and
/// `io_ring_destroy` all test `active` before touching `phys_addr`.
///
/// Cost: exit path only, one pass over 16 slots under the table's own lock.
pub fn io_ring_release_all(tid: u32) {
    let mut rings = IO_RINGS.lock_irqsave();
    for i in 0..MAX_IO_RINGS {
        let state = &mut rings[i];
        // `owner_task` is a TID widened to `usize` at both create sites
        // (`SYS_IO_SETUP` in dispatch.rs and `io_ring_create_cap`). Compare in
        // `usize` so the `usize::MAX` sentinel cannot alias a real TID.
        if !state.active || state.owner_task != tid as usize {
            continue;
        }
        if state.in_flight {
            // Deferred: hand the free to `release_ring`. Clearing `active`
            // first is what makes this safe — `claim_ring` will refuse from
            // here on, so exactly one pass remains and it owns the free.
            state.active = false;
            state.orphaned = true;
        } else {
            let phys = state.phys_addr;
            *state = IoRingState::empty();
            let _ = robot_os_mm::pmm::free_page(robot_os_mm::addr::PhysAddr::new(phys));
        }
    }
}

// ---------------------------------------------------------------------------
// IO Ring dispatch table — avoids circular dependencies (F00.1)
// ---------------------------------------------------------------------------

/// Error codes for IO ring operations.
pub const IO_OK: i32 = 0;
pub const IO_ERR_INVALID_OP: i32 = -1;
pub const IO_ERR_INVALID_PARAM: i32 = -2;
pub const IO_ERR_NO_OPS: i32 = -3;

/// Dispatch table for IO ring operations. Registered by kernel at boot.
/// Each function pointer maps to a hardware-level operation.
pub struct IoRingOps {
    pub read_sensor:   fn(sensor_type: u32, buf: *mut u8, buf_len: usize) -> i32,
    pub write_gpio:    fn(pin: u32, value: u32) -> i32,
    pub read_gpio:     fn(pin: u32) -> i32,
    /// Mirrors `sys_i2c_read(bus, addr, reg, buf, len)`. `addr_reg` used to be
    /// one packed word; see [`SqEntry`] for why it had to be split.
    pub i2c_read:      fn(bus: u32, addr: u32, reg: u32, buf: *mut u8, len: usize) -> i32,
    /// Mirrors `sys_i2c_write(bus, addr, data, len)`: `data[0]` is the
    /// register, so there is no separate `reg` argument.
    pub i2c_write:     fn(bus: u32, addr: u32, data: *const u8, len: usize) -> i32,
    pub pwm_set:       fn(channel: u32, duty: u32) -> i32,
    pub motor_speed:   fn(left: i32, right: i32) -> i32,
    pub net_send:      fn(fd: u32, data: *const u8, len: usize) -> i32,
    pub net_recv:      fn(fd: u32, buf: *mut u8, len: usize) -> i32,
    /// TID that owns socket `fd`, or `None` if the fd is closed / invalid.
    ///
    /// **WHY this is a hook and not a direct call (the net opcodes).**
    /// Sockets have no `HandleKind` variant, so `OP_NET_SEND`/`OP_NET_RECV`
    /// used to be denied outright to unprivileged rings — there was nothing
    /// to check them against. But the socket syscalls do not use handles
    /// either: `sys_send_syscall`/`sys_recv_syscall` gate on
    /// `socket_access_ok(fd)`, which compares the caller's TID against an
    /// owner stamp written by `sys_socket` (`crates/net/src/socket.rs`
    /// `socket_owner`). That is a check the ring *can* apply exactly — it
    /// only needs the owner lookup, and `robot_os_ipc` does not depend on
    /// `robot_os_net`. Routing it through the dispatch table that already
    /// carries `net_send`/`net_recv` keeps the check identical to the
    /// syscall's without adding a crate edge.
    pub net_owner:     fn(fd: u32) -> Option<u32>,
}

/// Global dispatch table (set once at kernel init).
static mut OPS: Option<&'static IoRingOps> = None;

/// Register the IO ring dispatch table. Called once during kernel boot.
pub fn io_ring_register_ops(ops: &'static IoRingOps) {
    unsafe { OPS = Some(ops); }
}

/// Process all pending SQ entries for a ring. Returns number of completions,
/// or a negative error code.
///
/// This runs inline in the SYS_IO_SUBMIT syscall — no separate kernel thread.
/// Processes all entries from sq_head to sq_tail in batch.
pub fn io_ring_submit(ring_id: u32) -> i32 {
    if ring_id as usize >= MAX_IO_RINGS {
        return IO_ERR_INVALID_PARAM;
    }

    let ops = unsafe { match OPS {
        Some(o) => o,
        None => return IO_ERR_NO_OPS,
    }};

    // Take an exclusive in-flight claim, then drop the table lock before
    // dispatching: `dispatch_sqe` calls into drivers and must not run with
    // interrupts disabled. `release_ring` below closes the claim.
    let (phys, owner_tid, owner_privileged) = match claim_ring(ring_id) {
        Some(v) => v,
        None => return IO_ERR_INVALID_PARAM,
    };

    // SAFETY: the in-flight claim keeps `io_ring_destroy` from freeing the
    // page for the duration of this block.
    let completions = unsafe {
        let ring = phys as *mut IoRing;

        let mut head = (*ring).sq_head.load(Ordering::Acquire);
        let tail = (*ring).sq_tail.load(Ordering::Acquire);
        let mut completions: i32 = 0;

        // W2-C4: `sq_tail` lives on the page mapped into userspace and is not
        // validated on write — a producer bug (or hostile userspace) can set it
        // arbitrarily far from `sq_head`. `head != tail` alone is unbounded:
        // with wrapping u32 arithmetic this can take up to 2^32 iterations to
        // converge, hanging this hart (this runs inline in SYS_IO_SUBMIT) and
        // re-dispatching real actuator ops (motor/gpio/i2c) every iteration
        // until the watchdog resets the board. Bound the work done per call to
        // the ring's actual physical capacity — a well-behaved producer never
        // lets more than RING_SQ_SIZE entries be outstanding at once.
        let pending = tail.wrapping_sub(head).min(RING_SQ_SIZE as u32);

        for _ in 0..pending {
            let sq_idx = (head as usize) % RING_SQ_SIZE;
            let sqe = (*ring).sq_entries[sq_idx];

            // Dispatch based on opcode
            let result = dispatch_sqe(
                &sqe, &mut (*ring).data_buf, ops, owner_tid, owner_privileged,
            );

            // Write completion entry
            let cq_tail = (*ring).cq_tail.load(Ordering::Acquire);
            let cq_idx = (cq_tail as usize) % RING_CQ_SIZE;
            (*ring).cq_entries[cq_idx] = CqEntry {
                user_data: sqe.user_data,
                result,
                flags: 0,
            };
            (*ring).cq_tail.store(cq_tail.wrapping_add(1), Ordering::Release);

            head = head.wrapping_add(1);
            completions += 1;
        }

        // Advance SQ head
        (*ring).sq_head.store(head, Ordering::Release);
        completions
    };

    release_ring(ring_id);
    completions
}

/// Permission denied for a ring op whose owner lacks the capability.
///
/// Reuses the existing negative-status convention of this module rather than
/// adding an errno: completions carry an `i32` result, and userspace already
/// treats any negative value as a failed op.
///
/// CAVEAT: `CqEntry.result` also carries raw *driver* return values
/// (`ops.read_gpio` returns an `i32` level, `ops.i2c_read` a byte count, and
/// any of them may return a negative errno). There is no reserved band for
/// kernel-side rejections — `IO_ERR_*` appears nowhere in `crates/abi`,
/// `crates/libsys` or `userspace/` — so `-4` is simply the next value after
/// the existing three, and a reader cannot tell it apart from a driver that
/// returned `-4`. Carving out a reserved range is an ABI decision.
pub const IO_ERR_PERM: i32 = -4;

/// Does the ring's owner hold the handle an opcode needs?
///
/// **WHY this is keyed on the ring's owner and not the current task
/// (W3-F3):** `io_ring_worker_poll` drains rings from a kernel worker, where
/// `current_user_pt() == 0`; a "current task" check would pass there for
/// every ring and enforce nothing. Rings created by kernel tasks keep full
/// access (`owner_privileged`), matching the convention in
/// `syscall::cap_check`.
fn ring_cap_ok(
    owner_tid: u32,
    owner_privileged: bool,
    kind: crate::handle::HandleKind,
    need_write: bool,
) -> bool {
    if owner_privileged {
        return true;
    }
    crate::handle::handle_owned_by(owner_tid, kind, need_write)
}

/// Dispatch a single SQ entry to the appropriate hardware operation.
///
/// **WHY every actuator arm is capability-checked (W3-F3):** this function
/// used to execute `OP_MOTOR_SPEED`, `OP_WRITE_GPIO`, `OP_PWM_SET` and
/// `OP_I2C_WRITE` with no `cap_check` whatsoever. It was inert only because
/// `io_ring_register_ops` has no callers, so `OPS` is `None` — the day a
/// board registers the dispatch table, an io_ring becomes a complete bypass
/// of the capability system that every equivalent syscall
/// (`sys_motor_set_target`, `sys_gpio_write`, …) does enforce. The checks
/// mirror those syscalls' `cap_check` calls exactly, kind for kind.
///
/// **What this batch changed.** Every opcode is now checked against the same
/// authority its syscall counterpart uses, and no opcode is denied for want
/// of a check that could not be expressed:
///
///  * **I2C** — was a blanket deny for unprivileged rings, because the ABI
///    fused address and register (and the buffer offset) into `param1`.
///    Splitting them (see [`SqEntry`]) makes `HandleKind::I2c(bus, addr)`
///    reconstructible, so the check is now literally `sys_i2c_read`'s.
///  * **Sockets** — was a blanket deny because sockets have no `HandleKind`.
///    They do not need one: `sys_send_syscall` gates on the socket's owner
///    stamp, and `IoRingOps::net_owner` exposes that same lookup.
///  * **Narrowed parameters** — `Sensor`, `Pwm` and the I2C bus/address are
///    `u8` in `HandleKind` but arrive as `u32`/`u16`. They are rejected out
///    of range rather than truncated; see `narrow_u8!` below for why that was
///    a live bypass and not a tidiness issue.
fn dispatch_sqe(
    sqe: &SqEntry,
    data_buf: &mut [u8; RING_DATA_BUF_SIZE],
    ops: &IoRingOps,
    owner_tid: u32,
    owner_priv: bool,
) -> i32 {
    use crate::handle::HandleKind;
    let p0 = sqe.param0;
    let p1 = sqe.param1;
    let p2 = sqe.param2;

    // Local shorthand so each arm reads like its syscall counterpart.
    macro_rules! need_cap {
        ($kind:expr, $write:expr) => {
            if !ring_cap_ok(owner_tid, owner_priv, $kind, $write) {
                return IO_ERR_PERM;
            }
        };
    }

    // **WHY every narrowed parameter is range-checked first (found in
    // passing, same class as W3-F3).** `HandleKind` stores sensor type, PWM
    // channel and I2C bus/address as `u8`, while the ring ABI delivers them
    // as `u32`/`u16`. The pre-existing arms wrote `HandleKind::Sensor(p0 as
    // u8)` and then handed the *un-narrowed* `p0` to the driver, so
    // `p0 = 256` passed the capability check for `Sensor(0)` and drove
    // sensor 256. That is a live capability bypass by aliasing: a task
    // granted one device can reach every 256th device above it. Rejecting
    // out-of-range values — rather than truncating — is what makes the value
    // checked and the value used the same value.
    macro_rules! narrow_u8 {
        ($v:expr) => {{
            let v = $v;
            if v > u8::MAX as u32 { return IO_ERR_INVALID_PARAM; }
            v as u8
        }};
    }

    // Bounds a `data_buf` window. `checked_add` rather than `offset + len`:
    // the kernel builds with `overflow-checks = true` and `panic = "abort"`,
    // so on a 32-bit target a wrapping sum here would be a board reset driven
    // from ring 3. On RV64 the sum cannot overflow, but the check is free and
    // the property should not depend on the pointer width.
    let window_ok = |offset: usize, len: usize| -> bool {
        matches!(offset.checked_add(len), Some(end) if end <= RING_DATA_BUF_SIZE)
    };

    match sqe.opcode {
        OP_NOP => IO_OK,

        OP_READ_SENSOR => {
            need_cap!(HandleKind::Sensor(narrow_u8!(p0)), false);
            let offset = p1 as usize;
            let len = p2 as usize;
            if !window_ok(offset, len) {
                return IO_ERR_INVALID_PARAM;
            }
            let buf_ptr = data_buf[offset..].as_mut_ptr();
            (ops.read_sensor)(p0, buf_ptr, len)
        }

        OP_WRITE_GPIO => {
            need_cap!(HandleKind::Gpio(p0), true);
            (ops.write_gpio)(p0, p1)
        }

        OP_READ_GPIO => {
            need_cap!(HandleKind::Gpio(p0), false);
            (ops.read_gpio)(p0)
        }

        // I2C now carries bus in `param0` and the device address in its own
        // `addr` field, so the check below is *byte for byte* the one
        // `sys_i2c_read` performs: `cap_check(HandleKind::I2c(bus as u8,
        // addr as u8), false)`. Before the split, addr and reg shared
        // `param1` with the `data_buf` offset and the handle could not be
        // rebuilt, so both opcodes were denied to every unprivileged ring —
        // a whole class of legitimate work the capability system was
        // supposed to permit.
        OP_I2C_READ => {
            let bus  = narrow_u8!(p0);
            let addr = narrow_u8!(sqe.addr as u32);
            // `reg` is not part of the capability, but it is narrowed to u8
            // by the driver: a silently truncated register writes/reads the
            // wrong register on a live actuator bus. Reject instead.
            let reg  = narrow_u8!(sqe.reg as u32);
            need_cap!(HandleKind::I2c(bus, addr), false);
            let offset = p1 as usize;
            let len = p2 as usize;
            if !window_ok(offset, len) {
                return IO_ERR_INVALID_PARAM;
            }
            let buf_ptr = data_buf[offset..].as_mut_ptr();
            (ops.i2c_read)(bus as u32, addr as u32, reg as u32, buf_ptr, len)
        }

        // Write is the actuator direction — this is how the PWM/motor
        // expanders are driven — so it demands `write` on the same handle,
        // matching `sys_i2c_write`'s `cap_check(…, true)`.
        OP_I2C_WRITE => {
            let bus  = narrow_u8!(p0);
            let addr = narrow_u8!(sqe.addr as u32);
            need_cap!(HandleKind::I2c(bus, addr), true);
            let offset = p1 as usize;
            let len = p2 as usize;
            if !window_ok(offset, len) {
                return IO_ERR_INVALID_PARAM;
            }
            let data_ptr = data_buf[offset..].as_ptr();
            (ops.i2c_write)(bus as u32, addr as u32, data_ptr, len)
        }

        OP_PWM_SET => {
            need_cap!(HandleKind::Pwm(narrow_u8!(p0)), true);
            (ops.pwm_set)(p0, p1)
        }

        // `motor_speed(left, right)` drives BOTH wheels in one call and
        // carries no motor id, so the equivalent of `sys_motor_*`'s
        // `cap_check(HandleKind::Motor(id), true)` is to require write on
        // both ids. The kernel provisions exactly `Motor(0)` and `Motor(1)`
        // RW for the drivetrain (`kernel/src/main.rs`), so a task legitimately
        // allowed to drive holds both; requiring only one would let a task
        // granted a single wheel command the pair. Most safety-relevant
        // opcode in the table — deny on any doubt.
        OP_MOTOR_SPEED => {
            need_cap!(HandleKind::Motor(0), true);
            need_cap!(HandleKind::Motor(1), true);
            (ops.motor_speed)(p0 as i32, p1 as i32)
        }

        // Sockets are not handles, so there is no `HandleKind` to check — but
        // the socket syscalls do not use handles either. `sys_send_syscall` /
        // `sys_recv_syscall` gate on `socket_access_ok(fd)`, i.e. on the
        // owner TID stamped into the socket by `sys_socket`. `ops.net_owner`
        // is that same lookup, so the ring now applies the syscall's check
        // instead of a blanket deny. Privileged (kernel-created) rings keep
        // their bypass, exactly as `ring_cap_ok` gives them.
        OP_NET_SEND => {
            if !owner_priv && (ops.net_owner)(p0) != Some(owner_tid) {
                return IO_ERR_PERM;
            }
            let offset = p1 as usize;
            let len = p2 as usize;
            if !window_ok(offset, len) {
                return IO_ERR_INVALID_PARAM;
            }
            let data_ptr = data_buf[offset..].as_ptr();
            (ops.net_send)(p0, data_ptr, len)
        }

        OP_NET_RECV => {
            if !owner_priv && (ops.net_owner)(p0) != Some(owner_tid) {
                return IO_ERR_PERM;
            }
            let offset = p1 as usize;
            let len = p2 as usize;
            if !window_ok(offset, len) {
                return IO_ERR_INVALID_PARAM;
            }
            let buf_ptr = data_buf[offset..].as_mut_ptr();
            (ops.net_recv)(p0, buf_ptr, len)
        }

        OP_CAMERA_CAPTURE => {
            // Stub — camera capture is complex, completed in a later phase
            IO_OK
        }

        OP_IRQ_WAIT => {
            // Stub — IRQ wait is handled by task_block in a later integration
            IO_OK
        }

        _ => IO_ERR_INVALID_OP,
    }
}

// ===========================================================================
// M05: Async IO Ring worker
// ===========================================================================

use core::sync::atomic::{AtomicBool, Ordering as AOrd};

/// Flag set when at least one ring has pending SQEs.
/// The IO Ring worker task polls this to avoid spinning when there is no work.
pub static IO_RING_WORK_PENDING: AtomicBool = AtomicBool::new(false);

/// Signal that a ring has new SQEs (called from SYS_IO_SUBMIT_ASYNC syscall).
#[inline]
pub fn io_ring_signal_async() {
    IO_RING_WORK_PENDING.store(true, AOrd::Release);
}

/// Check if there is pending async work.
#[inline]
pub fn io_ring_has_async_work() -> bool {
    IO_RING_WORK_PENDING.load(AOrd::Acquire)
}

// ──────────────────────────────────────────────────────────────────────────
// Cap<IoRing> typed wrappers (RFC-0003 W5 batch 3)
// ──────────────────────────────────────────────────────────────────────────
//
// Same mechanical pattern as the Port and Shm batches: the typed
// entry validates the cap against the caller's `CapTable`, then
// delegates to the existing integer-handle logic. `io_ring_create_cap`
// follows the Shm "creates + grants atomically" shape — on
// cap-table exhaustion the ring is rolled back so callers never
// observe a half-created state.

/// Errors returned by the typed `io_ring_*_cap` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoRingCapError {
    /// Capability dereference failed (stale / wrong kind / missing perms).
    Cap(crate::cap::CapError),
    /// Out of memory or no free ring slot.
    NoMem,
    /// Ring doesn't exist or has been destroyed.
    Closed,
    /// Cap-table slot table is full — the ring was rolled back.
    Full,
    /// Underlying `io_ring_submit` returned a negative status; see
    /// [`IO_ERR_INVALID_OP`] / [`IO_ERR_INVALID_PARAM`] / [`IO_ERR_NO_OPS`].
    SubmitError(i32),
}

impl From<crate::cap::CapError> for IoRingCapError {
    fn from(e: crate::cap::CapError) -> Self {
        Self::Cap(e)
    }
}

/// Typed `io_ring_create`: allocates a ring and mints a `Cap<IoRing>`
/// into `tid`'s cap-table. Returns the cap and the physical address
/// of the ring page (caller maps it into userspace via `sys_mmap` or
/// equivalent). On cap-table exhaustion the ring is rolled back.
pub fn io_ring_create_cap(
    tid: u32,
) -> Result<(crate::cap::Cap<crate::cap::targets::IoRing>, u64), IoRingCapError> {
    let (ring_id, phys_addr) = io_ring_create(tid as usize).ok_or(IoRingCapError::NoMem)?;
    match crate::cap_store::grant::<crate::cap::targets::IoRing>(
        tid,
        crate::cap::CapPerms::RW,
        ring_id,
    ) {
        Some(cap) => Ok((cap, phys_addr as u64)),
        None => {
            // Roll back the ring so cap-table exhaustion is not a leak. The
            // ring was created microseconds ago and has never been submitted,
            // so the `in_flight` refusal cannot fire here.
            io_ring_destroy(ring_id);
            Err(IoRingCapError::Full)
        }
    }
}

/// Typed `io_ring_submit`: validates the cap (requires `WRITE`)
/// and processes submission queue entries. Returns the number of
/// SQEs processed (≥ 0) or `IoRingCapError::SubmitError`.
pub fn io_ring_submit_cap(
    table: &crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::IoRing>,
) -> Result<u32, IoRingCapError> {
    let ring_id = table.get(cap, crate::cap::CapPerms::WRITE)?;
    let n = io_ring_submit(ring_id);
    if n >= 0 {
        Ok(n as u32)
    } else {
        Err(IoRingCapError::SubmitError(n))
    }
}

/// Typed `io_ring_destroy`: validates the cap (requires `WRITE`), frees the
/// ring + its backing page, **and revokes the cap**.
///
/// **WHY the revoke is here (W3-F5):** ring ids are allocated first-free-slot
/// and destroying a ring does not touch the cap-table slot's generation —
/// the only thing `CapTable::get` validates. A cap left live after its ring
/// is gone therefore keeps resolving to the same id, and the next
/// `io_ring_create` hands that id to whoever asks next: task A destroys ring
/// 0, task B creates and receives ring 0, and A's stale cap now submits into
/// B's ring. The previous doc told callers to revoke separately; no caller
/// did.
///
/// Returns `Closed` if the ring could not be destroyed because a submit pass
/// is in flight — the cap is left intact so the caller can retry.
pub fn io_ring_destroy_cap(
    table: &mut crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::IoRing>,
) -> Result<(), IoRingCapError> {
    let ring_id = table.get(cap, crate::cap::CapPerms::WRITE)?;
    if !io_ring_destroy(ring_id) {
        return Err(IoRingCapError::Closed);
    }
    table.revoke(cap);
    Ok(())
}

/// Process all pending SQEs across ALL active rings — called from worker task.
///
/// Returns the total number of SQEs processed in this round.
pub fn io_ring_worker_poll() -> i32 {
    IO_RING_WORK_PENDING.store(false, AOrd::Release);

    let ops = unsafe { match OPS {
        Some(o) => o,
        None => return 0,
    }};

    let mut total = 0;
    for i in 0..MAX_IO_RINGS {
        // Same claim/release discipline as `io_ring_submit`: never hold the
        // table lock across `dispatch_sqe`, never dereference `phys_addr`
        // without a claim. A ring already in flight on another hart is
        // skipped this round (`IO_RING_WORK_PENDING` re-arms below).
        let (phys, owner_tid, owner_priv) = match claim_ring(i as u32) {
            Some(v) => v,
            None => continue,
        };
        // SAFETY: the claim keeps `io_ring_destroy` from freeing the page.
        unsafe {
            let ring = phys as *mut IoRing;
            let head = (*ring).sq_head.load(AOrd::Acquire);
            let tail = (*ring).sq_tail.load(AOrd::Acquire);
            if head == tail {
                release_ring(i as u32);
                continue; // no work on this ring
            }

            let count = io_ring_process_ring(ring, ops, owner_tid, owner_priv);
            if count > 0 {
                total += count;
                // If ring still has pending entries, signal again for next round.
                let new_head = (*ring).sq_head.load(AOrd::Acquire);
                let new_tail = (*ring).sq_tail.load(AOrd::Acquire);
                if new_head != new_tail {
                    IO_RING_WORK_PENDING.store(true, AOrd::Release);
                }
            }
        }
        release_ring(i as u32);
    }
    total
}

/// Process all pending SQEs in a single ring. Returns count processed.
unsafe fn io_ring_process_ring(
    ring: *mut IoRing,
    ops: &IoRingOps,
    owner_tid: u32,
    owner_priv: bool,
) -> i32 {
    let mut head = (*ring).sq_head.load(AOrd::Acquire);
    let tail     = (*ring).sq_tail.load(AOrd::Acquire);
    let mut done = 0;

    // W2-C4: same unbounded-`sq_tail` hazard as io_ring_submit() above — cap
    // this round to the ring's physical capacity. `io_ring_worker_poll()`
    // already re-arms `IO_RING_WORK_PENDING` when a ring still has entries
    // left after a round, so any backlog beyond this cap is picked up on the
    // next poll instead of hanging this round.
    let pending = tail.wrapping_sub(head).min(RING_SQ_SIZE as u32);

    for _ in 0..pending {
        let sq_idx = (head as usize) % RING_SQ_SIZE;
        let sqe    = (*ring).sq_entries[sq_idx];
        let result = dispatch_sqe(
            &sqe, &mut (*ring).data_buf, ops, owner_tid, owner_priv,
        );
        let cq_tail = (*ring).cq_tail.load(AOrd::Acquire);
        let cq_idx  = (cq_tail as usize) % RING_CQ_SIZE;
        (*ring).cq_entries[cq_idx] = CqEntry { user_data: sqe.user_data, result, flags: 0 };
        (*ring).cq_tail.store(cq_tail.wrapping_add(1), AOrd::Release);
        head = head.wrapping_add(1);
        done += 1;
    }
    (*ring).sq_head.store(head, AOrd::Release);
    done
}

/// Wipe the ring table **without** returning pages to the allocator. Host-test
/// hygiene only — the suite shares one static `IO_RINGS`, and going through
/// `free_page` here would corrupt the free-counting the orphan test relies on
/// (the test allocator is reset separately). Never built into the kernel: a
/// reachable "destroy every ring on the board" entry point is precisely what
/// `io_ring_access_ok` exists to prevent.
#[cfg(test)]
pub fn __io_ring_reset_for_tests() {
    let mut rings = IO_RINGS.lock_irqsave();
    for i in 0..MAX_IO_RINGS {
        rings[i] = IoRingState::empty();
    }
}

/// `(active, in_flight, orphaned, phys_addr)` for a slot (host tests).
#[cfg(test)]
pub fn __io_ring_slot_for_tests(ring_id: u32) -> (bool, bool, bool, usize) {
    if ring_id as usize >= MAX_IO_RINGS { return (false, false, false, 0); }
    let rings = IO_RINGS.lock_irqsave();
    let s = &rings[ring_id as usize];
    (s.active, s.in_flight, s.orphaned, s.phys_addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicU32, AtomicUsize};
    use std::sync::Mutex;

    static SERIAL: Mutex<()> = Mutex::new(());

    const OWNER: u32 = 1;
    const OTHER: u32 = 2;

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        __io_ring_reset_for_tests();
        robot_os_mm::shim_reset();
        robot_os_sched::shim_reset();
        // Kernel-mode creator → `owner_privileged`, so `dispatch_sqe`'s
        // capability checks pass and the submit path is drivable.
        robot_os_sched::shim_set_current(OWNER, 0);
        g
    }

    // ── The orphan window ──────────────────────────────────────────────────
    //
    // Reproduces "the owning task dies while a submit pass is in flight"
    // deterministically and single-threaded: `dispatch_sqe` runs with the
    // table lock dropped, so an opcode handler that calls
    // `io_ring_release_all` re-enters the table from exactly the window the
    // `in_flight` flag exists to protect.

    static ORPHAN_ARMED: AtomicBool = AtomicBool::new(false);
    static ORPHAN_TID: AtomicU32 = AtomicU32::new(0);
    static ORPHAN_PHYS: AtomicUsize = AtomicUsize::new(0);
    static ORPHAN_RING: AtomicU32 = AtomicU32::new(0);
    /// Frees observed on the ring's page *while the pass was still running*.
    static FREES_DURING_PASS: AtomicU32 = AtomicU32::new(u32::MAX);
    /// Owner reported by `io_ring_owner` during the pass.
    static OWNER_DURING_PASS: AtomicUsize = AtomicUsize::new(0);

    fn hook_read_sensor(_ty: u32, _buf: *mut u8, _len: usize) -> i32 {
        if ORPHAN_ARMED.swap(false, AOrd::SeqCst) {
            io_ring_release_all(ORPHAN_TID.load(AOrd::SeqCst));
            FREES_DURING_PASS.store(
                robot_os_mm::shim_free_count(ORPHAN_PHYS.load(AOrd::SeqCst)),
                AOrd::SeqCst,
            );
            OWNER_DURING_PASS.store(
                io_ring_owner(ORPHAN_RING.load(AOrd::SeqCst)).unwrap_or(usize::MAX),
                AOrd::SeqCst,
            );
        }
        7
    }

    /// Last `(bus, addr, reg, len)` seen by the I2C hooks, so a test can prove
    /// the driver got the *same* address the capability was checked against.
    static I2C_SEEN: Mutex<Option<(u32, u32, u32, usize)>> = Mutex::new(None);
    fn i2c_seen() -> Option<(u32, u32, u32, usize)> {
        *I2C_SEEN.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn i2c_seen_clear() {
        *I2C_SEEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Socket owner table for `net_owner`: fd `NET_FD` belongs to `NET_OWNER`.
    const NET_FD: u32 = 3;
    const NET_OWNER: u32 = OWNER;

    fn nop_wg(_p: u32, _v: u32) -> i32 { 0 }
    fn nop_rg(_p: u32) -> i32 { 0 }
    fn hook_i2c_r(b: u32, a: u32, r: u32, _buf: *mut u8, l: usize) -> i32 {
        *I2C_SEEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((b, a, r, l));
        0
    }
    fn hook_i2c_w(b: u32, a: u32, _d: *const u8, l: usize) -> i32 {
        *I2C_SEEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((b, a, u32::MAX, l));
        0
    }
    fn nop_pwm(_c: u32, _d: u32) -> i32 { 0 }
    fn nop_motor(_l: i32, _r: i32) -> i32 { 0 }
    fn nop_send(_f: u32, _d: *const u8, _l: usize) -> i32 { 0 }
    fn nop_recv(_f: u32, _b: *mut u8, _l: usize) -> i32 { 0 }
    fn hook_net_owner(fd: u32) -> Option<u32> {
        if fd == NET_FD { Some(NET_OWNER) } else { None }
    }

    static TEST_OPS: IoRingOps = IoRingOps {
        read_sensor: hook_read_sensor,
        write_gpio:  nop_wg,
        read_gpio:   nop_rg,
        i2c_read:    hook_i2c_r,
        i2c_write:   hook_i2c_w,
        pwm_set:     nop_pwm,
        motor_speed: nop_motor,
        net_send:    nop_send,
        net_recv:    nop_recv,
        net_owner:   hook_net_owner,
    };

    /// Queue one `OP_READ_SENSOR` on the ring page.
    unsafe fn queue_sensor_sqe(phys: usize) {
        let ring = phys as *mut IoRing;
        (*ring).sq_entries[0] = SqEntry {
            opcode: OP_READ_SENSOR,
            flags: 0,
            param0: 0,
            param1: 0,
            param2: 0,
            addr: 0,
            reg: 0,
            user_data: 0xABCD_1234,
        };
        (*ring).sq_tail.store(1, Ordering::Release);
    }

    /// Submit exactly one SQE and return its completion `result`.
    ///
    /// Goes through the real `io_ring_submit`, so the claim/release discipline
    /// and the owner/privilege plumbing are exercised too — a test that called
    /// `dispatch_sqe` directly would prove nothing about what ring 3 reaches.
    unsafe fn submit_one(id: u32, phys: usize, sqe: SqEntry) -> i32 {
        let ring = phys as *mut IoRing;
        let head = (*ring).sq_head.load(Ordering::Acquire);
        let tail = (*ring).sq_tail.load(Ordering::Acquire);
        (*ring).sq_entries[(tail as usize) % RING_SQ_SIZE] = sqe;
        (*ring).sq_tail.store(tail.wrapping_add(1), Ordering::Release);
        let cq_before = (*ring).cq_tail.load(Ordering::Acquire);
        assert_eq!(io_ring_submit(id), 1, "submit did not produce one completion");
        let _ = head;
        (*ring).cq_entries[(cq_before as usize) % RING_CQ_SIZE].result
    }

    fn sqe(opcode: u16, param0: u32, param1: u32, param2: u32, addr: u16, reg: u16) -> SqEntry {
        SqEntry { opcode, flags: 0, param0, param1, param2, addr, reg, user_data: 0 }
    }

    #[test]
    fn owner_death_mid_pass_defers_the_page_free_to_release_ring() {
        let _g = setup();
        io_ring_register_ops(&TEST_OPS);

        let (id, phys) = io_ring_create(OWNER as usize).unwrap();
        ORPHAN_ARMED.store(true, AOrd::SeqCst);
        ORPHAN_TID.store(OWNER, AOrd::SeqCst);
        ORPHAN_PHYS.store(phys, AOrd::SeqCst);
        ORPHAN_RING.store(id, AOrd::SeqCst);
        FREES_DURING_PASS.store(u32::MAX, AOrd::SeqCst);
        unsafe { queue_sensor_sqe(phys) };

        let completions = io_ring_submit(id);

        // The handler ran, so the task really did "die" mid-pass.
        assert_eq!(completions, 1);
        assert_ne!(FREES_DURING_PASS.load(AOrd::SeqCst), u32::MAX, "hook never ran");

        // THE PROPERTY: the exit hook must NOT free the page under a live
        // pass — that is the use-after-free `in_flight` exists to prevent.
        assert_eq!(
            FREES_DURING_PASS.load(AOrd::SeqCst), 0,
            "page was freed while a submit pass was still writing to it"
        );
        // ...but the slot is already closed to new claims.
        assert_eq!(OWNER_DURING_PASS.load(AOrd::SeqCst), usize::MAX);

        // And once the claim is released, the page is freed exactly once.
        assert_eq!(robot_os_mm::shim_free_count(phys), 1);
        let (active, in_flight, orphaned, _) = __io_ring_slot_for_tests(id);
        assert!(!active && !in_flight && !orphaned);
        assert_eq!(robot_os_mm::shim_pages_in_use(), 0);

        // The completion the dying task's pass produced is still coherent —
        // it was written into a page that stayed valid throughout.
        // (Read before reuse; the slot is reusable immediately after.)
        let (id2, _phys2) = io_ring_create(OTHER as usize).unwrap();
        assert_eq!(id2, id);
    }

    #[test]
    fn a_ring_orphaned_mid_pass_accepts_no_new_claim() {
        let _g = setup();
        io_ring_register_ops(&TEST_OPS);
        let (id, phys) = io_ring_create(OWNER as usize).unwrap();
        ORPHAN_ARMED.store(true, AOrd::SeqCst);
        ORPHAN_TID.store(OWNER, AOrd::SeqCst);
        ORPHAN_PHYS.store(phys, AOrd::SeqCst);
        ORPHAN_RING.store(id, AOrd::SeqCst);
        unsafe { queue_sensor_sqe(phys) };
        io_ring_submit(id);

        // After the deferred free the slot is empty, so every accessor that
        // could dereference `phys_addr` refuses first.
        assert!(io_ring_owner(id).is_none());
        assert_eq!(io_ring_pending(id), 0);
        assert!(!io_ring_destroy(id));
        assert_eq!(robot_os_mm::shim_free_count(phys), 1);
    }

    // ── The ordinary (not in flight) path ──────────────────────────────────

    #[test]
    fn release_all_frees_only_the_dying_tasks_rings_and_their_pages() {
        let _g = setup();
        let (a, pa) = io_ring_create(OWNER as usize).unwrap();
        let (b, pb) = io_ring_create(OTHER as usize).unwrap();
        let (c, pc) = io_ring_create(OWNER as usize).unwrap();
        assert_eq!(robot_os_mm::shim_pages_in_use(), 3);

        io_ring_release_all(OWNER);

        assert!(io_ring_owner(a).is_none());
        assert!(io_ring_owner(c).is_none());
        assert_eq!(robot_os_mm::shim_free_count(pa), 1);
        assert_eq!(robot_os_mm::shim_free_count(pc), 1);
        // The other task's ring and page are untouched.
        assert_eq!(io_ring_owner(b), Some(OTHER as usize));
        assert_eq!(robot_os_mm::shim_free_count(pb), 0);
        assert_eq!(robot_os_mm::shim_pages_in_use(), 1);
    }

    #[test]
    fn release_all_is_idempotent_and_never_double_frees() {
        let _g = setup();
        let (_a, pa) = io_ring_create(OWNER as usize).unwrap();
        io_ring_release_all(OWNER);
        io_ring_release_all(OWNER);
        io_ring_release_all(OWNER);
        assert_eq!(robot_os_mm::shim_free_count(pa), 1);
    }

    #[test]
    fn exhausting_the_table_then_killing_the_owner_restores_capacity() {
        let _g = setup();
        for i in 0..MAX_IO_RINGS {
            assert!(io_ring_create(OWNER as usize).is_some(), "slot {i}");
        }
        // The permanent-failure state before IPC-3: 16 dead tasks and
        // SYS_IO_SETUP never succeeds again, with 16 pages gone for good.
        assert!(io_ring_create(OTHER as usize).is_none());

        io_ring_release_all(OWNER);

        assert_eq!(robot_os_mm::shim_pages_in_use(), 0);
        assert!(io_ring_create(OTHER as usize).is_some());
    }

    #[test]
    fn release_all_ignores_uninvolved_tasks() {
        let _g = setup();
        let (a, pa) = io_ring_create(OWNER as usize).unwrap();
        io_ring_release_all(999);
        assert_eq!(io_ring_owner(a), Some(OWNER as usize));
        // `owner_task` holds `usize::MAX` when unowned; a u32 TID can never
        // equal it, so an absurd TID must not sweep inactive slots.
        io_ring_release_all(u32::MAX);
        assert_eq!(io_ring_owner(a), Some(OWNER as usize));
        assert_eq!(robot_os_mm::shim_free_count(pa), 0);
    }

    // ── The per-opcode capability matrix (the `param1` split) ──────────────
    //
    // Every test here drives a **ring-3-owned** ring (`user_pt != 0`), which
    // is the only configuration where `dispatch_sqe`'s checks do anything.
    // Both halves are asserted each time: the holder of the exact handle gets
    // through, and every other identity/handle in the space is refused.

    use crate::handle::{handle_grant, handle_revoke_all, HandleKind, HandlePerms};

    /// A ring owned by an unprivileged task, with `HANDLES` wiped clean.
    fn setup_unpriv() -> (std::sync::MutexGuard<'static, ()>, u32, usize) {
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        __io_ring_reset_for_tests();
        robot_os_mm::shim_reset();
        robot_os_sched::shim_reset();
        handle_revoke_all(OWNER);
        handle_revoke_all(OTHER);
        ORPHAN_ARMED.store(false, AOrd::SeqCst);
        i2c_seen_clear();
        io_ring_register_ops(&TEST_OPS);
        // Non-zero user page table = ring 3 ⇒ `owner_privileged == false`.
        robot_os_sched::shim_set_current(OWNER, 0x1000);
        let (id, phys) = io_ring_create(OWNER as usize).unwrap();
        (g, id, phys)
    }

    #[test]
    fn i2c_read_needs_the_same_handle_the_syscall_needs() {
        let (_g, id, phys) = setup_unpriv();
        // `sys_i2c_read` checks `HandleKind::I2c(bus, addr)` with read perms.
        handle_grant(OWNER, HandleKind::I2c(1, 0x68), HandlePerms::RO).unwrap();

        // The legitimate call goes through...
        let r = unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 1, 0, 4, 0x68, 0x3B)) };
        assert_eq!(r, 0, "the holder of I2c(1,0x68) was refused its own device");
        // ...and the driver saw exactly the bus/addr/reg that were checked.
        assert_eq!(i2c_seen(), Some((1, 0x68, 0x3B, 4)));

        // ...but nothing else on the bus, and no other bus, is reachable.
        for addr in 0u16..=0xFF {
            if addr == 0x68 { continue; }
            i2c_seen_clear();
            let r = unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 1, 0, 4, addr, 0)) };
            assert_eq!(r, IO_ERR_PERM, "addr 0x{addr:02x} on bus 1 was not refused");
            assert_eq!(i2c_seen(), None, "a refused op still reached the driver");
        }
        for bus in 0u32..=0xFF {
            if bus == 1 { continue; }
            let r = unsafe { submit_one(id, phys, sqe(OP_I2C_READ, bus, 0, 4, 0x68, 0)) };
            assert_eq!(r, IO_ERR_PERM, "bus {bus} was not refused");
        }
    }

    #[test]
    fn i2c_write_needs_write_perms_read_only_is_not_enough() {
        let (_g, id, phys) = setup_unpriv();
        handle_grant(OWNER, HandleKind::I2c(0, 0x40), HandlePerms::RO).unwrap();
        // Read is allowed by an RO handle...
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 0, 0, 1, 0x40, 0)) },
            0
        );
        // ...the actuator direction is not. This is the half that matters:
        // an I2C write is how the PWM/motor expanders are driven.
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_I2C_WRITE, 0, 0, 2, 0x40, 0)) },
            IO_ERR_PERM
        );
        // With RW it is.
        handle_revoke_all(OWNER);
        handle_grant(OWNER, HandleKind::I2c(0, 0x40), HandlePerms::RW).unwrap();
        i2c_seen_clear();
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_I2C_WRITE, 0, 0, 2, 0x40, 0)) },
            0
        );
        // `sys_i2c_write` takes the register as data[0], so no reg is passed.
        assert_eq!(i2c_seen(), Some((0, 0x40, u32::MAX, 2)));
    }

    #[test]
    fn another_tasks_i2c_handle_does_not_authorize_this_ring() {
        let (_g, id, phys) = setup_unpriv();
        // The device exists and somebody holds it — just not the ring's owner.
        handle_grant(OTHER, HandleKind::I2c(1, 0x68), HandlePerms::RW).unwrap();
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 1, 0, 4, 0x68, 0)) },
            IO_ERR_PERM
        );
        assert_eq!(i2c_seen(), None);
    }

    /// **The aliasing hole this batch closed.** `HandleKind` stores these as
    /// `u8`; the ring delivers `u32`/`u16`. Truncating meant `p0 = 256` was
    /// checked as device 0 and then executed as device 256.
    #[test]
    fn a_parameter_that_does_not_fit_its_handle_width_is_rejected_not_truncated() {
        let (_g, id, phys) = setup_unpriv();
        handle_grant(OWNER, HandleKind::Sensor(0), HandlePerms::RW).unwrap();
        handle_grant(OWNER, HandleKind::Pwm(0), HandlePerms::RW).unwrap();
        handle_grant(OWNER, HandleKind::I2c(0, 0), HandlePerms::RW).unwrap();

        // Baseline: device 0 works for each.
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_READ_SENSOR, 0, 0, 8, 0, 0)) }, 7);
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_PWM_SET, 0, 50, 0, 0, 0)) }, 0);
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 0, 0, 1, 0, 0)) }, 0);

        // Every alias of device 0 is refused, on every narrowed parameter.
        for k in 1u32..=4 {
            let alias = k * 256;
            assert_eq!(
                unsafe { submit_one(id, phys, sqe(OP_READ_SENSOR, alias, 0, 8, 0, 0)) },
                IO_ERR_INVALID_PARAM, "sensor {alias} aliased sensor 0"
            );
            assert_eq!(
                unsafe { submit_one(id, phys, sqe(OP_PWM_SET, alias, 50, 0, 0, 0)) },
                IO_ERR_INVALID_PARAM, "pwm {alias} aliased pwm 0"
            );
            assert_eq!(
                unsafe { submit_one(id, phys, sqe(OP_I2C_READ, alias, 0, 1, 0, 0)) },
                IO_ERR_INVALID_PARAM, "i2c bus {alias} aliased bus 0"
            );
            i2c_seen_clear();
            assert_eq!(
                unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 0, 0, 1, alias as u16, 0)) },
                IO_ERR_INVALID_PARAM, "i2c addr {alias} aliased addr 0"
            );
            assert_eq!(i2c_seen(), None);
        }
        // And the largest values the fields can hold do not panic either.
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_READ_SENSOR, u32::MAX, 0, 8, 0, 0)) },
            IO_ERR_INVALID_PARAM
        );
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_I2C_READ, u32::MAX, 0, 1, u16::MAX, u16::MAX)) },
            IO_ERR_INVALID_PARAM
        );
    }

    /// A truncated I2C register silently talks to the wrong register on a live
    /// actuator bus, so it is refused even though it is not part of the cap.
    #[test]
    fn an_out_of_range_i2c_register_is_refused() {
        let (_g, id, phys) = setup_unpriv();
        handle_grant(OWNER, HandleKind::I2c(0, 0x40), HandlePerms::RW).unwrap();
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 0, 0, 1, 0x40, 0x100)) },
            IO_ERR_INVALID_PARAM
        );
        assert_eq!(i2c_seen(), None);
    }

    /// The net opcodes now apply the socket syscalls' own check
    /// (`socket_access_ok`: owner stamp on the fd) instead of a blanket deny.
    #[test]
    fn net_opcodes_are_gated_on_the_sockets_owner_stamp() {
        let (_g, id, phys) = setup_unpriv();
        // The ring's owner owns NET_FD.
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_NET_SEND, NET_FD, 0, 4, 0, 0)) }, 0);
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_NET_RECV, NET_FD, 0, 4, 0, 0)) }, 0);
        // Every other fd in the space belongs to somebody else or nobody.
        for fd in 0u32..64 {
            if fd == NET_FD { continue; }
            assert_eq!(
                unsafe { submit_one(id, phys, sqe(OP_NET_SEND, fd, 0, 4, 0, 0)) },
                IO_ERR_PERM, "fd {fd} was not refused"
            );
            assert_eq!(
                unsafe { submit_one(id, phys, sqe(OP_NET_RECV, fd, 0, 4, 0, 0)) },
                IO_ERR_PERM, "fd {fd} was not refused"
            );
        }
    }

    /// A ring owned by a *different* unprivileged task must not reach the
    /// socket either, even for an fd that exists.
    #[test]
    fn net_opcodes_refuse_a_ring_owned_by_a_stranger() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        __io_ring_reset_for_tests();
        robot_os_mm::shim_reset();
        robot_os_sched::shim_reset();
        io_ring_register_ops(&TEST_OPS);
        robot_os_sched::shim_set_current(OTHER, 0x2000);
        let (id, phys) = io_ring_create(OTHER as usize).unwrap();
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_NET_SEND, NET_FD, 0, 4, 0, 0)) },
            IO_ERR_PERM
        );
    }

    /// `data_buf` windows: the last legal byte works, one past it is refused,
    /// and a length that would wrap `usize` does not panic (the kernel builds
    /// with `overflow-checks = true` and `panic = "abort"`).
    #[test]
    fn data_buffer_windows_are_bounded_without_panicking() {
        let (_g, id, phys) = setup_unpriv();
        handle_grant(OWNER, HandleKind::Sensor(0), HandlePerms::RW).unwrap();
        let last = RING_DATA_BUF_SIZE as u32;
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_READ_SENSOR, 0, last - 1, 1, 0, 0)) }, 7);
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_READ_SENSOR, 0, last, 0, 0, 0)) }, 7);
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_READ_SENSOR, 0, last, 1, 0, 0)) },
            IO_ERR_INVALID_PARAM
        );
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_READ_SENSOR, 0, u32::MAX, u32::MAX, 0, 0)) },
            IO_ERR_INVALID_PARAM
        );
    }

    /// Motor is the most safety-relevant opcode: it drives both wheels in one
    /// call and carries no motor id, so it requires write on *both*.
    #[test]
    fn motor_speed_requires_write_on_both_wheels() {
        let (_g, id, phys) = setup_unpriv();
        handle_grant(OWNER, HandleKind::Motor(0), HandlePerms::RW).unwrap();
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_MOTOR_SPEED, 100, 100, 0, 0, 0)) },
            IO_ERR_PERM, "one wheel's handle commanded the pair"
        );
        handle_grant(OWNER, HandleKind::Motor(1), HandlePerms::RW).unwrap();
        assert_eq!(
            unsafe { submit_one(id, phys, sqe(OP_MOTOR_SPEED, 100, 100, 0, 0, 0)) },
            0
        );
    }

    /// The whole matrix is inert for a kernel-created ring, by design: the
    /// async worker drains every ring from a kernel task.
    #[test]
    fn a_kernel_owned_ring_keeps_its_bypass() {
        let _g = setup();
        io_ring_register_ops(&TEST_OPS);
        let (id, phys) = io_ring_create(OWNER as usize).unwrap();
        // No handles granted at all, yet every opcode runs.
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_I2C_READ, 1, 0, 4, 0x68, 0)) }, 0);
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_MOTOR_SPEED, 0, 0, 0, 0, 0)) }, 0);
        assert_eq!(unsafe { submit_one(id, phys, sqe(OP_NET_SEND, 999, 0, 4, 0, 0)) }, 0);
    }

    #[test]
    fn out_of_range_and_boundary_ring_ids_never_panic() {
        let _g = setup();
        for id in [MAX_IO_RINGS as u32, MAX_IO_RINGS as u32 + 1, u32::MAX, u32::MAX - 1] {
            assert!(io_ring_owner(id).is_none());
            assert_eq!(io_ring_pending(id), 0);
            assert!(!io_ring_destroy(id));
            assert_eq!(io_ring_submit(id), IO_ERR_INVALID_PARAM);
        }
        let last = MAX_IO_RINGS as u32 - 1;
        assert!(io_ring_owner(last).is_none());
        assert!(!io_ring_destroy(last));
    }
}
