#![no_std]

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

pub use channel::{
    channel_create, channel_send, channel_recv, channel_destroy, channel_info,
    MAX_CHANNELS,
};

pub use pipe::{
    pipe_init, pipe_create, pipe_read, pipe_write,
    pipe_close_read, pipe_close_write, pipe_available, pipe_space,
    PIPE_BUF_SIZE, MAX_PIPES, PipeState, Pipe,
};

pub use signal::{
    signal_init, signal_send, signal_pending, signal_set_handler,
    signal_get_mask, signal_set_mask, signal_valid, signal_catchable,
    signal_default_action, SigDefaultAction,
    SIGHUP, SIGINT, SIGQUIT, SIGILL, SIGTRAP, SIGABRT, SIGBUS, SIGFPE,
    SIGKILL, SIGUSR1, SIGSEGV, SIGUSR2, SIGPIPE, SIGALRM, SIGTERM,
    SIGSTKFLT, SIGCHLD, SIGCONT, SIGSTOP, SIGTSTP, NSIG,
    SIG_DFL, SIG_IGN,
};

pub use io_ring::{
    IoRing, IoRingState, IoRingOps, SqEntry, CqEntry,
    io_ring_create, io_ring_destroy, io_ring_pending,
    io_ring_submit, io_ring_register_ops,
    MAX_IO_RINGS, RING_SQ_SIZE, RING_CQ_SIZE, RING_DATA_BUF_SIZE,
    OP_NOP, OP_READ_SENSOR, OP_WRITE_GPIO, OP_READ_GPIO,
    OP_I2C_READ, OP_I2C_WRITE, OP_PWM_SET, OP_MOTOR_SPEED,
    OP_NET_SEND, OP_NET_RECV, OP_CAMERA_CAPTURE, OP_IRQ_WAIT,
    IO_OK, IO_ERR_INVALID_OP, IO_ERR_INVALID_PARAM, IO_ERR_NO_OPS,
};

pub use port::{
    Port, PortSource, PortSourceKind, PortEvent,
    port_create, port_destroy, port_bind, port_poll, port_has_events,
    port_queue_event, port_owner,
    MAX_PORTS, PORT_MAX_SOURCES,
};

pub use handle::{
    HandleEntry, HandleKind, HandlePerms,
    handle_grant, handle_revoke, handle_dup, handle_check,
    handle_kind, handle_count, handle_revoke_all,
    MAX_HANDLES_PER_TASK, MAX_HANDLES_GLOBAL,
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
    ShmRegion, ShmPerms,
    shm_create, shm_acquire, shm_release, shm_info, shm_page_phys,
    MAX_SHM_REGIONS, MAX_SHM_PAGES,
};

pub use rpc::{
    RpcPending,
    rpc_register, rpc_reply, rpc_get_reply, rpc_cancel_all,
    MAX_PENDING_RPCS, RPC_MSG_MAX_LEN,
};
