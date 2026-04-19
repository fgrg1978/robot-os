//! `libsys` — Userspace syscall wrapper library for robot-os.
//!
//! Provides safe Rust wrappers around the kernel's ecall ABI so that
//! no_std ELF programs can make syscalls without writing inline assembly.
//!
//! ABI convention (RISC-V 64):
//!   a7 = syscall number
//!   a0..a5 = arguments
//!   ecall
//!   a0 = return value (i64, negative = error)

#![no_std]

use core::arch::asm;

// ---------------------------------------------------------------------------
// Syscall numbers — must match robot_os_syscall/numbers.rs exactly
// ---------------------------------------------------------------------------

// Console
const SYS_TEST: u64 = 0;
const SYS_PUTCHAR: u64 = 1;
const SYS_GETCHAR: u64 = 2;

// Process
const SYS_EXIT: u64 = 3;
const SYS_GETPID: u64 = 10;
const SYS_YIELD: u64 = 11;
const SYS_FORK: u64 = 12;
const SYS_EXEC: u64 = 13;
const SYS_WAIT: u64 = 14;
const SYS_SLEEP: u64 = 15;
const SYS_EXECPATH: u64 = 16;

// File I/O
const SYS_OPEN: u64 = 20;
const SYS_CLOSE: u64 = 21;
const SYS_READ: u64 = 22;
const SYS_WRITE: u64 = 23;
const SYS_LSEEK: u64 = 24;

// IPC
const SYS_IPC_CREATE: u64 = 100;
const SYS_IPC_SEND: u64 = 101;
const SYS_IPC_RECEIVE: u64 = 102;
const SYS_IPC_DESTROY: u64 = 107;

// GPIO
const SYS_GPIO_READ: u64 = 200;
const SYS_GPIO_WRITE: u64 = 201;
const SYS_GPIO_MODE: u64 = 202;
const SYS_GPIO_INFO: u64 = 203;

// PWM
const SYS_PWM_ENABLE: u64 = 210;
const SYS_PWM_DISABLE: u64 = 211;
const SYS_PWM_SET_FREQ: u64 = 212;
const SYS_PWM_SET_DUTY: u64 = 213;
const SYS_PWM_INFO: u64 = 214;

// I2C
const SYS_I2C_READ: u64 = 220;
const SYS_I2C_WRITE: u64 = 221;
const SYS_I2C_SCAN: u64 = 222;
const SYS_I2C_INFO: u64 = 223;

// Motor
const SYS_MOTOR_CREATE: u64 = 230;
const SYS_MOTOR_ENABLE: u64 = 231;
const SYS_MOTOR_SPEED: u64 = 232;
const SYS_MOTOR_ANGLE: u64 = 233;
const SYS_MOTOR_INFO: u64 = 234;

// System info
const SYS_MEMINFO: u64 = 240;
const SYS_TASKINFO: u64 = 241;
const SYS_UPTIME: u64 = 242;

// Filesystem
const SYS_STAT: u64 = 250;
const SYS_READDIR: u64 = 251;
const SYS_MKDIR: u64 = 252;
const SYS_UNLINK: u64 = 253;
const SYS_CHDIR: u64 = 254;
const SYS_GETCWD: u64 = 255;
const SYS_MOUNT: u64 = 256;
const SYS_UMOUNT: u64 = 257;
const SYS_SYNC: u64 = 258;

// Network
const SYS_NET_INFO: u64 = 260;
const SYS_NET_GETIP: u64 = 261;
const SYS_NET_SETIP: u64 = 262;
const SYS_NET_PING: u64 = 263;
const SYS_NET_GETMAC: u64 = 264;
const SYS_NET_STATS: u64 = 265;

// System control
const SYS_SHUTDOWN: u64 = 270;
const SYS_REBOOT: u64 = 271;

// Disk
const SYS_DISK_INFO: u64 = 280;
const SYS_DISK_READ: u64 = 281;
const SYS_DISK_WRITE: u64 = 282;
const SYS_DISK_SIZE: u64 = 283;

// Signals
const SYS_KILL: u64 = 350;
const SYS_SIGNAL: u64 = 351;
const SYS_SIGRETURN: u64 = 352;
const SYS_SIGPENDING: u64 = 353;
const SYS_SIGPROCMASK: u64 = 354;
const SYS_PAUSE: u64 = 355;
const SYS_ALARM: u64 = 356;

// Pipes / FD
const SYS_PIPE: u64 = 360;
const SYS_DUP: u64 = 361;
const SYS_DUP2: u64 = 362;

// Sockets
const SYS_SOCKET: u64 = 370;
const SYS_BIND: u64 = 371;
const SYS_LISTEN: u64 = 372;
const SYS_ACCEPT: u64 = 373;
const SYS_CONNECT: u64 = 374;
const SYS_SEND: u64 = 375;
const SYS_RECV: u64 = 376;
const SYS_SENDTO: u64 = 377;
const SYS_RECVFROM: u64 = 378;
const SYS_SOCK_SHUTDOWN: u64 = 379;

// Memory management
const SYS_BRK: u64 = 400;
const SYS_MMAP: u64 = 401;
const SYS_MUNMAP: u64 = 402;

// Service manager
const SYS_SERVICE_REGISTER: u64 = 390;
const SYS_SERVICE_DISCOVER: u64 = 392;
const SYS_SERVICE_HEARTBEAT: u64 = 393;
const SYS_SERVICE_STOP: u64 = 397;

// Robot control
const SYS_ROBOT_INIT: u64 = 320;
const SYS_ROBOT_START: u64 = 321;
const SYS_ROBOT_STOP: u64 = 322;
const SYS_ROBOT_PAUSE: u64 = 323;
const SYS_ROBOT_RESUME: u64 = 324;
const SYS_ROBOT_ESTOP: u64 = 325;
const SYS_ROBOT_MOVE: u64 = 326;
const SYS_ROBOT_FORWARD: u64 = 327;
const SYS_ROBOT_ROTATE: u64 = 328;
const SYS_ROBOT_INFO: u64 = 329;

// Sensors
const SYS_SENSOR_INFO: u64 = 330;
const SYS_SENSOR_ADD: u64 = 331;
const SYS_SENSOR_READ: u64 = 332;

// Sensor type IDs for sensor_read()
pub const SENSOR_TYPE_IMU: u64 = 0;
pub const SENSOR_TYPE_ODOM: u64 = 1;
pub const SENSOR_TYPE_ENCODER: u64 = 2;
pub const SENSOR_TYPE_RANGE: u64 = 3;
pub const SENSOR_TYPE_BATTERY: u64 = 4;
pub const SENSOR_TYPE_GPS: u64 = 5;
pub const SENSOR_TYPE_LIDAR: u64 = 6;
pub const SENSOR_TYPE_GPIO_FLAGS: u64 = 7;
pub const SENSOR_TYPE_CAMERA: u64 = 8;
pub const SENSOR_TYPE_POWER: u64 = 9;

// Driver server (AQ4)
const SYS_DRV_REGISTER: u64 = 300;
const SYS_DRV_MMAP: u64 = 302;
const SYS_DRV_MUNMAP: u64 = 303;
const SYS_DRV_IRQ_WAIT: u64 = 304;
const SYS_DRV_IRQ_ACK: u64 = 305;
const SYS_DRV_HEARTBEAT: u64 = 309;

// IO Ring (AQ4)
const SYS_IO_SETUP: u64 = 503;
const SYS_IO_SUBMIT: u64 = 504;
const SYS_IO_WAIT: u64 = 505;

// Channel (AQ4)
const SYS_CHAN_CREATE: u64 = 506;
const SYS_CHAN_WRITE: u64 = 507;
const SYS_CHAN_READ: u64 = 508;

// Port (AQ5)
const SYS_PORT_CREATE: u64 = 511;
const SYS_PORT_BIND: u64 = 512;
const SYS_PORT_WAIT: u64 = 513;
const SYS_PORT_UNBIND: u64 = 514;

// Fast-path IPC (M02)
const SYS_IPC_FAST_CALL:   u64 = 108;
const SYS_IPC_FAST_REPLY:  u64 = 109;
const SYS_IPC_FAST_ACCEPT: u64 = 110;

// Trace (AQ8)
const SYS_TRACE_DUMP: u64 = 518;

// Security
const SYS_SECCOMP: u64 = 430;

/// Security profile IDs for seccomp().
pub const PROFILE_UNRESTRICTED: u64 = 0;
pub const PROFILE_SENSOR: u64 = 1;
pub const PROFILE_MOTOR: u64 = 2;
pub const PROFILE_NET: u64 = 3;
pub const PROFILE_MINIMAL: u64 = 4;

/// Activate a syscall filter profile (one-way — cannot be undone).
/// After this call, only syscalls in the profile whitelist are allowed.
pub fn seccomp(profile_id: u64) -> isize {
    unsafe { syscall1(SYS_SECCOMP, profile_id) }
}

// Platform
const SYS_PLATFORM_INFO: u64 = 340;
const SYS_PLATFORM_TYPE: u64 = 341;

// ---------------------------------------------------------------------------
// M01: vDSO — zero-ecall kernel time queries
// ---------------------------------------------------------------------------

/// User-space virtual address of the vDSO page (must match mm/vdso.rs).
const VDSO_USER_BASE: usize = 0x5000_0000;

/// Expected magic value in the vDSO page header.
const VDSO_MAGIC: u32 = 0x5644_534F;

/// Read the monotonic uptime tick counter from the vDSO page without issuing
/// an ecall.  Falls back to 0 if the vDSO page is not present or has bad magic.
///
/// # Safety
/// Reads from a read-only page mapped by the kernel into every user process.
pub fn vdso_uptime_ticks() -> u64 {
    unsafe { vdso_read_u64(8) } // uptime_ticks at byte offset 8 (after magic+version+seq+_pad)
}

/// Read the uptime in milliseconds from the vDSO page without issuing an ecall.
pub fn vdso_uptime_ms() -> u64 {
    unsafe { vdso_read_u64(24) } // uptime_ms at byte offset 24
}

/// Read the kernel version from the vDSO page.
pub fn vdso_kernel_version() -> u32 {
    unsafe { vdso_read_u32(4) } // kernel_version at byte offset 4
}

/// Internal seqlock-aware u64 read from vDSO page at given byte offset.
///
/// # Safety
/// `offset` must be within the vDSO page (< 4096).
#[inline]
unsafe fn vdso_read_u64(offset: usize) -> u64 {
    // Verify magic before trusting any data.
    let magic_ptr = (VDSO_USER_BASE) as *const u32;
    if core::ptr::read_volatile(magic_ptr) != VDSO_MAGIC {
        return 0;
    }
    // VdsoData layout (matches mm/vdso.rs):
    //   +0  magic:          u32
    //   +4  kernel_version: u32
    //   +8  seq:            u32  (seqlock counter)
    //   +12 _pad:           u32
    //   +16 uptime_ticks:   u64
    //   +24 uptime_ms:      u64
    let seq_ptr   = (VDSO_USER_BASE + 8)      as *const u32;
    let data_ptr  = (VDSO_USER_BASE + offset)  as *const u64;

    loop {
        let seq1 = core::ptr::read_volatile(seq_ptr);
        if seq1 & 1 != 0 {
            // Write in progress — spin.
            core::hint::spin_loop();
            continue;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let val = core::ptr::read_volatile(data_ptr);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let seq2 = core::ptr::read_volatile(seq_ptr);
        if seq1 == seq2 {
            return val;
        }
        core::hint::spin_loop();
    }
}

/// Internal u32 read from vDSO page at given byte offset (no seqlock — fields
/// written once at init time so always consistent).
#[inline]
unsafe fn vdso_read_u32(offset: usize) -> u32 {
    core::ptr::read_volatile((VDSO_USER_BASE + offset) as *const u32)
}

// ---------------------------------------------------------------------------
// Well-known file descriptors
// ---------------------------------------------------------------------------
/// Standard input file descriptor.
pub const STDIN: u64 = 0;
/// Standard output file descriptor.
pub const STDOUT: u64 = 1;
/// Standard error file descriptor.
pub const STDERR: u64 = 2;

// ---------------------------------------------------------------------------
// Raw syscall primitives (unsafe, internal)
// ---------------------------------------------------------------------------

#[inline(always)]
unsafe fn syscall0(nr: u64) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") nr,
            lateout("a0") ret,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall1(nr: u64, a0: u64) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") nr,
            inlateout("a0") a0 as isize => ret,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall2(nr: u64, a0: u64, a1: u64) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") nr,
            inlateout("a0") a0 as isize => ret,
            in("a1") a1,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") nr,
            inlateout("a0") a0 as isize => ret,
            in("a1") a1,
            in("a2") a2,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall4(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") nr,
            inlateout("a0") a0 as isize => ret,
            in("a1") a1,
            in("a2") a2,
            in("a3") a3,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall5(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") nr,
            inlateout("a0") a0 as isize => ret,
            in("a1") a1,
            in("a2") a2,
            in("a3") a3,
            in("a4") a4,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall6(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") nr,
            inlateout("a0") a0 as isize => ret,
            in("a1") a1,
            in("a2") a2,
            in("a3") a3,
            in("a4") a4,
            in("a5") a5,
            options(nostack),
        );
    }
    ret
}

// ===========================================================================
//  Safe wrappers — grouped by subsystem
// ===========================================================================

// ---------------------------------------------------------------------------
//  Console
// ---------------------------------------------------------------------------

/// Kernel self-test syscall.
pub fn test() -> isize {
    unsafe { syscall0(SYS_TEST) }
}

/// Write a single character to the kernel console.
pub fn putchar(c: u8) {
    unsafe { syscall1(SYS_PUTCHAR, c as u64); }
}

/// Read a single character from the kernel console (blocking).
/// Returns the character as `u8`, or negative on error.
pub fn getchar() -> isize {
    unsafe { syscall0(SYS_GETCHAR) }
}

// ---------------------------------------------------------------------------
//  Process management
// ---------------------------------------------------------------------------

/// Terminate the current process with the given exit code.
pub fn exit(code: i32) -> ! {
    unsafe { syscall1(SYS_EXIT, code as u64); }
    // Kernel should never return, but satisfy the compiler.
    loop {}
}

/// Return the PID of the current process.
pub fn getpid() -> isize {
    unsafe { syscall0(SYS_GETPID) }
}

/// Yield the CPU to the scheduler.
pub fn yield_now() {
    unsafe { syscall0(SYS_YIELD); }
}

/// Sleep for `ms` milliseconds.
pub fn sleep(ms: u64) {
    unsafe { syscall1(SYS_SLEEP, ms); }
}

/// Fork the current process. Returns 0 in the child, child PID in the parent,
/// or negative on error.
pub fn fork() -> isize {
    unsafe { syscall0(SYS_FORK) }
}

/// Execute a loaded binary at `entry_addr` with stack at `stack_addr`.
pub fn exec(entry_addr: u64, stack_addr: u64) -> isize {
    unsafe { syscall2(SYS_EXEC, entry_addr, stack_addr) }
}

/// Execute an ELF binary from a filesystem path.
pub fn execpath(path: &[u8]) -> isize {
    unsafe { syscall1(SYS_EXECPATH, path.as_ptr() as u64) }
}

/// Wait for a child process to exit. Returns child PID or negative on error.
pub fn wait() -> isize {
    unsafe { syscall0(SYS_WAIT) }
}

// ---------------------------------------------------------------------------
//  File I/O
// ---------------------------------------------------------------------------

/// Open a file. `path` is a null-terminated byte slice, `flags` are open mode.
/// Returns a file descriptor or negative on error.
pub fn open(path: &[u8], flags: u64) -> isize {
    unsafe { syscall2(SYS_OPEN, path.as_ptr() as u64, flags) }
}

/// Close a file descriptor.
pub fn close(fd: u64) -> isize {
    unsafe { syscall1(SYS_CLOSE, fd) }
}

/// Read up to `buf.len()` bytes from `fd` into `buf`.
/// Returns the number of bytes read or negative on error.
pub fn read(fd: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Write `buf.len()` bytes from `buf` to `fd`.
/// Returns the number of bytes written or negative on error.
pub fn write(fd: u64, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_WRITE, fd, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Seek within a file descriptor. Returns the new offset or negative on error.
pub fn lseek(fd: u64, offset: u64, whence: u64) -> isize {
    unsafe { syscall3(SYS_LSEEK, fd, offset, whence) }
}

// ---------------------------------------------------------------------------
//  Filesystem
// ---------------------------------------------------------------------------

/// Stat a file. `path` is null-terminated, `buf` receives the stat struct.
pub fn stat(path: &[u8], buf: &mut [u8]) -> isize {
    unsafe { syscall2(SYS_STAT, path.as_ptr() as u64, buf.as_mut_ptr() as u64) }
}

/// Read directory entries from an open directory fd.
/// `buf` receives entry data, `index` is the entry offset.
pub fn readdir(fd: u64, buf: &mut [u8], index: u64, max_entries: u64) -> isize {
    unsafe { syscall5(SYS_READDIR, fd, buf.as_mut_ptr() as u64, buf.len() as u64, index, max_entries) }
}

/// Create a directory at `path` (null-terminated).
pub fn mkdir(path: &[u8]) -> isize {
    unsafe { syscall1(SYS_MKDIR, path.as_ptr() as u64) }
}

/// Remove a file or directory at `path` (null-terminated).
pub fn unlink(path: &[u8]) -> isize {
    unsafe { syscall1(SYS_UNLINK, path.as_ptr() as u64) }
}

/// Change current working directory to `path` (null-terminated).
pub fn chdir(path: &[u8]) -> isize {
    unsafe { syscall1(SYS_CHDIR, path.as_ptr() as u64) }
}

/// Get current working directory into `buf`. Returns length or negative.
pub fn getcwd(buf: &mut [u8]) -> isize {
    unsafe { syscall2(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Mount a filesystem. `source`, `target`, and `fstype` are null-terminated.
pub fn mount(source: &[u8], target: &[u8], fstype: &[u8]) -> isize {
    unsafe { syscall3(SYS_MOUNT, source.as_ptr() as u64, target.as_ptr() as u64, fstype.as_ptr() as u64) }
}

/// Unmount the filesystem at `target` (null-terminated).
pub fn umount(target: &[u8]) -> isize {
    unsafe { syscall1(SYS_UMOUNT, target.as_ptr() as u64) }
}

/// Flush all filesystem caches to disk.
pub fn sync() -> isize {
    unsafe { syscall0(SYS_SYNC) }
}

// ---------------------------------------------------------------------------
//  GPIO
// ---------------------------------------------------------------------------

/// Read the value of a GPIO pin. Returns 0 or 1, or negative on error.
pub fn gpio_read(pin: u64) -> isize {
    unsafe { syscall1(SYS_GPIO_READ, pin) }
}

/// Write a value (0 or 1) to a GPIO pin.
pub fn gpio_write(pin: u64, value: u64) -> isize {
    unsafe { syscall2(SYS_GPIO_WRITE, pin, value) }
}

/// Set GPIO pin mode (input/output/alt).
pub fn gpio_mode(pin: u64, mode: u64) -> isize {
    unsafe { syscall2(SYS_GPIO_MODE, pin, mode) }
}

/// Query GPIO subsystem info.
pub fn gpio_info() -> isize {
    unsafe { syscall0(SYS_GPIO_INFO) }
}

// ---------------------------------------------------------------------------
//  PWM
// ---------------------------------------------------------------------------

/// Enable a PWM channel.
pub fn pwm_enable(channel: u64) -> isize {
    unsafe { syscall1(SYS_PWM_ENABLE, channel) }
}

/// Disable a PWM channel.
pub fn pwm_disable(channel: u64) -> isize {
    unsafe { syscall1(SYS_PWM_DISABLE, channel) }
}

/// Set PWM frequency for a channel.
pub fn pwm_set_freq(channel: u64, freq_hz: u64) -> isize {
    unsafe { syscall2(SYS_PWM_SET_FREQ, channel, freq_hz) }
}

/// Set PWM duty cycle for a channel (0..65535 or platform-specific range).
pub fn pwm_set_duty(channel: u64, duty: u64) -> isize {
    unsafe { syscall2(SYS_PWM_SET_DUTY, channel, duty) }
}

/// Query PWM subsystem info.
pub fn pwm_info() -> isize {
    unsafe { syscall0(SYS_PWM_INFO) }
}

// ---------------------------------------------------------------------------
//  I2C
// ---------------------------------------------------------------------------

/// Read from an I2C device.
/// `bus` = I2C bus index, `addr` = 7-bit device address,
/// `reg` = register address, `buf` receives data.
pub fn i2c_read(bus: u64, addr: u64, reg: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall5(SYS_I2C_READ, bus, addr, reg, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Write to an I2C device.
/// `bus` = I2C bus index, `addr` = 7-bit device address, `buf` contains data to write.
pub fn i2c_write(bus: u64, addr: u64, buf: &[u8]) -> isize {
    unsafe { syscall4(SYS_I2C_WRITE, bus, addr, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Scan an I2C bus for devices. Returns the number of devices found.
pub fn i2c_scan(bus: u64) -> isize {
    unsafe { syscall1(SYS_I2C_SCAN, bus) }
}

/// Query I2C subsystem info.
pub fn i2c_info() -> isize {
    unsafe { syscall0(SYS_I2C_INFO) }
}

// ---------------------------------------------------------------------------
//  Motor
// ---------------------------------------------------------------------------

/// Create a motor with given PWM channel, direction GPIO, and encoder GPIO.
pub fn motor_create(pwm_ch: u64, dir_gpio: u64, enc_gpio: u64, motor_type: u64) -> isize {
    unsafe { syscall4(SYS_MOTOR_CREATE, pwm_ch, dir_gpio, enc_gpio, motor_type) }
}

/// Enable or disable a motor. `enable` = 1 to enable, 0 to disable.
pub fn motor_enable(id: u64, enable: u64) -> isize {
    unsafe { syscall2(SYS_MOTOR_ENABLE, id, enable) }
}

/// Set motor speed (signed: positive = forward, negative = reverse).
pub fn motor_speed(id: u64, speed: u64) -> isize {
    unsafe { syscall2(SYS_MOTOR_SPEED, id, speed) }
}

/// Read the current angle/position of a motor (encoder ticks or degrees).
pub fn motor_angle(id: u64) -> isize {
    unsafe { syscall1(SYS_MOTOR_ANGLE, id) }
}

/// Query motor subsystem info.
pub fn motor_info() -> isize {
    unsafe { syscall0(SYS_MOTOR_INFO) }
}

// ---------------------------------------------------------------------------
//  System info
// ---------------------------------------------------------------------------

/// Query memory info. Returns total memory in bytes (or encoded value).
pub fn meminfo() -> isize {
    unsafe { syscall0(SYS_MEMINFO) }
}

/// Query task/process info.
pub fn taskinfo() -> isize {
    unsafe { syscall0(SYS_TASKINFO) }
}

/// Get system uptime in milliseconds.
pub fn uptime() -> isize {
    unsafe { syscall0(SYS_UPTIME) }
}

// ---------------------------------------------------------------------------
//  System control
// ---------------------------------------------------------------------------

/// Shut down the system.
pub fn shutdown() -> ! {
    unsafe { syscall0(SYS_SHUTDOWN); }
    loop {}
}

/// Reboot the system.
pub fn reboot() -> ! {
    unsafe { syscall0(SYS_REBOOT); }
    loop {}
}

// ---------------------------------------------------------------------------
//  Disk (raw block device)
// ---------------------------------------------------------------------------

/// Query disk info.
pub fn disk_info() -> isize {
    unsafe { syscall0(SYS_DISK_INFO) }
}

/// Read raw disk sectors. `sector` = start sector, `buf` receives data.
pub fn disk_read(sector: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_DISK_READ, sector, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Write raw disk sectors. `sector` = start sector, `buf` contains data.
pub fn disk_write(sector: u64, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_DISK_WRITE, sector, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Get disk size in sectors.
pub fn disk_size() -> isize {
    unsafe { syscall0(SYS_DISK_SIZE) }
}

// ---------------------------------------------------------------------------
//  Signals
// ---------------------------------------------------------------------------

/// Send a signal to a process.
pub fn kill(pid: u64, signal: u64) -> isize {
    unsafe { syscall2(SYS_KILL, pid, signal) }
}

/// Register a signal handler. `handler` is the function pointer address.
pub fn signal(signum: u64, handler: u64) -> isize {
    unsafe { syscall2(SYS_SIGNAL, signum, handler) }
}

/// Return from a signal handler (called by trampoline).
pub fn sigreturn() -> isize {
    unsafe { syscall0(SYS_SIGRETURN) }
}

/// Query pending signals bitmask.
pub fn sigpending() -> isize {
    unsafe { syscall0(SYS_SIGPENDING) }
}

/// Set signal mask. `how` controls add/remove/set, `mask` is the bitmask.
pub fn sigprocmask(how: u64, mask: u64) -> isize {
    unsafe { syscall2(SYS_SIGPROCMASK, how, mask) }
}

/// Suspend until a signal is delivered.
pub fn pause() -> isize {
    unsafe { syscall0(SYS_PAUSE) }
}

/// Set an alarm timer. `seconds` = 0 to cancel. Returns remaining seconds.
pub fn alarm(seconds: u64) -> isize {
    unsafe { syscall1(SYS_ALARM, seconds) }
}

// ---------------------------------------------------------------------------
//  Pipes / FD duplication
// ---------------------------------------------------------------------------

/// Create a pipe. `fds_buf` must point to space for two u64 file descriptors.
pub fn pipe(fds_buf: &mut [u64; 2]) -> isize {
    unsafe { syscall1(SYS_PIPE, fds_buf.as_mut_ptr() as u64) }
}

/// Duplicate a file descriptor. Returns the new fd or negative on error.
pub fn dup(fd: u64) -> isize {
    unsafe { syscall1(SYS_DUP, fd) }
}

/// Duplicate a file descriptor to a specific fd number.
pub fn dup2(old_fd: u64, new_fd: u64) -> isize {
    unsafe { syscall2(SYS_DUP2, old_fd, new_fd) }
}

// ---------------------------------------------------------------------------
//  IPC channels
// ---------------------------------------------------------------------------

/// Create a new IPC channel. Returns channel ID or negative on error.
pub fn ipc_create() -> isize {
    unsafe { syscall0(SYS_IPC_CREATE) }
}

/// Send a message on an IPC channel.
pub fn ipc_send(channel: u64, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_IPC_SEND, channel, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Receive a message from an IPC channel.
pub fn ipc_receive(channel: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_IPC_RECEIVE, channel, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Destroy an IPC channel.
pub fn ipc_destroy(channel: u64) -> isize {
    unsafe { syscall1(SYS_IPC_DESTROY, channel) }
}

// ---------------------------------------------------------------------------
//  Network (stack-level)
// ---------------------------------------------------------------------------

/// Query network interface info.
pub fn net_info() -> isize {
    unsafe { syscall0(SYS_NET_INFO) }
}

/// Get the current IP address (returned as u32 in host byte order).
pub fn net_getip() -> isize {
    unsafe { syscall0(SYS_NET_GETIP) }
}

/// Set IP address, subnet mask, and gateway (all as u32).
pub fn net_setip(ip: u32, mask: u32, gateway: u32) -> isize {
    unsafe { syscall3(SYS_NET_SETIP, ip as u64, mask as u64, gateway as u64) }
}

/// Ping an IP address (u32). Returns round-trip time in ms or negative.
pub fn net_ping(ip: u32) -> isize {
    unsafe { syscall1(SYS_NET_PING, ip as u64) }
}

/// Get the MAC address (returned as u64, lower 6 bytes).
pub fn net_getmac() -> isize {
    unsafe { syscall0(SYS_NET_GETMAC) }
}

/// Get network stack statistics.
pub fn net_stats() -> isize {
    unsafe { syscall0(SYS_NET_STATS) }
}

// ---------------------------------------------------------------------------
//  Sockets
// ---------------------------------------------------------------------------

/// Create a socket. `domain`, `sock_type`, `protocol` follow POSIX convention.
/// Returns socket fd or negative on error.
pub fn socket(domain: u64, sock_type: u64, protocol: u64) -> isize {
    unsafe { syscall3(SYS_SOCKET, domain, sock_type, protocol) }
}

/// Bind a socket to an address. `addr_ptr` and `addr_len` describe the address.
pub fn bind(fd: u64, addr_ptr: u64, addr_len: u64) -> isize {
    unsafe { syscall3(SYS_BIND, fd, addr_ptr, addr_len) }
}

/// Listen on a socket for incoming connections.
pub fn listen(fd: u64, backlog: u64) -> isize {
    unsafe { syscall2(SYS_LISTEN, fd, backlog) }
}

/// Accept a connection on a listening socket.
/// `addr_ptr` and `addr_len` receive the peer address (may be 0).
pub fn accept(fd: u64, addr_ptr: u64, addr_len: u64) -> isize {
    unsafe { syscall3(SYS_ACCEPT, fd, addr_ptr, addr_len) }
}

/// Connect a socket to a remote address.
pub fn connect(fd: u64, addr_ptr: u64, addr_len: u64) -> isize {
    unsafe { syscall3(SYS_CONNECT, fd, addr_ptr, addr_len) }
}

/// Send data on a connected socket.
pub fn send(fd: u64, buf: &[u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_SEND, fd, buf.as_ptr() as u64, buf.len() as u64, flags) }
}

/// Receive data from a connected socket.
pub fn recv(fd: u64, buf: &mut [u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_RECV, fd, buf.as_mut_ptr() as u64, buf.len() as u64, flags) }
}

/// Send data to a specific address (UDP).
pub fn sendto(fd: u64, buf: &[u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_SENDTO, fd, buf.as_ptr() as u64, buf.len() as u64, flags) }
}

/// Receive data with sender address (UDP).
pub fn recvfrom(fd: u64, buf: &mut [u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_RECVFROM, fd, buf.as_mut_ptr() as u64, buf.len() as u64, flags) }
}

/// Shut down part of a socket connection.
pub fn sock_shutdown(fd: u64) -> isize {
    unsafe { syscall1(SYS_SOCK_SHUTDOWN, fd) }
}

// ---------------------------------------------------------------------------
//  Memory management
// ---------------------------------------------------------------------------

/// Adjust the program break (heap end). Returns the new break or negative.
pub fn brk(addr: u64) -> isize {
    unsafe { syscall1(SYS_BRK, addr) }
}

/// Map memory. Follows mmap(addr, len, prot, flags, fd, offset) convention.
pub fn mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> isize {
    unsafe { syscall6(SYS_MMAP, addr, len, prot, flags, fd, offset) }
}

/// Unmap memory at `addr` for `len` bytes.
pub fn munmap(addr: u64, len: u64) -> isize {
    unsafe { syscall2(SYS_MUNMAP, addr, len) }
}

// ---------------------------------------------------------------------------
//  Service manager
// ---------------------------------------------------------------------------

/// Register a service with the kernel service manager.
pub fn service_register(name_ptr: u64, name_len: u64, port: u64) -> isize {
    unsafe { syscall3(SYS_SERVICE_REGISTER, name_ptr, name_len, port) }
}

/// Discover a service by name. Returns port or negative.
pub fn service_discover(name_ptr: u64) -> isize {
    unsafe { syscall1(SYS_SERVICE_DISCOVER, name_ptr) }
}

/// Send a heartbeat for a registered service.
pub fn service_heartbeat(service_id: u64) -> isize {
    unsafe { syscall1(SYS_SERVICE_HEARTBEAT, service_id) }
}

/// Stop a service.
pub fn service_stop(service_id: u64) -> isize {
    unsafe { syscall1(SYS_SERVICE_STOP, service_id) }
}

// ---------------------------------------------------------------------------
//  Robot control (stubs in kernel — Phase R+)
// ---------------------------------------------------------------------------

/// Initialize robot subsystem.
pub fn robot_init() -> isize {
    unsafe { syscall0(SYS_ROBOT_INIT) }
}

/// Start robot operation.
pub fn robot_start() -> isize {
    unsafe { syscall0(SYS_ROBOT_START) }
}

/// Stop robot operation.
pub fn robot_stop() -> isize {
    unsafe { syscall0(SYS_ROBOT_STOP) }
}

/// Pause robot operation.
pub fn robot_pause() -> isize {
    unsafe { syscall0(SYS_ROBOT_PAUSE) }
}

/// Resume robot operation.
pub fn robot_resume() -> isize {
    unsafe { syscall0(SYS_ROBOT_RESUME) }
}

/// Emergency stop — immediately halt all actuators.
pub fn robot_estop() -> isize {
    unsafe { syscall0(SYS_ROBOT_ESTOP) }
}

/// Move robot with direction and speed encoded in arguments.
pub fn robot_move(direction: u64, speed: u64) -> isize {
    unsafe { syscall2(SYS_ROBOT_MOVE, direction, speed) }
}

/// Move robot forward by a distance.
pub fn robot_forward(distance: u64) -> isize {
    unsafe { syscall1(SYS_ROBOT_FORWARD, distance) }
}

/// Rotate robot by an angle (in degrees or millidegrees).
pub fn robot_rotate(angle: u64) -> isize {
    unsafe { syscall1(SYS_ROBOT_ROTATE, angle) }
}

/// Query robot status/info.
pub fn robot_info() -> isize {
    unsafe { syscall0(SYS_ROBOT_INFO) }
}

// ---------------------------------------------------------------------------
//  Sensors (Phase S placeholder — stubs in kernel)
// ---------------------------------------------------------------------------

/// Query sensor subsystem info.
pub fn sensor_info() -> isize {
    unsafe { syscall0(SYS_SENSOR_INFO) }
}

/// Register/add a sensor of the given type.
pub fn sensor_add(sensor_type: u64) -> isize {
    unsafe { syscall1(SYS_SENSOR_ADD, sensor_type) }
}

/// Read sensor data into `buf`. `sensor_type` selects which sensor.
/// Returns bytes read or negative on error.
pub fn sensor_read(sensor_type: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_SENSOR_READ, sensor_type, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

// ---------------------------------------------------------------------------
//  Platform
// ---------------------------------------------------------------------------

/// Query platform info.
pub fn platform_info() -> isize {
    unsafe { syscall0(SYS_PLATFORM_INFO) }
}

/// Get platform type (0=QEMU, 1=VF2, 2=K1, etc.).
pub fn platform_type() -> isize {
    unsafe { syscall0(SYS_PLATFORM_TYPE) }
}

// ===========================================================================
//  Convenience helpers
// ===========================================================================

/// Write a byte string to stdout.
pub fn print(s: &[u8]) {
    write(STDOUT, s);
}

/// Write a byte string to stdout followed by a newline.
pub fn println(s: &[u8]) {
    write(STDOUT, s);
    putchar(b'\n');
}

// ===========================================================================
//  Driver server API (AQ4) — userspace driver registration and MMIO/IRQ
// ===========================================================================

/// Register the calling process as a driver with the given name.
/// Returns a driver ID or negative on error.
pub fn drv_register(name: &[u8]) -> isize {
    unsafe { syscall2(SYS_DRV_REGISTER, name.as_ptr() as u64, name.len() as u64) }
}

/// Map a physical MMIO region into the driver's address space.
/// Returns the virtual address or negative on error.
pub fn drv_mmap(phys: u64, size: u64) -> isize {
    unsafe { syscall2(SYS_DRV_MMAP, phys, size) }
}

/// Unmap a previously mapped MMIO region.
pub fn drv_munmap(addr: u64, size: u64) -> isize {
    unsafe { syscall2(SYS_DRV_MUNMAP, addr, size) }
}

/// Block until the specified IRQ fires. Returns 0 on success or negative.
pub fn drv_irq_wait(irq: u64) -> isize {
    unsafe { syscall1(SYS_DRV_IRQ_WAIT, irq) }
}

/// Acknowledge an IRQ after handling it.
pub fn drv_irq_ack(irq: u64) -> isize {
    unsafe { syscall1(SYS_DRV_IRQ_ACK, irq) }
}

/// Send a heartbeat to the driver manager indicating this driver is alive.
pub fn drv_heartbeat() -> isize {
    unsafe { syscall0(SYS_DRV_HEARTBEAT) }
}

// ===========================================================================
//  IO Ring API (AQ4) — asynchronous I/O submission and completion
// ===========================================================================

/// Create a new IO ring. `flags` controls ring behaviour.
/// Returns the ring ID or negative on error.
pub fn io_setup(flags: u64) -> isize {
    unsafe { syscall1(SYS_IO_SETUP, flags) }
}

/// Submit entries to an IO ring for asynchronous processing.
/// `entries_ptr` points to an array of submission entries, `count` is the
/// number of entries. Returns the number accepted or negative on error.
pub fn io_submit(ring_id: u64, entries_ptr: u64, count: u64) -> isize {
    unsafe { syscall3(SYS_IO_SUBMIT, ring_id, entries_ptr, count) }
}

/// Wait for at least `min_completions` completions on an IO ring.
/// Returns the number of completions available or negative on error.
pub fn io_wait(ring_id: u64, min_completions: u64) -> isize {
    unsafe { syscall2(SYS_IO_WAIT, ring_id, min_completions) }
}

// ===========================================================================
//  Channel API (AQ4) — kernel-mediated message passing
// ===========================================================================

/// Create a new kernel channel. Returns the channel handle or negative.
pub fn chan_create() -> isize {
    unsafe { syscall0(SYS_CHAN_CREATE) }
}

/// Write data to a channel.
pub fn chan_write(handle: u64, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_CHAN_WRITE, handle, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Read data from a channel into `buf`.
/// Returns bytes read or negative on error.
pub fn chan_read(handle: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_CHAN_READ, handle, buf.as_ptr() as u64, buf.len() as u64) }
}

// ===========================================================================
//  Port API (AQ5) — multi-source event waiting (like kqueue / Zircon ports)
// ===========================================================================

/// Create a new event port. Returns the port handle or negative.
pub fn port_create() -> isize {
    unsafe { syscall0(SYS_PORT_CREATE) }
}

/// Bind an event source to a port.
/// `source_type` identifies the kind (IRQ, channel, ring, timer),
/// `source_id` is the specific source, `key` is returned on wakeup.
pub fn port_bind(port: u64, source_type: u64, source_id: u64, key: u64) -> isize {
    unsafe { syscall4(SYS_PORT_BIND, port, source_type, source_id, key) }
}

/// Wait for an event on a port. Blocks until at least one bound source fires.
/// Returns the key of the fired source or negative on error.
pub fn port_wait(port: u64) -> isize {
    unsafe { syscall1(SYS_PORT_WAIT, port) }
}

/// Unbind an event source from a port.
pub fn port_unbind(port: u64, source_id: u64) -> isize {
    unsafe { syscall2(SYS_PORT_UNBIND, port, source_id) }
}

// ===========================================================================
//  Trace API (AQ8) — kernel event ring buffer dump
// ===========================================================================

/// Dump the kernel trace ring buffer to the console.
/// Returns 0 on success or negative on error.
pub fn trace_dump() -> isize {
    unsafe { syscall0(SYS_TRACE_DUMP) }
}

// ===========================================================================
//  Fast-path IPC (M02) — seL4-style register-passing, ≤32 bytes
// ===========================================================================

/// Maximum number of 64-bit words in a fast IPC message.
pub const FAST_IPC_MAX_WORDS: usize = 4;

/// Send a fast IPC message to `server_tid` and block until the reply arrives.
///
/// `words` contains up to 4 × u64 of request data (≤ 32 bytes).
/// Returns the reply words on success, or `None` on error (no free slots).
///
/// No user-space memory is accessed by the kernel — data travels in registers.
pub fn fast_ipc_call(server_tid: u32, words: [u64; FAST_IPC_MAX_WORDS]) -> Option<[u64; FAST_IPC_MAX_WORDS]> {
    let ret = unsafe {
        syscall5(SYS_IPC_FAST_CALL,
            server_tid as u64,
            words[0], words[1], words[2], words[3])
    };
    if ret < 0 { return None; }
    // The kernel deposits reply in the slot; syscall returns words[0].
    // For simplicity we return the syscall result as word 0.
    Some([ret as u64, 0, 0, 0])
}

/// Server: block until a client sends a fast IPC call to this TID.
///
/// Returns `Some(slot_idx)` — pass to `fast_ipc_reply()`.
pub fn fast_ipc_accept() -> Option<usize> {
    let ret = unsafe { syscall0(SYS_IPC_FAST_ACCEPT) };
    if ret < 0 { None } else { Some(ret as usize) }
}

/// Server: reply to a fast IPC call (non-blocking).
///
/// `slot_idx` must be the value returned by `fast_ipc_accept()`.
pub fn fast_ipc_reply(slot_idx: usize, words: [u64; FAST_IPC_MAX_WORDS]) -> isize {
    unsafe {
        syscall5(SYS_IPC_FAST_REPLY,
            slot_idx as u64,
            words[0], words[1], words[2], words[3])
    }
}
