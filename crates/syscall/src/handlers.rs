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
    socket_create, socket_bind, socket_connect,
    socket_listen_bound, socket_accept,
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
    // Userspace: check handle table
    let tid = robot_os_sched::current_task_tid();
    // Search all handles for one matching this resource and owner
    for h in 0..robot_os_ipc::MAX_HANDLES_GLOBAL as u32 {
        if let Some(k) = robot_os_ipc::handle_check(h, tid, need_write) {
            if k == kind {
                return true;
            }
        }
    }
    false
}

/// Permission denied error code.
const E_PERM: i64 = -99;

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

#[cfg(target_pointer_width = "64")]
pub fn sys_fork() -> i64   {
    robot_os_sched::process::sys_fork_impl()
}
#[cfg(target_pointer_width = "32")]
pub fn sys_fork() -> i64 { -1 }
pub fn sys_wait() -> i64   { -1 }  // Phase 8+

/// SYS_EXEC: a0 = pointer to ELF data in kernel memory (for now).
///
/// In a real system a0 would be a user-space path string, but for Phase 7 we
/// accept a raw `(ptr, len)` pair: a0 = data pointer, a1 = byte length.
/// The ELF is loaded into a new user address space.  On success the
/// trap_handler will switch to U-mode on SRET.
#[cfg(target_pointer_width = "64")]
pub fn sys_exec(data_ptr: u64, len: u64) -> i64 {
    if data_ptr == 0 || len == 0 { return -1; }
    let elf = unsafe {
        core::slice::from_raw_parts(data_ptr as *const u8, len as usize)
    };
    robot_os_sched::exec_user(elf)
}
#[cfg(target_pointer_width = "32")]
pub fn sys_exec(_data_ptr: u64, _len: u64) -> i64 { -1 }

#[cfg(target_pointer_width = "64")]
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

    // Read the entire ELF into a heap-allocated buffer.
    let mut buf: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    let mut tmp = [0u8; 512];
    loop {
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, tmp.as_mut_ptr(), 512);
        if n <= 0 { break; }
        buf.extend_from_slice(&tmp[..n as usize]);
    }
    robot_os_fs::vfs_close(&mut fd_table, fd);

    if buf.is_empty() { return -1; }
    robot_os_sched::exec_user(&buf)
}
#[cfg(target_pointer_width = "32")]
pub fn sys_execpath(_path_ptr: u64) -> i64 { -1 }

pub fn sys_sleep(ms: u64) -> i64 {
    // Busy-wait via CLINT (rough approximation; real sleep needs scheduler support)
    let ticks = ms * 10_000;  // ~10MHz CLINT
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
            for i in 0..chunk {
                robot_os_drivers::uart::putc(tmp[i]);
            }
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

pub fn sys_mkdir(path_ptr: u64) -> i64 {
    let path = unsafe { robot_os_fs::cstr_to_bytes(path_ptr as *const u8) };
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
    let path = unsafe { robot_os_fs::cstr_to_bytes(path_ptr as *const u8) };
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
                if size_out != 0 {
                    let sb = (size as u32).to_le_bytes();
                    robot_os_sched::copy_to_user(size_out as usize, sb.as_ptr(), 4);
                }
                if is_dir_out != 0 {
                    let db = (is_dir as u32).to_le_bytes();
                    robot_os_sched::copy_to_user(is_dir_out as usize, db.as_ptr(), 4);
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

pub fn sys_disk_read(sector: u64, count: u64, buf: u64) -> i64 {
    // Validate: null pointer, sane count (max 256 sectors = 128 KiB), overflow.
    if buf == 0 || count == 0 || count > 256 { return -1; }
    let byte_len = (count as usize).checked_mul(512).unwrap_or(0);
    if byte_len == 0 { return -1; }
    let buf_slice = unsafe {
        core::slice::from_raw_parts_mut(buf as *mut u8, byte_len)
    };
    match robot_os_drivers::virtio::blk::read(sector, count as u32, buf_slice) {
        Ok(()) => 0,
        Err(()) => -1,
    }
}

pub fn sys_disk_write(sector: u64, count: u64, buf: u64) -> i64 {
    if buf == 0 || count == 0 || count > 256 { return -1; }
    let byte_len = (count as usize).checked_mul(512).unwrap_or(0);
    if byte_len == 0 { return -1; }
    let buf_slice = unsafe {
        core::slice::from_raw_parts(buf as *const u8, byte_len)
    };
    match robot_os_drivers::virtio::blk::write(sector, count as u32, buf_slice) {
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
                // Write {read_fd, write_fd} to the provided pointer.
                let ptr = pipefd_ptr as *mut u32;
                unsafe {
                    core::ptr::write(ptr,        ridx as u32);
                    core::ptr::write(ptr.add(1), widx as u32);
                }
            }
            0
        }
    }
}

// ── Service manager ───────────────────────────────────────────────────────────

/// SYS_SERVICE_REGISTER: a0 = name_ptr, a1 = tid, a2 = ipc_channel.
pub fn sys_service_register(name_ptr: u64, tid: u64, channel: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let name = unsafe { robot_os_fs::cstr_to_bytes(name_ptr as *const u8) };
    service_register(name, tid as u32, channel as u32) as i64
}

/// SYS_SERVICE_DISCOVER: a0 = name_ptr.  Returns tid on success, -1 if not found.
pub fn sys_service_discover(name_ptr: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let name = unsafe { robot_os_fs::cstr_to_bytes(name_ptr as *const u8) };
    match service_discover(name) {
        Some(entry) => entry.tid as i64,
        None        => -1,
    }
}

/// SYS_SERVICE_HEARTBEAT: a0 = name_ptr.
pub fn sys_service_heartbeat(name_ptr: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let name = unsafe { robot_os_fs::cstr_to_bytes(name_ptr as *const u8) };
    service_heartbeat(name) as i64
}

/// SYS_SERVICE_STOP: a0 = name_ptr.
pub fn sys_service_stop_handler(name_ptr: u64) -> i64 {
    if name_ptr == 0 { return -1; }
    let name = unsafe { robot_os_fs::cstr_to_bytes(name_ptr as *const u8) };
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
pub fn sys_i2c_read(bus: u64, addr: u64, reg: u64, buf_ptr: u64, len: u64) -> i64 {
    if !cap_check(robot_os_ipc::HandleKind::I2c(bus as u8, addr as u8), false) { return E_PERM; }
    if buf_ptr == 0 || len == 0 { return -1; }
    let buf = unsafe {
        core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len as usize)
    };
    i2c_read(bus as u8, addr as u8, reg as u8, buf) as i64
}

/// SYS_I2C_WRITE: a0=bus, a1=addr, a2=data_ptr, a3=len.
/// data[0] = register address, data[1..] = bytes to write.
pub fn sys_i2c_write(bus: u64, addr: u64, data_ptr: u64, len: u64) -> i64 {
    if data_ptr == 0 || len == 0 { return -1; }
    let data = unsafe {
        core::slice::from_raw_parts(data_ptr as *const u8, len as usize)
    };
    i2c_write(bus as u8, addr as u8, data) as i64
}

pub fn sys_i2c_scan(bus: u64) -> i64 {
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
pub fn sys_socket(domain: u64, sock_type: u64, proto: u64) -> i64 {
    socket_create(domain as u32, sock_type as u32, proto as u32) as i64
}

/// SYS_BIND: a0=fd, a1=sockaddr_ptr, a2=addrlen. Returns 0 or -1.
pub fn sys_bind(fd: u64, addr_ptr: u64, _addrlen: u64) -> i64 {
    match read_sockaddr(addr_ptr) {
        Some(addr) => socket_bind(fd as i32, &addr) as i64,
        None       => -1,
    }
}

/// SYS_LISTEN: a0=fd, a1=backlog (ignored). Returns 0 or -1.
pub fn sys_listen_syscall(fd: u64, _backlog: u64) -> i64 {
    socket_listen_bound(fd as i32) as i64
}

/// SYS_ACCEPT: a0=fd, a1=addr_out (ignored), a2=addrlen_out (ignored).
/// Polls until an Established connection is ready; returns new fd or -1.
pub fn sys_accept(fd: u64, _addr_out: u64, _addrlen_out: u64) -> i64 {
    for _ in 0..50_000u32 {
        robot_os_net::net_poll();
        let r = socket_accept(fd as i32);
        if r >= 0 { return r as i64; }
        robot_os_sched::task_yield();
    }
    -1
}

/// SYS_CONNECT: a0=fd, a1=sockaddr_ptr, a2=addrlen.
pub fn sys_connect_syscall(fd: u64, addr_ptr: u64, _addrlen: u64) -> i64 {
    match read_sockaddr(addr_ptr) {
        Some(addr) => socket_connect(fd as i32, &addr, 0xC000 + fd as u16) as i64,
        None       => -1,
    }
}

/// SYS_SEND / SYS_SENDTO: a0=fd, a1=buf_ptr, a2=len, a3=flags (ignored).
pub fn sys_send_syscall(fd: u64, buf_ptr: u64, len: u64, _flags: u64) -> i64 {
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
pub fn sys_recv_syscall(fd: u64, buf_ptr: u64, len: u64, _flags: u64) -> i64 {
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

/// SYS_SOCK_SHUTDOWN / close a socket fd.
pub fn sys_sock_close(fd: u64) -> i64 {
    socket_close(fd as i32); 0
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
#[cfg(target_pointer_width = "64")]
pub fn sys_mmap(addr: u64, length: u64, _prot: u64, _flags: u64, fd: u64, _offset: u64) -> i64 {
    // Only support anonymous mappings (fd == -1 or fd == u64::MAX)
    if fd != u64::MAX && fd as i64 != -1 { return -1; }
    let user_pt = robot_os_sched::current_user_pt();
    if user_pt == 0 { return -1; } // kernel task

    let len = length as usize;
    if len == 0 { return -1; }

    let page_size = robot_os_arch::mmu::PAGE_SIZE;
    let num_pages = (len + page_size - 1) / page_size;

    // Use brk as base for anonymous mappings, then advance brk
    let base = robot_os_sched::update_user_brk(0) as usize;
    let aligned_base = (base + page_size - 1) & !(page_size - 1);

    let mut va = aligned_base;
    for _ in 0..num_pages {
        match robot_os_mm::pmm::alloc_page() {
            Ok(page) => {
                let flags = robot_os_arch::mmu::PteFlags::USER_RW
                    | robot_os_arch::mmu::PteFlags::ACCESSED
                    | robot_os_arch::mmu::PteFlags::DIRTY;
                let _ = robot_os_mm::vmm::map(user_pt, va, page.as_usize(), flags);
            }
            Err(_) => return -1, // OOM
        }
        va += page_size;
    }

    // Advance brk past the mapped region
    robot_os_sched::update_user_brk(va as u64);

    // If caller specified an addr hint, we ignore it (simplified)
    let _ = addr;
    aligned_base as i64
}

#[cfg(target_pointer_width = "32")]
pub fn sys_mmap(_a: u64, _b: u64, _c: u64, _d: u64, _e: u64, _f: u64) -> i64 { -1 }

/// SYS_MUNMAP: unmap pages.  Simplified: just marks pages as unmapped.
#[cfg(target_pointer_width = "64")]
pub fn sys_munmap(addr: u64, length: u64) -> i64 {
    let user_pt = robot_os_sched::current_user_pt();
    if user_pt == 0 { return -1; }

    let page_size = robot_os_arch::mmu::PAGE_SIZE;
    let len = length as usize;
    if len == 0 { return -1; }

    let mut va = addr as usize & !(page_size - 1);
    let end = va + ((len + page_size - 1) & !(page_size - 1));
    while va < end {
        let _ = robot_os_mm::vmm::unmap(user_pt, va);
        va += page_size;
    }
    0
}

#[cfg(target_pointer_width = "32")]
pub fn sys_munmap(_addr: u64, _length: u64) -> i64 { -1 }

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
    let ticks = seconds * 10_000_000; // ~10 MHz CLINT
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
            let mut jpeg_buf = [0u8; robot_os_drivers::csi::JPEG_MAX_SIZE];
            let jpeg_len = robot_os_drivers::csi::csi_capture_jpeg(&mut jpeg_buf);
            if jpeg_len == 0 { return 0; }
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

// ── Stubs for unimplemented subsystems ───────────────────────────────────────

pub fn sys_stub() -> i64 { -1 }
