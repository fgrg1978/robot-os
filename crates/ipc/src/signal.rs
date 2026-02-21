/// Signal subsystem — port of kernel/core/signal.c + kernel/include/signal.h

pub const SIGHUP:    u32 = 1;
pub const SIGINT:    u32 = 2;
pub const SIGQUIT:   u32 = 3;
pub const SIGILL:    u32 = 4;
pub const SIGTRAP:   u32 = 5;
pub const SIGABRT:   u32 = 6;
pub const SIGBUS:    u32 = 7;
pub const SIGFPE:    u32 = 8;
pub const SIGKILL:   u32 = 9;
pub const SIGUSR1:   u32 = 10;
pub const SIGSEGV:   u32 = 11;
pub const SIGUSR2:   u32 = 12;
pub const SIGPIPE:   u32 = 13;
pub const SIGALRM:   u32 = 14;
pub const SIGTERM:   u32 = 15;
pub const SIGSTKFLT: u32 = 16;
pub const SIGCHLD:   u32 = 17;
pub const SIGCONT:   u32 = 18;
pub const SIGSTOP:   u32 = 19;
pub const SIGTSTP:   u32 = 20;
pub const NSIG:      u32 = 32;

pub const SIG_DFL: usize = 0;   // Default action
pub const SIG_IGN: usize = 1;   // Ignore signal

#[derive(Clone, Copy)]
pub enum SigDefaultAction {
    Term,   // Terminate process
    Ignore, // Ignore
    Core,   // Dump core (we just terminate)
    Stop,   // Stop process
    Cont,   // Continue if stopped
}

pub fn signal_default_action(signum: u32) -> SigDefaultAction {
    match signum {
        SIGHUP | SIGINT | SIGQUIT | SIGBUS | SIGFPE |
        SIGPIPE | SIGALRM | SIGTERM => SigDefaultAction::Term,

        SIGKILL  => SigDefaultAction::Term,
        SIGILL | SIGSEGV | SIGTRAP => SigDefaultAction::Core,
        SIGSTOP | SIGTSTP  => SigDefaultAction::Stop,
        SIGCONT  => SigDefaultAction::Cont,
        SIGCHLD | SIGUSR1 | SIGUSR2 => SigDefaultAction::Ignore,
        _ => SigDefaultAction::Term,
    }
}

pub fn signal_valid(signum: u32) -> bool {
    signum >= 1 && signum < NSIG
}

pub fn signal_catchable(signum: u32) -> bool {
    signum != SIGKILL && signum != SIGSTOP
}

// ── Per-task signal state (indexed by task ID) ──────────────────────────────

use robot_os_sync::SpinLock;

const MAX_TASKS: usize = 64;

#[derive(Clone, Copy)]
struct SigState {
    pending:  u32,   // Bitmask of pending signals
    mask:     u32,   // Blocked signals mask
    handlers: [usize; NSIG as usize],  // Handler fn pointers (SIG_DFL/SIG_IGN/fn)
}

impl SigState {
    const fn new() -> Self {
        SigState { pending: 0, mask: 0, handlers: [SIG_DFL; NSIG as usize] }
    }
}

struct SigTable {
    tids:  [u32; MAX_TASKS],
    state: [SigState; MAX_TASKS],
    count: usize,
}

impl SigTable {
    const fn new() -> Self {
        SigTable {
            tids:  [0u32; MAX_TASKS],
            state: [SigState::new(); MAX_TASKS],
            count: 0,
        }
    }

    fn find(&self, tid: u32) -> Option<usize> {
        for i in 0..self.count {
            if self.tids[i] == tid { return Some(i); }
        }
        None
    }

    fn get_or_create(&mut self, tid: u32) -> usize {
        if let Some(idx) = self.find(tid) { return idx; }
        if self.count < MAX_TASKS {
            let idx = self.count;
            self.tids[idx]  = tid;
            self.state[idx] = SigState::new();
            self.count += 1;
            idx
        } else {
            0
        }
    }
}

static SIG_TABLE: SpinLock<SigTable> = SpinLock::new(SigTable::new());

pub fn signal_init() {
    // Nothing to do — SIG_TABLE is zero-initialized.
}

/// Send signal `signum` to task `tid`.
pub fn signal_send(tid: u32, signum: u32) -> i32 {
    if !signal_valid(signum) { return -1; }
    let mut t = SIG_TABLE.lock();
    let idx = t.get_or_create(tid);
    t.state[idx].pending |= 1 << signum;
    0
}

/// Check if any signals are pending for the current task.
pub fn signal_pending() -> u32 {
    let tid = robot_os_sched::current_task_tid();
    let t = SIG_TABLE.lock();
    match t.find(tid) {
        Some(idx) => t.state[idx].pending & !t.state[idx].mask,
        None      => 0,
    }
}

/// Set signal handler for current task.
pub fn signal_set_handler(signum: u32, handler: usize) -> usize {
    if !signal_valid(signum) { return SIG_DFL; }
    let tid = robot_os_sched::current_task_tid();
    let mut t = SIG_TABLE.lock();
    let idx = t.get_or_create(tid);
    let old = t.state[idx].handlers[signum as usize];
    if signal_catchable(signum) {
        t.state[idx].handlers[signum as usize] = handler;
    }
    old
}

/// Get signal mask for current task.
pub fn signal_get_mask() -> u32 {
    let tid = robot_os_sched::current_task_tid();
    let t = SIG_TABLE.lock();
    match t.find(tid) {
        Some(idx) => t.state[idx].mask,
        None      => 0,
    }
}

/// Set signal mask for current task.
pub fn signal_set_mask(mask: u32) {
    let tid = robot_os_sched::current_task_tid();
    let mut t = SIG_TABLE.lock();
    let idx = t.get_or_create(tid);
    // SIGKILL and SIGSTOP cannot be masked
    t.state[idx].mask = mask & !((1 << SIGKILL) | (1 << SIGSTOP));
}
