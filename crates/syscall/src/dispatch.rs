/// Syscall dispatch — called from trap handler on ecall.
///
/// Registers: a7 = syscall number, a0..a5 = arguments, a0 = return value.
///
/// Security layers (AQ6 + AQ11):
///   1. Syscall filter: per-task whitelist rejects unauthorized syscalls.
///   2. Handle checks: (future) validate resource handles per-task.

use crate::numbers::*;
use crate::handlers::*;

/// Error code for denied syscall (filter or capability violation).
const E_PERM: i64 = -1;

/// Extra return registers a syscall arm may hand back to ring 3, on top of
/// the `i64` that lands in `a0`.
///
/// **WHY this exists (CARRIL 4).** `SYS_IPC_FAST_ACCEPT` has to give the
/// server a caller TID *and* four request words. One `i64` cannot carry
/// forty bytes, and fast IPC exists precisely so that ≤32 bytes travel in
/// registers without the kernel touching user memory — routing them through
/// a user pointer and `copy_to_user` would pay a per-page permission walk and
/// a copy on the one path this kernel optimises, forever, to avoid a one-time
/// six-line change in the trap handler.
///
/// The trap handler cannot simply be handed `&mut` to its register file: the
/// `reg_snapshot` it already passes as `regs` is a **copy** on its own stack
/// (it must be, because `frame.regs[10]` is overwritten with the return value
/// before a forked child ever reads it). So the out-registers travel back in
/// this struct and the handler copies them into the real `TrapFrame`.
///
/// **`written` is not decoration.** `a1`..`a5` are argument registers, and
/// every existing `libsys` wrapper passes its arguments as `in("a1")`,
/// `in("a2")`… — operands rustc is entitled to assume the `asm!` block leaves
/// untouched. Clobbering them on *every* syscall would be undefined behaviour
/// in ring 3 across the whole tree. Only an arm that explicitly opts in via
/// [`SyscallOut::set`] gets its registers copied back, and only wrappers
/// written for that arm may declare them `lateout`.
#[derive(Clone, Copy)]
pub struct SyscallOut {
    /// Values destined for `a1`..`a5`, in that order.
    pub regs: [u64; SYSCALL_OUT_REGS],
    /// True when an arm filled `regs` and the trap handler must copy them
    /// into the trap frame. False for every other syscall.
    pub written: bool,
}

/// How many extra return registers [`SyscallOut`] carries: `a1`..`a5`.
pub const SYSCALL_OUT_REGS: usize = 5;

impl SyscallOut {
    /// An empty set — nothing to copy back.
    pub const fn new() -> Self {
        Self { regs: [0; SYSCALL_OUT_REGS], written: false }
    }

    /// Opt this syscall into the copy-back and fill `a1`..`a5`.
    #[inline]
    pub fn set(&mut self, values: [u64; SYSCALL_OUT_REGS]) {
        self.regs = values;
        self.written = true;
    }
}

impl Default for SyscallOut {
    fn default() -> Self { Self::new() }
}

/// Backwards-compatible entry point: dispatches and **discards** any extra
/// return registers.
///
/// **Callers that keep using this see no request payload from
/// `SYS_IPC_FAST_ACCEPT`.** The delivery added in CARRIL 4 is inert until the
/// trap handler switches to [`syscall_dispatch_out`] and copies
/// [`SyscallOut::regs`] into the live `TrapFrame` — see the doc on
/// `SyscallOut`. This shim exists so the kernel's synthetic boot-time call
/// (`kernel/src/main.rs`, the `sys_drv_invoke` smoke) needs no change at all;
/// it is not a second supported ABI.
#[allow(clippy::too_many_arguments)]
pub fn syscall_dispatch(
    num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64,
    sepc: u64, user_sp: u64, regs: &[u64; 32],
) -> i64 {
    let mut out = SyscallOut::new();
    syscall_dispatch_out(num, a0, a1, a2, a3, a4, a5, sepc, user_sp, regs, &mut out)
}

/// Fast-IPC path trace, compiled out unless `--features ipc-trace`.
///
/// **WHY it is off by default and must stay that way.** Each line is a UART
/// write, and a UART write costs ~160 us per 64 bytes under QEMU (measured by
/// `userspace/latbench`). Turning this on does not observe the fast path — it
/// replaces its timing entirely, which for a race is the difference between
/// seeing the bug and hiding it. Use it to answer "where does the exchange
/// stop", never "how long does the exchange take".
macro_rules! ipc_trace {
    ($($arg:tt)*) => {
        #[cfg(feature = "ipc-trace")]
        { robot_os_drivers::kprintln!($($arg)*); }
    };
}

/// May the current task touch event port `port_id`?
///
/// **WHY this exists (W3-F2):** `SYS_PORT_BIND` / `SYS_PORT_WAIT` /
/// `SYS_PORT_UNBIND` passed a raw userspace `port_id` straight through, and
/// `port_bind` / `port_poll` / `port_destroy` validate only
/// `port_id < MAX_PORTS`. `Port::owner_task` was recorded at create time and
/// `port_owner()` existed to read it — with zero callers anywhere in the
/// tree. So any task could destroy another task's port, dequeue its events
/// (including the opaque `user_key` the owner uses to correlate them), or
/// fill its 16-entry source list to deny it any further binds. The port id
/// space is small and dense, so this needed no guessing at all.
///
/// Kernel tasks (`user_pt == 0`) bypass, the same convention `cap_check` and
/// `handle_revoke` use. `port_owner` returns `usize::MAX` for an unowned or
/// out-of-range slot, which no real TID can equal, so an inactive port is
/// denied to userspace by construction.
#[inline]
fn port_access_ok(port_id: u64) -> bool {
    if robot_os_sched::current_user_pt() == 0 {
        return true;
    }
    let tid = robot_os_sched::current_task_tid() as usize;
    robot_os_ipc::port_owner(port_id as u32) == tid
}

/// May the current task touch io_ring `ring_id`?
///
/// **WHY (W3-F3):** same shape as [`port_access_ok`]. `SYS_IO_SUBMIT` and
/// `SYS_IO_WAIT` took a raw userspace ring id bounded only by `MAX_IO_RINGS`
/// (16), and `IoRingState::owner_task` was written by `io_ring_create` and
/// read by nothing — so any task could drive another task's ring (executing
/// its queued actuator ops) or observe its completion depth.
#[inline]
fn io_ring_access_ok(ring_id: u64) -> bool {
    if robot_os_sched::current_user_pt() == 0 {
        return true;
    }
    let tid = robot_os_sched::current_task_tid() as usize;
    robot_os_ipc::io_ring_owner(ring_id as u32) == Some(tid)
}

/// Main syscall dispatch.  Arguments are raw register values (u64).
/// Returns the result that will be written back into a0.
///
/// `sepc`/`user_sp` are the trap frame's own PC/SP at ecall time — kernel-
/// internal trap metadata, not part of the user-facing syscall ABI (a0-a5
/// are the only user-supplied arguments). K-A15: passed through as plain
/// parameters (hart-local, on this call's own stack) so `SYS_FORK` can hand
/// them to `sys_fork_impl` without going through shared mutable state that
/// a concurrent syscall on another hart could clobber first.
///
/// `out` is the write-back channel for arms that must return more than one
/// register — today only `SYS_IPC_FAST_ACCEPT`. See [`SyscallOut`]. The trap
/// handler must copy `out.regs` into `frame.regs[11..16]` when `out.written`
/// is set, and must not touch them otherwise.
#[allow(clippy::too_many_arguments)]
pub fn syscall_dispatch_out(
    num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64,
    sepc: u64, user_sp: u64, regs: &[u64; 32],
    out: &mut SyscallOut,
) -> i64 {
    // AQ11: Syscall filter — reject if current task's whitelist doesn't include this syscall.
    {
        let filter = robot_os_sched::current_syscall_filter();
        if filter.enabled && !filter.is_allowed(num as u16) {
            robot_os_ipc::trace_event(
                robot_os_ipc::TRACE_SYSCALL,
                num as u32, 0xDEAD, 0, 0,  // 0xDEAD = denied marker
            );
            return E_PERM;
        }
    }

    match num {
        // Console
        SYS_TEST    => sys_test(),
        SYS_PUTCHAR => sys_putchar(a0),
        SYS_GETCHAR => sys_getchar(),

        // Process
        SYS_EXIT    => sys_exit(a0),
        SYS_GETPID  => sys_getpid(),
        SYS_YIELD   => sys_yield(),
        SYS_FORK    => sys_fork(sepc, user_sp, regs),
        SYS_EXEC     => sys_exec(a0, a1),
        SYS_EXECPATH => sys_execpath(a0),
        SYS_WAIT     => sys_wait(),
        SYS_SLEEP    => sys_sleep(a0),

        // File I/O
        SYS_OPEN    => sys_open(a0, a1),
        SYS_CLOSE   => sys_close(a0),
        SYS_READ    => sys_read(a0, a1, a2),
        SYS_WRITE   => sys_write(a0, a1, a2),
        SYS_LSEEK   => sys_lseek(a0, a1, a2),

        // Filesystem
        SYS_MKDIR   => sys_mkdir(a0),
        SYS_UNLINK  => sys_unlink(a0),
        SYS_READDIR => sys_readdir(a0, a1, a2, a3, a4),
        SYS_MOUNT   => sys_mount(a0, a1, a2),
        SYS_UMOUNT  => sys_umount(a0),
        SYS_SYNC    => sys_sync(),
        SYS_STAT    => sys_stat(a0, a1),

        // System info
        SYS_MEMINFO  => sys_meminfo(),
        SYS_TASKINFO => sys_taskinfo(),
        SYS_UPTIME   => sys_uptime(),

        // System control
        SYS_SHUTDOWN => sys_shutdown(),
        SYS_REBOOT   => sys_reboot(),

        // Disk
        SYS_DISK_INFO  => sys_disk_info(),
        SYS_DISK_READ  => sys_disk_read(a0, a1, a2),
        SYS_DISK_WRITE => sys_disk_write(a0, a1, a2),
        SYS_DISK_SIZE  => sys_disk_size(),

        // Signals
        SYS_KILL        => sys_kill(a0, a1),
        SYS_SIGNAL      => sys_signal(a0, a1),
        SYS_SIGPENDING  => sys_sigpending(),
        SYS_SIGPROCMASK => sys_sigprocmask(a0, a1),
        SYS_PAUSE       => sys_pause(),
        SYS_ALARM       => sys_alarm(a0),
        SYS_SIGRETURN   => sys_sigreturn(),

        // Pipes / FD
        SYS_PIPE => sys_pipe(a0),
        SYS_DUP  => sys_dup(a0),
        SYS_DUP2 => sys_dup2(a0, a1),

        // Service manager
        SYS_SERVICE_REGISTER  => sys_service_register(a0, a1, a2),
        SYS_SERVICE_DISCOVER  => sys_service_discover(a0),
        SYS_SERVICE_HEARTBEAT => sys_service_heartbeat(a0),
        SYS_SERVICE_STOP      => sys_service_stop_handler(a0),
        SYS_SERVICE_UNREGISTER | SYS_SERVICE_LIST |
        SYS_SERVICE_INFO | SYS_SERVICE_START => sys_stub(),

        // E11.AQ3 — userspace driver framework.
        SYS_DRIVER_REGISTER   => sys_driver_register(a0, a1, a2, a3),
        SYS_DRIVER_UNREGISTER => sys_driver_unregister(a0),
        SYS_DRIVER_POLL_EVENT => sys_driver_poll_event(a0, a1),
        SYS_DRIVER_FETCH_REQ  => sys_driver_fetch_request(a0, a1),
        SYS_DRIVER_REPLY      => sys_driver_reply(a0, a1),
        SYS_DRIVER_REQUEST    => sys_driver_request(a0, a1, a2, a3, a4),
        SYS_DRIVER_TRY_REPLY  => sys_driver_try_reply(a0, a1, a2),
        SYS_DRIVER_STATS      => sys_driver_stats(a0),

        // GPIO
        SYS_GPIO_READ  => sys_gpio_read(a0),
        SYS_GPIO_WRITE => sys_gpio_write(a0, a1),
        SYS_GPIO_MODE  => sys_gpio_mode(a0, a1),
        SYS_GPIO_INFO  => sys_gpio_info(),

        // PWM
        SYS_PWM_ENABLE   => sys_pwm_enable(a0),
        SYS_PWM_DISABLE  => sys_pwm_disable(a0),
        SYS_PWM_SET_FREQ => sys_pwm_set_freq(a0, a1),
        SYS_PWM_SET_DUTY => sys_pwm_set_duty(a0, a1),
        SYS_PWM_INFO     => sys_pwm_info(),

        // I2C
        SYS_I2C_READ  => sys_i2c_read(a0, a1, a2, a3, a4),
        SYS_I2C_WRITE => sys_i2c_write(a0, a1, a2, a3),
        SYS_I2C_SCAN  => sys_i2c_scan(a0),
        SYS_I2C_INFO  => sys_i2c_info(),

        // Motor
        SYS_MOTOR_CREATE => sys_motor_create(a0, a1, a2, a3),
        SYS_MOTOR_ENABLE => sys_motor_enable(a0, a1),
        SYS_MOTOR_SPEED  => sys_motor_speed(a0, a1),
        SYS_MOTOR_ANGLE  => sys_motor_angle(a0),
        SYS_MOTOR_INFO   => sys_motor_info(),

        // Memory management
        SYS_BRK    => sys_brk(a0),
        SYS_MMAP   => sys_mmap(a0, a1, a2, a3, a4, a5),
        SYS_MUNMAP => sys_munmap(a0, a1),
        // E11/AQ9: explicit COW fork (equivalent to SYS_FORK today).
        SYS_FORK_COW    => sys_fork_cow(sepc, user_sp, regs),
        // E11/AQ10: reserve a virtual range without physical backing.
        SYS_ALLOC_DEMAND => sys_alloc_demand(a0),

        // ADC
        SYS_ADC_READ => sys_adc_read(a0),

        // Buzzer
        SYS_BUZZER_TONE => sys_buzzer_tone(a0, a1),
        SYS_BUZZER_OFF  => sys_buzzer_off(),

        // IPC channels
        SYS_IPC_CREATE  => sys_ipc_create(),
        SYS_IPC_SEND    => sys_ipc_send(a0, a1, a2),
        SYS_IPC_RECEIVE => sys_ipc_recv(a0, a1, a2),
        SYS_IPC_DESTROY => sys_ipc_destroy(a0),
        // IPC_CALL (F00.5): synchronous RPC
        // a0 = server_channel, a1 = msg_ptr, a2 = msg_len, a3 = reply_buf_ptr, a4 = reply_buf_cap
        SYS_IPC_CALL => {
            let tid = robot_os_sched::current_task_tid();
            let ch = a0 as usize;
            // Copy message from user space
            let mut msg_buf = [0u8; robot_os_ipc::RPC_MSG_MAX_LEN];
            let msg_len = (a2 as usize).min(robot_os_ipc::RPC_MSG_MAX_LEN);
            if msg_len > 0 && a1 != 0 {
                // The length was always capped, so this was never an overflow —
                // it was a *pointer* violation.  `sstatus.SUM` is never set in
                // this tree, so S-mode cannot read a USER page: the raw
                // `copy_nonoverlapping` here only ever "worked" for kernel and
                // MMIO addresses, i.e. it was a 64-byte kernel-memory read
                // primitive whose output the caller got back through the RPC
                // channel — and a guaranteed fatal load fault (board reset) for
                // an honest user pointer.  `copy_from_user` walks
                // `vmm::translate_user`, enforcing VALID+USER+READ at every
                // leaf level, so kernel/MMIO sources are rejected.
                if !robot_os_sched::copy_from_user(msg_buf.as_mut_ptr(), a1 as usize, msg_len) {
                    return -1;
                }
            }
            // Register the pending RPC *before* publishing the request.
            //
            // **WHY the order matters (K-C10 follow-up).** `channel_send` makes
            // the request visible to the server, which on another hart can
            // receive and answer it immediately. `rpc_reply` returns `None`
            // when the caller has no registered RPC, so an answer landing in
            // the window between the send and the register was dropped on the
            // floor: `SYS_IPC_REPLY` returned -1, the server moved on believing
            // it had served the call, and this task then blocked on `Rpc(tid)`
            // waiting for a reply that no longer existed — a permanent sleep
            // reachable from ring 3 with no attacker, just two harts and bad
            // timing. Fast-IPC never had this asymmetry: `fast_ipc_call` claims
            // its slot before it wakes the server. Registering first gives this
            // path the same shape. Do not reorder these two calls back.
            match robot_os_ipc::rpc_register(tid, a0 as u32) {
                Some(_rpc_id) => {
                    // Publish the request only now that we are registered to
                    // receive its answer. A failed send means no server will
                    // ever reply, and `RPC_PENDING` is a fixed 16-entry BSS
                    // table — leaving the registration behind would burn a slot
                    // for the life of the board. This task blocks inside this
                    // arm, so it holds at most one pending RPC and cancelling
                    // "all" of its entries cancels exactly this one.
                    if robot_os_ipc::channel_send(ch, &msg_buf[..msg_len]) != 0 {
                        robot_os_ipc::rpc_cancel_all(tid);
                        return -1; // channel full or invalid
                    }
                    // Block caller until IPC_REPLY wakes us
                    robot_os_sched::task_block(robot_os_sched::WaitReason::Rpc(tid));
                    // After wake-up, retrieve reply
                    let mut reply_tmp = [0u8; robot_os_ipc::RPC_MSG_MAX_LEN];
                    match robot_os_ipc::rpc_get_reply(tid, &mut reply_tmp) {
                        Some(reply_len) => {
                            // Copy reply to caller's buffer if provided
                            if a3 != 0 {
                                let copy_len = (reply_len as usize)
                                    .min(a4 as usize)
                                    .min(reply_tmp.len());
                                // Write side of the same violation: the bytes
                                // come from whoever served the RPC (they supply
                                // them via SYS_IPC_REPLY), and `a3` was an
                                // unchecked destination — an arbitrary kernel
                                // write of attacker-chosen content.
                                // `copy_to_user` enforces VALID+USER+WRITE per
                                // page and breaks COW properly.
                                if !robot_os_sched::copy_to_user(
                                    a3 as usize, reply_tmp.as_ptr(), copy_len)
                                {
                                    return -1;
                                }
                            }
                            reply_len as i64
                        }
                        None => -1,
                    }
                }
                None => -1, // no free RPC slots
            }
        }
        // IPC_REPLY (F00.5): complete a pending RPC
        // a0 = caller_tid, a1 = reply_ptr, a2 = reply_len
        SYS_IPC_REPLY => {
            // A task must never complete its own pending RPC: `SYS_IPC_CALL`
            // blocks the caller on `WaitReason::Rpc(self)`, so a self-reply
            // is either a confused caller or a deliberate attempt to unblock
            // out of the RPC wait with self-chosen data.  Full validation —
            // that the replier actually owns the channel the call was sent to
            // — needs an accessor for `RpcPending::server_channel`, which
            // lives in `crates/ipc/src/rpc.rs` and is not exposed; see the
            // audit note.  This is the part that can be enforced from here.
            if a0 as u32 == robot_os_sched::current_task_tid() { return -1; }

            let mut reply_buf = [0u8; robot_os_ipc::RPC_MSG_MAX_LEN];
            let reply_len = (a2 as usize).min(robot_os_ipc::RPC_MSG_MAX_LEN);
            if reply_len > 0 && a1 != 0 {
                // Same pointer violation as SYS_IPC_CALL's read side: a raw
                // deref of a user-supplied address, only ever functional for
                // kernel/MMIO memory (SUM is never set), disclosing 64 bytes
                // of kernel state into the caller's reply buffer.
                if !robot_os_sched::copy_from_user(reply_buf.as_mut_ptr(), a1 as usize, reply_len) {
                    return -1;
                }
            }
            match robot_os_ipc::rpc_reply(a0 as u32, &reply_buf[..reply_len]) {
                Some(caller_tid) => {
                    robot_os_sched::wake_by_rpc(caller_tid);
                    0
                }
                None => -1,
            }
        }
        // IPC_SHARE (F00.4): create shared memory region
        // a0 = page_count, a1 = perms (0=RO, 1=RW)
        SYS_IPC_SHARE => {
            let tid = robot_os_sched::current_task_tid();
            let perms = if a1 != 0 { robot_os_ipc::ShmPerms::ReadWrite }
                        else { robot_os_ipc::ShmPerms::ReadOnly };
            match robot_os_ipc::shm_create(tid, a0 as usize, perms) {
                Some(id) => id as i64,
                None => -1,
            }
        }
        // IPC_UNSHARE (F00.4): release shared memory reference
        // a0 = shm_id
        //
        // W3-F1. This arm used to be `shm_release(a0)` with no check of any
        // kind. `a0` is a raw userspace integer validated only against
        // MAX_SHM_REGIONS (16), and `shm_release` unconditionally decremented
        // the region's refcount — so calling it in a loop drove ANY live
        // region to zero, freeing every physical page back to the PMM while
        // the legitimate holder's USER_RW PTEs stayed valid and the frames
        // were handed to someone else. A write-after-free with no race, from
        // ring 3, in sixteen guesses.
        //
        // Two things close it, and both are needed:
        //   1. `shm_release` now takes the caller's TID and only gives back a
        //      reference that caller actually took (crates/ipc/src/shm.rs).
        //   2. The caller's own mapping is torn down *first*, below — a
        //      reference must never be dropped while the dropper still has
        //      page-table entries into the region, or step 1 alone would still
        //      let the owner free pages out from under its own live PTEs.
        SYS_IPC_UNSHARE => {
            let shm_id = a0 as u32;
            let tid = robot_os_sched::current_task_tid();
            // Tear down this task's mapping before releasing the reference.
            if let Some((va, pages)) = robot_os_ipc::shm_take_mapping(tid, shm_id) {
                let user_pt = robot_os_sched::current_user_pt();
                if user_pt != 0 {
                    for i in 0..pages {
                        // `checked_mul`/`checked_add`: `pages` is bounded by
                        // MAX_SHM_PAGES and `va` by the MMIO VA window, so this
                        // cannot realistically overflow — but with
                        // `overflow-checks = true` an arithmetic overflow here
                        // would abort the board, so it is written to stop
                        // rather than panic.
                        let off = match i.checked_mul(robot_os_arch::mmu::PAGE_SIZE) {
                            Some(v) => v,
                            None => break,
                        };
                        match va.checked_add(off) {
                            // Same rule as every other VA-taking syscall in
                            // this file: user and kernel page tables share
                            // their upper-level tables, so unmapping a VA at
                            // or above USER_VA_TOP edits the *kernel's* page
                            // table (that is how `sys_munmap` once zeroed the
                            // UART PTE and reset the board). The shm/MMIO
                            // window sits at 0x6000_0000 and is capped below
                            // this line, so a recorded VA above it can only
                            // mean the record is stale — e.g. survived an
                            // `exec` that replaced the address space. Stop
                            // rather than touch it.
                            Some(page_va) if page_va < crate::handlers::USER_VA_TOP => {
                                robot_os_mm::vmm::unmap(user_pt, page_va)
                            }
                            _ => break,
                        }
                    }
                }
            }
            if robot_os_ipc::shm_release(tid, shm_id) { 0 } else { -1 }
        }
        // IPC_MAP (F00.4): map a shared memory region into caller's address space.
        // a0 = shm_id → returns VA base (user pointer), or -1 on failure.
        // Increments shm ref_count; caller must IPC_UNSHARE when done.
        //
        // W3-F1. Previously ungated: `shm_id` is a userspace-chosen index into
        // a 16-entry global table with the owner stored as a plain (and, until
        // now, never-read) field, so any task could map every live region RW
        // into its own address space — camera frames, LiDAR scans, inference
        // buffers — by walking 0..16.
        SYS_IPC_MAP => {
            let shm_id = a0 as u32;
            let tid = robot_os_sched::current_task_tid();
            // Ownership gate. Kernel tasks (`user_pt == 0`) bypass, matching
            // the convention in `cap_check` / `handle_revoke`. NOTE: this
            // restricts the *untyped* path to the creating task; cross-task
            // sharing is the typed `Cap<Shm>` path's job, where the right to
            // map is carried by an explicitly granted capability rather than
            // by guessing a small integer.
            if robot_os_sched::current_user_pt() != 0
                && robot_os_ipc::shm_owner(shm_id) != Some(tid)
            {
                return E_PERM;
            }
            // One mapping per (task, region): the release path has exactly one
            // VA slot to tear down, and an untracked second alias would be a
            // mapping the refcount does not know about.
            if robot_os_ipc::shm_has_mapping(tid, shm_id) {
                return -1;
            }
            // Acquire ref and get page count + perms.
            match robot_os_ipc::shm_acquire(tid, shm_id) {
                Some((page_count, perms)) => {
                    // Collect physical page addresses.
                    let mut pages = [0usize; robot_os_ipc::MAX_SHM_PAGES];
                    let mut ok = true;
                    for i in 0..page_count {
                        match robot_os_ipc::shm_page_phys(shm_id, i) {
                            Some(p) => pages[i] = p,
                            None    => { ok = false; break; }
                        }
                    }
                    if ok {
                        let rw = perms == robot_os_ipc::ShmPerms::ReadWrite;
                        match robot_os_sched::process::shm_map_user(&pages[..page_count], rw) {
                            Some(va) => {
                                if robot_os_ipc::shm_note_mapping(tid, shm_id, va, page_count) {
                                    va as i64
                                } else {
                                    // Could not record the mapping, so nothing
                                    // would ever be able to unmap it. Keep the
                                    // reference (leaking one of 16 region slots)
                                    // rather than release it: releasing would put
                                    // the pages back in the PMM with live user
                                    // PTEs still pointing at them.
                                    -1
                                }
                            }
                            // `shm_map_user` returns None on a *partial* mapping
                            // and does not report how many pages it installed, so
                            // we cannot unmap what it left behind. Deliberately do
                            // NOT release the reference here: a leaked region is
                            // recoverable, freeing pages under unknown live PTEs
                            // is an arbitrary-write primitive.
                            None => -1,
                        }
                    } else {
                        robot_os_ipc::shm_release(tid, shm_id);
                        -1
                    }
                }
                None => -1,
            }
        }

        // M02: Fast-path IPC — register-passing, ≤32 bytes, zero-copy.

        // SYS_IPC_FAST_CALL: client side.
        // a0 = server_tid, a1..a4 = data words (up to 4 × u64 = 32 bytes).
        // Blocks until server replies.  Returns: d0 in a0 on wake (words in caller context).
        SYS_IPC_FAST_CALL => {
            let server_tid = a0 as u32;
            let caller_tid = robot_os_sched::current_task_tid();
            let words = [a1 as u64, a2 as u64, a3 as u64, a4 as u64];
            ipc_trace!("[IPC] CALL  tid={} -> srv={} w0={:#x}",
                caller_tid, server_tid, words[0]);
            match robot_os_ipc::fast_ipc_call(caller_tid, server_tid, words) {
                // `handle`, not a slot index: the generation-tagged exchange
                // id (same encoding as the server's FAST_ACCEPT handle). The
                // client blocks on it, so a wake can only match THIS
                // exchange — the client-side ABA closure.
                Some(handle) => {
                    ipc_trace!("[IPC] CALL  tid={} handle={:#x} claimed", caller_tid, handle);
                    // Wake server if it is blocked in FAST_ACCEPT.
                    robot_os_sched::wake_fast_ipc_server(server_tid);

                    // **WHY blocking once is not enough (K-C10).** The fix for
                    // the lost-wakeup race stamps `wake_pending` on a task that
                    // has not reached `task_block` yet, and `block_current`
                    // consumes that stamp by returning *immediately*. So this
                    // path can now come back from `task_block` with no reply
                    // waiting — a legitimate spurious wake, not an error. The
                    // old code read that as failure and answered -1, turning
                    // the cure into a different bug.
                    //
                    // Retrying needs to tell two indistinguishable situations
                    // apart, which is why `fast_ipc_wait_state` exists:
                    //   `Waiting` — the slot is still ours and unanswered, so
                    //               going back to sleep is correct;
                    //   `Gone`    — the server died and `fast_ipc_release_all`
                    //               reclaimed the slot, so sleeping again would
                    //               be sleeping forever;
                    //   `Ready`   — the reply is there; collect it.
                    // Guessing either way is worse than the -1 this replaces.
                    //
                    // The bound is a backstop, not the mechanism: each spurious
                    // wake consumes one stamp and stamps are not latched, so a
                    // correct system converges in one or two turns. Looping
                    // unbounded inside a syscall would hand ring 3 a way to pin
                    // a hart if that assumption ever broke.
                    const MAX_SPURIOUS_WAKES: u32 = 8;
                    let mut result = -1i64;
                    for _turn in 0..MAX_SPURIOUS_WAKES {
                        ipc_trace!("[IPC] CALL  tid={} handle={:#x} blocking (turn {})",
                            caller_tid, handle, _turn);
                        robot_os_sched::task_block(
                            robot_os_sched::WaitReason::FastIpcClient(handle)
                        );
                        ipc_trace!("[IPC] CALL  tid={} handle={:#x} woke (turn {})",
                            caller_tid, handle, _turn);
                        match robot_os_ipc::fast_ipc_collect(caller_tid) {
                            // Full reply delivery: a0 = reply[0] (the return
                            // value, as always), a1..a3 = reply[1..3] via
                            // `SyscallOut` — same register-delivery contract
                            // as FAST_ACCEPT, and the same rule: `out` is
                            // written on SUCCESS ONLY. The -1 exhaustion path
                            // must leave a1..a5 untouched or ring 3 inherits
                            // a previous exchange's payload. libsys's wrapper
                            // declares a1..a5 `lateout` — the shared syscallN
                            // helpers (in("aN")) must never carry this call.
                            Some(reply) => {
                                out.set([reply[1], reply[2], reply[3], 0, 0]);
                                result = reply[0] as i64;
                                break;
                            }
                            None => match robot_os_ipc::fast_ipc_wait_state(handle, caller_tid) {
                                robot_os_ipc::FastIpcWait::Waiting => continue,
                                // `Ready` here means the reply landed between
                                // the collect and this probe; one more turn
                                // picks it up without blocking, because the
                                // reply's own wake stamped us.
                                robot_os_ipc::FastIpcWait::Ready   => continue,
                                robot_os_ipc::FastIpcWait::Gone    => {
                                    ipc_trace!("[IPC] CALL  tid={} handle={:#x} GONE (server died?)",
                                        caller_tid, handle);
                                    break;
                                }
                            },
                        }
                    }
                    ipc_trace!("[IPC] CALL  tid={} handle={:#x} -> rc={}",
                        caller_tid, handle, result);
                    result
                }
                None => {
                    ipc_trace!("[IPC] CALL  tid={} -> srv={} NO SLOT (or bad tid)",
                        caller_tid, server_tid);
                    -1 // no free slots — fall back to channel IPC
                }
            }
        }

        // SYS_IPC_FAST_ACCEPT: server side.
        // Blocks until a client calls FAST_CALL targeting this TID.
        //
        // ABI: a0 = slot index (the handle for the subsequent FAST_REPLY), or
        // -1 if nothing pending. On success ONLY, a1 = caller TID and
        // a2..a5 = the four request words, delivered through `SyscallOut`.
        //
        // **WHY the delivery is here and not in a user buffer (CARRIL 4).**
        // Until this change the arm read `(slot_idx, caller_tid, words)` out
        // of `fast_ipc_accept` and threw two thirds of it away, while the
        // comment that stood here claimed "returns caller_tid in a0; data
        // words in a1..a4 (written via TrapFrame by waker)". No waker wrote
        // anything: no code in the tree touched the server's frame. A ring-3
        // server learned *that* it had been called and could answer, but
        // never *what* was asked — which is not an RPC transport, and is why
        // the whole path had never carried a real request.
        //
        // The alternative considered was a user pointer plus `copy_to_user`.
        // It needs no trap-handler change, but it charges this path a
        // per-page VALID+USER+WRITE walk and a 40-byte copy on every accept,
        // permanently — on the one path whose entire reason to exist is
        // moving ≤32 bytes in registers without touching user memory. The
        // register route costs five stores into a struct the handler already
        // has in hand.
        //
        // **This is inert until the trap handler copies `out` back.** See
        // `syscall_dispatch` (the shim) — it discards `out`, so a caller that
        // has not migrated to `syscall_dispatch_out` still sees exactly the
        // old, payload-less behaviour. `libsys::fast_ipc_accept_req` detects
        // that case explicitly instead of reporting stale registers as data.
        SYS_IPC_FAST_ACCEPT => {
            let server_tid = robot_os_sched::current_task_tid();
            ipc_trace!("[IPC] ACCEPT srv={} polling", server_tid);
            // Check if a call is already waiting.
            match robot_os_ipc::fast_ipc_accept(server_tid) {
                // `handle` is the 63-bit generation-tagged handle (57 gen +
                // 6 idx), NOT a bare slot index — it goes back to ring 3
                // verbatim for the subsequent FAST_REPLY. Masking or
                // "cleaning" it here would strip the generation and reopen
                // the slot-ABA this handle exists to close.
                Some((handle, caller_tid, words)) => {
                    ipc_trace!("[IPC] ACCEPT srv={} handle={:#x} from tid={} w0={:#x} (no block)",
                        server_tid, handle, caller_tid, words[0]);
                    out.set([caller_tid as u64, words[0], words[1], words[2], words[3]]);
                    handle as i64
                }
                None => {
                    // Same spurious-wake problem as FAST_CALL above, and for
                    // the same reason: `wake_pending` makes `task_block`
                    // return without the awaited event. Blocking exactly once
                    // and answering -1 turned every stamped server into a
                    // failed accept, which is how a server loop stops serving
                    // while every one of its clients waits on it.
                    //
                    // **The two arms are NOT symmetric.** FAST_CALL owns a
                    // slot, so `fast_ipc_wait_state` can tell "spurious wake,
                    // keep sleeping" from "server died, give up". An accepting
                    // server owns nothing: there is no slot to probe, and
                    // "woken spuriously" and "genuinely nothing pending" are
                    // the same observation from here. So the bounded retry is
                    // not a refinement of a better test — it is the only test
                    // available, and running out of turns yields -1 exactly as
                    // an empty queue would.
                    const MAX_SPURIOUS_WAKES: u32 = 8;
                    let mut result = -1i64;
                    for _turn in 0..MAX_SPURIOUS_WAKES {
                        ipc_trace!("[IPC] ACCEPT srv={} blocking (turn {})", server_tid, _turn);
                        robot_os_sched::task_block(
                            robot_os_sched::WaitReason::FastIpcServer(server_tid)
                        );
                        if let Some((handle, caller_tid, words)) =
                            robot_os_ipc::fast_ipc_accept(server_tid)
                        {
                            ipc_trace!("[IPC] ACCEPT srv={} handle={:#x} from tid={} w0={:#x} (after block)",
                                server_tid, handle, caller_tid, words[0]);
                            // Same delivery as the no-block path above (and
                            // the same generation-tagged handle — see there).
                            // Both success paths must fill `out`, and the -1
                            // exhaustion path below must not: leaving stale
                            // values in a1..a5 on a failed accept would hand
                            // ring 3 the previous exchange's payload.
                            out.set([caller_tid as u64,
                                     words[0], words[1], words[2], words[3]]);
                            result = handle as i64;
                            break;
                        }
                    }
                    if result < 0 {
                        ipc_trace!("[IPC] ACCEPT srv={} EXHAUSTED {} turns -> -1",
                            server_tid, MAX_SPURIOUS_WAKES);
                    }
                    result
                }
            }
        }

        // SYS_IPC_FAST_REPLY: server side.
        // a0 = slot_idx (from FAST_ACCEPT return value).
        // a1..a4 = reply data words.
        // Wakes the client. Returns 0 on success.
        //
        // **WHY the replier's identity is taken from the scheduler and never
        // from a register (IPC-1).** `fast_ipc_reply` used to authorize
        // nothing: `Slot::server_tid` was written by `alloc_slot` and read only
        // to find pending work, never to decide who may answer — a field
        // written and never read, the same signature that produced the
        // `HANDLES`, `port`, `io_ring` and `shm` holes. `slot_idx` arrives raw
        // in `a0` and the space is 0..63, so any ring-3 task could sweep it and
        // hand every blocked client a reply of its choosing, impersonating any
        // IPC server on the board. Passing `current_task_tid()` here — never an
        // argument register — is what makes the check unforgeable.
        SYS_IPC_FAST_REPLY => {
            // `a0` is the opaque handle from FAST_ACCEPT, **not** a slot index.
            // It carries a generation tag in its upper bits and must be passed
            // through untouched — no mask, no sign extension. A handle whose
            // bit 63 is set is rejected outright rather than truncated, so a
            // server that stored a negative return value and replayed it
            // cannot land on a live slot.
            let handle = a0;
            let replier_tid = robot_os_sched::current_task_tid();
            let privileged = robot_os_sched::current_user_pt() == 0;
            let words = [a1 as u64, a2 as u64, a3 as u64, a4 as u64];
            ipc_trace!("[IPC] REPLY srv={} handle={:#x} w0={:#x}", replier_tid, handle, words[0]);
            match robot_os_ipc::fast_ipc_reply(handle, replier_tid, privileged, words) {
                // `_slot_idx`: only the ipc-trace build reads it, and the
                // warning gate compiles without that feature.
                robot_os_ipc::FastIpcReply::Woke { caller_tid, slot_idx: _slot_idx } => {
                    // **WHY the wake is addressed by TID (K-C10).** The
                    // sweep-keyed `wake_fast_ipc_client` matches on
                    // `WaitReason::FastIpcClient(handle)`, which only exists
                    // once the client is already `Blocked`. `SYS_IPC_FAST_CALL`
                    // claims its slot, wakes the server and only *then* blocks,
                    // so on SMP this reply can land in the gap — `try_wake_task`
                    // sees a task that is not `Blocked` yet, returns early, and
                    // the client sleeps forever holding its slot. The TID-keyed
                    // variant stamps the wake in that case, which
                    // `block_current` consumes before committing to `Blocked`.
                    //
                    // The handle passed through is the server's own `a0`,
                    // verbatim — client and server handles of one exchange
                    // are the same value (the generation only advances on
                    // free), so this matches exactly the WaitReason the
                    // client blocked on.
                    robot_os_sched::wait::wake_fast_ipc_client_tid(
                        caller_tid, handle);
                    ipc_trace!("[IPC] REPLY srv={} slot={} woke client tid={}",
                        replier_tid, _slot_idx, caller_tid);
                    0
                }
                // Distinct code on purpose: `Stale` is reachable **only** by
                // the slot's current legitimate owner, so it leaks nothing, and
                // it is the one answer that tells a server "your handle died,
                // the exchange is gone" rather than "something was wrong".
                // Collapsing it into -1 would lose that and nothing else.
                robot_os_ipc::FastIpcReply::Stale => {
                    ipc_trace!("[IPC] REPLY srv={} handle={:#x} STALE (generation retired)",
                        replier_tid, handle);
                    -2
                }
                robot_os_ipc::FastIpcReply::Refused => {
                    ipc_trace!("[IPC] REPLY srv={} handle={:#x} REFUSED (not owner / not accepted)",
                        replier_tid, handle);
                    -1
                }
            }
        }

        // M04: Lease-based IPC — zero-copy large transfer via time-bounded SHM grant.

        // SYS_IPC_LEASE_GRANT: lessor grants a SHM region to lessee.
        // a0=shm_id, a1=lessee_tid, a2=expire_ticks (0=no expiry).
        // Returns lease_id on success, -1 on error.
        SYS_IPC_LEASE_GRANT => {
            let lessor = robot_os_sched::current_task_tid();
            let lessee = a1 as u32;
            match robot_os_ipc::lease_grant(a0 as usize, lessor, lessee, a2) {
                Some(lease_id) => {
                    // **WHY this wake exists at all (K-C10 audit).** Without
                    // it, `SYS_IPC_LEASE_ACCEPT` below was a permanent sleep:
                    // it blocks the lessee on `FastIpcServer(lessee)`, and the
                    // only three callers of `wake_fast_ipc_server` in the whole
                    // tree pass a *server* tid (fast-IPC), a *lessor* tid
                    // (LEASE_RETURN) and a *lessor* tid again (the timer ISR's
                    // expiry path). Nothing ever passed a lessee. A task that
                    // called ACCEPT before its lessor called GRANT never woke
                    // up — not a race, just the ordinary ordering, and
                    // unrelated to the lost-wakeup class. Granting is the event
                    // ACCEPT is waiting for, so this is where the wake belongs.
                    //
                    // The TID-keyed wake also handles grant-before-block: it
                    // stamps `wake_pending` when the lessee has not reached
                    // `task_block` yet.
                    robot_os_sched::wake_fast_ipc_server(lessee);
                    lease_id as i64
                }
                None => -1,
            }
        }

        // SYS_IPC_LEASE_ACCEPT: lessee accepts a pending lease from lessor_tid.
        // a0=lessor_tid. Blocks until a lease arrives.
        // Returns lease_id (shm_id can be queried separately).
        SYS_IPC_LEASE_ACCEPT => {
            let lessee = robot_os_sched::current_task_tid();
            match robot_os_ipc::lease_accept(lessee) {
                Some((lease_id, _shm_id)) => lease_id as i64,
                None => {
                    // No lease pending — block on a timer (caller should retry).
                    // Use FastIpcServer wait as a generic "lease wait" reason.
                    robot_os_sched::task_block(
                        robot_os_sched::WaitReason::FastIpcServer(lessee)
                    );
                    // After wake: try again
                    match robot_os_ipc::lease_accept(lessee) {
                        Some((lease_id, _)) => lease_id as i64,
                        None => -1,
                    }
                }
            }
        }

        // SYS_IPC_LEASE_RETURN: lessee returns the lease buffer to lessor.
        // a0=lease_id. Wakes the lessor.
        //
        // **WHY the caller's identity is taken from the scheduler (IPC-6).**
        // Both this arm and LEASE_FREE below passed `a0` straight through to
        // functions that received no caller at all, so they could not authorize
        // even in principle. `MAX_LEASES` is small and dense — nothing to guess.
        // A stranger calling RETURN wakes the *lessor* into believing its buffer
        // is back while the real lessee still has it mapped: a data race over
        // shared memory, driven from ring 3, not a mere annoyance. A stranger
        // calling FREE destroys a hand-off in flight between two other tasks.
        SYS_IPC_LEASE_RETURN => {
            let caller = robot_os_sched::current_task_tid();
            let privileged = robot_os_sched::current_user_pt() == 0;
            match robot_os_ipc::lease_return(a0 as usize, caller, privileged) {
                Some(lessor_tid) => {
                    // Wake the lessor (may be blocked in lease_wait_return).
                    robot_os_sched::wake_fast_ipc_server(lessor_tid);
                    0
                }
                None => -1,
            }
        }

        // SYS_IPC_LEASE_FREE: lessor frees the lease entry after reclaiming buffer.
        // a0=lease_id. See LEASE_RETURN above for why the caller is checked.
        SYS_IPC_LEASE_FREE => {
            let caller = robot_os_sched::current_task_tid();
            let privileged = robot_os_sched::current_user_pt() == 0;
            // Now reports refusal instead of silently answering success: a task
            // that is told 0 for a lease it does not own learns nothing and
            // moves on believing it freed something.
            if robot_os_ipc::lease_free(a0 as usize, caller, privileged) { 0 } else { -1 }
        }

        // SYS_CAP_GRANT: delegate a cap the caller holds to another task.
        // a0=target_tid, a1=cap_handle, a2=want_perms. The grantor is bound
        // to the calling task inside the handler — never taken from ring 3.
        SYS_CAP_GRANT => sys_cap_grant(a0, a1, a2),

        // Network
        SYS_NET_INFO   => sys_net_info(),
        SYS_NET_GETIP  => sys_net_getip(),
        SYS_NET_SETIP  => sys_net_setip(a0, a1, a2),
        SYS_NET_PING   => sys_net_ping(a0),
        SYS_NET_GETMAC => sys_net_getmac(),
        SYS_NET_STATS  => sys_net_stats(),
        // F05: DNS resolver — a0 = hostname_ptr, a1 = hostname_len, a2 = result_ip_ptr
        SYS_DNS_RESOLVE => {
            let mut name_buf = [0u8; 64];
            let name_len = (a1 as usize).min(name_buf.len());
            if name_len == 0 || a0 == 0 { return -1; }
            // The ABI here is (ptr, len), not a NUL-terminated string — there
            // is no libsys wrapper guaranteeing a terminator — so this uses
            // `copy_from_user` rather than `copy_cstr_from_user`, which would
            // change the calling convention.  Either way the point is the same:
            // the old raw read dereferenced an unvalidated address, and since
            // `sstatus.SUM` is never set it could only ever succeed against
            // kernel/MMIO memory, turning the hostname argument into a
            // 64-byte kernel-memory read (an MMIO read here can also have side
            // effects on device registers).
            if !robot_os_sched::copy_from_user(name_buf.as_mut_ptr(), a0 as usize, name_len) {
                return -1;
            }
            let hostname = match core::str::from_utf8(&name_buf[..name_len]) {
                Ok(s) => s,
                Err(_) => return -1,
            };
            match robot_os_net::dns::resolve(hostname) {
                Some(ip) => {
                    if a2 != 0 {
                        // `a2` was an unchecked destination for a 4-byte store
                        // whose value the attacker influences by controlling
                        // the DNS answer — i.e. a targeted 4-byte write to any
                        // kernel address or MMIO register.  `copy_to_user`
                        // enforces VALID+USER+WRITE on the destination page.
                        if !robot_os_sched::copy_to_user(a2 as usize, ip.as_ptr(), 4) {
                            return -1;
                        }
                    }
                    // Return IP as u32 (network byte order)
                    i64::from(u32::from_be_bytes(ip))
                }
                None => -1,
            }
        }
        // F05.2: NTP — a0 unused for SYNC; OFFSET returns Unix seconds
        SYS_NTP_SYNC   => robot_os_net::ntp::ntp_sync() as i64,
        SYS_NTP_OFFSET => robot_os_net::ntp::ntp_offset() as i64,
        SYS_MCAST_JOIN => sys_stub(),
        // SYS_SHUTDOWN (270) and SYS_REBOOT (271) handled above
        SYS_MCAST_LEAVE ..= SYS_SECURE_RECV => sys_stub(),  // 272..=276

        SYS_FDT_INFO   ..= SYS_FDT_DUMP      => sys_stub(),

        // ── F06: Driver server syscalls ─────────────────────────────────────
        // F06.1: SYS_DRV_REGISTER — a0=name_ptr, a1=name_len → drv_id or -1
        SYS_DRV_REGISTER => {
            let mut name = [0u8; 32];
            let name_len = (a1 as usize).min(32);
            if robot_os_sched::copy_from_user(name.as_mut_ptr(), a0 as usize, name_len) {
                match robot_os_sched::driver_register(&name[..name_len]) {
                    Some(id) => id as i64,
                    None     => -1,
                }
            } else { -1 }
        }

        // F06.1: SYS_DRV_UNREGISTER — a0=drv_id (mark as Stopped)
        SYS_DRV_UNREGISTER => sys_stub(),

        // F06.2: SYS_DRV_MMAP — a0=drv_id, a1=mmio_idx → user VA or -1
        SYS_DRV_MMAP => {
            let drv_id   = a0 as usize;
            let mmio_idx = a1 as usize;
            match robot_os_sched::driver_info(drv_id) {
                Some(info) if mmio_idx < info.mmio_count as usize => {
                    let region = info.mmio[mmio_idx];
                    // Capability check, mirroring SYS_MMIO_MAP below: mapping a
                    // device's registers USER_RW into ring 3 is a full grant of
                    // that device (bus-mastering peripherals can then DMA over
                    // kernel memory).  This arm had no check at all — it was
                    // inert only because nothing currently populates the driver
                    // MMIO table, which is not a security property.
                    // `cap_check` returns true for kernel tasks (user_pt == 0),
                    // so in-kernel drivers are unaffected.
                    let kind = robot_os_ipc::HandleKind::MmioRegion(region.base, region.size);
                    if !cap_check(kind, false) {
                        return E_PERM;
                    }
                    // Map the MMIO region into the calling process's page table.
                    match robot_os_sched::mmio_map_user(region.base, region.size) {
                        Some(va) => va as i64,
                        None     => -1,
                    }
                }
                _ => -1,
            }
        }

        // F06.2: SYS_DRV_MUNMAP — a0=va (stub: full unmap not yet supported)
        SYS_DRV_MUNMAP => sys_stub(),

        // F06.3: SYS_DRV_IRQ_WAIT — a0=irq_num → blocks until IRQ fires, returns 0
        SYS_DRV_IRQ_WAIT => {
            let irq = a0 as u32;
            robot_os_sched::task_block(robot_os_sched::WaitReason::Irq(irq));
            0
        }

        // F06.3: SYS_DRV_IRQ_ACK — a0=irq_num → acknowledge PLIC
        SYS_DRV_IRQ_ACK => {
            {
                // Require the same `Irq(n)` handle SYS_IRQ_BIND demands.
                // Without it any task could send PLIC completion for any
                // enabled IRQ, stealing another driver's completion and
                // letting the line re-arm while that driver is still handling
                // it.  `plic::complete` is itself bounds- and enable-checked,
                // so this closes the ownership hole rather than a memory one.
                let irq = a0 as u32;
                if !cap_check(robot_os_ipc::HandleKind::Irq(irq), false) {
                    return E_PERM;
                }
                let hart = robot_os_arch::cpu::hart_id() as u32;
                robot_os_drivers::plic::complete(hart, irq);
            }
            0
        }

        // F06.4: SYS_DRV_DMA_ALLOC — a0=size_bytes → phys addr or -1
        // Allocates one physically contiguous page (4 KiB minimum unit).
        // Drivers requesting larger DMA buffers should call multiple times.
        SYS_DRV_DMA_ALLOC => {
            // KERNEL-ONLY.  See the SYS_DRV_DMA_FREE comment below: the pair
            // has no ownership model, and this arm is the aiming aid — it
            // hands a raw physical address straight to the caller.  Nothing in
            // `crates/libsys` or `userspace/` invokes it, so gating costs
            // nothing today; re-opening it to ring 3 requires a per-TID
            // provenance table *and* a DMA capability kind, neither of which
            // exists yet.
            if robot_os_sched::current_user_pt() != 0 {
                return E_PERM;
            }
            const DMA_MAX_SINGLE_ALLOC: usize = 65536; // 64 KiB = 16 pages
            let size = a0 as usize;
            if size == 0 || size > DMA_MAX_SINGLE_ALLOC {
                -1
            } else {
                // NOTE (pre-existing, unrelated to the gate): this returns a
                // single 4 KiB page no matter what `size` asked for.
                match robot_os_mm::pmm::alloc_page() {
                    Ok(phys) => phys.0 as i64,
                    Err(_)   => -1,
                }
            }
        }

        // F06.4: SYS_DRV_DMA_FREE — a0=phys_addr
        SYS_DRV_DMA_FREE => {
            // KERNEL-ONLY.  This arm used to free an arbitrary, fully
            // user-controlled physical address with no capability check and no
            // allocation provenance.  `pmm::init` marks kernel pages as
            // allocated, so the bitmap's double-free guard *passes* for them:
            // `ecall(SYS_DRV_DMA_FREE, 0x8020_0000)` returned kernel text to
            // the free list.  The next `alloc_page` handed that page out and
            // zeroed it — or the attacker claimed it via brk/mmap and got it
            // mapped USER_RW, i.e. arbitrary kernel read/write from ring 3.
            //
            // A correct fix needs per-TID DMA-page ownership so a free can
            // only release a page the caller allocated; that table also needs
            // a task-exit cleanup hook and a quota, which is more machinery
            // than this lane should introduce for a syscall with zero callers.
            // Gated to kernel tasks instead: an unreachable syscall cannot be
            // an escape primitive.
            if robot_os_sched::current_user_pt() != 0 {
                return E_PERM;
            }
            use robot_os_mm::addr::PhysAddr;
            let _ = robot_os_mm::pmm::free_page(PhysAddr(a0 as usize));
            0
        }

        // F06.4: SYS_DRV_DMA_SYNC — a0=phys_addr, a1=size (cache flush — no-op on QEMU)
        SYS_DRV_DMA_SYNC => {
            // On VF2/K1 hardware this would issue a RISC-V fence instruction.
            // QEMU has coherent caches so no action is needed.
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            0
        }

        // F06.5: SYS_DRV_HEARTBEAT — a0=drv_id → 0
        SYS_DRV_HEARTBEAT => {
            let now_ms = robot_os_drivers::clint::get_time()
                / (robot_os_drivers::clint::TIMER_FREQ / 1000);
            robot_os_sched::driver_heartbeat_with_time(a0 as usize, now_ms);
            0
        }

        // F06.6: SYS_DRV_GET_DEVICE — a0=drv_id, a1=out_ptr, a2=out_len → bytes written or -1
        SYS_DRV_GET_DEVICE => {
            match robot_os_sched::driver_info(a0 as usize) {
                Some(info) => {
                    // Copy driver name to userspace (out_ptr, out_len bytes)
                    let name_len = info.name.iter().position(|&b| b == 0)
                        .unwrap_or(info.name.len());
                    let copy_len = name_len.min(a2 as usize);
                    if robot_os_sched::copy_to_user(
                        a1 as usize, info.name.as_ptr(), copy_len)
                    {
                        copy_len as i64
                    } else { -1 }
                }
                None => -1,
            }
        }
        SYS_ROBOT_INIT ..= SYS_SENSOR_ADD     => sys_stub(),
        SYS_SENSOR_READ => sys_sensor_read(a0, a1, a2),
        SYS_PLATFORM_INFO ..= SYS_PLATFORM_TYPE => sys_stub(),

        // Sockets (Phase 9)
        SYS_SOCKET   => sys_socket(a0, a1, a2),
        SYS_BIND     => sys_bind(a0, a1, a2),
        SYS_LISTEN   => sys_listen_syscall(a0, a1),
        SYS_ACCEPT   => sys_accept(a0, a1, a2),
        SYS_CONNECT  => sys_connect_syscall(a0, a1, a2),
        SYS_SEND     => sys_send_syscall(a0, a1, a2, a3),
        SYS_RECV     => sys_recv_syscall(a0, a1, a2, a3),
        SYS_SENDTO   => sys_send_syscall(a0, a1, a2, a3),
        SYS_RECVFROM => sys_recv_syscall(a0, a1, a2, a3),
        SYS_SOCK_SHUTDOWN => sys_sock_close(a0),
        SYS_GETSOCKNAME | SYS_GETPEERNAME => sys_stub(),

        // Security (AQ11): activate syscall filter (one-way)
        SYS_SECCOMP => robot_os_sched::seccomp::activate_profile(a0),

        // IO Ring (AQ1)
        SYS_IO_SETUP => {
            let tid = robot_os_sched::current_task_tid() as usize;
            match robot_os_ipc::io_ring_create(tid) {
                Some((ring_id, _sq_base)) => ring_id as i64,
                None => -1,
            }
        }
        SYS_IO_SUBMIT => {
            // a0 = ring_id — process all pending SQ entries and write CQ completions.
            if !io_ring_access_ok(a0) { return E_PERM; }
            robot_os_ipc::io_ring_submit(a0 as u32) as i64
        }
        SYS_IO_WAIT => {
            // a0 = ring_id — return number of pending completions.
            if !io_ring_access_ok(a0) { return E_PERM; }
            robot_os_ipc::io_ring_pending(a0 as u32) as i64
        }

        // M05: Async IO Ring submit — non-blocking, worker task processes SQEs.
        // a0 = ring_id (currently unused — worker polls all rings).
        // Returns 0 immediately; completions appear in CQ when worker runs.
        //
        // NOTE (W3-F3): `a0` is ignored and the worker sweeps every ring, so
        // this call lets any task trigger processing of any other task's
        // queued SQEs. It is contained rather than blocked: `dispatch_sqe`
        // now checks each opcode against the *ring owner's* capabilities, so
        // the worst a stranger can do is make the owner's own already-queued
        // work run earlier. Blocking it properly needs a per-ring async
        // signal, which is an ABI change — see the report.
        SYS_IO_SUBMIT_ASYNC => {
            robot_os_ipc::io_ring_signal_async();
            0
        }

        // Channels (AQ1)
        SYS_CHAN_CREATE => {
            match robot_os_ipc::channel_create() {
                Some(idx) => idx as i64,
                None => -1,
            }
        }
        SYS_CHAN_WRITE => sys_ipc_send(a0, a1, a2),
        SYS_CHAN_READ  => sys_ipc_recv(a0, a1, a2),

        // PHANES Phase 1 W3 — Cap<T> typed IPC (RFC-0003).
        SYS_CHAN_WRITE_TYPED => sys_chan_write_typed(a0, a1, a2),
        SYS_CHAN_READ_TYPED  => sys_chan_read_typed(a0, a1, a2),

        // PHANES Phase 1 W5 — Cap<Port> typed port API.
        SYS_PORT_CREATE_TYPED  => sys_port_create_typed(),
        SYS_PORT_POLL_TYPED    => sys_port_poll_typed(a0, a1),
        SYS_PORT_DESTROY_TYPED => sys_port_destroy_typed(a0),

        // PHANES Phase 1 W5 batch 2 — Cap<Shm> typed shared-memory API.
        SYS_SHM_CREATE_TYPED   => sys_shm_create_typed(a0, a1),
        SYS_SHM_ACQUIRE_TYPED  => sys_shm_acquire_typed(a0, a1),
        SYS_SHM_RELEASE_TYPED  => sys_shm_release_typed(a0),

        // PHANES Phase 1 W5 batch 3 — Cap<IoRing> typed io_ring API.
        SYS_IORING_CREATE_TYPED  => sys_ioring_create_typed(a0),
        SYS_IORING_SUBMIT_TYPED  => sys_ioring_submit_typed(a0),
        SYS_IORING_DESTROY_TYPED => sys_ioring_destroy_typed(a0),

        // PHANES Phase 1 W5 batch 5.1 — Cap<Gpio> typed hardware API.
        SYS_GPIO_READ_TYPED      => sys_gpio_read_typed(a0),
        SYS_GPIO_WRITE_TYPED     => sys_gpio_write_typed(a0, a1),
        SYS_GPIO_SET_DIR_TYPED   => sys_gpio_set_dir_typed(a0, a1),

        // PHANES Phase 1 W5 batch 5.2 — Cap<I2c> typed hardware API.
        SYS_I2C_READ_TYPED       => sys_i2c_read_typed(a0, a1, a2, a3),
        SYS_I2C_WRITE_TYPED      => sys_i2c_write_typed(a0, a1, a2),
        SYS_I2C_DETECT_TYPED     => sys_i2c_detect_typed(a0),

        // PHANES Phase 1 W5 batch 5.3 — Cap<Pwm> typed hardware API.
        SYS_PWM_ENABLE_TYPED        => sys_pwm_enable_typed(a0),
        SYS_PWM_DISABLE_TYPED       => sys_pwm_disable_typed(a0),
        SYS_PWM_SET_PERIOD_TYPED    => sys_pwm_set_period_typed(a0, a1),
        SYS_PWM_SET_DUTY_TYPED      => sys_pwm_set_duty_typed(a0, a1),
        SYS_PWM_SET_DUTY_PCT_TYPED  => sys_pwm_set_duty_pct_typed(a0, a1),

        // PHANES Phase 1 W5 batch 5.4 — Cap<Motor> typed hardware API
        // (opens the cap-typed extension range 550..=569).
        SYS_MOTOR_SET_TARGET_TYPED  => sys_motor_set_target_typed(a0, a1, a2),
        SYS_MOTOR_TICK_TYPED        => sys_motor_tick_typed(a0, a1, a2, a3, a4),
        SYS_MOTOR_ENABLE_TYPED      => sys_motor_enable_typed(a0, a1),
        SYS_MOTOR_ENABLED_TYPED     => sys_motor_enabled_typed(a0),
        SYS_MOTOR_SET_GAINS_TYPED   => sys_motor_set_gains_typed(a0, a1, a2, a3),
        SYS_MOTOR_RESET_TYPED       => sys_motor_reset_typed(a0),

        // RFC-0002 Driver registry bridge — userspace invokes a
        // driver by (kind, op). Six args; uses a5 (previously a5).
        SYS_DRV_INVOKE           => sys_drv_invoke(a0, a1, a2, a3, a4, a5),

        // MMIO mapping (F00.2): a0 = phys_base, a1 = size_bytes
        // Maps a physical MMIO region into the calling task's userspace page table.
        // Requires the caller to hold a Handle(MmioRegion(phys_base, size)).
        SYS_MMIO_MAP => {
            {
                let phys_base = a0 as usize;
                let size = a1 as usize;
                // Capability check: caller must own MmioRegion handle
                let kind = robot_os_ipc::HandleKind::MmioRegion(phys_base, size);
                if !cap_check(kind, false) {
                    return E_PERM;
                }
                // Map into user page table (implemented in process module)
                match robot_os_sched::process::mmio_map_user(phys_base, size) {
                    Some(va) => va as i64,
                    None => -1,
                }
            }
        }

        // IRQ binding (F00.3): a0 = irq_number, a1 = target_type, a2 = target_id, a3 = user_key
        // target_type: 0=wake_task (default via scheduler), 1=queue_to_port
        SYS_IRQ_BIND => {
            let irq = a0 as u32;
            // Capability check: caller must own Irq handle
            let kind = robot_os_ipc::HandleKind::Irq(irq);
            if !cap_check(kind, false) {
                return E_PERM;
            }
            let tid = robot_os_sched::current_task_tid();
            let target = match a1 {
                0 => robot_os_ipc::IrqTarget::WakeTask(tid),
                1 => robot_os_ipc::IrqTarget::QueueToPort(a2 as u32, a3),
                _ => return -1,
            };
            robot_os_ipc::irq_bind(irq, tid, target) as i64
        }

        // Ports (AQ5)
        SYS_PORT_CREATE => {
            let tid = robot_os_sched::current_task_tid() as usize;
            match robot_os_ipc::port_create(tid) {
                Some(id) => id as i64,
                None => -1,
            }
        }
        SYS_PORT_BIND => {
            // a0 = port_id, a1 = source_type, a2 = source_id, a3 = user_key
            // source_type: 0=channel, 1=ring, 2=irq, 3=timer
            if !port_access_ok(a0) { return E_PERM; }
            /// Port source type constants for syscall interface.
            const PORT_SRC_CHANNEL: u64 = 0;
            const PORT_SRC_RING:    u64 = 1;
            const PORT_SRC_IRQ:     u64 = 2;
            const PORT_SRC_TIMER:   u64 = 3;

            let kind = match a1 {
                PORT_SRC_CHANNEL => robot_os_ipc::PortSourceKind::Channel(a2 as u32),
                PORT_SRC_RING    => robot_os_ipc::PortSourceKind::Ring(a2 as u32),
                PORT_SRC_IRQ     => robot_os_ipc::PortSourceKind::Irq(a2 as u32),
                PORT_SRC_TIMER   => robot_os_ipc::PortSourceKind::Timer(a2),
                _ => return -1,
            };
            if robot_os_ipc::port_bind(a0 as u32, kind, a3) { 0 } else { -1 }
        }
        SYS_PORT_WAIT => {
            // a0 = port_id — poll for one event, return key or -1.
            // The check must come BEFORE the first poll: the miss path blocks
            // on `WaitReason::Port(a0)`, so a non-owner that got past here
            // would sleep forever on somebody else's port.
            if !port_access_ok(a0) { return E_PERM; }
            match robot_os_ipc::port_poll(a0 as u32) {
                Some(evt) => evt.key as i64,
                None => {
                    // Block until an event arrives (AQ0: IO-wait).
                    robot_os_sched::task_block(robot_os_sched::WaitReason::Port(a0 as u32));
                    match robot_os_ipc::port_poll(a0 as u32) {
                        Some(evt) => evt.key as i64,
                        None => -1,
                    }
                }
            }
        }
        SYS_PORT_UNBIND => {
            // a0 = port_id — destroy the port entirely (simplified).
            if !port_access_ok(a0) { return E_PERM; }
            robot_os_ipc::port_destroy(a0 as u32);
            0
        }

        // Handles (AQ6 + F00.6: generalized grant)
        // a0 = owner_tid, a1 = kind_type, a2 = param0, a3 = param1, a4 = perms_bits
        // kind_type: 0=Sensor, 1=Gpio, 2=I2c, 3=Pwm, 4=Motor, 8=Irq, 9=MmioRegion
        // (5=Channel, 6=Ring, 7=Port retired — those HandleKind variants had no
        // cap_check consumer; those kind codes now fall through to EINVAL below)
        // perms_bits: bit0=read, bit1=write, bit2=execute, bit3=duplicate
        SYS_HANDLE_GRANT => {
            // Only kernel tasks can grant handles (user_pt == 0)
            if robot_os_sched::current_user_pt() != 0 {
                return E_PERM;
            }
            /// Handle kind type constants for syscall interface.
            const HANDLE_KIND_SENSOR:      u64 = 0;
            const HANDLE_KIND_GPIO:        u64 = 1;
            const HANDLE_KIND_I2C:         u64 = 2;
            const HANDLE_KIND_PWM:         u64 = 3;
            const HANDLE_KIND_MOTOR:       u64 = 4;
            const HANDLE_KIND_IRQ:         u64 = 8;
            const HANDLE_KIND_MMIO_REGION: u64 = 9;

            let kind = match a1 {
                HANDLE_KIND_SENSOR      => robot_os_ipc::HandleKind::Sensor(a2 as u8),
                HANDLE_KIND_GPIO        => robot_os_ipc::HandleKind::Gpio(a2 as u32),
                HANDLE_KIND_I2C         => robot_os_ipc::HandleKind::I2c(a2 as u8, a3 as u8),
                HANDLE_KIND_PWM         => robot_os_ipc::HandleKind::Pwm(a2 as u8),
                HANDLE_KIND_MOTOR       => robot_os_ipc::HandleKind::Motor(a2 as u32),
                HANDLE_KIND_IRQ         => robot_os_ipc::HandleKind::Irq(a2 as u32),
                HANDLE_KIND_MMIO_REGION => robot_os_ipc::HandleKind::MmioRegion(a2 as usize, a3 as usize),
                _ => return -1,
            };

            /// Permission bit masks for handle grants.
            const PERM_BIT_READ:      u64 = 0x1;
            const PERM_BIT_WRITE:     u64 = 0x2;
            const PERM_BIT_EXECUTE:   u64 = 0x4;
            const PERM_BIT_DUPLICATE: u64 = 0x8;

            let perms = robot_os_ipc::HandlePerms {
                read:      a4 & PERM_BIT_READ      != 0,
                write:     a4 & PERM_BIT_WRITE     != 0,
                execute:   a4 & PERM_BIT_EXECUTE   != 0,
                duplicate: a4 & PERM_BIT_DUPLICATE != 0,
            };

            match robot_os_ipc::handle_grant(a0 as u32, kind, perms) {
                Some(id) => id as i64,
                None => -1,
            }
        }
        SYS_HANDLE_REVOKE => {
            // a0 = handle_id. W2-C3: a userspace caller may revoke only a
            // handle it owns; kernel tasks (user_pt == 0) are privileged.
            let caller_tid = robot_os_sched::current_task_tid();
            let privileged = robot_os_sched::current_user_pt() == 0;
            if robot_os_ipc::handle_revoke(a0 as u32, caller_tid, privileged) { 0 } else { -1 }
        }
        SYS_HANDLE_DUP => {
            // a0 = handle_id, a1 = new_owner_tid. W2-C3: a userspace caller may
            // dup only a handle it owns, and only into its own table; kernel
            // tasks (user_pt == 0) retain unrestricted delegation.
            let caller_tid = robot_os_sched::current_task_tid();
            let privileged = robot_os_sched::current_user_pt() == 0;
            match robot_os_ipc::handle_dup(a0 as u32, caller_tid, a1 as u32, privileged) {
                Some(id) => id as i64,
                None => -1,
            }
        }

        // Trace (AQ8)
        SYS_TRACE_DUMP => {
            /// Default number of trace entries to dump.
            const TRACE_DUMP_DEFAULT_COUNT: usize = 50;
            let count = if a0 == 0 { TRACE_DUMP_DEFAULT_COUNT } else { a0 as usize };
            robot_os_ipc::trace_dump(count);
            0
        }

        _ => -1,
    }
}
