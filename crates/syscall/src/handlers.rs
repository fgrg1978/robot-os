/// Syscall handlers — port of kernel/core/syscall.c
///
/// Each function corresponds to one syscall.  Handlers for subsystems that
/// are not yet fully implemented return -1.
///
/// Security (AQ6): Hardware-access syscalls validate handles when the caller
/// is a user-space process (user_pt != 0). Kernel tasks bypass handle checks
/// (they have full access). This allows gradual migration: existing kernel
/// tasks work unchanged, new userspace drivers must be granted handles.

use robot_os_fs::{FdTable, vfs_open, vfs_close, vfs_read, vfs_write, vfs_lseek,
                  inode_alloc, dir_add_entry, dir_remove_entry,
                  INODE_DIR, PERM_READ, PERM_WRITE, PERM_EXEC};
use robot_os_sync::SpinLock;
use robot_os_ipc::{signal_send, signal_pending, signal_set_handler, signal_get_mask, signal_set_mask,
                   pipe_create};
use robot_os_service::{service_register, service_discover, service_stop, service_heartbeat};
use robot_os_drivers::gpio::{gpio_read, gpio_write, gpio_set_direction, gpio_info, GpioDir};
use robot_os_drivers::pwm::{pwm_enable, pwm_disable, pwm_set_period, pwm_set_duty, pwm_info};
use robot_os_drivers::i2c::{i2c_read, i2c_write, i2c_scan, i2c_info};
use robot_os_robot::{motor_init, motor_set, motor_stop, motor_info, MotorDir};
use robot_os_net::{
    socket_bind,
    socket_listen_bound,
    socket_create_owned, socket_accept_owned,
    socket_send, socket_recv, socket_close,
    SockAddr, net_info, net_get_ip,
};

// ── AQ6: Handle-based capability check ────────────────────────────────────────
//
// Userspace processes must have a valid handle for hardware resources.
// Kernel tasks (user_pt == 0) bypass this check — full access.
// Returns true if the caller has permission, false to deny.

/// Check if the current task is allowed to access a hardware resource.
/// Kernel tasks always pass. Userspace tasks need a matching handle.
pub fn cap_check(kind: robot_os_ipc::HandleKind, need_write: bool) -> bool {
    // Kernel tasks have full access (no handle needed)
    if robot_os_sched::current_user_pt() == 0 {
        return true;
    }
    // Userspace: one locked pass over the handle table.
    //
    // This used to call `handle_check` once per index, and `handle_check`
    // takes `HANDLES.lock_irqsave()` — so a single capability check cost up
    // to MAX_HANDLES_GLOBAL (256) lock/unlock pairs, each one toggling
    // interrupts. Measured from ring 3 (`userspace/latbench`, QEMU virt):
    // a denied check ran ~98 us slower than the bare syscall floor, and a
    // granted one paid ~250-380 ns for every table slot it had to walk past
    // before finding its match. Every hardware syscall goes through here.
    let tid = robot_os_sched::current_task_tid();
    robot_os_ipc::handle_owned_by(tid, kind, need_write)
}

/// Permission denied error code.
const E_PERM: i64 = -99;

// ── User virtual-address ceiling ─────────────────────────────────────────────
//
// Mirrors `USER_STACK_TOP` in `crates/sched/src/process.rs:16` (private there,
// so it cannot be imported).  Keep the two in sync: `process.rs` refuses to
// load an ELF segment at or above this address and places the user stack top
// exactly here, so nothing legitimately reachable from ring 3 lives above it.
//
// WHY a syscall-side copy exists at all: user and kernel page tables **share
// their L1/L0 tables** (`vmm::copy_kernel_entries_to_user` splices the kernel
// and MMIO entries into every user PT).  A VA-taking syscall that walks the
// "user" page table above this line is therefore editing the *kernel's* page
// table.  `sys_munmap(0x1000_0000, 4096)` used to zero the UART PTE for every
// hart; the next `kprintln!` faulted in S-mode and, with `panic = "abort"`,
// reset the board.  On a robot that is a physical-safety event, so any syscall
// that takes a raw VA range must reject addresses at or above this bound
// before touching a PTE.
pub(crate) const USER_VA_TOP: usize = 0x0000_0000_8000_0000; // 2 GiB

// ── ELF bounce buffer for SYS_EXEC / SYS_EXECPATH ────────────────────────────
//
// Largest ELF the tree currently produces is ~12.5 KiB (`build/brain_client.elf`);
// 128 KiB leaves an order of magnitude of headroom while staying far cheaper in
// `.bss` than the shell's 256 KiB buffer (which VF2/K1 linker scripts also pay
// for).
const EXEC_MAX_BYTES: usize = 128 * 1024;

/// Shared kernel bounce buffer for both exec syscalls.
///
/// WHY it exists:
///   - `SYS_EXEC` used to build a slice straight off the user-supplied
///     `(ptr, len)` pair with `from_raw_parts`.  `sstatus.SUM` is never set in
///     this tree, so S-mode cannot read a USER page at all: the old code was
///     only ever "functional" for kernel/MMIO addresses (an ELF-parser oracle
///     over kernel memory) and took a fatal, unrecoverable S-mode load fault —
///     i.e. a board reset — for any honest user pointer.  Routing through
///     `copy_from_user` walks `vmm::translate_user`, which enforces
///     VALID+USER+READ at every leaf level and rejects kernel/MMIO outright.
///   - `SYS_EXECPATH` used to `Vec::extend_from_slice` the whole file with no
///     total cap.  A large file on the FAT32 image exhausted the kernel heap;
///     the allocation error path panics, and `panic = "abort"` turns that into
///     a reset.  A fixed static cannot exhaust anything.
///
/// Serialised by a `SpinLock` because both callers hold it across
/// `exec_user`, which returns normally (it publishes the exec hand-off on
/// the caller's own task rather than switching away), so the guard always
/// drops. The lock protects exactly the shared BUFFER — two harts exec'ing
/// at once must not interleave their ELF bytes; the hand-off itself is
/// per-task since K-C21 and needs no serialisation here.
static EXEC_BOUNCE: SpinLock<[u8; EXEC_MAX_BYTES]> = SpinLock::new([0u8; EXEC_MAX_BYTES]);

// ── Global kernel FD table ────────────────────────────────────────────────────
// Used until per-process FD tables are implemented in Phase 7+.

static KERNEL_FD_TABLE: SpinLock<FdTable> = SpinLock::new(FdTable::new());

// ── Console I/O ───────────────────────────────────────────────────────────────

pub fn sys_test() -> i64 {
    robot_os_drivers::uart::puts("[SYSCALL] test ok\n");
    0
}

pub fn sys_putchar(c: u64) -> i64 {
    robot_os_drivers::uart::putc(c as u8);
    0
}

pub fn sys_getchar() -> i64 {
    if robot_os_drivers::uart::can_read() {
        robot_os_drivers::uart::getc() as i64
    } else {
        -1
    }
}

// ── Process ───────────────────────────────────────────────────────────────────

pub fn sys_exit(_code: u64) -> i64 {
    robot_os_sched::task_exit()
}

pub fn sys_getpid() -> i64 {
    robot_os_sched::current_task_tid() as i64
}

pub fn sys_yield() -> i64 {
    robot_os_sched::task_yield();
    0
}

pub fn sys_fork(sepc: u64, user_sp: u64, regs: &[u64; 32]) -> i64 {
    robot_os_sched::process::sys_fork_impl(sepc, user_sp, regs)
}
pub fn sys_wait() -> i64   { -1 }  // Phase 8+

/// SYS_EXEC: a0 = pointer to ELF data in kernel memory (for now).
///
/// In a real system a0 would be a user-space path string, but for Phase 7 we
/// accept a raw `(ptr, len)` pair: a0 = data pointer, a1 = byte length.
/// The ELF is loaded into a new user address space.  On success the
/// trap_handler will switch to U-mode on SRET.
pub fn sys_exec(data_ptr: u64, len: u64) -> i64 {
    if data_ptr == 0 || len == 0 { return -1; }
    // Reject before copying rather than truncating: a silently-clipped ELF
    // would be parsed as a corrupt image and the failure would be reported as
    // "bad ELF" instead of "too large".
    let len = len as usize;
    if len > EXEC_MAX_BYTES { return -1; }

    let mut buf = EXEC_BOUNCE.lock();
    // `copy_from_user` validates the whole range page by page through
    // `vmm::translate_user` (VALID+USER+READ), so a kernel/MMIO pointer is
    // rejected here instead of being dereferenced.  See EXEC_BOUNCE above.
    if !robot_os_sched::copy_from_user(buf.as_mut_ptr(), data_ptr as usize, len) {
        return -1;
    }
    robot_os_sched::exec_user(&buf[..len])
}

pub fn sys_execpath(path_ptr: u64) -> i64 {
    if path_ptr == 0 { return -1; }

    // Copy the path from user space.
    let mut path_buf = [0u8; 256];
    if robot_os_sched::copy_cstr_from_user(&mut path_buf, path_ptr as usize).is_none() {
        return -1;
    }
    let path_len = path_buf.iter().position(|&b| b == 0).unwrap_or(0);
    if path_len == 0 { return -1; }
    let path = &path_buf[..path_len];

    // Open the file via VFS.
    let mut fd_table = {
        let t = KERNEL_FD_TABLE.lock();
        *t
    };
    let fd = robot_os_fs::vfs_open(&mut fd_table, path, robot_os_fs::O_RDONLY);
    if fd < 0 { return -1; }

    // Read the ELF into the fixed kernel bounce buffer.  The previous version
    // grew an unbounded `Vec` on the kernel heap: a large file on the mounted
    // FAT32 image exhausted it, and the allocation-error path panics — which,
    // under `panic = "abort"`, resets the board.  A fixed buffer with an
    // explicit cap cannot do that.
    let mut buf = EXEC_BOUNCE.lock();
    let mut total = 0usize;
    loop {
        // Never hand `vfs_read` a length that would run past the buffer: the
        // remaining-space clamp is what makes the cap real.
        let want = EXEC_MAX_BYTES.saturating_sub(total).min(512);
        if want == 0 {
            // The file reached the cap.  Refuse rather than exec a possibly
            // truncated image (which would be misreported as a corrupt ELF).
            // A file of exactly EXEC_MAX_BYTES is rejected too — erring toward
            // the safe side, and two orders of magnitude above any ELF the
            // tree currently builds.
            robot_os_fs::vfs_close(&mut fd_table, fd);
            return -1;
        }
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, unsafe { buf.as_mut_ptr().add(total) }, want);
        if n <= 0 { break; }
        // Defensive clamp: a driver returning more than `want` must not be
        // able to walk `total` past the buffer end.
        total = total.saturating_add((n as usize).min(want)).min(EXEC_MAX_BYTES);
    }
    robot_os_fs::vfs_close(&mut fd_table, fd);

    if total == 0 { return -1; }
    robot_os_sched::exec_user(&buf[..total])
}

pub fn sys_sleep(ms: u64) -> i64 {
    // Busy-wait via CLINT (rough approximation; real sleep needs scheduler support)
    let ticks = ms.saturating_mul(10_000);  // ~10MHz CLINT
    let start = robot_os_drivers::clint::get_time();
    while robot_os_drivers::clint::get_time().wrapping_sub(start) < ticks {}
    0
}

// ── File I/O ──────────────────────────────────────────────────────────────────

pub fn sys_open(path_ptr: u64, flags: u64) -> i64 {
    // When called from user space, copy the path string safely.
    if robot_os_sched::current_user_pt() != 0 {
        let mut path_buf = [0u8; 256];
        match robot_os_sched::copy_cstr_from_user(&mut path_buf, path_ptr as usize) {
            Some(_) => {
                let path_len = path_buf.iter().position(|&b| b == 0).unwrap_or(0);
                let path = &path_buf[..path_len];
                let mut table = KERNEL_FD_TABLE.lock();
                vfs_open(&mut *table, path, flags as u32) as i64
            }
            None => -1,
        }
    } else {
        let path = unsafe { robot_os_fs::cstr_to_bytes(path_ptr as *const u8) };
        let mut table = KERNEL_FD_TABLE.lock();
        vfs_open(&mut *table, path, flags as u32) as i64
    }
}

pub fn sys_close(fd: u64) -> i64 {
    let mut table = KERNEL_FD_TABLE.lock();
    vfs_close(&mut *table, fd as i32) as i64
}

pub fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    let count = count as usize;
    if robot_os_sched::current_user_pt() != 0 {
        // Read into a kernel temp buffer, then copy_to_user.
        const MAX: usize = 4096;
        let chunk = count.min(MAX);
        let mut tmp = [0u8; MAX];
        let n = {
            let mut table = KERNEL_FD_TABLE.lock();
            vfs_read(&mut *table, fd as i32, tmp.as_mut_ptr(), chunk)
        };
        if n > 0 {
            if !robot_os_sched::copy_to_user(buf as usize, tmp.as_ptr(), n as usize) {
                return -1;
            }
        }
        n as i64
    } else {
        let mut table = KERNEL_FD_TABLE.lock();
        vfs_read(&mut *table, fd as i32, buf as *mut u8, count) as i64
    }
}

pub fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    let count = count as usize;
    if robot_os_sched::current_user_pt() != 0 {
        // Copy user buffer into kernel temp buffer.
        const MAX: usize = 4096;
        let chunk = count.min(MAX);
        let mut tmp = [0u8; MAX];
        if !robot_os_sched::copy_from_user(tmp.as_mut_ptr(), buf as usize, chunk) {
            return -1;
        }
        // fd 1 (stdout) and fd 2 (stderr): write directly to UART.
        // The kernel FD table does not pre-open stdio for user processes.
        if fd == 1 || fd == 2 {
            // Two changes from the old byte-at-a-time loop:
            //
            // 1. One `uart::acquire()` for the whole write. `kprintln!`
            //    already takes the UART guard per line, but this path took
            //    nothing at all — so a userspace write and a kernel print on
            //    another hart interleaved mid-line. That is not just ugly:
            //    the CI scenarios grep this output, and a spliced line makes
            //    a passing run look like a failing one.
            //
            // 2. `write_str_translated` loads the 16-byte TX FIFO per
            //    line-status poll instead of polling once per byte. Measured
            //    from ring 3 (`userspace/latbench`): a 64-byte write cost
            //    241 us against a 2.3 us syscall floor, essentially all of it
            //    MMIO polling.
            //
            // The guard disables interrupts for the duration, so the write is
            // bounded deliberately: `chunk` is capped at MAX above.
            let _guard = robot_os_drivers::uart::acquire();
            robot_os_drivers::uart::write_str_translated(&tmp[..chunk]);
            return chunk as i64;
        }
        let mut table = KERNEL_FD_TABLE.lock();
        vfs_write(&mut *table, fd as i32, tmp.as_ptr(), chunk) as i64
    } else {
        let mut table = KERNEL_FD_TABLE.lock();
        vfs_write(&mut *table, fd as i32, buf as *const u8, count) as i64
    }
}

pub fn sys_lseek(fd: u64, offset: u64, whence: u64) -> i64 {
    let mut table = KERNEL_FD_TABLE.lock();
    vfs_lseek(&mut *table, fd as i32, offset as i32, whence as i32) as i64
}

// ── Filesystem ────────────────────────────────────────────────────────────────

/// Maximum filesystem path length copied from userspace. Matches sys_open.
const SYS_PATH_MAX: usize = 256;

/// Copy a NUL-terminated path from a user pointer into `dst`, returning the
/// path slice. Mirrors `sys_open`'s pattern so a malformed/kernel pointer can't
/// drive the FS code via raw `cstr_to_bytes`.
fn copy_path_from_user<'a>(dst: &'a mut [u8], path_ptr: u64) -> Option<&'a [u8]> {
    if robot_os_sched::current_user_pt() != 0 {
        robot_os_sched::copy_cstr_from_user(dst, path_ptr as usize)?;
        let len = dst.iter().position(|&b| b == 0).unwrap_or(dst.len());
        Some(&dst[..len])
    } else {
        Some(unsafe { robot_os_fs::cstr_to_bytes(path_ptr as *const u8) })
    }
}

pub fn sys_mkdir(path_ptr: u64) -> i64 {
    let mut buf = [0u8; SYS_PATH_MAX];
    let path = match copy_path_from_user(&mut buf, path_ptr) { Some(p) => p, None => return -1 };
    let (parent_idx, name) = robot_os_fs::path_parent(path);
    if parent_idx == robot_os_fs::NO_IDX || name.is_empty() { return -1; }
    let dir_idx = inode_alloc(INODE_DIR, PERM_READ | PERM_WRITE | PERM_EXEC);
    if dir_idx == robot_os_fs::NO_IDX { return -1; }
    match dir_add_entry(parent_idx, name, dir_idx) {
        Ok(()) => 0,
        Err(()) => { robot_os_fs::inode_free(dir_idx); -1 }
    }
}

pub fn sys_unlink(path_ptr: u64) -> i64 {
    let mut buf = [0u8; SYS_PATH_MAX];
    let path = match copy_path_from_user(&mut buf, path_ptr) { Some(p) => p, None => return -1 };
    let (parent_idx, name) = robot_os_fs::path_parent(path);
    if parent_idx == robot_os_fs::NO_IDX || name.is_empty() { return -1; }
    match dir_remove_entry(parent_idx, name) {
        Ok(()) => 0,
        Err(()) => -1,
    }
}

/// SYS_READDIR: read directory entry at `index`.
/// a0=path_ptr, a1=index, a2=name_out (64-byte buf), a3=size_out (*u32), a4=is_dir_out (*u32).
/// Returns 0 on success, -1 if out of range or error.
pub fn sys_readdir(path_ptr: u64, index: u64, name_out: u64, size_out: u64, is_dir_out: u64) -> i64 {
    if path_ptr == 0 || name_out == 0 { return -1; }

    // Resolve path to directory inode
    let dir_idx = if robot_os_sched::current_user_pt() != 0 {
        let mut path_buf = [0u8; 256];
        match robot_os_sched::copy_cstr_from_user(&mut path_buf, path_ptr as usize) {
            Some(_) => robot_os_fs::path_lookup(&path_buf),
            None => return -1,
        }
    } else {
        let path = unsafe { robot_os_fs::cstr_to_bytes(path_ptr as *const u8) };
        robot_os_fs::path_lookup(path)
    };

    if dir_idx == robot_os_fs::NO_IDX { return -1; }

    match robot_os_fs::dir_entry_at(dir_idx, index as u32) {
        Some((name, size, is_dir)) => {
            let name_len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            let copy_len = name_len.min(63); // leave room for NUL
            if robot_os_sched::current_user_pt() != 0 {
                let mut tmp = [0u8; 64];
                tmp[..copy_len].copy_from_slice(&name[..copy_len]);
                if !robot_os_sched::copy_to_user(name_out as usize, tmp.as_ptr(), 64) {
                    return -1;
                }
                // Checked like `name_out` above: returning 0 with these
                // fields unwritten would be a silent partial success the
                // caller cannot detect.
                if size_out != 0 {
                    let sb = (size as u32).to_le_bytes();
                    if !robot_os_sched::copy_to_user(size_out as usize, sb.as_ptr(), 4) {
                        return -1;
                    }
                }
                if is_dir_out != 0 {
                    let db = (is_dir as u32).to_le_bytes();
                    if !robot_os_sched::copy_to_user(is_dir_out as usize, db.as_ptr(), 4) {
                        return -1;
                    }
                }
            } else {
                unsafe {
                    let out = name_out as *mut u8;
                    core::ptr::write_bytes(out, 0, 64);
                    core::ptr::copy_nonoverlapping(name.as_ptr(), out, copy_len);
                    if size_out != 0 {
                        *(size_out as *mut u32) = size;
                    }
                    if is_dir_out != 0 {
                        *(is_dir_out as *mut u32) = is_dir as u32;
                    }
                }
            }
            0
        }
        None => -1,
    }
}

pub fn sys_mount(_src: u64, _tgt: u64, _fs: u64) -> i64  { -1 }
pub fn sys_umount(_tgt: u64) -> i64                       { -1 }
pub fn sys_sync() -> i64                                  { 0 }
pub fn sys_stat(_path: u64, _stat: u64) -> i64            { -1 }

// ── System info ───────────────────────────────────────────────────────────────

pub fn sys_meminfo() -> i64 {
    robot_os_mm::pmm::free_pages() as i64
}

pub fn sys_taskinfo() -> i64 { 0 }

pub fn sys_uptime() -> i64 {
    robot_os_drivers::clint::get_time() as i64
}

// ── System control ────────────────────────────────────────────────────────────

pub fn sys_shutdown() -> i64 {
    robot_os_arch::sbi::shutdown();
}

pub fn sys_reboot() -> i64 {
    robot_os_arch::sbi::reboot();
}

// ── Disk ─────────────────────────────────────────────────────────────────────

/// Maximum sectors per disk syscall (= 64 KiB at 512 B/sector).
/// Sized to fit the kernel-stack bounce buffer below.
const DISK_MAX_SECTORS: u64 = 128;
const DISK_BOUNCE_BYTES: usize = (DISK_MAX_SECTORS as usize) * 512;

pub fn sys_disk_read(sector: u64, count: u64, buf: u64) -> i64 {
    // Validate: null pointer, sane count, overflow.
    if buf == 0 || count == 0 || count > DISK_MAX_SECTORS { return -1; }
    let byte_len = (count as usize).checked_mul(512).unwrap_or(0);
    if byte_len == 0 || byte_len > DISK_BOUNCE_BYTES { return -1; }
    // Bounce: read into kernel stack first, copy_to_user validates user ptr.
    // Without this, a malicious user could pass a kernel address and the
    // VirtIO driver would DMA-write into kernel memory.
    static mut DISK_RD_BUF: [u8; DISK_BOUNCE_BYTES] = [0u8; DISK_BOUNCE_BYTES];
    // SAFETY: serialised by the disk subsystem's own lock; only one
    // disk syscall in flight per CPU (syscalls run with preemption
    // disabled in the kernel half).
    let kbuf = unsafe { &mut *core::ptr::addr_of_mut!(DISK_RD_BUF) };
    match robot_os_drivers::virtio::blk::read(sector, count as u32, &mut kbuf[..byte_len]) {
        Ok(()) => {
            if !robot_os_sched::copy_to_user(buf as usize, kbuf.as_ptr(), byte_len) {
                return -1;
            }
            0
        }
        Err(()) => -1,
    }
}

pub fn sys_disk_write(sector: u64, count: u64, buf: u64) -> i64 {
    if buf == 0 || count == 0 || count > DISK_MAX_SECTORS { return -1; }
    let byte_len = (count as usize).checked_mul(512).unwrap_or(0);
    if byte_len == 0 || byte_len > DISK_BOUNCE_BYTES { return -1; }
    static mut DISK_WR_BUF: [u8; DISK_BOUNCE_BYTES] = [0u8; DISK_BOUNCE_BYTES];
    let kbuf = unsafe { &mut *core::ptr::addr_of_mut!(DISK_WR_BUF) };
    if !robot_os_sched::copy_from_user(kbuf.as_mut_ptr(), buf as usize, byte_len) {
        return -1;
    }
    match robot_os_drivers::virtio::blk::write(sector, count as u32, &kbuf[..byte_len]) {
        Ok(()) => 0,
        Err(()) => -1,
    }
}

pub fn sys_disk_size() -> i64 {
    robot_os_drivers::virtio::blk::capacity_sectors() as i64
}

pub fn sys_disk_info() -> i64 {
    let secs = robot_os_drivers::virtio::blk::capacity_sectors();
    robot_os_drivers::kprintln!("[DISK] {} sectors ({} MB)", secs, secs / 2048);
    0
}

// ── Signals ───────────────────────────────────────────────────────────────────

/// SYS_KILL: send signal `signum` to task `tid`.
pub fn sys_kill(tid: u64, signum: u64) -> i64 {
    signal_send(tid as u32, signum as u32) as i64
}

/// SYS_SIGNAL: set signal handler for current task.
/// a0 = signum, a1 = handler fn ptr (SIG_DFL=0, SIG_IGN=1, or fn addr).
pub fn sys_signal(signum: u64, handler: u64) -> i64 {
    signal_set_handler(signum as u32, handler as usize) as i64
}

/// SYS_SIGPENDING: return bitmask of pending (unblocked) signals.
pub fn sys_sigpending() -> i64 {
    signal_pending() as i64
}

/// SYS_SIGPROCMASK: get/set signal mask.
/// a0 = how (0=GET, 1=SET, 2=BLOCK, 3=UNBLOCK), a1 = mask, returns old mask.
pub fn sys_sigprocmask(how: u64, mask: u64) -> i64 {
    let old = signal_get_mask();
    match how {
        0 => { /* GET — return old mask, no change */ }
        1 => { signal_set_mask(mask as u32); }
        2 => { signal_set_mask(old | mask as u32); }    // SIG_BLOCK
        3 => { signal_set_mask(old & !(mask as u32)); } // SIG_UNBLOCK
        _ => {}
    }
    old as i64
}

// ── Pipes ─────────────────────────────────────────────────────────────────────

/// SYS_PIPE: create a pipe.  a0 = pointer to int[2] { read_fd, write_fd }.
/// Returns 0 on success, -1 on failure.
pub fn sys_pipe(pipefd_ptr: u64) -> i64 {
    match pipe_create() {
        None => -1,
        Some((ridx, widx)) => {
            if pipefd_ptr != 0 {
                // Boundary rule: write the two fds through copy_to_user when a
                // user process is calling. A raw write would bypass SUM and
                // pointer validation (the user could pass a kernel VA).
                let fds: [u32; 2] = [ridx as u32, widx as u32];
                let bytes = unsafe {
                    core::slice::from_raw_parts(fds.as_ptr() as *const u8,
                                                core::mem::size_of_val(&fds))
                };
                if robot_os_sched::current_user_pt() != 0 {
                    if !robot_os_sched::copy_to_user(pipefd_ptr as usize,
                                                    bytes.as_ptr(), bytes.len()) {
                        // Pipe was already created; we leak the two fds rather
                        // than half-undo. Returning -1 is honest about the
                        // copy-out failure.
                        return -1;
                    }
                } else {
                    let ptr = pipefd_ptr as *mut u32;
                    unsafe {
                        core::ptr::write(ptr,        ridx as u32);
                        core::ptr::write(ptr.add(1), widx as u32);
                    }
                }
            }
            0
        }
    }
}

// ── Service manager ───────────────────────────────────────────────────────────

/// Max service-name length copied from userspace.
const SYS_SERVICE_NAME_MAX: usize = 64;

/// SYS_SERVICE_REGISTER: a0 = name_ptr, a1 = tid, a2 = ipc_channel.
pub fn sys_service_register(name_ptr: u64, tid: u64, channel: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let mut buf = [0u8; SYS_SERVICE_NAME_MAX];
    let name = match copy_path_from_user(&mut buf, name_ptr) { Some(n) => n, None => return -1 };
    service_register(name, tid as u32, channel as u32) as i64
}

/// SYS_SERVICE_DISCOVER: a0 = name_ptr.  Returns tid on success, -1 if not found.
pub fn sys_service_discover(name_ptr: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let mut buf = [0u8; SYS_SERVICE_NAME_MAX];
    let name = match copy_path_from_user(&mut buf, name_ptr) { Some(n) => n, None => return -1 };
    match service_discover(name) {
        Some(entry) => entry.tid as i64,
        None        => -1,
    }
}

/// SYS_SERVICE_HEARTBEAT: a0 = name_ptr.
pub fn sys_service_heartbeat(name_ptr: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let mut buf = [0u8; SYS_SERVICE_NAME_MAX];
    let name = match copy_path_from_user(&mut buf, name_ptr) { Some(n) => n, None => return -1 };
    service_heartbeat(name) as i64
}

/// SYS_SERVICE_STOP: a0 = name_ptr.
pub fn sys_service_stop_handler(name_ptr: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let mut buf = [0u8; SYS_SERVICE_NAME_MAX];
    let name = match copy_path_from_user(&mut buf, name_ptr) { Some(n) => n, None => return -1 };
    service_stop(name) as i64
}

// ── GPIO ──────────────────────────────────────────────────────────────────────

pub fn sys_gpio_read(pin: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Gpio(pin as u32), false) { return E_PERM; }
    gpio_read(pin as u32) as i64
}

pub fn sys_gpio_write(pin: u64, val: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Gpio(pin as u32), true) { return E_PERM; }
    gpio_write(pin as u32, val as u32) as i64
}

/// SYS_GPIO_MODE: a0=pin, a1=mode (0=Input, 1=Output).
pub fn sys_gpio_mode(pin: u64, mode: u64) -> i64 {
    // SC-1 cap check — sibling GPIO syscalls all gate on this; reconfiguring
    // a pin used by a sensor driver from an unprivileged process was a DoS.
    if !cap_check(robot_os_ipc::HandleKind::Gpio(pin as u32), true) { return E_PERM; }
    let dir = if mode == 1 { GpioDir::Output } else { GpioDir::Input };
    gpio_set_direction(pin as u32, dir) as i64
}

pub fn sys_gpio_info() -> i64 {
    gpio_info(); 0
}

// ── PWM ───────────────────────────────────────────────────────────────────────

pub fn sys_pwm_enable(ch: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Pwm(ch as u8), true) { return E_PERM; }
    pwm_enable(ch as u32) as i64
}

pub fn sys_pwm_disable(ch: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Pwm(ch as u8), true) { return E_PERM; }
    pwm_disable(ch as u32) as i64
}

/// SYS_PWM_SET_FREQ: a0=channel, a1=period_ns.
pub fn sys_pwm_set_freq(ch: u64, period_ns: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Pwm(ch as u8), true) { return E_PERM; }
    pwm_set_period(ch as u32, period_ns as u32) as i64
}

/// SYS_PWM_SET_DUTY: a0=channel, a1=duty_ns.
pub fn sys_pwm_set_duty(ch: u64, duty_ns: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Pwm(ch as u8), true) { return E_PERM; }
    pwm_set_duty(ch as u32, duty_ns as u32) as i64
}

pub fn sys_pwm_info() -> i64 {
    pwm_info(); 0
}

// ── I2C ───────────────────────────────────────────────────────────────────────

/// SYS_I2C_READ: a0=bus, a1=addr, a2=reg, a3=buf_ptr, a4=len.
///
/// User-supplied buffer is bounced through a kernel-stack temp via
/// copy_to_user so a malicious user pointer (kernel or unmapped VA)
/// cannot make the I2C driver write into kernel memory.
pub fn sys_i2c_read(bus: u64, addr: u64, reg: u64, buf_ptr: u64, len: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::I2c(bus as u8, addr as u8), false) { return E_PERM; }
    if buf_ptr == 0 || len == 0 { return -1; }
    /// Largest single I2C read — caps stack usage and bounds attacker scope.
    const I2C_MAX_XFER: usize = 256;
    let n = (len as usize).min(I2C_MAX_XFER);
    let mut tmp = [0u8; I2C_MAX_XFER];
    let r = i2c_read(bus as u8, addr as u8, reg as u8, &mut tmp[..n]);
    if r < 0 { return r as i64; }
    if !robot_os_sched::copy_to_user(buf_ptr as usize, tmp.as_ptr(), n) {
        return -1;
    }
    r as i64
}

/// SYS_I2C_WRITE: a0=bus, a1=addr, a2=data_ptr, a3=len.
/// data[0] = register address, data[1..] = bytes to write.
///
/// SC-1 capability check (was missing); user data bounced through
/// kernel temp via copy_from_user so an unmapped/kernel src pointer
/// cannot make the driver read kernel memory or page-fault.
pub fn sys_i2c_write(bus: u64, addr: u64, data_ptr: u64, len: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::I2c(bus as u8, addr as u8), true) { return E_PERM; }
    if data_ptr == 0 || len == 0 { return -1; }
    const I2C_MAX_XFER: usize = 256;
    let n = (len as usize).min(I2C_MAX_XFER);
    let mut tmp = [0u8; I2C_MAX_XFER];
    if !robot_os_sched::copy_from_user(tmp.as_mut_ptr(), data_ptr as usize, n) {
        return -1;
    }
    i2c_write(bus as u8, addr as u8, &tmp[..n]) as i64
}

pub fn sys_i2c_scan(bus: u64) -> i64 {
    // SC-1: require an I2C cap on the bus to enumerate it. Sibling
    // `sys_i2c_read`/`sys_i2c_write` already gate on this; `scan` was an
    // information-disclosure gap (lets an unprivileged process probe the bus).
    if !cap_check(robot_os_ipc::HandleKind::I2c(bus as u8, 0), true) { return E_PERM; }
    i2c_scan(bus as u8); 0
}

pub fn sys_i2c_info() -> i64 {
    i2c_info(); 0
}

// ── Motor ─────────────────────────────────────────────────────────────────────

/// SYS_MOTOR_CREATE: a0=id, a1=pwm_ch, a2=dir_pin_a, a3=dir_pin_b.
pub fn sys_motor_create(id: u64, pwm_ch: u64, dir_a: u64, dir_b: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Motor(id as u32), true) { return E_PERM; }
    motor_init(id as u32, pwm_ch as u32, dir_a as u32, dir_b as u32) as i64
}

/// SYS_MOTOR_ENABLE: a0=id, a1=direction (0=fwd, 1=rev, 2=brake, 3=coast).
pub fn sys_motor_enable(id: u64, dir: u64) -> i64 {
    // SC-1 capability check — was previously missing; sibling motor syscalls
    // (motor_create, motor_speed, motor_stop) all gate on this. Without it any
    // userspace process could drive the motors fwd/back/brake/coast without
    // holding a Motor capability — a real safety hole on a physical robot.
    if !cap_check(robot_os_ipc::HandleKind::Motor(id as u32), true) { return E_PERM; }
    let d = match dir {
        0 => MotorDir::Forward,
        1 => MotorDir::Backward,
        2 => MotorDir::Brake,
        _ => MotorDir::Coast,
    };
    motor_set(id as u32, d, 50) as i64  // default 50% speed
}

/// SYS_MOTOR_SPEED: a0=id, a1=speed_pct (0-100).
pub fn sys_motor_speed(id: u64, speed_pct: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Motor(id as u32), true) { return E_PERM; }
    if speed_pct == 0 {
        motor_stop(id as u32) as i64
    } else {
        motor_set(id as u32, MotorDir::Forward, speed_pct as u32) as i64
    }
}

pub fn sys_motor_info() -> i64 {
    motor_info(); 0
}

/// SYS_MOTOR_ANGLE: a0=motor_id (0=left, 1=right).
/// Returns accumulated encoder ticks for the specified motor.
pub fn sys_motor_angle(id: u64) -> i64 {
    let (ticks_l, ticks_r) = robot_os_robot::encoder_read();
    match id {
        0 => ticks_l,
        1 => ticks_r,
        _ => -1,
    }
}

// ── Memory management (Phase 7) ───────────────────────────────────────────────

/// SYS_BRK: a0 = new brk address (0 = query current brk).
/// Returns the new (or current) brk address.
pub fn sys_brk(addr: u64) -> i64 {
    robot_os_sched::sys_brk_impl(addr)
}

// ── Network (Phase 9) ─────────────────────────────────────────────────────────

/// Owner stamp to record for a socket created by the *current* caller.
///
/// Kernel tasks get [`robot_os_net::SOCK_OWNER_KERNEL`]; a user task gets its
/// own TID. Returns `None` when a user task has no resolvable TID, in which
/// case the caller must refuse to create the socket rather than stamp it with
/// 0 — a socket stamped 0 is owned by nobody, so its creator could never use
/// it and the exit hook would never reclaim it: a permanently leaked slot out
/// of only 16.
#[inline]
fn socket_owner_for_caller() -> Option<u32> {
    if robot_os_sched::current_user_pt() == 0 {
        return Some(robot_os_net::SOCK_OWNER_KERNEL);
    }
    match robot_os_sched::current_task_tid() {
        0   => None, // 0 means "no current task"; NEXT_TID never issues it.
        tid => Some(tid),
    }
}

/// May the current task touch socket `fd`?
///
/// **WHY this exists:** `robot_os_net::SOCKS` is one flat 16-entry array and
/// the fd *is* the userspace-chosen index into it. Every `socket_*` entry
/// point validated only `fd < MAX_SOCKETS`, and the syscalls passed the raw
/// register straight through with no per-task fd table in between — so any
/// task could enumerate fd 0..15 and read another task's inbound TCP stream,
/// inject bytes into its outbound stream, or tear down its connection. That
/// includes the OTA channel and the brain link. Sixteen guesses covered the
/// whole table.
///
/// Same shape as `port_access_ok` / the `shm_owner` gate in `dispatch.rs`:
/// the owner is stamped at create/accept time and checked here, rather than
/// asking "who is running now" at use time — kernel-side poll paths and
/// workers run with `user_pt == 0`, where a current-task check enforces
/// nothing at all.
///
/// Kernel callers (`user_pt == 0`) bypass. In practice the bypass is
/// structural rather than conditional: `kernel/src/main.rs` and
/// `crates/shell` call `robot_os_net::socket_*` directly and never traverse
/// these handlers.
///
/// `socket_owner` returns `None` for an out-of-range, free or unowned slot,
/// and a real TID is never 0 or `SOCK_OWNER_KERNEL`, so denial is the
/// default for anything a user task did not create.
#[inline]
fn socket_access_ok(fd: u64) -> bool {
    if robot_os_sched::current_user_pt() == 0 {
        return true;
    }
    // Range-check before narrowing: `fd` is a raw register, and `fd as i32`
    // on a large value wraps (possibly negative). Bounding it here also
    // makes `fd as u16` safe for the ephemeral-port arithmetic in
    // `sys_connect_syscall`.
    if fd >= robot_os_net::MAX_SOCKETS as u64 {
        return false;
    }
    let tid = robot_os_sched::current_task_tid();
    if tid == 0 {
        return false;
    }
    robot_os_net::socket_owner(fd as i32) == Some(tid)
}

/// Parse a `sockaddr_in` from a (kernel or user-space) pointer.
/// Layout: family(u16 LE), port(u16 BE), addr(4 bytes), 8 bytes pad.
fn read_sockaddr(ptr: u64) -> Option<SockAddr> {
    if ptr == 0 { return None; }
    // For user-space callers, copy via page tables; kernel callers use raw ptr.
    let mut raw = [0u8; 16];
    if robot_os_sched::current_user_pt() != 0 {
        if !robot_os_sched::copy_from_user(raw.as_mut_ptr(), ptr as usize, 16) {
            return None;
        }
    } else {
        let src = unsafe { core::slice::from_raw_parts(ptr as *const u8, 16) };
        raw.copy_from_slice(src);
    }
    let family = u16::from_le_bytes([raw[0], raw[1]]);
    let port   = u16::from_be_bytes([raw[2], raw[3]]);
    let addr   = [raw[4], raw[5], raw[6], raw[7]];
    Some(SockAddr { family, port, addr })
}

/// SYS_SOCKET: a0=domain, a1=type, a2=proto. Returns socket fd or -1.
///
/// Stamps the new socket with the caller's TID. That stamp is the only thing
/// standing between a user task and every other task's connection — see
/// [`socket_access_ok`].
pub fn sys_socket(domain: u64, sock_type: u64, proto: u64) -> i64 {
    let owner = match socket_owner_for_caller() {
        Some(o) => o,
        None    => return -1,
    };
    socket_create_owned(domain as u32, sock_type as u32, proto as u32, owner) as i64
}

/// SYS_BIND: a0=fd, a1=sockaddr_ptr, a2=addrlen. Returns 0 or -1.
pub fn sys_bind(fd: u64, addr_ptr: u64, _addrlen: u64) -> i64 {
    // Ownership gate: binding another task's socket would let an attacker
    // steer the port it is about to listen on.
    if !socket_access_ok(fd) { return -1; }
    match read_sockaddr(addr_ptr) {
        Some(addr) => socket_bind(fd as i32, &addr) as i64,
        None       => -1,
    }
}

/// SYS_LISTEN: a0=fd, a1=backlog (ignored). Returns 0 or -1.
pub fn sys_listen_syscall(fd: u64, _backlog: u64) -> i64 {
    // Ownership gate: without it a task could make another task's socket
    // listen on the port it had bound, hijacking inbound connections.
    if !socket_access_ok(fd) { return -1; }
    socket_listen_bound(fd as i32) as i64
}

/// SYS_ACCEPT: a0=fd, a1=addr_out (ignored), a2=addrlen_out (ignored).
/// Polls until an Established connection is ready; returns new fd or -1.
pub fn sys_accept(fd: u64, _addr_out: u64, _addrlen_out: u64) -> i64 {
    // Gate + resolve owner ONCE, outside the retry loop: the loop runs up to
    // 50_000 times and each attempt would otherwise take the `SOCKS` lock
    // just to re-answer a question that cannot change, contending with
    // `net_poll` on every iteration.
    //
    // Denying here stops a task from accepting on a listening fd it does not
    // own, which is the fd-enumeration path this gate exists to close.
    //
    // SCOPE — what this does NOT close (separate, still-open finding):
    // `socket_accept_owned` resolves the listener's `local.port` and calls
    // `tcp::accept(port)`, which is keyed on the **port**, not on the fd or
    // the owner. `socket_bind`'s TCP arm only records the port and
    // `tcp::listen` does not reject a second listener on an already-listening
    // port. So a task can bind a socket it legitimately owns to a port the
    // kernel is already listening on and race for inbound connections — the
    // ownership gate passes because it owns every fd it touches. Closing
    // that needs port-space arbitration in `tcp::listen`, which is a
    // different mechanism and deliberately not attempted here.
    if !socket_access_ok(fd) { return -1; }
    let owner = match socket_owner_for_caller() {
        Some(o) => o,
        None    => return -1,
    };
    for _ in 0..50_000u32 {
        robot_os_net::net_poll();
        // The accepted connection is stamped to the *accepting* task, so the
        // new fd is reachable by this caller and nobody else.
        let r = socket_accept_owned(fd as i32, owner);
        if r >= 0 { return r as i64; }
        robot_os_sched::task_yield();
    }
    -1
}

/// SYS_CONNECT: a0=fd, a1=sockaddr_ptr, a2=addrlen.
pub fn sys_connect_syscall(fd: u64, addr_ptr: u64, _addrlen: u64) -> i64 {
    // Ownership gate FIRST — before `fd` is used for anything at all.
    // Two things depend on that ordering:
    //   1. Security: connecting another task's socket redirects its stream
    //      to an attacker-chosen peer.
    //   2. Liveness: the ephemeral source port below is derived from `fd`.
    //      With `overflow-checks = true` and `panic = "abort"`, an
    //      out-of-range `fd` (e.g. 16384, giving 0xC000 + 0x4000 = 65536)
    //      used to overflow the u16 and abort — a full board reset, i.e. a
    //      physical-safety event, reachable by one syscall from any task.
    if !socket_access_ok(fd) { return -1; }
    // Belt and braces on the port arithmetic: the gate above already bounds
    // `fd` to 0..MAX_SOCKETS, but `saturating_add` means a future reordering
    // of this function cannot reintroduce the panic.
    let src_port = 0xC000u16.saturating_add(fd as u16);
    match read_sockaddr(addr_ptr) {
        // Yield-aware: connect must not report success until the handshake
        // completes, and waiting without yielding would burn the hart.
        Some(addr) => robot_os_net::socket::socket_connect_with_yield(
            fd as i32, &addr, src_port,
            robot_os_sched::task_yield,
        ) as i64,
        None       => -1,
    }
}

/// SYS_SEND / SYS_SENDTO: a0=fd, a1=buf_ptr, a2=len, a3=flags (ignored).
///
/// Gated here rather than in `dispatch.rs` because `SYS_SEND` and
/// `SYS_SENDTO` both land on this one handler: one check covers both arms.
pub fn sys_send_syscall(fd: u64, buf_ptr: u64, len: u64, _flags: u64) -> i64 {
    // Ownership gate: this is the byte-injection half of the finding — an
    // ungated send lets any task write into another task's outbound stream,
    // e.g. forging commands on the brain link or corrupting an OTA image.
    if !socket_access_ok(fd) { return -1; }
    if buf_ptr == 0 || len == 0 { return -1; }
    let count = (len as usize).min(1460);
    if robot_os_sched::current_user_pt() != 0 {
        let mut tmp = [0u8; 1460];
        if !robot_os_sched::copy_from_user(tmp.as_mut_ptr(), buf_ptr as usize, count) {
            return -1;
        }
        socket_send(fd as i32, &tmp[..count]) as i64
    } else {
        let data = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, count) };
        socket_send(fd as i32, data) as i64
    }
}

/// SYS_RECV / SYS_RECVFROM: a0=fd, a1=buf_ptr, a2=len, a3=flags (ignored). Non-blocking.
///
/// Gated here rather than in `dispatch.rs` because `SYS_RECV` and
/// `SYS_RECVFROM` both land on this one handler: one check covers both arms.
pub fn sys_recv_syscall(fd: u64, buf_ptr: u64, len: u64, _flags: u64) -> i64 {
    // Ownership gate: this is the disclosure half of the finding — an
    // ungated recv drains another task's inbound stream, both reading its
    // traffic and stealing the bytes from the rightful owner.
    if !socket_access_ok(fd) { return -1; }
    if buf_ptr == 0 || len == 0 { return -1; }
    robot_os_net::net_poll();
    let count = (len as usize).min(4096);
    let mut tmp = [0u8; 4096];
    let n = socket_recv(fd as i32, &mut tmp[..count]);
    if n > 0 {
        if robot_os_sched::current_user_pt() != 0 {
            if !robot_os_sched::copy_to_user(buf_ptr as usize, tmp.as_ptr(), n as usize) {
                return -1;
            }
        } else {
            unsafe { core::ptr::copy_nonoverlapping(tmp.as_ptr(), buf_ptr as *mut u8, n as usize); }
        }
    }
    n as i64
}

/// SYS_SOCK_SHUTDOWN / close a socket fd. Returns 0, or -1 if the caller
/// does not own `fd`.
pub fn sys_sock_close(fd: u64) -> i64 {
    // Ownership gate: this is the denial-of-service half of the finding — an
    // ungated close tears down another task's connection, and closing the
    // brain link or the OTA channel mid-transfer is a control-plane outage.
    //
    // NOTE the return value: this used to be unconditionally 0. A denied
    // close must report -1, otherwise the caller is told "closed" when
    // nothing happened.
    if !socket_access_ok(fd) { return -1; }
    socket_close(fd as i32);
    0
}

/// SYS_NET_INFO: print network interface info.
pub fn sys_net_info() -> i64 {
    net_info(); 0
}

/// SYS_NET_GETIP: return IP address as u32 (big-endian).
pub fn sys_net_getip() -> i64 {
    let ip = net_get_ip();
    u32::from_be_bytes(ip) as i64
}

// ── MMAP / MUNMAP ────────────────────────────────────────────────────────────

/// SYS_MMAP: anonymous memory mapping.
/// a0=addr (hint, 0=any), a1=length, a2=prot, a3=flags, a4=fd (-1 anon), a5=offset.
/// Returns mapped virtual address or -1 on error.
pub fn sys_mmap(addr: u64, length: u64, _prot: u64, _flags: u64, fd: u64, _offset: u64) -> i64 {
    // Only support anonymous mappings (fd == -1 or fd == u64::MAX)
    if fd != u64::MAX && fd as i64 != -1 { return -1; }
    let user_pt = robot_os_sched::current_user_pt();
    if user_pt == 0 { return -1; } // kernel task

    let len = length as usize;
    if len == 0 { return -1; }
    // Cap the request.  `length` used to be unbounded, so a single
    // `mmap(0, u64::MAX, ..)` from ring 3 drained every free physical page —
    // and because the OOM path below returned without unwinding, the pages
    // already mapped stayed lost for the lifetime of the boot.  Same ceiling
    // `sys_alloc_demand` already enforces, so the two allocators agree on how
    // much one call may claim.
    if len > robot_os_mm::demand::MAX_DEMAND_ALLOC_BYTES { return -1; }

    let page_size = robot_os_arch::mmu::PAGE_SIZE;
    let num_pages = len.saturating_add(page_size - 1) / page_size;

    // Use brk as base for anonymous mappings, then advance brk
    // **WHY the brk itself has to be checked (null-guard follow-up).** Both
    // brk-based allocators take `update_user_brk(0)` as their base, and a task
    // whose brk was never initialised reports **0** (`scheduler.rs` zeroes
    // `user_brk` at task creation). The range was then reserved starting at VA
    // 0 and the syscall answered `0` — a *successful* allocation whose base is
    // the null pointer. Callers cannot tell that apart from a real address.
    //
    // `vmm::USER_GUARD_LIMIT` now makes the first access to such a pointer kill
    // the task instead of quietly succeeding on a zero page, which turns a
    // silent corruption into a loud death — but the allocation itself is still
    // nonsense, and handing out a null base is the actual defect. Refuse it
    // here, at the only place that can tell the difference.
    let base = robot_os_sched::update_user_brk(0) as usize;
    if base < robot_os_mm::vmm::USER_GUARD_LIMIT { return -1; }
    let aligned_base = base.saturating_add(page_size - 1) & !(page_size - 1);
    // The mapping must stay strictly below the user VA ceiling: above it the
    // "user" page table is the kernel's (shared L1/L0 tables), so mapping
    // there would install USER_RW pages into kernel address space.
    let span = match num_pages.checked_mul(page_size) {
        Some(s) => s,
        None    => return -1,
    };
    let end_va = match aligned_base.checked_add(span) {
        Some(e) => e,
        None    => return -1,
    };
    if end_va > USER_VA_TOP { return -1; }

    let mut va = aligned_base;
    for _ in 0..num_pages {
        match robot_os_mm::pmm::alloc_page() {
            Ok(page) => {
                let flags = robot_os_arch::mmu::PteFlags::USER_RW
                    | robot_os_arch::mmu::PteFlags::ACCESSED
                    | robot_os_arch::mmu::PteFlags::DIRTY;
                if robot_os_mm::vmm::map(user_pt, va, page.as_usize(), flags).is_err() {
                    // This page never made it into the PT — free it directly,
                    // then unwind everything mapped so far.
                    let _ = robot_os_mm::pmm::free_page(page);
                    mmap_unwind(user_pt, aligned_base, va);
                    return -1;
                }
            }
            Err(_) => {
                // OOM part-way through.  Without this the caller got -1 while
                // the kernel silently kept every page already mapped.
                mmap_unwind(user_pt, aligned_base, va);
                return -1;
            }
        }
        va += page_size;
    }

    // Advance brk past the mapped region
    robot_os_sched::update_user_brk(va as u64);

    // If caller specified an addr hint, we ignore it (simplified).
    // NOTE: `prot`/`flags` are still ignored and every page is mapped USER_RW.
    // Tightening that here would break userspace that maps with PROT_READ and
    // then writes; it needs an ABI-level decision, not a patch in this handler.
    let _ = addr;
    aligned_base as i64
}

/// Free and unmap the pages `sys_mmap` installed in `[base, end)` after a
/// mid-loop failure.  No side table is needed: every page in that range was
/// allocated by this call and mapped USER_RW, so `translate_user` resolves it
/// and the physical page is exclusively ours to release.  `vmm::unmap` issues
/// `sfence.vma` for the address, so no stale TLB entry survives the free.
fn mmap_unwind(user_pt: usize, base: usize, end: usize) {
    let page_size = robot_os_arch::mmu::PAGE_SIZE;
    let mut v = base;
    while v < end {
        if let Some(pa) = robot_os_mm::vmm::translate_user(user_pt, v, false) {
            robot_os_mm::vmm::unmap(user_pt, v);
            let _ = robot_os_mm::pmm::free_page(robot_os_mm::addr::PhysAddr(pa & !(page_size - 1)));
        }
        v = v.saturating_add(page_size);
    }
}

/// SYS_MUNMAP: unmap pages.  Simplified: just marks pages as unmapped.
pub fn sys_munmap(addr: u64, length: u64) -> i64 {
    let user_pt = robot_os_sched::current_user_pt();
    if user_pt == 0 { return -1; }

    let page_size = robot_os_arch::mmu::PAGE_SIZE;
    let len = length as usize;
    if len == 0 { return -1; }

    // Bound the request to the user half of the address space, and bound its
    // size.  Both checks are load-bearing:
    //
    //   * User and kernel page tables share their L1/L0 tables, so unmapping a
    //     VA at or above `USER_VA_TOP` zeroes a PTE the *kernel* is using.
    //     `munmap(0x1000_0000, 4096)` cleared the UART mapping for every hart;
    //     the next `kprintln!` took a fatal S-mode store fault and, with
    //     `panic = "abort"`, reset the board.  Kernel text at vpn2 = 2 also got
    //     its megapage split by `vmm::unmap` on the way through.
    //   * `length = u64::MAX` made the old `saturating_add` clamp `end` to
    //     `usize::MAX`, so the loop below ran ~2^52 iterations inside the
    //     kernel with interrupts on but no way out — an unkillable hang.
    //
    // Rejecting (rather than clamping) keeps the failure visible to the caller
    // instead of silently unmapping a different range than it asked for.
    let start = addr as usize & !(page_size - 1);
    if start >= USER_VA_TOP { return -1; }
    if len > robot_os_mm::demand::MAX_DEMAND_ALLOC_BYTES { return -1; }
    let rounded = len.saturating_add(page_size - 1) & !(page_size - 1);
    let end = match start.checked_add(rounded) {
        Some(e) if e <= USER_VA_TOP => e,
        _ => return -1,
    };

    let mut va = start;
    while va < end {
        robot_os_mm::vmm::unmap(user_pt, va);
        va += page_size;
    }
    0
}

// ── E11 / AQ10: Demand-paging allocator ─────────────────────────────────────
//
// SYS_ALLOC_DEMAND reserves a user virtual range without consuming any
// physical memory up front; the pages materialize on first access.
//
// a0 = size in bytes (rounded up to PAGE_SIZE).  Returns base VA or -1.

/// Minimum size a demand allocation must request (one page).
const DEMAND_ALLOC_MIN_BYTES: usize = 1;

pub fn sys_alloc_demand(size: u64) -> i64 {
    let user_pt = robot_os_sched::current_user_pt();
    if user_pt == 0 { return -1; } // kernel task

    let size = size as usize;
    if size < DEMAND_ALLOC_MIN_BYTES {
        return -1;
    }
    if size > robot_os_mm::demand::MAX_DEMAND_ALLOC_BYTES {
        return -1;
    }

    let page_size = robot_os_arch::mmu::PAGE_SIZE;
    let num_pages = (size + page_size - 1) / page_size;

    // Use the user brk as the allocation base (same convention as sys_mmap),
    // page-aligned upward. Same null-brk refusal as `sys_mmap` — see the
    // comment there for why a base of 0 is a defect and not just an oddity.
    let base = robot_os_sched::update_user_brk(0) as usize;
    if base < robot_os_mm::vmm::USER_GUARD_LIMIT { return -1; }
    let aligned_base = (base + page_size - 1) & !(page_size - 1);

    // Reserve the range.  map_demand_range returns on first failure; any
    // partially-installed demand PTEs stay in the page table (harmless: they
    // are invalid, so a stray access just traps and is rejected).
    if robot_os_mm::demand::map_demand_range(user_pt, aligned_base, num_pages).is_err() {
        return -1;
    }

    // Advance brk past the reservation so subsequent sys_mmap / sys_brk
    // calls don't collide.
    let end_va = aligned_base + num_pages * page_size;
    robot_os_sched::update_user_brk(end_va as u64);

    aligned_base as i64
}

// ── E11 / AQ9: COW fork (alias) ─────────────────────────────────────────────
//
// SYS_FORK_COW is semantically identical to SYS_FORK — the existing
// SYS_FORK implementation already forwards to `vmm::fork_cow` internally
// (see `sched::process::sys_fork_impl`).  Exposed separately so userspace
// can probe for COW support or request it explicitly.

pub fn sys_fork_cow(sepc: u64, user_sp: u64, regs: &[u64; 32]) -> i64 {
    sys_fork(sepc, user_sp, regs)
}

// ── DUP / DUP2 ──────────────────────────────────────────────────────────────

/// SYS_DUP: duplicate a file descriptor.  Returns new fd or -1.
pub fn sys_dup(fd: u64) -> i64 {
    let mut table = KERNEL_FD_TABLE.lock();
    robot_os_fs::fd_dup(&mut *table, fd as i32) as i64
}

/// SYS_DUP2: duplicate fd `old` to fd `new`.  Returns `new` or -1.
pub fn sys_dup2(oldfd: u64, newfd: u64) -> i64 {
    let mut table = KERNEL_FD_TABLE.lock();
    robot_os_fs::fd_dup2(&mut *table, oldfd as i32, newfd as i32) as i64
}

// ── PAUSE / ALARM ────────────────────────────────────────────────────────────

/// SYS_PAUSE: suspend until a signal is delivered.
/// Simplified: yield in a loop checking for pending signals.
pub fn sys_pause() -> i64 {
    for _ in 0..1000u32 {
        if signal_pending() != 0 { return 0; }
        robot_os_sched::task_yield();
    }
    -1  // timeout (no signal received)
}

/// SYS_ALARM: set a timer that sends SIGALRM after `seconds`.
/// Simplified: immediate SIGALRM if seconds > 0 (no real timer integration).
pub fn sys_alarm(seconds: u64) -> i64 {
    if seconds == 0 { return 0; } // cancel (no-op)
    // Approximate: busy-wait then send SIGALRM to self
    let ticks = seconds.saturating_mul(10_000_000); // ~10 MHz CLINT
    let start = robot_os_drivers::clint::get_time();
    while robot_os_drivers::clint::get_time().wrapping_sub(start) < ticks {
        robot_os_sched::task_yield();
    }
    let tid = robot_os_sched::current_task_tid();
    signal_send(tid, robot_os_ipc::SIGALRM);
    0
}

/// SYS_SIGRETURN: return from signal handler (restore context).
/// Simplified: just return 0 (full signal frame restore requires trap frame plumbing).
pub fn sys_sigreturn() -> i64 { 0 }

// ── Network utility syscalls ─────────────────────────────────────────────────

/// SYS_NET_SETIP: a0=ip_u32 (big-endian), a1=mask_u32, a2=gw_u32.
pub fn sys_net_setip(ip: u64, mask: u64, gw: u64) -> i64 {
    let ip_bytes = (ip as u32).to_be_bytes();
    let mask_bytes = (mask as u32).to_be_bytes();
    let gw_bytes = (gw as u32).to_be_bytes();
    robot_os_net::net_set_ip(ip_bytes, mask_bytes, gw_bytes);
    0
}

/// SYS_NET_PING: a0 = destination IP as u32 (big-endian).
pub fn sys_net_ping(dst_ip: u64) -> i64 {
    let ip = (dst_ip as u32).to_be_bytes();
    robot_os_net::net_ping(ip) as i64
}

/// SYS_NET_GETMAC: returns MAC as u64 (lower 6 bytes).
pub fn sys_net_getmac() -> i64 {
    let mac = robot_os_net::net_get_mac();
    let mut val = 0u64;
    for i in 0..6 { val |= (mac[i] as u64) << (i * 8); }
    val as i64
}

/// SYS_NET_STATS: print network statistics.
pub fn sys_net_stats() -> i64 {
    robot_os_net::net_info();
    0
}

// ── IPC Channels ─────────────────────────────────────────────────────────────

/// SYS_IPC_CREATE: create a message channel. Returns channel index or -1.
pub fn sys_ipc_create() -> i64 {
    match robot_os_ipc::channel_create() {
        Some(idx) => idx as i64,
        None => -1,
    }
}

/// SYS_IPC_SEND: a0=channel, a1=data_ptr, a2=len. Returns 0 or -1.
pub fn sys_ipc_send(ch: u64, data_ptr: u64, len: u64) -> i64 {
    if data_ptr == 0 || len == 0 { return -1; }
    let count = (len as usize).min(64);
    let mut tmp = [0u8; 64];
    if robot_os_sched::current_user_pt() != 0 {
        if !robot_os_sched::copy_from_user(tmp.as_mut_ptr(), data_ptr as usize, count) {
            return -1;
        }
    } else {
        let src = unsafe { core::slice::from_raw_parts(data_ptr as *const u8, count) };
        tmp[..count].copy_from_slice(src);
    }
    robot_os_ipc::channel_send(ch as usize, &tmp[..count]) as i64
}

/// SYS_IPC_RECEIVE: a0=channel, a1=buf_ptr, a2=len. Returns bytes read, 0 if empty, -1 on error.
pub fn sys_ipc_recv(ch: u64, buf_ptr: u64, len: u64) -> i64 {
    if buf_ptr == 0 || len == 0 { return -1; }
    let count = (len as usize).min(64);
    let mut tmp = [0u8; 64];
    let n = robot_os_ipc::channel_recv(ch as usize, &mut tmp[..count]);
    if n > 0 {
        if robot_os_sched::current_user_pt() != 0 {
            if !robot_os_sched::copy_to_user(buf_ptr as usize, tmp.as_ptr(), n as usize) {
                return -1;
            }
        } else {
            unsafe { core::ptr::copy_nonoverlapping(tmp.as_ptr(), buf_ptr as *mut u8, n as usize); }
        }
    }
    n as i64
}

/// SYS_IPC_DESTROY: a0=channel. Returns 0 or -1.
pub fn sys_ipc_destroy(ch: u64) -> i64 {
    robot_os_ipc::channel_destroy(ch as usize);
    0
}

// ── Cap<T> typed channel I/O — RFC-0003 W3 ────────────────────────────────
//
// The legacy `sys_ipc_send` / `sys_ipc_recv` continue to work using the
// integer handle table (`robot_os_ipc::handle::*`). The typed entries
// below are the new, recommended path: they take a `Cap<Channel>`
// (encoded as a `u32` in `a0`) and dereference it through the calling
// task's per-tid `CapTable`. Errors map to PHANES-specific errnos so
// userspace can distinguish stale-cap from wrong-kind from missing-perms
// from underlying-channel-closed.
//
// Wave W5 will migrate remaining IPC syscalls to the same shape.

const CAP_CHANNEL_MAX_PAYLOAD: usize = 64;

fn errno_for_channel_err(e: robot_os_ipc::channel::ChannelCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::channel::ChannelCapError;
    match e {
        ChannelCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        ChannelCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        ChannelCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        ChannelCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
        ChannelCapError::Closed => Errno::EBADF.to_syscall_ret(),
        ChannelCapError::Full => Errno::EAGAIN.to_syscall_ret(),
        ChannelCapError::Empty => Errno::EAGAIN.to_syscall_ret(),
        ChannelCapError::BadArg => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_CHAN_WRITE_TYPED` (528): a0=cap_handle (u32), a1=data_ptr,
/// a2=len. Returns 0 on success or -Errno.
pub fn sys_chan_write_typed(cap_raw: u64, data_ptr: u64, len: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Channel, Cap};

    if data_ptr == 0 || len == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }
    let count = (len as usize).min(CAP_CHANNEL_MAX_PAYLOAD);
    let mut tmp = [0u8; CAP_CHANNEL_MAX_PAYLOAD];
    if robot_os_sched::current_user_pt() != 0 {
        if !robot_os_sched::copy_from_user(tmp.as_mut_ptr(), data_ptr as usize, count) {
            return Errno::EFAULT.to_syscall_ret();
        }
    } else {
        let src = unsafe { core::slice::from_raw_parts(data_ptr as *const u8, count) };
        tmp[..count].copy_from_slice(src);
    }
    let cap: Cap<Channel> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::channel::channel_send_cap(table, cap, &tmp[..count])
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_channel_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_CHAN_READ_TYPED` (529): a0=cap_handle, a1=buf_ptr, a2=len.
/// Returns bytes copied (≥ 1) on success, -EAGAIN if empty, or -Errno.
pub fn sys_chan_read_typed(cap_raw: u64, buf_ptr: u64, len: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Channel, Cap};

    if buf_ptr == 0 || len == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }
    let count = (len as usize).min(CAP_CHANNEL_MAX_PAYLOAD);
    let mut tmp = [0u8; CAP_CHANNEL_MAX_PAYLOAD];
    let cap: Cap<Channel> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::channel::channel_recv_cap(table, cap, &mut tmp[..count])
    });
    match result {
        Some(Ok(n)) => {
            if robot_os_sched::current_user_pt() != 0 {
                if !robot_os_sched::copy_to_user(buf_ptr as usize, tmp.as_ptr(), n) {
                    return Errno::EFAULT.to_syscall_ret();
                }
            } else {
                unsafe {
                    core::ptr::copy_nonoverlapping(tmp.as_ptr(), buf_ptr as *mut u8, n);
                }
            }
            n as i64
        }
        Some(Err(e)) => errno_for_channel_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_CAP_GRANT` (116): a0=target_tid, a1=cap_handle, a2=want_perms bits.
///
/// Delegates a cap the CALLER holds to `target_tid`, attenuated to
/// `want_perms`. Returns the wire handle as it appears in the target's
/// table (>0), or -Errno. The four rules (never amplify, DUP opt-in,
/// no revocation propagation, live-target-only with slot-alias refusal)
/// live in `robot_os_ipc::cap_store::delegate` — this is only the errno
/// mapping and the caller-identity binding, which is what makes the call
/// authorizable at all (the grantor is `current_task_tid()`, never an
/// argument).
pub fn sys_cap_grant(target_tid: u64, cap_raw: u64, want_perms: u64) -> i64 {
    use robot_os_abi::cap::{CapHandle, CapPerms};
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::cap_store::DelegateError;

    // Reject out-of-range bits instead of truncating: a caller that asked
    // for bits the ABI does not define must not silently get fewer. The
    // round-trip check covers both undefined low bits and anything above
    // the u8 the wire format carries.
    let perms = CapPerms::from_bits_truncate(want_perms as u8);
    if perms.bits() as u64 != want_perms {
        return Errno::EINVAL.to_syscall_ret();
    }
    let grantor = robot_os_sched::current_task_tid();
    match robot_os_ipc::cap_store::delegate(
        grantor,
        target_tid as u32,
        CapHandle::from_raw(cap_raw as u32),
        perms,
    ) {
        Ok(handle) => handle.as_raw() as i64,
        // Keep the typed path's CapError distinctions.
        Err(DelegateError::Source(CapError::Stale)) => Errno::ECAPSTALE.to_syscall_ret(),
        Err(DelegateError::Source(CapError::WrongKind)) => Errno::ECAPKIND.to_syscall_ret(),
        Err(DelegateError::Source(_)) => Errno::ECAPPERMS.to_syscall_ret(),
        Err(DelegateError::NotDelegable) | Err(DelegateError::Amplify) => {
            Errno::ECAPPERMS.to_syscall_ret()
        }
        Err(DelegateError::EmptyPerms) => Errno::EINVAL.to_syscall_ret(),
        // ESRCH (decisión del usuario 2026-08-23): "no live task by that
        // TID" is a different fix for the caller than "no such resource",
        // so it gets its own number. SlotAlias stays deliberately
        // indistinguishable from NoTarget: the difference is an
        // implementation detail of the task pool, not something ring 3
        // should be able to probe.
        Err(DelegateError::NoGrantor)
        | Err(DelegateError::NoTarget)
        | Err(DelegateError::SlotAlias) => Errno::ESRCH.to_syscall_ret(),
        // Same errno as TargetFull on purpose: both mean "the target cannot
        // take more", and distinguishing them would tell an attacker how
        // close the fill attack got. The kernel-side enum keeps them apart
        // for diagnostics.
        Err(DelegateError::TargetFull) | Err(DelegateError::QuotaExhausted) => {
            Errno::ENOSPC.to_syscall_ret()
        }
    }
}

/// `topology::cap_grant` helper — used by the seed-task setup in
/// kernel boot to mint initial caps for tasks. (Runtime cross-task
/// delegation from ring 3 is `SYS_CAP_GRANT` above; this remains the
/// boot-time mint that creates authority from nothing, which the
/// syscall deliberately cannot.)
pub fn kernel_grant_channel_cap(
    tid: u32,
    perms_bits: u8,
    channel_id: u32,
) -> Option<robot_os_abi::cap::CapHandle> {
    use robot_os_abi::cap::CapPerms;
    use robot_os_ipc::cap::{targets::Channel, Cap};
    let perms = CapPerms::from_bits_truncate(perms_bits);
    let cap: Cap<Channel> = robot_os_ipc::cap_store::grant(tid, perms, channel_id)?;
    Some(cap.raw())
}

// ── Cap<Port> typed handlers — RFC-0003 W5 ────────────────────────────────

fn errno_for_port_err(e: robot_os_ipc::port::PortCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::port::PortCapError;
    match e {
        PortCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        PortCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        PortCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        PortCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
        PortCapError::Full => Errno::EMFILE.to_syscall_ret(),
        PortCapError::Empty => Errno::EAGAIN.to_syscall_ret(),
        PortCapError::Closed => Errno::EBADF.to_syscall_ret(),
    }
}

/// `SYS_PORT_CREATE_TYPED` (530): no args. Allocates a port + grants
/// a `Cap<Port>` into the calling task's cap-table. Returns the raw
/// cap handle as `i64` (always > 0) on success, or `-Errno`.
pub fn sys_port_create_typed() -> i64 {
    use robot_os_abi::error::Errno;
    let tid = robot_os_sched::current_task_tid();
    match robot_os_ipc::port::port_create_cap(tid) {
        Some(cap) => cap.raw().as_raw() as i64,
        None => Errno::EMFILE.to_syscall_ret(),
    }
}

/// `SYS_PORT_POLL_TYPED` (531): a0=cap_handle, a1=out_ptr. Copies a
/// 16-byte `PortEvent` (key, source_type, source_id + padding) to
/// `out_ptr` on success. Returns 16 on success, -EAGAIN if empty, or
/// -Errno on cap / arg failure.
pub fn sys_port_poll_typed(cap_raw: u64, out_ptr: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Port, Cap};

    if out_ptr == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }
    let cap: Cap<Port> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::port::port_poll_cap(table, cap)
    });
    match result {
        Some(Ok(event)) => {
            // 16-byte ABI-stable encoding: key:u64, source_type:u8,
            // _pad:[u8;3], source_id:u32.
            let mut buf = [0u8; 16];
            buf[..8].copy_from_slice(&event.key.to_le_bytes());
            buf[8] = event.source_type;
            buf[12..16].copy_from_slice(&event.source_id.to_le_bytes());
            if robot_os_sched::current_user_pt() != 0 {
                if !robot_os_sched::copy_to_user(out_ptr as usize, buf.as_ptr(), 16) {
                    return Errno::EFAULT.to_syscall_ret();
                }
            } else {
                unsafe {
                    core::ptr::copy_nonoverlapping(buf.as_ptr(), out_ptr as *mut u8, 16);
                }
            }
            16
        }
        Some(Err(e)) => errno_for_port_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_PORT_DESTROY_TYPED` (532): a0=cap_handle. Frees the port slot
/// **and revokes the cap** (W3-F5) — see `port::port_destroy_cap` for why
/// leaving it live was a confused-deputy path onto the next task that gets
/// the recycled port id.
pub fn sys_port_destroy_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Port, Cap};

    let cap: Cap<Port> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::port::port_destroy_cap(table, cap)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_port_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

// ── Cap<Shm> typed handlers — RFC-0003 W5 batch 2 ─────────────────────────

/// `shm_acquire_typed` out-blob layout: 8 bytes total.
/// `[0..4] page_count u32 LE, [4] perms u8 (0=RO,1=RW), [5..8] pad`.
const SHM_ACQUIRE_OUT_BYTES: usize = 8;

fn errno_for_shm_err(e: robot_os_ipc::shm::ShmCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::shm::ShmCapError;
    match e {
        ShmCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        ShmCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        ShmCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        ShmCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
        ShmCapError::NoMem => Errno::ENOMEM.to_syscall_ret(),
        ShmCapError::BadArg => Errno::EINVAL.to_syscall_ret(),
        ShmCapError::Closed => Errno::EBADF.to_syscall_ret(),
        ShmCapError::Full => Errno::EMFILE.to_syscall_ret(),
    }
}

fn decode_shm_perms(raw: u64) -> Option<robot_os_ipc::shm::ShmPerms> {
    use robot_os_ipc::shm::ShmPerms;
    match raw {
        0 => Some(ShmPerms::ReadOnly),
        1 => Some(ShmPerms::ReadWrite),
        _ => None,
    }
}

/// `SYS_SHM_CREATE_TYPED` (533): a0=page_count, a1=perms_mode
/// (0=ReadOnly, 1=ReadWrite). Allocates a shared-memory region and
/// grants a `Cap<Shm>` into the caller's cap-table. Returns the raw
/// cap handle as `i64` (always > 0) on success, or `-Errno`.
pub fn sys_shm_create_typed(page_count: u64, perms_mode: u64) -> i64 {
    use robot_os_abi::error::Errno;
    let Some(perms) = decode_shm_perms(perms_mode) else {
        return Errno::EINVAL.to_syscall_ret();
    };
    let tid = robot_os_sched::current_task_tid();
    match robot_os_ipc::shm::shm_create_cap(tid, page_count as usize, perms) {
        Ok(cap) => cap.raw().as_raw() as i64,
        Err(e) => errno_for_shm_err(e),
    }
}

/// `SYS_SHM_ACQUIRE_TYPED` (534): a0=cap_handle, a1=out_ptr. Bumps
/// the region refcount and writes an 8-byte blob (page_count u32 LE,
/// perms u8, 3-byte pad) to `out_ptr`. Returns `SHM_ACQUIRE_OUT_BYTES`
/// on success or `-Errno`.
pub fn sys_shm_acquire_typed(cap_raw: u64, out_ptr: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Shm, Cap};
    use robot_os_ipc::shm::ShmPerms;

    if out_ptr == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }
    let cap: Cap<Shm> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::shm::shm_acquire_cap(tid, table, cap)
    });
    match result {
        Some(Ok((page_count, perms))) => {
            let mut buf = [0u8; SHM_ACQUIRE_OUT_BYTES];
            buf[..4].copy_from_slice(&(page_count as u32).to_le_bytes());
            buf[4] = match perms {
                ShmPerms::ReadOnly => 0,
                ShmPerms::ReadWrite => 1,
            };
            if robot_os_sched::current_user_pt() != 0 {
                if !robot_os_sched::copy_to_user(
                    out_ptr as usize,
                    buf.as_ptr(),
                    SHM_ACQUIRE_OUT_BYTES,
                ) {
                    return Errno::EFAULT.to_syscall_ret();
                }
            } else {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        buf.as_ptr(),
                        out_ptr as *mut u8,
                        SHM_ACQUIRE_OUT_BYTES,
                    );
                }
            }
            SHM_ACQUIRE_OUT_BYTES as i64
        }
        Some(Err(e)) => errno_for_shm_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_SHM_RELEASE_TYPED` (535): a0=cap_handle. Drops **this task's**
/// reference; frees pages when the last ref goes away, and revokes the cap
/// (W3-F5). References are booked per task (W3-F1), so a task can only give
/// back a reference it actually took.
pub fn sys_shm_release_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Shm, Cap};

    let cap: Cap<Shm> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::shm::shm_release_cap(tid, table, cap)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_shm_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

// ── Cap<Gpio> typed handlers — RFC-0003 W5 batch 5.1 ──────────────────────

fn errno_for_gpio_err(e: robot_os_ipc::gpio_cap::GpioCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::gpio_cap::GpioCapError;
    match e {
        GpioCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        GpioCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        GpioCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        GpioCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
        GpioCapError::BadPin => Errno::EINVAL.to_syscall_ret(),
        GpioCapError::BadDirValue => Errno::EINVAL.to_syscall_ret(),
        GpioCapError::DriverFault => Errno::EIO.to_syscall_ret(),
    }
}

/// `SYS_GPIO_READ_TYPED` (539): a0=cap_handle. Returns 0/1 or `-Errno`.
pub fn sys_gpio_read_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Gpio, Cap};

    let cap: Cap<Gpio> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::gpio_cap::gpio_read_cap(table, cap)
    });
    match result {
        Some(Ok(v)) => v as i64,
        Some(Err(e)) => errno_for_gpio_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_GPIO_WRITE_TYPED` (540): a0=cap_handle, a1=val. Returns 0 or `-Errno`.
pub fn sys_gpio_write_typed(cap_raw: u64, val: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Gpio, Cap};

    let cap: Cap<Gpio> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::gpio_cap::gpio_write_cap(table, cap, val as u32)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_gpio_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_GPIO_SET_DIR_TYPED` (541): a0=cap_handle, a1=dir. Returns 0 or `-Errno`.
pub fn sys_gpio_set_dir_typed(cap_raw: u64, dir: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Gpio, Cap};

    let cap: Cap<Gpio> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::gpio_cap::gpio_set_dir_cap(table, cap, dir as u32)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_gpio_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

// ── Cap<I2c> typed handlers — RFC-0003 W5 batch 5.2 ──────────────────────

fn errno_for_i2c_err(e: robot_os_ipc::i2c_cap::I2cCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::i2c_cap::I2cCapError;
    match e {
        I2cCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        I2cCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        I2cCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        I2cCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
        I2cCapError::BadLen => Errno::EINVAL.to_syscall_ret(),
        I2cCapError::DriverFault => Errno::EIO.to_syscall_ret(),
    }
}

/// `SYS_I2C_READ_TYPED` (542): a0=cap, a1=reg, a2=buf_ptr, a3=buf_len.
pub fn sys_i2c_read_typed(
    cap_raw: u64,
    reg: u64,
    out_ptr: u64,
    out_len: u64,
) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_abi::syscall_nr::I2C_TYPED_MAX_BYTES;
    use robot_os_ipc::cap::{targets::I2c, Cap};

    let n = out_len as usize;
    if n == 0 || n > I2C_TYPED_MAX_BYTES || out_ptr == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }

    let mut buf = [0u8; I2C_TYPED_MAX_BYTES];
    let cap: Cap<I2c> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::i2c_cap::i2c_read_cap(table, cap, reg as u8, &mut buf[..n])
    });
    let got = match result {
        Some(Ok(v)) => v,
        Some(Err(e)) => return errno_for_i2c_err(e),
        None => return Errno::EINVAL.to_syscall_ret(),
    };

    if got > 0 {
        if robot_os_sched::current_user_pt() != 0 {
            if !robot_os_sched::copy_to_user(out_ptr as usize, buf.as_ptr(), got) {
                return Errno::EFAULT.to_syscall_ret();
            }
        } else {
            unsafe {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), out_ptr as *mut u8, got);
            }
        }
    }
    got as i64
}

/// `SYS_I2C_WRITE_TYPED` (543): a0=cap, a1=data_ptr, a2=data_len.
pub fn sys_i2c_write_typed(cap_raw: u64, in_ptr: u64, in_len: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_abi::syscall_nr::I2C_TYPED_MAX_BYTES;
    use robot_os_ipc::cap::{targets::I2c, Cap};

    let n = in_len as usize;
    if n == 0 || n > I2C_TYPED_MAX_BYTES || in_ptr == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }

    let mut buf = [0u8; I2C_TYPED_MAX_BYTES];
    if robot_os_sched::current_user_pt() != 0 {
        if !robot_os_sched::copy_from_user(buf.as_mut_ptr(), in_ptr as usize, n) {
            return Errno::EFAULT.to_syscall_ret();
        }
    } else {
        unsafe {
            core::ptr::copy_nonoverlapping(in_ptr as *const u8, buf.as_mut_ptr(), n);
        }
    }

    let cap: Cap<I2c> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::i2c_cap::i2c_write_cap(table, cap, &buf[..n])
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_i2c_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_I2C_DETECT_TYPED` (544): a0=cap. Returns 0/1 or `-Errno`.
pub fn sys_i2c_detect_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::I2c, Cap};

    let cap: Cap<I2c> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::i2c_cap::i2c_detect_cap(table, cap)
    });
    match result {
        Some(Ok(v)) => v as i64,
        Some(Err(e)) => errno_for_i2c_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

// ── Cap<Pwm> typed handlers — RFC-0003 W5 batch 5.3 ──────────────────────

fn errno_for_pwm_err(e: robot_os_ipc::pwm_cap::PwmCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::pwm_cap::PwmCapError;
    match e {
        PwmCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        PwmCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        PwmCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        PwmCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
        PwmCapError::BadChannel => Errno::EINVAL.to_syscall_ret(),
        PwmCapError::DriverFault => Errno::EIO.to_syscall_ret(),
    }
}

/// Generic helper for the 5 PWM ops that all take
/// `(cap, payload) -> Result<(), PwmCapError>` shape.
fn sys_pwm_dispatch_inner<F>(cap_raw: u64, op: F) -> i64
where
    F: FnOnce(
        &robot_os_ipc::cap::CapTable,
        robot_os_ipc::cap::Cap<robot_os_ipc::cap::targets::Pwm>,
    ) -> Result<(), robot_os_ipc::pwm_cap::PwmCapError>,
{
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Pwm, Cap};
    let cap: Cap<Pwm> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result =
        robot_os_ipc::cap_store::with_table(tid, |table| op(table, cap));
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_pwm_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

pub fn sys_pwm_enable_typed(cap_raw: u64) -> i64 {
    sys_pwm_dispatch_inner(cap_raw, |t, c| {
        robot_os_ipc::pwm_cap::pwm_enable_cap(t, c)
    })
}

pub fn sys_pwm_disable_typed(cap_raw: u64) -> i64 {
    sys_pwm_dispatch_inner(cap_raw, |t, c| {
        robot_os_ipc::pwm_cap::pwm_disable_cap(t, c)
    })
}

pub fn sys_pwm_set_period_typed(cap_raw: u64, period_ns: u64) -> i64 {
    sys_pwm_dispatch_inner(cap_raw, |t, c| {
        robot_os_ipc::pwm_cap::pwm_set_period_cap(t, c, period_ns as u32)
    })
}

pub fn sys_pwm_set_duty_typed(cap_raw: u64, duty_ns: u64) -> i64 {
    sys_pwm_dispatch_inner(cap_raw, |t, c| {
        robot_os_ipc::pwm_cap::pwm_set_duty_cap(t, c, duty_ns as u32)
    })
}

pub fn sys_pwm_set_duty_pct_typed(cap_raw: u64, pct: u64) -> i64 {
    sys_pwm_dispatch_inner(cap_raw, |t, c| {
        robot_os_ipc::pwm_cap::pwm_set_duty_pct_cap(t, c, pct as u32)
    })
}

// ── Cap<Motor> typed handlers — RFC-0003 W5 batch 5.4 ────────────────────

fn errno_for_motor_err(e: robot_os_ipc::motor_cap::MotorCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::motor_cap::MotorCapError;
    match e {
        MotorCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        MotorCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        MotorCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        MotorCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
    }
}

pub fn sys_motor_set_target_typed(cap_raw: u64, speed_l: u64, speed_r: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Motor, Cap};
    let cap: Cap<Motor> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |t| {
        robot_os_ipc::motor_cap::motor_set_target_cap(
            t, cap, speed_l as i16, speed_r as i16,
        )
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_motor_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

pub fn sys_motor_tick_typed(
    cap_raw: u64,
    ticks_l: u64,
    ticks_r: u64,
    now: u64,
    out_ptr: u64,
) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_abi::syscall_nr::MOTOR_TICK_OUT_BYTES;
    use robot_os_ipc::cap::{targets::Motor, Cap};
    if out_ptr == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }
    let cap: Cap<Motor> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |t| {
        robot_os_ipc::motor_cap::motor_tick_cap(
            t, cap, ticks_l as i64, ticks_r as i64, now,
        )
    });
    let (pwm_l, pwm_r) = match result {
        Some(Ok(v)) => v,
        Some(Err(e)) => return errno_for_motor_err(e),
        None => return Errno::EINVAL.to_syscall_ret(),
    };
    let mut buf = [0u8; MOTOR_TICK_OUT_BYTES];
    buf[0..4].copy_from_slice(&pwm_l.to_le_bytes());
    buf[4..8].copy_from_slice(&pwm_r.to_le_bytes());
    if robot_os_sched::current_user_pt() != 0 {
        if !robot_os_sched::copy_to_user(
            out_ptr as usize,
            buf.as_ptr(),
            MOTOR_TICK_OUT_BYTES,
        ) {
            return Errno::EFAULT.to_syscall_ret();
        }
    } else {
        unsafe {
            core::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                out_ptr as *mut u8,
                MOTOR_TICK_OUT_BYTES,
            );
        }
    }
    MOTOR_TICK_OUT_BYTES as i64
}

pub fn sys_motor_enable_typed(cap_raw: u64, en: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Motor, Cap};
    let cap: Cap<Motor> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |t| {
        robot_os_ipc::motor_cap::motor_enable_cap(t, cap, en != 0)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_motor_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

pub fn sys_motor_enabled_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Motor, Cap};
    let cap: Cap<Motor> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |t| {
        robot_os_ipc::motor_cap::motor_enabled_cap(t, cap)
    });
    match result {
        Some(Ok(v)) => i64::from(v),
        Some(Err(e)) => errno_for_motor_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

pub fn sys_motor_set_gains_typed(cap_raw: u64, kp: u64, ki: u64, kd: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Motor, Cap};
    let cap: Cap<Motor> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |t| {
        robot_os_ipc::motor_cap::motor_set_gains_cap(
            t, cap, kp as i32, ki as i32, kd as i32,
        )
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_motor_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

pub fn sys_motor_reset_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::Motor, Cap};
    let cap: Cap<Motor> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |t| {
        robot_os_ipc::motor_cap::motor_reset_cap(t, cap)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_motor_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

// ── SYS_DRV_INVOKE — RFC-0002 Driver registry bridge ──────────────────────

fn errno_for_driver_err(e: robot_os_drivers::api::DriverError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_drivers::api::DriverError;
    match e {
        DriverError::NotInitialized => Errno::ENODEV.to_syscall_ret(),
        DriverError::BadOp => Errno::ENOSYS.to_syscall_ret(),
        DriverError::BadInput => Errno::EINVAL.to_syscall_ret(),
        DriverError::BadOutput => Errno::EINVAL.to_syscall_ret(),
        DriverError::Busy => Errno::EAGAIN.to_syscall_ret(),
        DriverError::IoFault => Errno::EIO.to_syscall_ret(),
        DriverError::Unsupported => Errno::ENOSYS.to_syscall_ret(),
        DriverError::NoMem => Errno::ENOMEM.to_syscall_ret(),
        DriverError::Other(_) => Errno::EIO.to_syscall_ret(),
    }
}

/// Translate a `DRV_KIND_*` value into the `CapKind` that represents
/// authority over that subsystem, or `None` when no such cap kind exists.
///
/// Only four driver kinds have a capability analogue today. The rest —
/// UART, SPI, DMA, CSI camera, LiDAR, IMU, GPS, ADC, NPU, CAN, xHCI — have
/// no `CapKind`, so there is nothing a client could be asked to hold, and
/// [`drv_invoke_authorized`] denies them to userspace. See the report:
/// closing that gap means extending `CapKind`, which is a wire-format
/// (`crates/abi`) change.
fn cap_kind_for_driver(kind: u32) -> Option<robot_os_ipc::cap::CapKind> {
    use robot_os_driver_server::{
        DRV_KIND_GPIO, DRV_KIND_I2C, DRV_KIND_MOTOR_PID, DRV_KIND_PWM,
    };
    use robot_os_ipc::cap::CapKind;
    match kind {
        DRV_KIND_GPIO => Some(CapKind::Gpio),
        DRV_KIND_I2C => Some(CapKind::I2c),
        DRV_KIND_PWM => Some(CapKind::Pwm),
        DRV_KIND_MOTOR_PID => Some(CapKind::Motor),
        _ => None,
    }
}

/// May the current task invoke `drv`?
///
/// **WHY this exists (W3-F9):** `DriverManifest::required_perms` is
/// documented as "cap-table permissions a client must hold to call this
/// driver", and a tree-wide grep found it referenced only in its own
/// definition and two *display* sites (`crates/shell`, `kernel/src/main.rs`
/// both just print it). `SYS_DRV_INVOKE` dispatched `handle_request` with no
/// check at all — so ring 3 reached the GPIO / I2C / PWM / motor drivers
/// directly, bypassing exactly the capability requirement the manifest
/// advertises, while the typed syscalls next door (`sys_motor_set_target_typed`
/// and friends) all demand a `Cap<T>`. A field that claims a protection the
/// code never applies is the bug class this whole batch is about.
///
/// Kernel callers (`user_pt == 0`) bypass, as everywhere else. Userspace is
/// denied for any driver kind with no `CapKind` analogue — fail-closed, and
/// nothing in `userspace/` invokes this syscall today, so no working path
/// regresses.
fn drv_invoke_authorized(drv: &&'static dyn robot_os_drivers::api::Driver) -> bool {
    if robot_os_sched::current_user_pt() == 0 {
        return true;
    }
    let manifest = drv.manifest();
    let cap_kind = match cap_kind_for_driver(manifest.kind) {
        Some(k) => k,
        None => return false,
    };
    let tid = robot_os_sched::current_task_tid();
    robot_os_ipc::cap_store::with_table(tid, |table| {
        table.holds_kind_with(cap_kind, manifest.required_perms)
    })
    .unwrap_or(false)
}

/// `SYS_DRV_INVOKE` (311): userspace bridge into the RFC-0002
/// Driver registry. `a0=kind, a1=op, a2=in_ptr, a3=in_len,
/// a4=out_ptr, a5=out_cap`. Returns bytes written to `out_ptr`
/// (≥ 0) or `-Errno`. Userspace callers must hold a capability of the
/// matching kind carrying the manifest's `required_perms` — see
/// [`drv_invoke_authorized`].
pub fn sys_drv_invoke(
    kind: u64,
    op: u64,
    in_ptr: u64,
    in_len: u64,
    out_ptr: u64,
    out_cap: u64,
) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_abi::syscall_nr::{
        DRIVER_INVOKE_MAX_INPUT_BYTES, DRIVER_INVOKE_MAX_OUTPUT_BYTES,
    };

    let in_len_u = in_len as usize;
    let out_cap_u = out_cap as usize;
    if in_len_u > DRIVER_INVOKE_MAX_INPUT_BYTES
        || out_cap_u > DRIVER_INVOKE_MAX_OUTPUT_BYTES
    {
        return Errno::EINVAL.to_syscall_ret();
    }

    // Per-call stack buffers — bounded by the consts above so the
    // syscall stack frame stays small.
    let mut in_buf = [0u8; DRIVER_INVOKE_MAX_INPUT_BYTES];
    let mut out_buf = [0u8; DRIVER_INVOKE_MAX_OUTPUT_BYTES];

    // Copy input from userspace.
    if in_len_u > 0 {
        if in_ptr == 0 {
            return Errno::EINVAL.to_syscall_ret();
        }
        if robot_os_sched::current_user_pt() != 0 {
            if !robot_os_sched::copy_from_user(
                in_buf.as_mut_ptr(),
                in_ptr as usize,
                in_len_u,
            ) {
                return Errno::EFAULT.to_syscall_ret();
            }
        } else {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    in_ptr as *const u8,
                    in_buf.as_mut_ptr(),
                    in_len_u,
                );
            }
        }
    }

    // Look up + dispatch. We hold the registry lock only across
    // the find — `handle_request` runs without the registry lock
    // so per-driver locks (e.g. UART SMP lock) won't deadlock
    // against the registry mutex.
    let drv = match robot_os_drivers::runtime::registry::REGISTRY
        .lock()
        .find_by_kind(kind as u32)
    {
        Some(d) => d,
        None => return Errno::ENODEV.to_syscall_ret(),
    };

    // W3-F9: enforce the manifest's declared client requirement.
    if !drv_invoke_authorized(&drv) {
        return Errno::EPERM.to_syscall_ret();
    }

    let result = drv.handle_request(
        op as u32,
        &in_buf[..in_len_u],
        &mut out_buf[..out_cap_u],
    );

    let n = match result {
        Ok(n) => n,
        Err(e) => return errno_for_driver_err(e),
    };

    // Copy output back to userspace.
    if n > 0 {
        if out_ptr == 0 {
            return Errno::EINVAL.to_syscall_ret();
        }
        if robot_os_sched::current_user_pt() != 0 {
            if !robot_os_sched::copy_to_user(
                out_ptr as usize,
                out_buf.as_ptr(),
                n,
            ) {
                return Errno::EFAULT.to_syscall_ret();
            }
        } else {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    out_buf.as_ptr(),
                    out_ptr as *mut u8,
                    n,
                );
            }
        }
    }
    n as i64
}

// ── Cap<IoRing> typed handlers — RFC-0003 W5 batch 3 ──────────────────────

/// Width in bytes of the phys-addr out-blob for SYS_IORING_CREATE_TYPED.
const IORING_PHYS_OUT_BYTES: usize = 8;

fn errno_for_ioring_err(e: robot_os_ipc::io_ring::IoRingCapError) -> i64 {
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::CapError;
    use robot_os_ipc::io_ring::IoRingCapError;
    match e {
        IoRingCapError::Cap(CapError::Stale) => Errno::ECAPSTALE.to_syscall_ret(),
        IoRingCapError::Cap(CapError::WrongKind) => Errno::ECAPKIND.to_syscall_ret(),
        IoRingCapError::Cap(CapError::MissingPerms) => Errno::ECAPPERMS.to_syscall_ret(),
        IoRingCapError::Cap(CapError::Contained) => Errno::EAGAIN.to_syscall_ret(),
        IoRingCapError::NoMem => Errno::ENOMEM.to_syscall_ret(),
        IoRingCapError::Closed => Errno::EBADF.to_syscall_ret(),
        IoRingCapError::Full => Errno::EMFILE.to_syscall_ret(),
        // SubmitError reuses EIO (the underlying integer status is
        // not surfaced through errno; userspace polls the CQ for
        // per-op results in the same way as legacy io_ring_submit).
        IoRingCapError::SubmitError(_) => Errno::EIO.to_syscall_ret(),
    }
}

/// `SYS_IORING_CREATE_TYPED` (536): a0=phys_out_ptr. Allocates a
/// ring + grants a `Cap<IoRing>` into the caller's cap-table.
/// Writes the physical address of the ring page (`u64 LE`) to
/// `phys_out_ptr` **for kernel callers only** — ring 3 receives zero.
/// Returns the raw cap handle (`i64` > 0) on success, or `-Errno`.
///
/// **WHY ring 3 gets zero (W3-F6):** this copied a kernel physical address
/// out to userspace. Userspace cannot do anything legitimate with it — it
/// has no way to map a raw frame, and S-mode never sets SUM so it cannot
/// dereference it either — while it hands an attacker a *known-valid*
/// physical frame address to feed to any syscall that takes one, and leaks
/// PMM layout for free. Nothing in `userspace/` or `crates/libsys` calls
/// this syscall today, so zeroing the field breaks no caller. Giving ring 3
/// a usable ring still needs a mapping path (the page must be installed in
/// the caller's page table and a *virtual* address returned); that is an ABI
/// addition in `crates/sched`, out of scope here — see the report.
pub fn sys_ioring_create_typed(phys_out_ptr: u64) -> i64 {
    use robot_os_abi::error::Errno;
    if phys_out_ptr == 0 {
        return Errno::EINVAL.to_syscall_ret();
    }
    let tid = robot_os_sched::current_task_tid();
    match robot_os_ipc::io_ring::io_ring_create_cap(tid) {
        Ok((cap, phys_addr)) => {
            let disclosed = if robot_os_sched::current_user_pt() != 0 {
                0u64
            } else {
                phys_addr
            };
            let buf = disclosed.to_le_bytes();
            if robot_os_sched::current_user_pt() != 0 {
                if !robot_os_sched::copy_to_user(
                    phys_out_ptr as usize,
                    buf.as_ptr(),
                    IORING_PHYS_OUT_BYTES,
                ) {
                    return Errno::EFAULT.to_syscall_ret();
                }
            } else {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        buf.as_ptr(),
                        phys_out_ptr as *mut u8,
                        IORING_PHYS_OUT_BYTES,
                    );
                }
            }
            cap.raw().as_raw() as i64
        }
        Err(e) => errno_for_ioring_err(e),
    }
}

/// `SYS_IORING_SUBMIT_TYPED` (537): a0=cap_handle. Processes SQEs;
/// returns the count of SQEs processed (≥ 0) or `-Errno`.
pub fn sys_ioring_submit_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::IoRing, Cap};

    let cap: Cap<IoRing> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::io_ring::io_ring_submit_cap(table, cap)
    });
    match result {
        Some(Ok(n)) => n as i64,
        Some(Err(e)) => errno_for_ioring_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

/// `SYS_IORING_DESTROY_TYPED` (538): a0=cap_handle. Frees the ring + its
/// backing page **and revokes the cap** (W3-F5) — see
/// `io_ring::io_ring_destroy_cap` for why leaving it live let a stale cap
/// drive the next task that received the recycled ring id. Returns `EBADF`
/// if a submit pass is in flight on the ring; retry.
pub fn sys_ioring_destroy_typed(cap_raw: u64) -> i64 {
    use robot_os_abi::cap::CapHandle;
    use robot_os_abi::error::Errno;
    use robot_os_ipc::cap::{targets::IoRing, Cap};

    let cap: Cap<IoRing> = Cap::from_raw(CapHandle::from_raw(cap_raw as u32));
    let tid = robot_os_sched::current_task_tid();
    let result = robot_os_ipc::cap_store::with_table(tid, |table| {
        robot_os_ipc::io_ring::io_ring_destroy_cap(table, cap)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => errno_for_ioring_err(e),
        None => Errno::EINVAL.to_syscall_ret(),
    }
}

// ── Sensor read (Phase S) ────────────────────────────────────────────────────

/// Sensor types for SYS_SENSOR_READ.
pub const SENSOR_TYPE_IMU:       u64 = 0;  // 24 bytes: accel[3] + gyro[3] as i32 LE
pub const SENSOR_TYPE_ODOM:      u64 = 1;  // 16 bytes: dist_mm(i64) + heading_cdeg(i64)
pub const SENSOR_TYPE_ENCODER:   u64 = 2;  // 16 bytes: enc_l(i64) + enc_r(i64)
pub const SENSOR_TYPE_RANGE:     u64 = 3;  //  4 bytes: front_mm(u16) + right_mm(u16)
pub const SENSOR_TYPE_BATTERY:   u64 = 4;  //  2 bytes: mv(u16)
pub const SENSOR_TYPE_GPS:       u64 = 5;  // 16 bytes: lat_deg7(i32) + lon_deg7(i32) + alt_cm(i32) + fix(u8) + sats(u8) + pad(u16)
pub const SENSOR_TYPE_LIDAR:     u64 = 6;  // N×4 bytes: [angle_cdeg(u16) + distance_mm(u16)] per point
pub const SENSOR_TYPE_GPIO_FLAGS: u64 = 7; // 2 bytes: u16 LE — PIR(0x0001) | SOUND(0x0002) | IR(0x0004)
pub const SENSOR_TYPE_CAMERA:    u64 = 8; // Variable: JPEG bytes from csi_capture_jpeg()
pub const SENSOR_TYPE_POWER:     u64 = 9; // 12 bytes: voltage_mv(u16) + current_ma(u16) + mah_used(u32) + pct(u8) + sag(u8) + failsafe(u8) + pad(u8)

// GPIO pins for digital sensors (must match guard mode pin assignment)
const GPIO_PIN_PIR: u32 = 13;
const GPIO_PIN_IR: u32 = 14;
const GPIO_PIN_SOUND: u32 = 15;

// Sensor flag bits (must match brain_protocol.rs)
const SENSOR_FLAG_PIR: u16   = 0x0001;
const SENSOR_FLAG_SOUND: u16 = 0x0002;
const SENSOR_FLAG_IR: u16    = 0x0004;

/// Helper: write sensor data to user buffer (handles both kernel and user-space callers).
/// Builds data in a kernel-side tmp buffer, then copies to user via copy_to_user if needed.
fn sensor_write_to_user(buf_ptr: u64, data: &[u8]) -> i64 {
    if robot_os_sched::current_user_pt() != 0 {
        // User-space caller — safe copy via page table walk
        if robot_os_sched::copy_to_user(buf_ptr as usize, data.as_ptr(), data.len()) {
            data.len() as i64
        } else {
            -1
        }
    } else {
        // Kernel caller — direct copy
        let out = unsafe {
            core::slice::from_raw_parts_mut(buf_ptr as *mut u8, data.len())
        };
        out.copy_from_slice(data);
        data.len() as i64
    }
}

/// SYS_SENSOR_READ: read sensor data.
///   a0 = sensor_type, a1 = user_buf_ptr, a2 = buf_len
///   Returns bytes written, or -1 on error.
pub fn sys_sensor_read(sensor_type: u64, buf_ptr: u64, buf_len: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::Sensor(sensor_type as u8), false) { return E_PERM; }
    if buf_ptr == 0 { return -1; }

    match sensor_type {
        SENSOR_TYPE_IMU => {
            const IMU_DATA_SIZE: usize = 24; // 6 × i32
            if (buf_len as usize) < IMU_DATA_SIZE { return -1; }
            if let Some(imu) = robot_os_imu::imu_read_scaled() {
                let mut tmp = [0u8; IMU_DATA_SIZE];
                for i in 0..3 {
                    let b = imu.accel_mg[i].to_le_bytes();
                    tmp[i * 4..i * 4 + 4].copy_from_slice(&b);
                }
                for i in 0..3 {
                    let b = imu.gyro_mdps[i].to_le_bytes();
                    tmp[12 + i * 4..12 + i * 4 + 4].copy_from_slice(&b);
                }
                sensor_write_to_user(buf_ptr, &tmp)
            } else {
                -1 // IMU not ready
            }
        }
        SENSOR_TYPE_ODOM => {
            const ODOM_DATA_SIZE: usize = 16; // 2 × i64
            if (buf_len as usize) < ODOM_DATA_SIZE { return -1; }
            let (dist_mm, heading_cdeg) = robot_os_robot::odom_get();
            let mut tmp = [0u8; ODOM_DATA_SIZE];
            tmp[0..8].copy_from_slice(&dist_mm.to_le_bytes());
            tmp[8..16].copy_from_slice(&heading_cdeg.to_le_bytes());
            sensor_write_to_user(buf_ptr, &tmp)
        }
        SENSOR_TYPE_ENCODER => {
            const ENC_DATA_SIZE: usize = 16; // 2 × i64
            if (buf_len as usize) < ENC_DATA_SIZE { return -1; }
            let (enc_l, enc_r) = robot_os_robot::encoder_read();
            let mut tmp = [0u8; ENC_DATA_SIZE];
            tmp[0..8].copy_from_slice(&enc_l.to_le_bytes());
            tmp[8..16].copy_from_slice(&enc_r.to_le_bytes());
            sensor_write_to_user(buf_ptr, &tmp)
        }
        SENSOR_TYPE_RANGE => {
            const RANGE_DATA_SIZE: usize = 4; // 2 × u16
            if (buf_len as usize) < RANGE_DATA_SIZE { return -1; }
            let front = robot_os_drivers::rangefinder::us_read_mm(0).unwrap_or(0) as u16;
            let right = robot_os_drivers::rangefinder::us_read_mm(1).unwrap_or(0) as u16;
            let mut tmp = [0u8; RANGE_DATA_SIZE];
            tmp[0..2].copy_from_slice(&front.to_le_bytes());
            tmp[2..4].copy_from_slice(&right.to_le_bytes());
            sensor_write_to_user(buf_ptr, &tmp)
        }
        SENSOR_TYPE_BATTERY => {
            const BATT_DATA_SIZE: usize = 2; // u16
            if (buf_len as usize) < BATT_DATA_SIZE { return -1; }
            const SIMULATED_BATTERY_MV: u16 = 3700;
            const BATTERY_ADC_CHANNEL: u8 = 0;
            const BATTERY_DIVIDER_RATIO: u32 = 2; // 1:1 voltage divider halves Vbat
            let mv: u16 = if robot_os_drivers::ads1115::ads1115_is_initialized() {
                robot_os_drivers::ads1115::ads1115_read_battery_mv(
                    BATTERY_ADC_CHANNEL, BATTERY_DIVIDER_RATIO
                ).unwrap_or(SIMULATED_BATTERY_MV as u32) as u16
            } else {
                SIMULATED_BATTERY_MV
            };
            sensor_write_to_user(buf_ptr, &mv.to_le_bytes())
        }
        SENSOR_TYPE_GPS => {
            const GPS_DATA_SIZE: usize = 16;
            if (buf_len as usize) < GPS_DATA_SIZE { return -1; }
            let mut tmp = [0u8; GPS_DATA_SIZE];
            if let Some(pos) = robot_os_gps::gps_read() {
                tmp[0..4].copy_from_slice(&pos.lat_deg7.to_le_bytes());
                tmp[4..8].copy_from_slice(&pos.lon_deg7.to_le_bytes());
                tmp[8..12].copy_from_slice(&pos.alt_mm.to_le_bytes());
                tmp[12] = pos.fix;
                tmp[13] = pos.sats;
                tmp[14..16].copy_from_slice(&0u16.to_le_bytes());
            }
            sensor_write_to_user(buf_ptr, &tmp)
        }
        SENSOR_TYPE_LIDAR => {
            // Read latest LiDAR scan into user buffer
            // Data format: N × [angle_cdeg(u16 LE) + distance_mm(u16 LE)]
            let count = robot_os_drivers::lidar::lidar_scan_count();
            if count == 0 { return 0; } // no scan available
            let needed = count * robot_os_drivers::lidar::SCAN_POINT_SIZE;
            if (buf_len as usize) < needed { return -1; }
            // Read into kernel tmp buffer, then copy to user
            let mut tmp = [0u8; robot_os_drivers::lidar::SCAN_DATA_MAX_BYTES];
            let bytes = robot_os_drivers::lidar::lidar_read_scan(&mut tmp);
            if bytes == 0 { return 0; }
            sensor_write_to_user(buf_ptr, &tmp[..bytes])
        }
        SENSOR_TYPE_GPIO_FLAGS => {
            const FLAGS_DATA_SIZE: usize = 2; // u16
            if (buf_len as usize) < FLAGS_DATA_SIZE { return -1; }
            let mut flags: u16 = 0;
            if robot_os_drivers::gpio::gpio_read(GPIO_PIN_PIR) == 1 {
                flags |= SENSOR_FLAG_PIR;
            }
            if robot_os_drivers::gpio::gpio_read(GPIO_PIN_SOUND) == 1 {
                flags |= SENSOR_FLAG_SOUND;
            }
            if robot_os_drivers::gpio::gpio_read(GPIO_PIN_IR) == 1 {
                flags |= SENSOR_FLAG_IR;
            }
            sensor_write_to_user(buf_ptr, &flags.to_le_bytes())
        }
        SENSOR_TYPE_POWER => {
            const PWR_SIZE: usize = robot_os_drivers::ina219::POWER_DATA_SIZE;
            if (buf_len as usize) < PWR_SIZE { return -1; }
            let mut tmp = [0u8; PWR_SIZE];
            let n = robot_os_drivers::ina219::ina219_read_power(&mut tmp);
            if n == 0 { return 0; }
            sensor_write_to_user(buf_ptr, &tmp[..n])
        }
        SENSOR_TYPE_CAMERA => {
            // The JPEG frame lives in a static, not on the kernel stack.
            //
            // WHY: `JPEG_MAX_SIZE` is 19,200 bytes and a task's kernel stack is
            // 16 KiB with the bottom 4 KiB as a guard page — 12 KiB usable.
            // Rust emits no stack probes, so the prologue of this handler moved
            // `sp` *past* the guard page in one step and landed in the adjacent
            // task's stack.  The guard never fires; the result is silent
            // cross-task memory corruption instead of a clean fault.  Same
            // bounce-buffer shape `sys_disk_read`/`sys_disk_write` already use.
            static CAM_BUF: SpinLock<[u8; robot_os_drivers::csi::JPEG_MAX_SIZE]> =
                SpinLock::new([0u8; robot_os_drivers::csi::JPEG_MAX_SIZE]);
            let mut jpeg_buf = CAM_BUF.lock();
            let jpeg_len = robot_os_drivers::csi::csi_capture_jpeg(&mut jpeg_buf[..]);
            if jpeg_len == 0 { return 0; }
            // Clamp before slicing: a driver reporting more than the buffer
            // holds must not panic here (`panic = "abort"` → board reset).
            let jpeg_len = jpeg_len.min(robot_os_drivers::csi::JPEG_MAX_SIZE);
            if (buf_len as usize) < jpeg_len { return -1; }
            sensor_write_to_user(buf_ptr, &jpeg_buf[..jpeg_len])
        }
        _ => -1,
    }
}

// ── ADC (ADS1115) ────────────────────────────────────────────────────────────

/// Read ADC channel in millivolts.  a0 = channel (0-3), returns mv or -1.
pub fn sys_adc_read(channel: u64) -> i64 {
    if channel > 3 { return -1; }
    match robot_os_drivers::ads1115::ads1115_read_mv(channel as u8) {
        Some(mv) => mv as i64,
        None => -1,
    }
}

// ── Buzzer ───────────────────────────────────────────────────────────────────

/// Play a tone.  a0 = frequency in Hz, a1 = duration in ms.
pub fn sys_buzzer_tone(freq_hz: u64, duration_ms: u64) -> i64 {
    const MAX_BUZZER_FREQ_HZ: u16 = 20_000;
    const MAX_BUZZER_DURATION_MS: u32 = 10_000;
    let freq = (freq_hz as u16).min(MAX_BUZZER_FREQ_HZ);
    let dur = (duration_ms as u32).min(MAX_BUZZER_DURATION_MS);
    robot_os_drivers::buzzer::buzzer_tone(freq, dur);
    0
}

/// Stop buzzer.
pub fn sys_buzzer_off() -> i64 {
    robot_os_drivers::buzzer::buzzer_off();
    0
}

// ── E11.AQ3 — Userspace driver framework ─────────────────────────────────────
//
// Each syscall forwards to crates/driver_server. The caller_tid is the
// current task id (from sched), used to route replies and block the
// right waiter.

fn driver_caller_tid() -> u32 {
    robot_os_sched::current_task_tid()
}

/// Register as a driver for `kind` with MMIO/IRQ resources.
/// a0=kind, a1=mmio_base, a2=mmio_size, a3=irq.
pub fn sys_driver_register(kind: u64, mmio_base: u64, mmio_size: u64, irq: u64) -> i64 {
    let ok = robot_os_driver_server::driver_register(
        kind as u32,
        driver_caller_tid(),
        mmio_base,
        mmio_size,
        irq as u32,
    );
    if ok { 0 } else { -1 }
}

pub fn sys_driver_unregister(kind: u64) -> i64 {
    if robot_os_driver_server::driver_unregister(kind as u32) { 0 } else { -1 }
}

/// Poll for the next event for this driver kind. a0=kind, a1=user_out_ptr.
pub fn sys_driver_poll_event(kind: u64, user_out_ptr: u64) -> i64 {
    let (evt, payload) = robot_os_driver_server::driver_poll_event(kind as u32);
    if user_out_ptr != 0 {
        // Same boundary rule as fetch/reply — raw write_volatile to a user VA
        // faults (no SUM bit, no pointer validation). Go through copy_to_user
        // when called from a user process.
        let bytes = payload.to_ne_bytes();
        if robot_os_sched::current_user_pt() != 0 {
            if !robot_os_sched::copy_to_user(user_out_ptr as usize, bytes.as_ptr(), bytes.len()) {
                return -1;
            }
        } else {
            unsafe { core::ptr::write_volatile(user_out_ptr as *mut u64, payload); }
        }
    }
    evt as i64
}

/// Fetch the next pending DriverRequest. a0=kind, a1=user_buf_ptr.
pub fn sys_driver_fetch_request(kind: u64, user_buf_ptr: u64) -> i64 {
    if user_buf_ptr == 0 { return -1; }
    match robot_os_driver_server::driver_fetch_request(kind as u32) {
        Some(req) => {
            // The userspace driver's buffer must be written via copy_to_user:
            // a raw write_volatile to the user VA faults (no SUM bit, no
            // pointer validation). DriverRequest is #[repr(C)] so its byte
            // image is identical to the userspace mirror.
            let n = core::mem::size_of::<robot_os_driver_server::DriverRequest>();
            let ok = if robot_os_sched::current_user_pt() != 0 {
                robot_os_sched::copy_to_user(
                    user_buf_ptr as usize,
                    &req as *const robot_os_driver_server::DriverRequest as *const u8,
                    n,
                )
            } else {
                unsafe {
                    core::ptr::write_volatile(
                        user_buf_ptr as *mut robot_os_driver_server::DriverRequest,
                        req,
                    );
                }
                true
            };
            if !ok { return -1; }
            robot_os_driver_server::TOTAL_REQUESTS
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            0
        }
        None => -1,
    }
}

/// Publish a DriverReply. a0=kind, a1=user_reply_ptr.
pub fn sys_driver_reply(kind: u64, user_reply_ptr: u64) -> i64 {
    if user_reply_ptr == 0 { return -1; }
    // Same boundary rule as fetch: read the reply via copy_from_user.
    let mut reply = robot_os_driver_server::DriverReply::zeroed();
    let n = core::mem::size_of::<robot_os_driver_server::DriverReply>();
    if robot_os_sched::current_user_pt() != 0 {
        if !robot_os_sched::copy_from_user(
            &mut reply as *mut robot_os_driver_server::DriverReply as *mut u8,
            user_reply_ptr as usize,
            n,
        ) {
            return -1;
        }
    } else {
        reply = unsafe {
            core::ptr::read_volatile(
                user_reply_ptr as *const robot_os_driver_server::DriverReply,
            )
        };
    }
    if robot_os_driver_server::driver_reply(kind as u32, reply) { 0 } else { -1 }
}

/// Client issues a request to a userspace driver.
/// a0=kind, a1=op, a2=input_ptr, a3=input_len, a4=out_cap. Returns token (0=fail).
pub fn sys_driver_request(
    kind: u64, op: u64, input_ptr: u64, input_len: u64, out_cap: u64,
) -> i64 {
    let len = (input_len as usize)
        .min(robot_os_driver_server::DRIVER_REQUEST_PAYLOAD_BYTES);
    // Bounce the userspace input through copy_from_user — a raw from_raw_parts
    // on the user pointer bypasses SUM + pointer validation.
    let mut buf = [0u8; robot_os_driver_server::DRIVER_REQUEST_PAYLOAD_BYTES];
    let slice: &[u8] = if input_ptr != 0 && len > 0 {
        if robot_os_sched::current_user_pt() != 0 {
            if !robot_os_sched::copy_from_user(buf.as_mut_ptr(), input_ptr as usize, len) {
                return 0; // 0 = submit failed (no token)
            }
            &buf[..len]
        } else {
            unsafe { core::slice::from_raw_parts(input_ptr as *const u8, len) }
        }
    } else {
        &[]
    };
    let cap = (out_cap as u16)
        .min(robot_os_driver_server::DRIVER_REPLY_PAYLOAD_BYTES as u16);
    let tok = robot_os_driver_server::driver_submit_request(
        kind as u32,
        driver_caller_tid(),
        op as u32,
        slice,
        cap,
    );
    tok as i64
}

/// Try to take a reply for a token. a0=kind, a1=token, a2=user_out_ptr.
pub fn sys_driver_try_reply(kind: u64, token: u64, user_out_ptr: u64) -> i64 {
    if user_out_ptr == 0 { return -1; }
    let mut r = robot_os_driver_server::DriverReply::zeroed();
    if !robot_os_driver_server::driver_try_take_reply(kind as u32, token, &mut r) {
        return -1;
    }
    // Boundary rule: write through copy_to_user from a user process.
    let n = core::mem::size_of::<robot_os_driver_server::DriverReply>();
    let ok = if robot_os_sched::current_user_pt() != 0 {
        robot_os_sched::copy_to_user(
            user_out_ptr as usize,
            &r as *const robot_os_driver_server::DriverReply as *const u8,
            n,
        )
    } else {
        unsafe {
            core::ptr::write_volatile(
                user_out_ptr as *mut robot_os_driver_server::DriverReply, r,
            );
        }
        true
    };
    if ok { 0 } else { -1 }
}

/// Copy DriverServerStats into user buffer.
pub fn sys_driver_stats(user_out_ptr: u64) -> i64 {
    if user_out_ptr == 0 { return -1; }
    let s = robot_os_driver_server::stats();
    let n = core::mem::size_of::<robot_os_driver_server::DriverServerStats>();
    let ok = if robot_os_sched::current_user_pt() != 0 {
        robot_os_sched::copy_to_user(
            user_out_ptr as usize,
            &s as *const robot_os_driver_server::DriverServerStats as *const u8,
            n,
        )
    } else {
        unsafe {
            core::ptr::write_volatile(
                user_out_ptr as *mut robot_os_driver_server::DriverServerStats, s,
            );
        }
        true
    };
    if ok { 0 } else { -1 }
}

// ── Stubs for unimplemented subsystems ───────────────────────────────────────

pub fn sys_stub() -> i64 { -1 }
