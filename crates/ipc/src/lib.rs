#![no_std]

pub mod cap;
pub mod cap_store;
pub mod channel;
pub mod pipe;
pub mod signal;
pub mod io_ring;
pub mod port;
pub mod handle;
pub mod trace;
pub mod irq_bind;
pub mod shm;
pub mod rpc;
pub mod fast_ipc;
pub mod lease;
pub mod zerocopy;

// W5 batch 5 — typed hardware caps. Live here (not in
// crates/drivers/) because `drivers → ipc` would create a Cargo
// cycle; `ipc → drivers` already exists.
pub mod gpio_cap;
pub mod i2c_cap;
pub mod pwm_cap;
pub mod motor_cap;

// P1 — topology → cap_store bridge (RFC-0003/RFC-0005 migration). See its
// module doc for the ordering contract and why it lives here rather than in
// `crates/topology` or `kernel/src/`.
pub mod cap_seed;

pub use channel::{
    channel_create, channel_send, channel_recv, channel_destroy, channel_info,
    channel_owner, MSG_MAX_LEN, RING_CAP,
    MAX_CHANNELS,
};

pub use pipe::{
    pipe_init, pipe_create, pipe_read, pipe_write,
    pipe_close_read, pipe_close_write, pipe_available, pipe_space,
    pipe_owner, pipe_read_buf, pipe_write_buf,
    PIPE_BUF_SIZE, MAX_PIPES, PipeState, Pipe,
};

pub use signal::{
    signal_init, signal_send, signal_pending, signal_set_handler,
    signal_get_mask, signal_set_mask, signal_valid, signal_catchable,
    signal_default_action, SigDefaultAction,
    signal_release, signal_table_len,
    SIGHUP, SIGINT, SIGQUIT, SIGILL, SIGTRAP, SIGABRT, SIGBUS, SIGFPE,
    SIGKILL, SIGUSR1, SIGSEGV, SIGUSR2, SIGPIPE, SIGALRM, SIGTERM,
    SIGSTKFLT, SIGCHLD, SIGCONT, SIGSTOP, SIGTSTP, NSIG,
    SIG_DFL, SIG_IGN,
};

pub use io_ring::{
    IoRing, IoRingState, IoRingOps, SqEntry, CqEntry,
    io_ring_create, io_ring_destroy, io_ring_pending, io_ring_owner,
    io_ring_submit, io_ring_register_ops, IO_ERR_PERM,
    io_ring_signal_async, io_ring_has_async_work, io_ring_worker_poll,
    io_ring_release_all,
    IO_RING_WORK_PENDING,
    MAX_IO_RINGS, RING_SQ_SIZE, RING_CQ_SIZE, RING_DATA_BUF_SIZE,
    OP_NOP, OP_READ_SENSOR, OP_WRITE_GPIO, OP_READ_GPIO,
    OP_I2C_READ, OP_I2C_WRITE, OP_PWM_SET, OP_MOTOR_SPEED,
    OP_NET_SEND, OP_NET_RECV, OP_CAMERA_CAPTURE, OP_IRQ_WAIT,
    IO_OK, IO_ERR_INVALID_OP, IO_ERR_INVALID_PARAM, IO_ERR_NO_OPS,
};

pub use port::{
    Port, PortSource, PortSourceKind, PortEvent,
    port_create, port_destroy, port_bind, port_poll, port_has_events,
    port_queue_event, port_owner, port_release_all,
    MAX_PORTS, PORT_MAX_SOURCES,
};

pub use handle::{
    HandleEntry, HandleKind, HandlePerms,
    handle_grant, handle_revoke, handle_dup,
    handle_revoke_all, handle_owned_by,
    MAX_HANDLES_GLOBAL,
};

pub use trace::{
    TraceEvent, trace_start, trace_stop, trace_is_enabled,
    trace_event, trace_irq, trace_sched, trace_syscall, trace_fault,
    trace_dump, trace_total,
    TRACE_IRQ, TRACE_SCHED, TRACE_SYSCALL, TRACE_DRIVER,
    TRACE_MM, TRACE_FAULT, TRACE_IPC, TRACE_USER,
    TRACE_BUF_SIZE,
};

pub use irq_bind::{
    IrqBinding, IrqTarget,
    irq_bind, irq_unbind_all, irq_dispatch,
    MAX_IRQ_BINDINGS,
};

pub use shm::{
    ShmRegion, ShmPerms, ShmHolder,
    shm_create, shm_acquire, shm_release, shm_info, shm_page_phys,
    shm_owner, shm_has_mapping, shm_note_mapping, shm_take_mapping,
    shm_release_all,
    MAX_SHM_REGIONS, MAX_SHM_PAGES, MAX_SHM_HOLDERS,
};

pub use rpc::{
    RpcPending,
    rpc_register, rpc_reply, rpc_get_reply, rpc_cancel_all,
    MAX_PENDING_RPCS, RPC_MSG_MAX_LEN,
};

pub use fast_ipc::{
    fast_ipc_call, fast_ipc_accept, fast_ipc_reply, fast_ipc_collect, fast_ipc_active,
    fast_ipc_release_all, fast_ipc_wait_state, FastIpcWait, fast_ipc_census, fast_ipc_slot_ids,
    FastIpcReply, fast_ipc_make_handle, fast_ipc_handle_slot,
    FAST_IPC_SLOT_BITS, FAST_IPC_SLOT_MASK, FAST_IPC_GEN_MASK,
    FAST_IPC_MAX_SLOTS, FAST_IPC_MAX_WORDS,
};

pub use lease::{
    LeaseEntry, LeaseState,
    lease_grant, lease_accept, lease_return, lease_is_returned, lease_wait_return,
    lease_free, lease_tick, lease_active_count, lease_release_all,
    MAX_LEASES,
};

pub use zerocopy::{
    BufferHandle, ZerocopyStats,
    buffer_addr, buffer_bytes, buffer_bytes_mut,
    pipeline_acquire, pipeline_submit, pipeline_submit_multi,
    pipeline_receive, pipeline_release,
    pipeline_register_queue, pipeline_unregister_queue,
    pipeline_stats, pipeline_in_use, pipeline_total_drops, pipeline_max_depth,
    ZEROCOPY_BUF_COUNT, ZEROCOPY_BUF_SIZE, ZEROCOPY_MAX_CONSUMERS,
    ZEROCOPY_RING_DEPTH, ZEROCOPY_RING_MASK, ZEROCOPY_PAGE_ALIGN,
    ZEROCOPY_INVALID_ID, ZEROCOPY_INITIAL_GENERATION,
    ZEROCOPY_OK, ZEROCOPY_ERR_INVALID_HANDLE, ZEROCOPY_ERR_STALE_GENERATION,
    ZEROCOPY_ERR_QUEUE_FULL, ZEROCOPY_ERR_INVALID_QUEUE, ZEROCOPY_ERR_INVALID_LEN,
};

// ---------------------------------------------------------------------------
// Task-exit resource reclamation (W3-F7)
// ---------------------------------------------------------------------------

/// Release **every** per-task resource `tid` holds. This is the function the
/// scheduler's task-exit hook must call.
///
/// **WHY it exists (W3-F7):** the exit hook registered in `kernel/src/main.rs`
/// was `handle_revoke_all`, which cleans only the *legacy* global handle
/// table. Two other per-task resource classes were never reclaimed:
///
///  * **Typed caps.** `cap_store`'s own module doc claimed `task_exit` calls
///    `cap_store::reset`; it had zero callers anywhere in the tree. That was
///    harmless only while `CAP_TABLES` was TID-indexed and TIDs were monotone
///    (nothing could ever collide). W3-F4 makes those tables *pool-slot*
///    indexed, and pool slots are recycled — so from that change on, an
///    un-reset table is a live inheritance path from a dead task to the next
///    occupant of its slot. F4 and F7 are one fix, not two.
///  * **Shared-memory references.** W3-F1 books shm references per task, so a
///    task that dies holding one pins the region — and its physical pages —
///    for the life of the board unless the exit path gives them back.
///
/// Ordering: the hook fires from `scheduler::task_exit` *before* the task is
/// marked `Zombie` and long before `do_schedule` frees its pool slot, so
/// `idx_for_tid(tid)` still resolves and `cap_store::reset` lands on the
/// right slot. See `cap_store::reset` for what breaks if that ever changes.
pub fn task_release_all(tid: u32) {
    // ── The order below is load-bearing. Read this before changing it. ──
    //
    // 1. Silence anything that can still *deliver into* this task's resources
    //    before those resources are recycled.
    // 2. Revoke authority.
    // 3. Give the resources back, waking anyone left blocked on them.
    //
    // IRQ bindings go first for a concrete reason: `irq_dispatch` calls
    // `port_queue_event` from IRQ context, and `port_release_all` below makes
    // the freed port ids immediately reusable. Freeing the ports while a
    // binding still points at them means an interrupt belonging to a dead task
    // gets delivered into the port of a live one — the same class of bug as
    // IPC-3 itself, only harder to see because it needs a device to fire.
    irq_bind::irq_unbind_all(tid);

    // Legacy AQ6 global handle table (previous sole behaviour of the hook).
    handle::handle_revoke_all(tid);
    // Typed Cap<T> table for this task's pool slot.
    cap_store::reset(tid);
    // Per-task signal state.
    //
    // **WHY this became mandatory rather than tidy.** `SigTable::get_or_create`
    // used to hand out **index 0** when its 64-entry table was full, so every
    // task past the 64th aliased its mask, handlers and pending set onto the
    // first task's slot — cross-task corruption that failed *open* and needed
    // no attacker, just a long-lived robot. It now fails closed and returns
    // `None`. Without this line the failure mode simply moves: after 64 task
    // lifetimes every new task loses signals entirely, `sys_alarm` discards its
    // error and waits forever, and `sys_pause` burns a thousand yields to
    // return -1. The fix and this reclamation are one change, not two.
    signal::signal_release(tid);
    // Pending RPCs this task issued as a client. (Its clients, if it *served*
    // RPCs, are a known gap — `RpcPending` records the caller and the channel
    // but never the server's TID, and `Channel` has no owner field to recover
    // it from. See `rpc::rpc_cancel_all`.)
    rpc::rpc_cancel_all(tid);
    // Leases this task holds as lessor or as lessee. Before `shm_release_all`
    // so a lessor blocked waiting for its buffer back is woken as early as
    // possible; the dead task's own mapping is inert either way.
    lease::lease_release_all(tid);
    // Shared-memory references booked against this task.
    shm::shm_release_all(tid);
    // Event ports owned by this task (safe now that its IRQ bindings are gone).
    port::port_release_all(tid);
    // IO rings owned by this task. A ring caught mid-pass by the async worker
    // is marked orphaned rather than freed, and its page is handed back at the
    // end of that pass — see `io_ring::io_ring_release_all`.
    io_ring::io_ring_release_all(tid);
    // Fast-IPC slots this task owns as caller or as server (IPC-3).
    //
    // **WHY it is safe for this (and `lease_release_all`) to re-enter the
    // scheduler.** Both wake tasks left blocked on a resource whose other end
    // just died — without that, an orphaned fast-IPC client or a waiting lessor
    // sleeps for the life of the board. That only works because
    // `scheduler::task_exit` invokes this hook *before* it takes `PoolGuard`
    // and the runqueue lock. Reorder that block and the exit path deadlocks
    // against itself, with no warning and no test to catch it.
    //
    // **WHY the leak mattered more than it looks.** The slot table is 64
    // entries in BSS. Once exhausted, `fast_ipc_call` returns `None` forever
    // and the dispatch arm answers -1, whose documented contract is "fall back
    // to channel IPC". So the *optimized* path this kernel exists to provide
    // dies silently, with every caller quietly taking the slow road and not a
    // single test failing.
    fast_ipc::fast_ipc_release_all(tid);
}
