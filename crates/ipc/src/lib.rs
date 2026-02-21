#![no_std]

pub mod channel;
pub mod pipe;
pub mod signal;

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
