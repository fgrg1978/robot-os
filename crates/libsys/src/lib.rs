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

// RFC-0002 Driver registry bridge (A5).
const SYS_DRV_INVOKE: u64 = 311;

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

// RFC-0002 driver-server family (E11.AQ3) — a userspace process serves a
// driver `kind` for the in-kernel UserDriverProxy. Distinct from the
// SYS_DRV_* (300) MMIO-ownership family above.
const SYS_DRIVER_REGISTER: u64 = 520;
const SYS_DRIVER_FETCH_REQ: u64 = 523;
const SYS_DRIVER_REPLY: u64 = 524;

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

// Untyped shared memory (F00.4). `SYS_IPC_MAP` is gated to the region's
// creator (`dispatch.rs`, SYS_IPC_MAP arm) — cross-task sharing is the
// typed `Cap<Shm>` path's job, not this one.
const SYS_IPC_SHARE:   u64 = 105;
const SYS_IPC_UNSHARE: u64 = 106;
const SYS_IPC_MAP:     u64 = 115;
// Cross-task cap delegation (frozen in crates/abi/src/syscall_nr.rs).
const SYS_CAP_GRANT:   u64 = 116;

// IO ring: async completion signal (AQ4 follow-up).
const SYS_IO_SUBMIT_ASYNC: u64 = 519;

// Cap<T> typed IPC — RFC-0003 W3+ (numbers frozen in
// `crates/abi/src/syscall_nr.rs`, mirrored in `crates/syscall/src/numbers.rs`;
// both files were diffed against this list).
const SYS_CHAN_WRITE_TYPED: u64 = 528;
const SYS_CHAN_READ_TYPED:  u64 = 529;
const SYS_PORT_CREATE_TYPED:  u64 = 530;
const SYS_PORT_POLL_TYPED:    u64 = 531;
const SYS_PORT_DESTROY_TYPED: u64 = 532;
const SYS_SHM_CREATE_TYPED:  u64 = 533;
const SYS_SHM_ACQUIRE_TYPED: u64 = 534;
const SYS_SHM_RELEASE_TYPED: u64 = 535;
const SYS_IORING_CREATE_TYPED:  u64 = 536;
const SYS_IORING_SUBMIT_TYPED:  u64 = 537;
const SYS_IORING_DESTROY_TYPED: u64 = 538;
const SYS_GPIO_READ_TYPED:    u64 = 539;
const SYS_GPIO_WRITE_TYPED:   u64 = 540;
const SYS_GPIO_SET_DIR_TYPED: u64 = 541;
const SYS_I2C_READ_TYPED:   u64 = 542;
const SYS_I2C_WRITE_TYPED:  u64 = 543;
const SYS_I2C_DETECT_TYPED: u64 = 544;
const SYS_PWM_ENABLE_TYPED:       u64 = 545;
const SYS_PWM_DISABLE_TYPED:      u64 = 546;
const SYS_PWM_SET_PERIOD_TYPED:   u64 = 547;
const SYS_PWM_SET_DUTY_TYPED:     u64 = 548;
const SYS_PWM_SET_DUTY_PCT_TYPED: u64 = 549;
const SYS_MOTOR_SET_TARGET_TYPED: u64 = 550;
const SYS_MOTOR_TICK_TYPED:       u64 = 551;
const SYS_MOTOR_ENABLE_TYPED:     u64 = 552;
const SYS_MOTOR_ENABLED_TYPED:    u64 = 553;
const SYS_MOTOR_SET_GAINS_TYPED:  u64 = 554;
const SYS_MOTOR_RESET_TYPED:      u64 = 555;

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
    // uptime_ticks is at byte offset 16 (magic u32 + kernel_version u32 +
    // seq u32 + _pad u32). Offset 8 is `seq` itself: reading it returned the
    // seqlock counter, which advances by 2 per publish and so still looks
    // like a plausible monotonic tick counter — which is why this went
    // unnoticed. See the layout comment in `vdso_read_u64` below.
    unsafe { vdso_read_u64(16) }
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
// Error codes returned by the kernel
// ---------------------------------------------------------------------------
//
// WHY these are here: the kernel has TWO "denied" codes and they are not
// interchangeable, so a caller that hard-codes one will misread the other.
//   * `crates/syscall/src/handlers.rs:56`  — `const E_PERM: i64 = -99`,
//     returned by every `cap_check` failure inside a handler (gpio, i2c,
//     pwm, motor).
//   * `crates/syscall/src/dispatch.rs:13`  — `const E_PERM: i64 = -1`,
//     returned by the seccomp filter and by the capability checks written
//     directly in the dispatch arms (SYS_DRV_MMAP, SYS_MMIO_MAP,
//     SYS_IRQ_BIND, SYS_IPC_MAP, port/ring ownership).
// Test only for `rc < 0` unless you have checked which of the two applies to
// the specific syscall number.

/// Capability denied by a handler-side `cap_check`
/// (`crates/syscall/src/handlers.rs`).
pub const E_PERM_HANDLER: isize = -99;

/// Denied by the dispatcher: seccomp filter, or a capability check written
/// in the dispatch arm itself (`crates/syscall/src/dispatch.rs`).
pub const E_PERM_DISPATCH: isize = -1;

/// Invalid argument, produced *by this library* before the ecall is issued.
/// The kernel never returns this value on its own — every kernel rejection in
/// this tree is `-1` or `-99`. See [`has_nul`].
pub const E_INVAL: isize = -22;

// ---------------------------------------------------------------------------
// NUL-terminated string safety
// ---------------------------------------------------------------------------

/// Build a `&'static [u8; N+1]` from a byte-string literal, with the NUL
/// terminator appended **at compile time**.
///
/// WHY this exists: every path-taking syscall in this kernel is read with
/// `robot_os_sched::copy_cstr_from_user` (`crates/sched/src/process.rs:509`),
/// which scans forward from the pointer until it finds a zero byte — the
/// slice length this library passes is never seen by the kernel. A caller
/// writing `sys::open(b"/fat/CONFIG.INI", 0)` therefore hands the kernel a
/// pointer into `.rodata` and lets it walk past the literal into whatever the
/// linker placed next. That is exactly the bug that was found in
/// `userspace/brain_client` on 2026-08-21.
///
/// `cstr!` removes the possibility instead of documenting it:
///
/// ```ignore
/// let fd = sys::open(sys::cstr!(b"/fat/CONFIG.INI"), 0);
/// ```
///
/// An embedded NUL is a *compile* error (a `const` panic is evaluated at
/// build time and never reaches the running image, so this is safe under
/// `panic = "abort"`).
///
/// Zero runtime cost: the terminated array is a `const` item in `.rodata`.
#[macro_export]
macro_rules! cstr {
    ($lit:literal) => {{
        const SRC: &[u8] = $lit;
        const N: usize = SRC.len() + 1;
        const BUF: [u8; N] = {
            let mut out = [0u8; N];
            let mut i = 0;
            while i < SRC.len() {
                // A NUL inside the literal would silently truncate the path
                // the kernel actually opens. Refuse at build time.
                assert!(SRC[i] != 0, "cstr!: embedded NUL in literal");
                out[i] = SRC[i];
                i += 1;
            }
            out
        };
        &BUF
    }};
}

/// Does `s` contain a NUL byte anywhere?
///
/// This is the exact predicate the kernel's `copy_cstr_from_user` needs to
/// terminate: it stops at the FIRST zero byte, so the contract is *contains*
/// a NUL, not *ends with* one. A 256-byte scratch buffer holding
/// `b"/fat/X\0"` plus trailing slack is legal and must not be rejected.
///
/// Cost: O(len), one byte-compare per byte, on paths bounded by the kernel's
/// `SYS_PATH_MAX` (256). Every caller is a filesystem or service-registry
/// syscall — none is on a control-loop hot path. Nothing in the hot path
/// (`sensor_read`, `motor_speed`, `write`) calls this.
#[inline]
pub fn has_nul(s: &[u8]) -> bool {
    let mut i = 0;
    while i < s.len() {
        if s[i] == 0 {
            return true;
        }
        i += 1;
    }
    false
}

/// Build a `sockaddr_in` in the exact 16-byte layout
/// `read_sockaddr` parses (`crates/syscall/src/handlers.rs:789`):
/// `family(u16 LE) | port(u16 BE) | addr(4 bytes) | 8 bytes pad`.
///
/// The kernel reads exactly 16 bytes, never `addrlen`, so handing it a
/// shorter buffer is a read past the end of the caller's object. Producing
/// the array here is what makes `&[u8; 16]` on `bind`/`connect` enforceable.
pub fn sockaddr_in(ip: [u8; 4], port: u16) -> [u8; 16] {
    /// AF_INET, as `read_sockaddr` expects it (little-endian u16).
    const AF_INET: u16 = 2;
    let mut sa = [0u8; 16];
    let fam = AF_INET.to_le_bytes();
    let p = port.to_be_bytes();
    sa[0] = fam[0];
    sa[1] = fam[1];
    sa[2] = p[0];
    sa[3] = p[1];
    sa[4] = ip[0];
    sa[5] = ip[1];
    sa[6] = ip[2];
    sa[7] = ip[3];
    sa
}

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

/// Read a single character from the kernel console.
///
/// **NON-BLOCKING.** `sys_getchar` (`handlers.rs:122`) tests
/// `uart::can_read()` and returns `-1` immediately when the RX FIFO is
/// empty — it does not wait. Callers wanting blocking behaviour must loop
/// with [`yield_now`]. (This doc said "blocking" until 2026-08-21; nothing
/// in `userspace/` called it, so nobody noticed.)
///
/// Returns the byte as a non-negative value, or `-1` if no byte is ready.
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

/// Maximum ELF image accepted by [`exec`]. Mirrors `EXEC_MAX_BYTES` in
/// `crates/syscall/src/handlers.rs:82` (128 KiB). A larger image is
/// **rejected**, not truncated.
pub const EXEC_MAX_BYTES: usize = 128 * 1024;

/// Replace the calling process's address space with the ELF image in `elf`.
///
/// ABI (`sys_exec`, `crates/syscall/src/handlers.rs:156`; dispatch arm
/// `crates/syscall/src/dispatch.rs:93`):
///   a0 = pointer to the ELF **image bytes**, a1 = image length in bytes.
///
/// The kernel bounces the whole range through `copy_from_user` into
/// `EXEC_BOUNCE`, a static `[u8; EXEC_MAX_BYTES]`, then calls `exec_user`.
/// It therefore requires the *bytes of an ELF file*, readable through the
/// caller's own page table.
///
/// **This wrapper previously declared `exec(entry_addr, stack_addr)` and
/// passed an entry point where the kernel reads a pointer.** Nothing called
/// it, which is why the drift survived. If either side moves again, it must
/// move here and in `sys_exec` together.
///
/// Returns only on failure (`-1`): `elf` empty, longer than
/// [`EXEC_MAX_BYTES`], not readable through the caller's page table, or not
/// a loadable ELF. On success the trap handler enters the new image on
/// `sret` and this call never returns.
pub fn exec(elf: &[u8]) -> isize {
    unsafe { syscall2(SYS_EXEC, elf.as_ptr() as u64, elf.len() as u64) }
}

/// Replace the calling process's address space with the ELF at `path`.
///
/// ABI (`sys_execpath`, `handlers.rs:174`): a0 = pointer to a **NUL-
/// terminated** path. The kernel reads it with `copy_cstr_from_user` into a
/// 256-byte buffer; the slice length is not transmitted. Build `path` with
/// [`cstr!`] or include the `\0` yourself.
///
/// The file is read into the same 128 KiB bounce buffer as [`exec`]; a file
/// that reaches the cap is refused rather than truncated.
///
/// Returns `-1` on failure; does not return on success.
pub fn execpath(path: &[u8]) -> isize {
    // Guard, not trust: without a NUL the kernel scans past the caller's
    // slice. See `has_nul`.
    if !has_nul(path) {
        return E_INVAL;
    }
    unsafe { syscall1(SYS_EXECPATH, path.as_ptr() as u64) }
}

/// Wait for a child process to exit.
///
/// **Unimplemented in the kernel.** `sys_wait` (`handlers.rs:148`) is
/// `-1  // Phase 8+`. Always returns `-1`.
pub fn wait() -> isize {
    unsafe { syscall0(SYS_WAIT) }
}

// ---------------------------------------------------------------------------
//  File I/O
// ---------------------------------------------------------------------------

/// Largest single transfer honoured by [`read`] / [`write`]. The kernel
/// bounces through a 4 KiB stack buffer (`handlers.rs:265`, `:288`) and
/// silently clamps anything longer — a short count is normal, not an error.
pub const IO_MAX_BYTES: usize = 4096;

/// Open a file.
///
/// ABI (`sys_open`, `handlers.rs:236`): a0 = pointer to a **NUL-terminated**
/// path, a1 = open flags. The kernel reads the path with
/// `copy_cstr_from_user` into a 256-byte buffer: it scans for a zero byte
/// and **never sees `path.len()`**. Passing `b"/fat/CONFIG.INI"` without the
/// terminator makes the kernel read past the literal.
///
/// This wrapper now rejects a slice with no NUL ([`E_INVAL`]) instead of
/// letting the kernel walk. Prefer [`cstr!`], which appends the terminator
/// at compile time and cannot be forgotten:
///
/// ```ignore
/// let fd = sys::open(sys::cstr!(b"/fat/CONFIG.INI"), 0);
/// ```
///
/// Returns a file descriptor, or negative on error.
pub fn open(path: &[u8], flags: u64) -> isize {
    if !has_nul(path) {
        return E_INVAL;
    }
    unsafe { syscall2(SYS_OPEN, path.as_ptr() as u64, flags) }
}

/// Close a file descriptor.
pub fn close(fd: u64) -> isize {
    unsafe { syscall1(SYS_CLOSE, fd) }
}

/// Read from `fd` into `buf`.
///
/// ABI (`sys_read`, `handlers.rs:261`): a0 = fd, a1 = buf, a2 = count.
/// The kernel clamps `count` to [`IO_MAX_BYTES`], so a buffer larger than
/// 4 KiB is filled at most 4 KiB per call.
///
/// Returns bytes read (may be less than `buf.len()`), or negative on error.
pub fn read(fd: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Write `buf` to `fd`.
///
/// ABI (`sys_write`, `handlers.rs:284`): a0 = fd, a1 = buf, a2 = count,
/// clamped to [`IO_MAX_BYTES`]. fd 1/2 go straight to the UART (the kernel
/// FD table does not pre-open stdio for user processes) and return the
/// clamped count.
///
/// Returns bytes written, or negative on error.
pub fn write(fd: u64, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_WRITE, fd, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Seek within a file descriptor.
///
/// ABI (`sys_lseek`, `handlers.rs:326`): the kernel narrows the offset to
/// **`i32`** before handing it to `vfs_lseek` (`offset as i32`), so the
/// usable range is `i32::MIN..=i32::MAX` — hence `i32` here rather than the
/// `u64` this wrapper used to take, which silently truncated.
///
/// `whence` follows the VFS convention (0 = SET, 1 = CUR, 2 = END).
/// Returns the new offset or negative on error.
pub fn lseek(fd: u64, offset: i32, whence: u64) -> isize {
    unsafe { syscall3(SYS_LSEEK, fd, offset as i64 as u64, whence) }
}

// ---------------------------------------------------------------------------
//  Filesystem
// ---------------------------------------------------------------------------

/// Stat a file.
///
/// **Unimplemented in the kernel.** `sys_stat` (`handlers.rs:433`) is
/// `pub fn sys_stat(_path: u64, _stat: u64) -> i64 { -1 }` — it ignores both
/// arguments and never writes `buf`. Always returns `-1`. Kept so the number
/// stays claimed; do not build on it.
pub fn stat(path: &[u8], buf: &mut [u8]) -> isize {
    if !has_nul(path) {
        return E_INVAL;
    }
    unsafe { syscall2(SYS_STAT, path.as_ptr() as u64, buf.as_mut_ptr() as u64) }
}

/// Size of the name buffer [`readdir`] must be given. The kernel writes
/// **exactly** this many bytes, always (`handlers.rs:400`,
/// `copy_to_user(name_out, tmp, 64)`) — including on a short name, where the
/// tail is zero-padded.
pub const READDIR_NAME_BYTES: usize = 64;

/// Read one directory entry by index.
///
/// ABI (`sys_readdir`, `handlers.rs:376`; dispatch `dispatch.rs:108`):
///   a0 = **NUL-terminated directory path** (not an fd),
///   a1 = entry index,
///   a2 = name_out — a 64-byte buffer, always fully written,
///   a3 = size_out — `*u32` (4 bytes), or 0 to skip,
///   a4 = is_dir_out — `*u32` (4 bytes), or 0 to skip.
///
/// **The previous wrapper was `readdir(fd, buf, buf.len(), index,
/// max_entries)` — five arguments in an entirely different order, opening
/// with an fd where the kernel reads a path pointer.** Nothing called it.
///
/// The parameters are typed as fixed-size arrays deliberately: the kernel
/// never learns a length for any of the three outputs, so the only place the
/// 64/4/4 sizes can be enforced is here.
///
/// Returns 0 when an entry was written, `-1` when `index` is past the end or
/// the path does not resolve.
pub fn readdir(
    dir_path: &[u8],
    index: u64,
    name_out: &mut [u8; READDIR_NAME_BYTES],
    size_out: &mut u32,
    is_dir_out: &mut u32,
) -> isize {
    if !has_nul(dir_path) {
        return E_INVAL;
    }
    unsafe {
        syscall5(
            SYS_READDIR,
            dir_path.as_ptr() as u64,
            index,
            name_out.as_mut_ptr() as u64,
            size_out as *mut u32 as u64,
            is_dir_out as *mut u32 as u64,
        )
    }
}

/// Create a directory at `path` (**NUL-terminated** — see [`open`]).
pub fn mkdir(path: &[u8]) -> isize {
    if !has_nul(path) {
        return E_INVAL;
    }
    unsafe { syscall1(SYS_MKDIR, path.as_ptr() as u64) }
}

/// Remove a file or directory at `path` (**NUL-terminated** — see [`open`]).
pub fn unlink(path: &[u8]) -> isize {
    if !has_nul(path) {
        return E_INVAL;
    }
    unsafe { syscall1(SYS_UNLINK, path.as_ptr() as u64) }
}

/// Change current working directory to `path` (**NUL-terminated**).
///
/// **No kernel dispatch arm exists.** `SYS_CHDIR` (254) is declared in
/// `crates/syscall/src/numbers.rs:83` and has a wrapper here, but
/// `syscall_dispatch` has no `SYS_CHDIR` arm — it falls through to
/// `_ => -1`. Always returns `-1`.
pub fn chdir(path: &[u8]) -> isize {
    if !has_nul(path) {
        return E_INVAL;
    }
    unsafe { syscall1(SYS_CHDIR, path.as_ptr() as u64) }
}

/// Get current working directory into `buf`.
///
/// **No kernel dispatch arm exists** — `SYS_GETCWD` (255) falls through to
/// `_ => -1` in `syscall_dispatch`, exactly like [`chdir`]. `buf` is never
/// written. Always returns `-1`.
pub fn getcwd(buf: &mut [u8]) -> isize {
    unsafe { syscall2(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Mount a filesystem (all three arguments **NUL-terminated**).
///
/// **Unimplemented in the kernel.** `sys_mount` (`handlers.rs:430`) ignores
/// all three arguments and returns `-1`.
pub fn mount(source: &[u8], target: &[u8], fstype: &[u8]) -> isize {
    if !has_nul(source) || !has_nul(target) || !has_nul(fstype) {
        return E_INVAL;
    }
    unsafe { syscall3(SYS_MOUNT, source.as_ptr() as u64, target.as_ptr() as u64, fstype.as_ptr() as u64) }
}

/// Unmount the filesystem at `target` (**NUL-terminated**).
///
/// **Unimplemented in the kernel.** `sys_umount` (`handlers.rs:431`) returns
/// `-1` unconditionally.
pub fn umount(target: &[u8]) -> isize {
    if !has_nul(target) {
        return E_INVAL;
    }
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

/// Set GPIO pin direction.
///
/// ABI (`sys_gpio_mode`, `handlers.rs:636`): a1 is a **direction**, and the
/// kernel tests it as `if mode == 1 { Output } else { Input }` — there is no
/// "alt" mode, and any value other than 1 means Input. Requires a write-
/// capable `Gpio(pin)` capability, else [`E_PERM_HANDLER`].
pub fn gpio_mode(pin: u64, mode: u64) -> isize {
    unsafe { syscall2(SYS_GPIO_MODE, pin, mode) }
}

/// GPIO direction: input. See [`gpio_mode`].
pub const GPIO_DIR_INPUT: u64 = 0;
/// GPIO direction: output. See [`gpio_mode`].
pub const GPIO_DIR_OUTPUT: u64 = 1;

/// Print GPIO subsystem info to the kernel console. Always returns 0 —
/// `sys_gpio_info` (`handlers.rs:644`) is `gpio_info(); 0`; nothing is
/// returned to the caller.
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

/// Set the PWM **period** for a channel, in nanoseconds.
///
/// ABI (`sys_pwm_set_freq`, `handlers.rs:661`): despite the syscall's name,
/// a1 is `period_ns`, narrowed to `u32` and passed to
/// `pwm_set_period(ch, period_ns)`. It is **not** a frequency in Hz — a
/// caller passing 1000 meaning "1 kHz" gets a 1 µs period (1 MHz).
/// The parameter is named for what the kernel reads.
pub fn pwm_set_freq(channel: u64, period_ns: u64) -> isize {
    unsafe { syscall2(SYS_PWM_SET_FREQ, channel, period_ns) }
}

/// Set the PWM **on-time** for a channel, in nanoseconds.
///
/// ABI (`sys_pwm_set_duty`, `handlers.rs:667`): a1 is `duty_ns`, narrowed to
/// `u32` — an absolute pulse width, not a 0..65535 fraction and not a
/// percentage. Meaningful only relative to the period set by
/// [`pwm_set_freq`].
pub fn pwm_set_duty(channel: u64, duty_ns: u64) -> isize {
    unsafe { syscall2(SYS_PWM_SET_DUTY, channel, duty_ns) }
}

/// Print PWM subsystem info to the kernel console. Always returns 0.
pub fn pwm_info() -> isize {
    unsafe { syscall0(SYS_PWM_INFO) }
}

// ---------------------------------------------------------------------------
//  I2C
// ---------------------------------------------------------------------------

/// Read from an I2C device.
///
/// ABI (`sys_i2c_read`, `handlers.rs:683`): a0 = bus, a1 = 7-bit address,
/// a2 = register, a3 = buffer, a4 = length (clamped to [`I2C_MAX_XFER`]).
/// The kernel copies out `min(buf.len(), 256)` bytes, but **returns the
/// driver's status**, not a byte count. Requires a read-capable
/// `I2c(bus, addr)` capability, else [`E_PERM_HANDLER`].
pub fn i2c_read(bus: u64, addr: u64, reg: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall5(SYS_I2C_READ, bus, addr, reg, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Write to an I2C device.
///
/// ABI (`sys_i2c_write`, `handlers.rs:704`): a0 = bus, a1 = 7-bit address,
/// a2 = data pointer, a3 = length, clamped to [`I2C_MAX_XFER`].
///
/// **`buf[0]` is the register address**, `buf[1..]` the bytes written to it —
/// the wrapper does not prepend it for you. Requires a write-capable
/// `I2c(bus, addr)` capability, else [`E_PERM_HANDLER`].
pub fn i2c_write(bus: u64, addr: u64, buf: &[u8]) -> isize {
    unsafe { syscall4(SYS_I2C_WRITE, bus, addr, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Largest single I2C transfer the kernel will perform; longer requests are
/// silently clamped (`I2C_MAX_XFER`, `handlers.rs:687`/`:707`).
pub const I2C_MAX_XFER: usize = 256;

/// Probe an I2C bus and print what answers.
///
/// `sys_i2c_scan` (`handlers.rs:716`) is `i2c_scan(bus); 0` — the device
/// count goes to the kernel console, **not** to the caller. Returns 0 on
/// success, or [`E_PERM_HANDLER`] without an `I2c(bus, 0)` capability.
pub fn i2c_scan(bus: u64) -> isize {
    unsafe { syscall1(SYS_I2C_SCAN, bus) }
}

/// Print I2C subsystem info to the kernel console. Always returns 0.
pub fn i2c_info() -> isize {
    unsafe { syscall0(SYS_I2C_INFO) }
}

// ---------------------------------------------------------------------------
//  Motor
// ---------------------------------------------------------------------------

/// Initialise motor `id` with its PWM channel and two direction pins.
///
/// ABI (`sys_motor_create`, `handlers.rs:731`): a0 = **motor id**,
/// a1 = pwm channel, a2 = direction pin A, a3 = direction pin B. It calls
/// `motor_init(id, pwm_ch, dir_a, dir_b)`.
///
/// The previous wrapper was `(pwm_ch, dir_gpio, enc_gpio, motor_type)` —
/// shifted by one and with no `id` at all, so a1..a3 all landed on the wrong
/// parameter. There is no encoder pin and no motor type in this ABI.
/// Requires a write-capable `Motor(id)` capability, else [`E_PERM_HANDLER`].
pub fn motor_create(id: u64, pwm_ch: u64, dir_pin_a: u64, dir_pin_b: u64) -> isize {
    unsafe { syscall4(SYS_MOTOR_CREATE, id, pwm_ch, dir_pin_a, dir_pin_b) }
}

/// Motor direction for [`motor_set_direction`]: forward.
pub const MOTOR_DIR_FORWARD: u64 = 0;
/// Motor direction for [`motor_set_direction`]: backward.
pub const MOTOR_DIR_BACKWARD: u64 = 1;
/// Motor direction for [`motor_set_direction`]: brake (both pins high).
pub const MOTOR_DIR_BRAKE: u64 = 2;
/// Motor direction for [`motor_set_direction`]: coast (both pins low).
pub const MOTOR_DIR_COAST: u64 = 3;

/// Set motor `id`'s **direction**, at a kernel-fixed 50% speed.
///
/// ABI (`sys_motor_enable`, `handlers.rs:737`): a1 is a direction — one of
/// the `MOTOR_DIR_*` constants — and the handler ends in
/// `motor_set(id, d, 50)`. It is **not** an enable/disable flag: passing
/// `enable = 1` (as the old `motor_enable(id, enable)` wrapper invited)
/// selects BACKWARD at half speed, and passing `0` selects FORWARD at half
/// speed. There is no way to disable a motor through this syscall.
///
/// The 50% is hard-coded in the kernel and cannot be chosen from here; a
/// following [`motor_speed`] resets the direction to forward. See the note
/// on [`motor_speed`] — the untyped motor ABI cannot express a
/// (direction, speed) pair at all.
///
/// Requires a write-capable `Motor(id)` capability, else [`E_PERM_HANDLER`].
pub fn motor_set_direction(id: u64, dir: u64) -> isize {
    unsafe { syscall2(SYS_MOTOR_ENABLE, id, dir) }
}

/// Drive motor `id` **forward** at `speed_pct` percent (0..=100).
///
/// ABI (`sys_motor_speed`, `handlers.rs:753`): a1 is an unsigned percentage.
/// `speed_pct == 0` calls `motor_stop(id)`; anything else calls
/// `motor_set(id, MotorDir::Forward, speed_pct)`, and `motor_set`
/// (`crates/robot/src/motor.rs:91`) clamps with `speed_pct.min(100)`.
///
/// **Reverse is not expressible through this syscall.** This doc used to
/// read "signed: positive = forward, negative = reverse", and two callers
/// believed it: `userspace/reflex`'s `motor_backup()` and
/// `userspace/brain_client`'s `apply_actuator_cmd()` both sign-extended a
/// negative into `u64`. `MotorDir::Forward` is hard-coded in the handler and
/// the clamp turns the huge value into 100 — so "reverse away from the
/// obstacle" drove **full speed forward into it**. On QEMU that is a log
/// line; on the robot it is a collision.
///
/// Direction lives in [`motor_set_direction`], which then forces 50% speed.
/// Encoding both in one call needs a kernel-side change — see the ABI audit
/// report.
///
/// Requires a write-capable `Motor(id)` capability, else [`E_PERM_HANDLER`].
pub fn motor_speed(id: u64, speed_pct: u64) -> isize {
    unsafe { syscall2(SYS_MOTOR_SPEED, id, speed_pct) }
}

/// Read accumulated encoder ticks for motor `id`.
///
/// ABI (`sys_motor_angle`, `handlers.rs:768`): only `id` 0 (left) and 1
/// (right) exist — they index the two-element tuple from
/// `robot_os_robot::encoder_read()`. Any other id returns `-1`, which is
/// indistinguishable from a legitimate reading of -1 tick.
///
/// Ticks, not degrees. No capability check is performed on this syscall.
pub fn motor_angle(id: u64) -> isize {
    unsafe { syscall1(SYS_MOTOR_ANGLE, id) }
}

/// Print motor subsystem info to the kernel console. Always returns 0.
pub fn motor_info() -> isize {
    unsafe { syscall0(SYS_MOTOR_INFO) }
}

// ---------------------------------------------------------------------------
//  System info
// ---------------------------------------------------------------------------

/// Number of **free physical pages**.
///
/// `sys_meminfo` (`handlers.rs:437`) is `pmm::free_pages() as i64` — a page
/// count, not "total memory in bytes" as this doc used to claim. Multiply by
/// the 4096-byte page size for bytes.
pub fn meminfo() -> isize {
    unsafe { syscall0(SYS_MEMINFO) }
}

/// Query task/process info.
///
/// **Unimplemented.** `sys_taskinfo` (`handlers.rs:441`) is `{ 0 }`. Always
/// returns 0 and reports nothing.
pub fn taskinfo() -> isize {
    unsafe { syscall0(SYS_TASKINFO) }
}

/// Raw CLINT `mtime` counter — **ticks, not milliseconds**.
///
/// `sys_uptime` (`handlers.rs:443`) is `clint::get_time() as i64`. At the
/// ~10 MHz CLINT frequency this tree assumes
/// (`robot_os_drivers::clint::TIMER_FREQ`), milliseconds are
/// `uptime() / (TIMER_FREQ / 1000)`.
///
/// For milliseconds without an ecall at all, use [`vdso_uptime_ms`].
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

/// Print disk info to the kernel console. Always returns 0 — the sector
/// count goes to the console, not the caller. Use [`disk_size`].
pub fn disk_info() -> isize {
    unsafe { syscall0(SYS_DISK_INFO) }
}

/// Bytes per sector, as the disk syscalls assume (`handlers.rs:462`).
pub const DISK_SECTOR_BYTES: usize = 512;
/// Maximum sectors per [`disk_read`] / [`disk_write`] call
/// (`DISK_MAX_SECTORS`, `handlers.rs:461`). A larger request is rejected
/// with `-1`, not clamped.
pub const DISK_MAX_SECTORS: usize = 128;

/// Read whole sectors starting at `sector` into `buf`.
///
/// ABI (`sys_disk_read`, `handlers.rs:464`; dispatch `dispatch.rs:125`):
///   a0 = start sector, a1 = **sector count**, a2 = destination buffer.
///
/// **The previous wrapper passed `(sector, buf_ptr, buf_len)`** — the buffer
/// pointer landed in the kernel's `count` and the length in its `buf`. The
/// kernel then copies `count * 512` bytes to whatever address `buf.len()`
/// happened to be. Nothing called it.
///
/// The kernel is never told how large `buf` is, so the sector count is
/// derived here from `buf.len()` rather than taken as a parameter: a
/// caller-supplied count is a ring-3 buffer overflow behind an honest-looking
/// signature. `buf` must be a whole number of sectors, at least one and at
/// most [`DISK_MAX_SECTORS`]; anything else returns [`E_INVAL`] without
/// issuing the ecall. Any trailing partial sector is refused rather than
/// ignored — a short read is a data bug, not a rounding question.
///
/// Returns 0 on success (**not** a byte count), negative on error.
pub fn disk_read(sector: u64, buf: &mut [u8]) -> isize {
    let count = buf.len() / DISK_SECTOR_BYTES;
    if count == 0 || count > DISK_MAX_SECTORS || buf.len() % DISK_SECTOR_BYTES != 0 {
        return E_INVAL;
    }
    unsafe { syscall3(SYS_DISK_READ, sector, count as u64, buf.as_mut_ptr() as u64) }
}

/// Write whole sectors from `buf` starting at `sector`.
///
/// ABI (`sys_disk_write`, `handlers.rs:488`): a0 = start sector,
/// a1 = **sector count**, a2 = source buffer — same argument order the old
/// wrapper got wrong for [`disk_read`], and with the same derived-count rule
/// applied here. Returns 0 on success, negative on error.
pub fn disk_write(sector: u64, buf: &[u8]) -> isize {
    let count = buf.len() / DISK_SECTOR_BYTES;
    if count == 0 || count > DISK_MAX_SECTORS || buf.len() % DISK_SECTOR_BYTES != 0 {
        return E_INVAL;
    }
    unsafe { syscall3(SYS_DISK_WRITE, sector, count as u64, buf.as_ptr() as u64) }
}

/// Disk capacity in 512-byte sectors, or negative on error.
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
///
/// **Kernel stub.** `sys_sigreturn` (`handlers.rs:1134`) is `{ 0 }` — the
/// signal frame is not restored ("full signal frame restore requires trap
/// frame plumbing"). Returning from a handler does not resume the
/// interrupted context.
pub fn sigreturn() -> isize {
    unsafe { syscall0(SYS_SIGRETURN) }
}

/// Bitmask of pending, unblocked signals.
pub fn sigpending() -> isize {
    unsafe { syscall0(SYS_SIGPENDING) }
}

/// `how` value for [`sigprocmask`]: read the mask without changing it.
pub const SIG_GET: u64 = 0;
/// `how` value for [`sigprocmask`]: replace the mask outright.
pub const SIG_SETMASK: u64 = 1;
/// `how` value for [`sigprocmask`]: add `mask` to the blocked set.
pub const SIG_BLOCK: u64 = 2;
/// `how` value for [`sigprocmask`]: remove `mask` from the blocked set.
pub const SIG_UNBLOCK: u64 = 3;

/// Get or set the signal mask.
///
/// ABI (`sys_sigprocmask`, `handlers.rs:533`): a0 = `how` — one of the
/// `SIG_*` constants above; **any unrecognised value is silently ignored**
/// and behaves as `SIG_GET`. a1 = mask.
///
/// Always returns the *previous* mask, including for `SIG_GET`.
pub fn sigprocmask(how: u64, mask: u64) -> isize {
    unsafe { syscall2(SYS_SIGPROCMASK, how, mask) }
}

/// Suspend until a signal is delivered.
///
/// ABI (`sys_pause`, `handlers.rs:1109`): yields at most **1000 times**
/// checking for a pending signal, then gives up and returns `-1`. It is a
/// bounded poll, not an indefinite sleep — a caller that treats `-1` as an
/// error rather than a timeout will misreport a quiet system.
/// Returns 0 if a signal arrived.
pub fn pause() -> isize {
    unsafe { syscall0(SYS_PAUSE) }
}

/// Set an alarm timer. `seconds` = 0 cancels (a no-op).
///
/// Warning (`sys_alarm`, `handlers.rs:1119`): the handler does not arm a
/// timer. It **busy-yields for the whole duration inside the syscall**,
/// then sends `SIGALRM` to the caller and returns 0 — `alarm(5)` blocks for
/// five seconds. It never returns remaining seconds, as this doc used to
/// claim.
pub fn alarm(seconds: u64) -> isize {
    unsafe { syscall1(SYS_ALARM, seconds) }
}

// ---------------------------------------------------------------------------
//  Pipes / FD duplication
// ---------------------------------------------------------------------------

/// Create a pipe, writing `[read_fd, write_fd]` into `fds`.
///
/// ABI (`sys_pipe`, `handlers.rs:549`): a0 points at an **`int[2]`** — the
/// kernel builds `let fds: [u32; 2]` and copies out
/// `size_of_val(&fds)` = **8 bytes**.
///
/// This wrapper took `&mut [u64; 2]` (16 bytes). The kernel wrote 8 bytes
/// into it, so the caller read `fds[0]` as `(write_fd << 32) | read_fd` and
/// `fds[1]` as a stale 0 — two wrong fds from a call that returned success.
/// Nothing called it. `[u32; 2]` is the layout the kernel actually writes.
///
/// Returns 0 on success, `-1` if no pipe slot is free or the copy-out failed
/// (in which case the pipe was still created and its fds are leaked — see
/// the kernel comment at `handlers.rs:565`).
pub fn pipe(fds: &mut [u32; 2]) -> isize {
    unsafe { syscall1(SYS_PIPE, fds.as_mut_ptr() as u64) }
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

/// Largest IPC message the kernel moves. `sys_ipc_send` / `sys_ipc_recv`
/// bounce through a `[u8; 64]` and clamp with `.min(64)`
/// (`handlers.rs:1180`, `:1196`) — a longer message is **silently
/// truncated**, and the call still reports success.
pub const IPC_MSG_MAX: usize = 64;

/// Send a message on an IPC channel.
///
/// Truncated to [`IPC_MSG_MAX`] bytes. An empty `buf` returns `-1` (the
/// kernel rejects `len == 0`). Returns 0 on success.
pub fn ipc_send(channel: u64, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_IPC_SEND, channel, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Receive a message from an IPC channel.
///
/// **Non-blocking**: returns 0 when the channel is empty, which is not an
/// error. Reads at most [`IPC_MSG_MAX`] bytes. An empty `buf` returns `-1`.
pub fn ipc_receive(channel: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_IPC_RECEIVE, channel, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Destroy an IPC channel. Always returns 0, including for an invalid
/// channel (`sys_ipc_destroy`, `handlers.rs:1212`).
pub fn ipc_destroy(channel: u64) -> isize {
    unsafe { syscall1(SYS_IPC_DESTROY, channel) }
}

// ---------------------------------------------------------------------------
//  Network (stack-level)
// ---------------------------------------------------------------------------

/// Print network interface info to the kernel console. Always returns 0 —
/// nothing is reported to the caller (`sys_net_info`, `handlers.rs:891`).
pub fn net_info() -> isize {
    unsafe { syscall0(SYS_NET_INFO) }
}

/// Current IPv4 address, as `u32::from_be_bytes(addr)` — i.e. the first
/// octet in the **most significant** byte (`handlers.rs:896`). Feed it
/// straight back to [`net_ping`] / [`net_setip`], which decode with
/// `to_be_bytes`.
pub fn net_getip() -> isize {
    unsafe { syscall0(SYS_NET_GETIP) }
}

/// Set IP address, netmask and gateway.
///
/// Each `u32` is decoded with `to_be_bytes` (`handlers.rs:1139`): the first
/// octet is the most significant byte, matching [`net_getip`]. Always
/// returns 0.
pub fn net_setip(ip: u32, mask: u32, gateway: u32) -> isize {
    unsafe { syscall3(SYS_NET_SETIP, ip as u64, mask as u64, gateway as u64) }
}

/// Ping an IPv4 address (encoded as for [`net_setip`]).
pub fn net_ping(ip: u32) -> isize {
    unsafe { syscall1(SYS_NET_PING, ip as u64) }
}

/// MAC address packed into a `u64`, **least-significant byte first**:
/// `sys_net_getmac` (`handlers.rs:1154`) builds it as
/// `val |= mac[i] << (i * 8)`, so `mac[0]` is bits 0..7 — the reverse of the
/// big-endian convention [`net_getip`] uses. Unpack with `to_le_bytes()`
/// and take the low 6 bytes.
pub fn net_getmac() -> isize {
    unsafe { syscall0(SYS_NET_GETMAC) }
}

/// Print network statistics to the kernel console. Always returns 0;
/// `sys_net_stats` (`handlers.rs:1162`) is the same `net_info()` call as
/// [`net_info`] and returns no counters to the caller.
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

/// Size of the `sockaddr_in` the kernel reads. `read_sockaddr`
/// (`handlers.rs:789`) copies **exactly 16 bytes** and never looks at the
/// `addrlen` argument, so a shorter object is read past its end. Build the
/// address with [`sockaddr_in`].
pub const SOCKADDR_LEN: usize = 16;

/// Bind a socket to a local address.
///
/// ABI (`sys_bind`, `handlers.rs:813`): a0 = fd, a1 = sockaddr pointer,
/// a2 = addrlen — **ignored** (`_addrlen`). `addr` is typed `&[u8; 16]`
/// because 16 is the only length the kernel will read; see [`sockaddr_in`].
pub fn bind(fd: u64, addr: &[u8; SOCKADDR_LEN]) -> isize {
    unsafe { syscall3(SYS_BIND, fd, addr.as_ptr() as u64, SOCKADDR_LEN as u64) }
}

/// Listen on a bound socket.
///
/// ABI (`sys_listen_syscall`, `handlers.rs:821`): `backlog` is accepted and
/// **ignored** (`_backlog`); the handler calls `socket_listen_bound(fd)`.
pub fn listen(fd: u64, backlog: u64) -> isize {
    unsafe { syscall2(SYS_LISTEN, fd, backlog) }
}

/// Accept a connection on a listening socket.
///
/// ABI (`sys_accept`, `handlers.rs:827`): a1/a2 are `_addr_out` /
/// `_addrlen_out` and are **ignored** — the peer address is never reported,
/// so this wrapper does not offer the arguments. The handler polls the net
/// stack up to 50,000 times, yielding between attempts, then gives up.
///
/// Returns the new socket fd, or `-1` on timeout/error.
pub fn accept(fd: u64) -> isize {
    unsafe { syscall3(SYS_ACCEPT, fd, 0, 0) }
}

/// Connect a socket to a remote address.
///
/// ABI (`sys_connect_syscall`, `handlers.rs:838`): a0 = fd,
/// a1 = sockaddr pointer, a2 = addrlen (**ignored**). The handler blocks
/// yielding until the TCP handshake completes, so success means connected.
/// Local port is chosen by the kernel as `0xC000 + fd`.
pub fn connect(fd: u64, addr: &[u8; SOCKADDR_LEN]) -> isize {
    unsafe { syscall3(SYS_CONNECT, fd, addr.as_ptr() as u64, SOCKADDR_LEN as u64) }
}

/// Largest payload a single [`send`] transmits; longer buffers are clamped,
/// not rejected (`handlers.rs:853`).
pub const SOCK_SEND_MAX: usize = 1460;
/// Largest payload a single [`recv`] returns (`handlers.rs:870`).
pub const SOCK_RECV_MAX: usize = 4096;

/// Send data on a connected socket. `flags` is accepted and **ignored** by
/// the kernel (`_flags`). Returns bytes sent (clamped to [`SOCK_SEND_MAX`]).
pub fn send(fd: u64, buf: &[u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_SEND, fd, buf.as_ptr() as u64, buf.len() as u64, flags) }
}

/// Receive from a connected socket. **Non-blocking**: `sys_recv_syscall`
/// polls the net stack once and returns 0 if nothing has arrived. `flags` is
/// ignored. Returns bytes read (clamped to [`SOCK_RECV_MAX`]).
pub fn recv(fd: u64, buf: &mut [u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_RECV, fd, buf.as_mut_ptr() as u64, buf.len() as u64, flags) }
}

/// Send on a socket.
///
/// **There is no destination address in this ABI.** `SYS_SENDTO` is
/// dispatched to the very same `sys_send_syscall` as `SYS_SEND`
/// (`dispatch.rs:783`), whose four arguments are `(fd, buf, len, _flags)`.
/// The old doc — "Send data to a specific address (UDP)" — described an
/// address the wrapper never took and the kernel never reads: an unconnected
/// UDP socket has nowhere to send. Identical to [`send`]; kept only because
/// the number is claimed.
pub fn sendto(fd: u64, buf: &[u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_SENDTO, fd, buf.as_ptr() as u64, buf.len() as u64, flags) }
}

/// Receive on a socket.
///
/// **The sender address is never reported.** `SYS_RECVFROM` dispatches to
/// the same `sys_recv_syscall` as `SYS_RECV` (`dispatch.rs:784`). Identical
/// to [`recv`].
pub fn recvfrom(fd: u64, buf: &mut [u8], flags: u64) -> isize {
    unsafe { syscall4(SYS_RECVFROM, fd, buf.as_mut_ptr() as u64, buf.len() as u64, flags) }
}

/// **Closes** the socket — this is not a half-close.
///
/// `SYS_SOCK_SHUTDOWN` dispatches to `sys_sock_close` (`dispatch.rs:785`),
/// which calls `socket_close(fd)` and returns 0 unconditionally. There is no
/// `how` argument and no way to shut down only one direction, despite the
/// name. Always returns 0, including for an invalid fd.
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

/// Map anonymous memory.
///
/// ABI (`sys_mmap`, `handlers.rs:906`): the POSIX six-argument shape is
/// accepted, but the kernel supports **anonymous mappings only** — it
/// returns `-1` unless `fd == u64::MAX` (i.e. `-1`). `addr`, `prot`, `flags`
/// and `offset` are all ignored (`_prot`, `_flags`, `_offset`); the mapping
/// is always placed at the current brk and is always user read/write.
/// `len` is capped at `robot_os_mm::demand::MAX_DEMAND_ALLOC_BYTES`.
///
/// Returns the mapped virtual address, or `-1`. Kernel tasks get `-1`.
pub fn mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> isize {
    unsafe { syscall6(SYS_MMAP, addr, len, prot, flags, fd, offset) }
}

/// `fd` value [`mmap`] requires for an anonymous mapping.
pub const MAP_ANON_FD: u64 = u64::MAX;

/// Unmap memory at `addr` for `len` bytes.
pub fn munmap(addr: u64, len: u64) -> isize {
    unsafe { syscall2(SYS_MUNMAP, addr, len) }
}

// ---------------------------------------------------------------------------
//  RFC-0002 Driver registry bridge
// ---------------------------------------------------------------------------

/// Invoke a registered driver via the RFC-0002 registry.
///
/// `kind` is one of the `DRV_KIND_*` values (e.g. `0x0004` for UART,
/// `0x0001` for GPIO). `op` is the driver-defined op code (see the
/// driver's `*_OP_*` constants in `robot_os_drivers`). `input` is
/// the request payload, `output` is the reply buffer.
///
/// Returns the number of bytes written to `output` on success
/// (`Ok(n)`), or a negative errno on failure (`Err(errno)`):
/// - `-ENODEV` (-19): no driver registered for `kind`
/// - `-ENOSYS` (-38): driver does not support this `op`
/// - `-EINVAL` (-22): bad input layout or oversize buffer
/// - `-EAGAIN` (-11): driver busy, retry later
/// - `-EIO` (-5): underlying hardware error
///
/// Both buffers are bounded by [`DRIVER_INVOKE_MAX_INPUT_BYTES`] /
/// [`DRIVER_INVOKE_MAX_OUTPUT_BYTES`] (256 each); larger transfers
/// must use the F15 zero-copy pipeline.
pub fn drv_invoke(
    kind: u32,
    op: u32,
    input: &[u8],
    output: &mut [u8],
) -> Result<usize, isize> {
    let in_ptr = if input.is_empty() {
        0
    } else {
        input.as_ptr() as u64
    };
    let out_ptr = if output.is_empty() {
        0
    } else {
        output.as_mut_ptr() as u64
    };
    let rc = unsafe {
        syscall6(
            SYS_DRV_INVOKE,
            kind as u64,
            op as u64,
            in_ptr,
            input.len() as u64,
            out_ptr,
            output.len() as u64,
        )
    };
    if rc < 0 {
        Err(rc)
    } else {
        Ok(rc as usize)
    }
}

/// Maximum input payload bytes accepted by [`drv_invoke`]. Mirrors
/// `robot_os_abi::syscall_nr::DRIVER_INVOKE_MAX_INPUT_BYTES`.
pub const DRIVER_INVOKE_MAX_INPUT_BYTES: usize = 256;
/// Maximum output buffer bytes accepted by [`drv_invoke`]. Mirrors
/// `robot_os_abi::syscall_nr::DRIVER_INVOKE_MAX_OUTPUT_BYTES`.
pub const DRIVER_INVOKE_MAX_OUTPUT_BYTES: usize = 256;

// ---------------------------------------------------------------------------
//  Service manager
// ---------------------------------------------------------------------------

// Every syscall in this family is keyed by a **NUL-terminated service name**,
// read with `copy_cstr_from_user` into a 64-byte kernel buffer
// (`SYS_SERVICE_NAME_MAX`, `handlers.rs:586`). There is no numeric service
// id anywhere in this ABI — the old `service_heartbeat(service_id: u64)` and
// `service_stop(service_id: u64)` wrappers passed an integer straight into a
// pointer argument. Nothing called them.

/// Longest service name the kernel will copy (`handlers.rs:586`).
/// Longer names fail the `copy_cstr_from_user` bound and return `-1`.
pub const SERVICE_NAME_MAX: usize = 64;

/// Register a service under `name` (**NUL-terminated**).
///
/// ABI (`sys_service_register`, `handlers.rs:589`): a0 = name pointer,
/// a1 = **owning task id**, a2 = ipc channel. The previous wrapper passed
/// `(name_ptr, name_len, port)` — a length where the kernel reads a tid.
///
/// Returns 0 on success, negative on error.
pub fn service_register(name: &[u8], tid: u64, channel: u64) -> isize {
    if !has_nul(name) {
        return E_INVAL;
    }
    unsafe { syscall3(SYS_SERVICE_REGISTER, name.as_ptr() as u64, tid, channel) }
}

/// Look up a service by `name` (**NUL-terminated**).
///
/// ABI (`sys_service_discover`, `handlers.rs:597`): returns the registered
/// service's **task id** (`entry.tid`), not a port as this doc used to say.
/// `-1` if no such service.
pub fn service_discover(name: &[u8]) -> isize {
    if !has_nul(name) {
        return E_INVAL;
    }
    unsafe { syscall1(SYS_SERVICE_DISCOVER, name.as_ptr() as u64) }
}

/// Send a liveness heartbeat for the service called `name`
/// (**NUL-terminated** — `sys_service_heartbeat`, `handlers.rs:608`).
pub fn service_heartbeat(name: &[u8]) -> isize {
    if !has_nul(name) {
        return E_INVAL;
    }
    unsafe { syscall1(SYS_SERVICE_HEARTBEAT, name.as_ptr() as u64) }
}

/// Stop the service called `name` (**NUL-terminated** —
/// `sys_service_stop_handler`, `handlers.rs:616`).
pub fn service_stop(name: &[u8]) -> isize {
    if !has_nul(name) {
        return E_INVAL;
    }
    unsafe { syscall1(SYS_SERVICE_STOP, name.as_ptr() as u64) }
}

// ---------------------------------------------------------------------------
//  Robot control
// ---------------------------------------------------------------------------
//
// EVERY syscall in this block is a kernel stub. `dispatch.rs:771` collapses
// the whole range with `SYS_ROBOT_INIT ..= SYS_SENSOR_ADD => sys_stub()`,
// and `sys_stub` (`handlers.rs:2587`) is `-1`. That covers 320..=331: all
// ten robot calls, `sensor_info` and `sensor_add`. Arguments are not read;
// `robot_estop()` in particular does **nothing** and must not be relied on
// as a safety path. `sensor_read` (332) is the one live syscall in the range.
//
// They return -1, so a caller that checks the result is merely disappointed
// rather than misled — but do not read a -1 from these as "hardware absent".

/// Initialize robot subsystem. **Kernel stub — always returns `-1`.**
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
//  Sensors
// ---------------------------------------------------------------------------

/// Query sensor subsystem info.
/// **Kernel stub — always returns `-1`** (`dispatch.rs:771`).
pub fn sensor_info() -> isize {
    unsafe { syscall0(SYS_SENSOR_INFO) }
}

/// Register/add a sensor of the given type.
/// **Kernel stub — always returns `-1`** (`dispatch.rs:771`). Sensors need
/// no registration: [`sensor_read`] works without it.
pub fn sensor_add(sensor_type: u64) -> isize {
    unsafe { syscall1(SYS_SENSOR_ADD, sensor_type) }
}

/// Read one sensor's current sample into `buf`.
///
/// ABI (`sys_sensor_read`, `handlers.rs:2229`): a0 = one of the
/// `SENSOR_TYPE_*` constants, a1 = buffer, a2 = capacity. The kernel builds
/// the record in a kernel buffer and copies it out with `copy_to_user`,
/// returning the byte count.
///
/// `buf` must be at least as large as the record for that sensor type — the
/// per-type sizes are listed beside the constants in `handlers.rs:2243`
/// (IMU 24, ODOM 16, ENCODER 16, RANGE 4, BATTERY 2, GPS 16,
/// GPIO_FLAGS 2, POWER 12; LIDAR and CAMERA are variable).
///
/// Returns bytes written, or negative on error. Note `-1` means "the driver
/// had nothing to report", not "denied" — see [`E_PERM_HANDLER`].
pub fn sensor_read(sensor_type: u64, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_SENSOR_READ, sensor_type, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

// ---------------------------------------------------------------------------
//  Platform
// ---------------------------------------------------------------------------

/// Query platform info.
///
/// **Kernel stub — always returns `-1`.** `dispatch.rs:773` is
/// `SYS_PLATFORM_INFO ..= SYS_PLATFORM_TYPE => sys_stub()`.
pub fn platform_info() -> isize {
    unsafe { syscall0(SYS_PLATFORM_INFO) }
}

/// Get platform type.
///
/// **Kernel stub — always returns `-1`,** not a platform id. The 0=QEMU /
/// 1=VF2 / 2=K1 encoding this doc used to promise is not implemented
/// anywhere; the dispatch arm is `sys_stub()`.
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

/// Register the calling process as a driver named `name`.
///
/// ABI (`SYS_DRV_REGISTER`, `dispatch.rs:613`): a0 = name pointer,
/// a1 = length, **clamped to 32 bytes** — this one takes an explicit length,
/// not a NUL-terminated string, so a longer name is truncated silently.
///
/// Returns a driver id (the `drv_id` [`drv_mmap`] and [`drv_heartbeat`]
/// need), or `-1`.
pub fn drv_register(name: &[u8]) -> isize {
    unsafe { syscall2(SYS_DRV_REGISTER, name.as_ptr() as u64, name.len() as u64) }
}

/// Longest driver name [`drv_register`] records (`dispatch.rs:615`).
pub const DRV_NAME_MAX: usize = 32;

/// Map one of a registered driver's declared MMIO regions into its own
/// address space.
///
/// ABI (`SYS_DRV_MMAP`, `dispatch.rs:628`): a0 = **`drv_id`** (from
/// [`drv_register`]), a1 = **`mmio_idx`**, an index into that driver's
/// `info.mmio[]` table. The kernel looks the region up itself:
/// `driver_info(drv_id)`, bounds-check `mmio_idx < info.mmio_count`, then
/// `mmio_map_user(region.base, region.size)`.
///
/// **This wrapper previously declared `drv_mmap(phys, size)` and passed a
/// physical address where the kernel indexes a driver table.** A physical
/// base like `0x1000_0000` would have been read as driver id 268435456 and
/// failed the lookup — the caller's `size` becoming an MMIO index. The
/// syscall that *does* take `(phys_base, size)` is `SYS_MMIO_MAP`
/// (`dispatch.rs:888`), which has no wrapper in this library.
///
/// Requires an `MmioRegion(base, size)` capability for the region the table
/// names, else [`E_PERM_DISPATCH`] — note this denial comes from the
/// dispatcher (`-1`), not from a handler (`-99`).
///
/// Returns the mapped user virtual address, or negative on error.
pub fn drv_mmap(drv_id: u64, mmio_idx: u64) -> isize {
    unsafe { syscall2(SYS_DRV_MMAP, drv_id, mmio_idx) }
}

/// Unmap a previously mapped MMIO region.
///
/// **Unimplemented.** `SYS_DRV_MUNMAP` is `sys_stub()` (`dispatch.rs:657`)
/// and returns `-1`; both arguments are discarded. A region mapped by
/// [`drv_mmap`] stays mapped for the life of the process.
pub fn drv_munmap(addr: u64, size: u64) -> isize {
    unsafe { syscall2(SYS_DRV_MUNMAP, addr, size) }
}

/// Block until IRQ `irq` fires.
///
/// ABI (`dispatch.rs:660`): blocks on `WaitReason::Irq(irq)` and returns 0
/// when woken. No capability check on this arm — but [`drv_irq_ack`]
/// requires one, so a task that cannot ack should not be waiting.
pub fn drv_irq_wait(irq: u64) -> isize {
    unsafe { syscall1(SYS_DRV_IRQ_WAIT, irq) }
}

/// Acknowledge an IRQ (PLIC completion) after handling it.
///
/// Requires an `Irq(irq)` capability, else [`E_PERM_DISPATCH`]
/// (`dispatch.rs:667` — a dispatcher-side `-1`, not `-99`).
pub fn drv_irq_ack(irq: u64) -> isize {
    unsafe { syscall1(SYS_DRV_IRQ_ACK, irq) }
}

/// Tell the driver manager that driver `drv_id` is still alive.
///
/// ABI (`SYS_DRV_HEARTBEAT`, `dispatch.rs:747`): a0 = **`drv_id`**, passed
/// to `driver_heartbeat_with_time(a0 as usize, now_ms)`.
///
/// **This wrapper took no arguments and issued `syscall0`.** `syscall0`
/// declares `a0` as `lateout` only, so nothing writes it before the `ecall`
/// and the kernel read whatever the compiler had left in the register — an
/// arbitrary `drv_id`, refreshing some other driver's watchdog or none.
/// Silent because heartbeats have no return value to check.
///
/// Always returns 0, including for an unknown `drv_id`.
pub fn drv_heartbeat(drv_id: u64) -> isize {
    unsafe { syscall1(SYS_DRV_HEARTBEAT, drv_id) }
}

// ---- RFC-0002 driver-server: serve a driver `kind` from userspace (E11.AQ3) ----

/// Register the calling process as the userspace handler for driver `kind`.
/// `mmio_base`/`mmio_size`/`irq` are advisory metadata (0 if unused).
/// Returns 0 on success, negative on error.
pub fn drv_srv_register(kind: u32, mmio_base: u64, mmio_size: u64, irq: u32) -> isize {
    unsafe { syscall4(SYS_DRIVER_REGISTER, kind as u64, mmio_base, mmio_size, irq as u64) }
}

/// Fetch one pending request for `kind` into the caller's `DriverRequest`
/// buffer (pass `&mut req as *mut _ as *mut u8`).
///
/// ABI (`sys_driver_fetch_request`, `handlers.rs:2446`): the kernel writes
/// **exactly `size_of::<DriverRequest>()` bytes** to `req_ptr` and is never
/// told how large the destination is. The struct is `#[repr(C)]` and lives
/// in `crates/driver_server/src/lib.rs:107`:
///
/// ```text
/// token: u64 | client_tid: u32 | op: u32 | in_len: u16 | out_cap: u16
///            | input: [u8; 64]                      →  88 bytes on RV64
/// ```
///
/// `req_ptr` must therefore address at least that many bytes, correctly
/// aligned to 8. `userspace/gpio_drv` keeps a mirror of the struct; if
/// either copy changes, both must. This stays a raw pointer rather than a
/// typed reference so libsys need not depend on `driver_server`.
///
/// Returns 0 if a request was written, `-1` if the queue is empty.
pub fn drv_srv_fetch_request(kind: u32, req_ptr: *mut u8) -> isize {
    unsafe { syscall2(SYS_DRIVER_FETCH_REQ, kind as u64, req_ptr as u64) }
}

/// Post a `DriverReply` (pass `&reply as *const _ as *const u8`) for `kind`.
///
/// ABI (`sys_driver_reply`, `handlers.rs:2480`): the kernel **reads
/// `size_of::<DriverReply>()` bytes** from `reply_ptr` — the `#[repr(C)]`
/// struct at `crates/driver_server/src/lib.rs:137`:
///
/// ```text
/// token: u64 | status: i32 | out_len: u16 | _pad: u16
///            | output: [u8; 64]                     →  80 bytes on RV64
/// ```
///
/// Same mirroring rule as [`drv_srv_fetch_request`]. Returns 0 on success.
pub fn drv_srv_reply(kind: u32, reply_ptr: *const u8) -> isize {
    unsafe { syscall2(SYS_DRIVER_REPLY, kind as u64, reply_ptr as u64) }
}

// ===========================================================================
//  IO Ring API (AQ4) — asynchronous I/O submission and completion
// ===========================================================================

/// Create a new IO ring owned by the calling task.
///
/// ABI (`SYS_IO_SETUP`, `dispatch.rs:792`): `flags` is accepted and
/// **ignored** — the arm is `io_ring_create(tid)` and reads no argument.
/// Returns the ring id, or `-1` if no ring slot is free.
pub fn io_setup(flags: u64) -> isize {
    unsafe { syscall1(SYS_IO_SETUP, flags) }
}

/// Process every pending submission-queue entry on `ring_id`.
///
/// ABI (`SYS_IO_SUBMIT`, `dispatch.rs:799`): **`ring_id` is the only
/// argument the kernel reads** — the arm is `io_ring_submit(a0 as u32)`.
/// SQEs are written by the caller directly into the ring's shared pages, not
/// passed by pointer, so the old `(ring_id, entries_ptr, count)` signature
/// offered two arguments that went nowhere and implied a submission
/// mechanism that does not exist.
///
/// The caller must own the ring, else [`E_PERM_DISPATCH`].
/// Returns the number of entries processed.
pub fn io_submit(ring_id: u64) -> isize {
    unsafe { syscall1(SYS_IO_SUBMIT, ring_id) }
}

/// Number of completions currently pending on `ring_id`.
///
/// ABI (`SYS_IO_WAIT`, `dispatch.rs:804`): **does not wait.** The arm is
/// `io_ring_pending(a0 as u32)` — it reads the depth and returns
/// immediately, and the old `min_completions` argument was never read. Poll
/// with [`yield_now`] if you need to block.
///
/// The caller must own the ring, else [`E_PERM_DISPATCH`].
pub fn io_pending(ring_id: u64) -> isize {
    unsafe { syscall1(SYS_IO_WAIT, ring_id) }
}

// ===========================================================================
//  Channel API (AQ4) — kernel-mediated message passing
// ===========================================================================

/// Create a new kernel channel. Returns the channel handle or negative.
pub fn chan_create() -> isize {
    unsafe { syscall0(SYS_CHAN_CREATE) }
}

/// Write a message to a channel.
///
/// `SYS_CHAN_WRITE` dispatches to the same `sys_ipc_send` as `SYS_IPC_SEND`
/// (`dispatch.rs:833`), so the [`IPC_MSG_MAX`] clamp applies here too.
pub fn chan_write(handle: u64, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_CHAN_WRITE, handle, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Read a message from a channel into `buf`.
///
/// `SYS_CHAN_READ` dispatches to `sys_ipc_recv` (`dispatch.rs:834`);
/// see [`ipc_receive`]. Returns bytes read, 0 if the channel is empty, or
/// negative on error.
pub fn chan_read(handle: u64, buf: &mut [u8]) -> isize {
    // Was `buf.as_ptr()` on a `&mut [u8]` — the kernel writes through this
    // pointer, so it must be derived from the mutable borrow.
    unsafe { syscall3(SYS_CHAN_READ, handle, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

// ===========================================================================
//  Port API (AQ5) — multi-source event waiting (like kqueue / Zircon ports)
// ===========================================================================

/// Create a new event port. Returns the port handle or negative.
pub fn port_create() -> isize {
    unsafe { syscall0(SYS_PORT_CREATE) }
}

/// Port source kind: IPC channel. See [`port_bind`].
pub const PORT_SRC_CHANNEL: u64 = 0;
/// Port source kind: IO ring.
pub const PORT_SRC_RING: u64 = 1;
/// Port source kind: hardware IRQ.
pub const PORT_SRC_IRQ: u64 = 2;
/// Port source kind: timer (`source_id` is the deadline).
pub const PORT_SRC_TIMER: u64 = 3;

/// Bind an event source to a port.
///
/// ABI (`SYS_PORT_BIND`, `dispatch.rs:931`): a0 = port, a1 = source kind —
/// one of the `PORT_SRC_*` constants above, **in that numeric order**
/// (0 = channel, 1 = ring, 2 = irq, 3 = timer; the old doc listed them as
/// "IRQ, channel, ring, timer", which maps every value to the wrong source)
/// — a2 = source id, a3 = the opaque key [`port_wait`] returns.
///
/// An unrecognised kind returns `-1`; a port the caller does not own returns
/// [`E_PERM_DISPATCH`]. Returns 0 on success.
pub fn port_bind(port: u64, source_kind: u64, source_id: u64, key: u64) -> isize {
    unsafe { syscall4(SYS_PORT_BIND, port, source_kind, source_id, key) }
}

/// Wait for an event on a port, blocking until a bound source fires.
///
/// Returns the `key` given to [`port_bind`] for the source that fired,
/// [`E_PERM_DISPATCH`] if the caller does not own the port, or `-1` if the
/// task was woken with no event queued.
pub fn port_wait(port: u64) -> isize {
    unsafe { syscall1(SYS_PORT_WAIT, port) }
}

/// **Destroy** the port — this does not unbind a single source.
///
/// ABI (`SYS_PORT_UNBIND`, `dispatch.rs:968`): the arm reads only `port` and
/// calls `port_destroy(a0 as u32)`, dropping every bound source at once. The
/// old `port_unbind(port, source_id)` wrapper offered a `source_id` the
/// kernel never reads, so a caller aiming to detach one IRQ tore down the
/// whole port and kept waiting on a dead handle. The kernel's own comment
/// calls it "destroy the port entirely (simplified)".
///
/// Returns 0, or [`E_PERM_DISPATCH`] if the caller does not own the port.
pub fn port_destroy(port: u64) -> isize {
    unsafe { syscall1(SYS_PORT_UNBIND, port) }
}

// ===========================================================================
//  Trace API (AQ8) — kernel event ring buffer dump
// ===========================================================================

/// Dump the last `count` kernel trace entries to the console.
///
/// ABI (`SYS_TRACE_DUMP`, `dispatch.rs:1049`): a0 = entry count, where
/// **`0` means the kernel default of 50**.
///
/// **This wrapper took no arguments and issued `syscall0`**, which declares
/// `a0` as `lateout` only — nothing writes the register before the `ecall`,
/// so the kernel read leftover garbage as the count and dumped an arbitrary
/// number of entries. Same defect as the old [`drv_heartbeat`]: a syscall
/// that reads `a0` can never be reached through `syscall0`.
///
/// Always returns 0.
pub fn trace_dump(count: u64) -> isize {
    unsafe { syscall1(SYS_TRACE_DUMP, count) }
}

/// Entry count [`trace_dump`] uses when passed 0 (`dispatch.rs:1051`).
pub const TRACE_DUMP_DEFAULT_COUNT: u64 = 50;

// ===========================================================================
//  Fast-path IPC (M02) — seL4-style register-passing, ≤32 bytes
// ===========================================================================

/// Maximum number of 64-bit words in a fast IPC message.
pub const FAST_IPC_MAX_WORDS: usize = 4;

/// Send a fast IPC message to `server_tid` and block until the reply arrives,
/// receiving the FULL four-word reply.
///
/// ABI (`SYS_IPC_FAST_CALL`, `dispatch.rs`, fast-IPC arms): a0 = server TID,
/// a1..a4 = up to 4 × u64 of request data (≤ 32 bytes). On success a0 =
/// reply\[0\] and a1..a3 = reply\[1..3\], delivered through `SyscallOut`
/// exactly like FAST_ACCEPT's request delivery. On failure a0 = -1 and
/// a1..a5 are untouched. The kernel touches no user memory — data travels in
/// registers both ways.
///
/// **WHY this cannot go through the shared `syscallN` helpers.** Same reason
/// as [`fast_ipc_accept_req`]: results land in argument registers, so they
/// must be declared `lateout` in a dedicated block. Routing this through
/// `syscall5` (whose `in("a1")`… operands rustc may assume intact) would be
/// undefined behaviour the moment the kernel writes the reply back.
///
/// **The first reply word and the error code share `a0`.** Success is
/// `reply[0]`, failure is `-1`, and nothing tags which is which — so a
/// reply\[0\] with bit 63 set is reported here as a failed call. Keep the
/// FIRST fast-IPC reply word in the non-negative `i64` range; the other
/// three are unconstrained.
///
/// Returns `None` when the kernel refused the call: `server_tid` is not a
/// live TID, equals the caller (self-deadlock), or all
/// [`FAST_IPC_MAX_SLOTS`]-many slots are busy. Fall back to [`chan_write`].
pub fn fast_ipc_call_full(
    server_tid: u32,
    words: [u64; FAST_IPC_MAX_WORDS],
) -> Option<[u64; FAST_IPC_MAX_WORDS]> {
    let ret: isize;
    let r1: u64;
    let r2: u64;
    let r3: u64;
    unsafe {
        asm!(
            "ecall",
            in("a7") SYS_IPC_FAST_CALL,
            inlateout("a0") server_tid as u64 => ret,
            inlateout("a1") words[0] => r1,
            inlateout("a2") words[1] => r2,
            inlateout("a3") words[2] => r3,
            // The kernel writes zeros into a4/a5 on success (SyscallOut
            // always writes all five); declared clobbered, values discarded.
            inlateout("a4") words[3] => _,
            lateout("a5") _,
            options(nostack),
        );
    }
    if ret < 0 { return None; }
    Some([ret as u64, r1, r2, r3])
}

/// [`fast_ipc_call_full`] for callers that only need the first reply word —
/// the historical shape of this API, kept because most exchanges answer with
/// a single word and the ergonomics matter at every call site.
pub fn fast_ipc_call(server_tid: u32, words: [u64; FAST_IPC_MAX_WORDS]) -> Option<u64> {
    fast_ipc_call_full(server_tid, words).map(|r| r[0])
}

/// Number of fast-IPC slots in the kernel (`FAST_IPC_MAX_SLOTS`,
/// `crates/ipc/src/fast_ipc.rs`). A slot index returned by
/// [`fast_ipc_accept`] is always below this.
pub const FAST_IPC_MAX_SLOTS: usize = 64;

/// Low bits of a fast-IPC handle that hold the slot index; the rest is the
/// generation tag. Mirrors `FAST_IPC_SLOT_MASK` in `crates/ipc/src/fast_ipc.rs`
/// — the two must agree, and the kernel side carries a compile-time assert
/// tying it to `FAST_IPC_MAX_SLOTS`.
pub const FAST_IPC_SLOT_MASK: u64 = (FAST_IPC_MAX_SLOTS as u64) - 1;

/// One accepted fast-IPC request, as the server sees it.
///
/// `handle` is what [`fast_ipc_reply`] takes; `caller_tid` and `words` are the
/// request itself. Read `delivered` before trusting the latter two.
pub struct FastRequest {
    /// **Opaque handle** to hand to [`fast_ipc_reply`], exactly as received.
    ///
    /// It is NOT a slot index: the kernel tags it with a per-slot generation
    /// so a handle from a retired exchange cannot land on the slot's next
    /// occupant. Pass it through untouched — masking it, sign-extending it, or
    /// reconstructing it from `slot` all reintroduce the bug the tag closes.
    pub handle: u64,
    /// Slot index, decoded from `handle` purely for logging and for the
    /// caller's own bookkeeping. **Never** hand this to [`fast_ipc_reply`].
    pub slot: usize,
    /// TID of the task that issued `SYS_IPC_FAST_CALL`.
    pub caller_tid: u32,
    /// The four request words, exactly as the client passed them.
    pub words: [u64; FAST_IPC_MAX_WORDS],
    /// **False means the kernel did not deliver the payload**, and
    /// `caller_tid` / `words` are meaningless — not zero, not stale data from
    /// a previous call, simply undelivered.
    ///
    /// The kernel side of the delivery is inert until the trap handler in
    /// `kernel/src/main.rs` calls `syscall_dispatch_out` and copies
    /// `SyscallOut::regs` into the live `TrapFrame`. Distinguishing "the
    /// handler was never migrated" from "the server got the wrong words" is
    /// the whole reason this flag exists rather than a silent zero: two
    /// different bugs that otherwise produce the same failing assertion.
    pub delivered: bool,
}

/// Pre-loaded into `a1` before the `ecall` and looked for on return.
///
/// A TID is a `u32` widened to 64 bits, so a delivered `a1` can never be
/// `u64::MAX`; seeing the sentinel come back therefore means, unambiguously,
/// that nothing wrote the register. The trap entry saves and restores the
/// whole register file, so an unwritten `a1` is preserved verbatim.
const FAST_ACCEPT_SENTINEL: u64 = u64::MAX;

/// Server: block until a client sends a fast IPC call to this TID, and
/// receive the request.
///
/// ABI (`SYS_IPC_FAST_ACCEPT`): takes no argument. On success a0 = slot
/// index, a1 = caller TID, a2..a5 = the four request words. On failure
/// a0 = -1 and a1..a5 are untouched.
///
/// **WHY this cannot go through the shared `syscallN` helpers.** Those pass
/// their arguments as `in("a1")`, `in("a2")`… — operands rustc may assume the
/// `asm!` block leaves intact. This is the only syscall whose *results* land
/// in those registers, so it needs its own block declaring them `lateout`.
/// Calling `syscall0` and reading a1..a5 afterwards would be undefined
/// behaviour, and the compiler is free to make it look like it works.
///
/// Returns `None` when nothing was pending (the kernel's bounded
/// spurious-wake retry ran out, which is indistinguishable from an empty
/// queue from ring 3 — see the `SYS_IPC_FAST_ACCEPT` arm).
pub fn fast_ipc_accept_req() -> Option<FastRequest> {
    let ret: isize;
    let caller: u64;
    let w0: u64;
    let w1: u64;
    let w2: u64;
    let w3: u64;
    unsafe {
        asm!(
            "ecall",
            in("a7") SYS_IPC_FAST_ACCEPT,
            lateout("a0") ret,
            inlateout("a1") FAST_ACCEPT_SENTINEL => caller,
            lateout("a2") w0,
            lateout("a3") w1,
            lateout("a4") w2,
            lateout("a5") w3,
            options(nostack),
        );
    }
    if ret < 0 {
        return None;
    }
    // The kernel guarantees a non-negative handle (bit 63 is always clear), so
    // the `ret < 0` test above already separates handle from error code.
    let handle = ret as u64;
    Some(FastRequest {
        handle,
        // Decoded here, from the one place that knows the layout, so callers
        // never re-derive it and drift from the kernel's encoding.
        slot: (handle & FAST_IPC_SLOT_MASK) as usize,
        caller_tid: caller as u32,
        words: [w0, w1, w2, w3],
        delivered: caller != FAST_ACCEPT_SENTINEL,
    })
}

/// Server: block until a client sends a fast IPC call to this TID, keeping
/// only the reply handle.
///
/// For servers that answer without reading the request (the impersonation
/// tests, and any echo that does not depend on the payload). Servers that need
/// the request want [`fast_ipc_accept_req`].
///
/// The value returned is the **opaque handle** for [`fast_ipc_reply`], not a
/// slot index — it carries a generation tag. Use
/// `handle & FAST_IPC_SLOT_MASK` if you need the index for logging.
pub fn fast_ipc_accept() -> Option<u64> {
    fast_ipc_accept_req().map(|r| r.handle)
}

/// Server: reply to a fast IPC call (non-blocking).
///
/// ABI (`SYS_IPC_FAST_REPLY`): a0 = the **handle** [`fast_ipc_accept`]
/// returned, a1..a4 = reply words. Only `words[0]` reaches the client — see
/// [`fast_ipc_call`].
///
/// Returns `0` on success, `-2` if the handle is **stale** (its exchange is
/// gone and the slot has since been recycled), `-1` for anything else.
///
/// Two gates sit behind this call, and `ipctest` asserts both halves of each:
///
///  * **Ownership.** The kernel takes the replier's identity from the
///    scheduler, never from a register, so replying to another task's exchange
///    cannot impersonate its server.
///  * **Generation.** The handle carries a per-slot tag, so a handle kept from
///    a finished exchange cannot deliver attacker-chosen words to whoever
///    occupies that slot next.
///
/// Pass `handle` through **untouched**: no mask, no sign extension, no
/// rebuilding it from `FastRequest::slot`.
pub fn fast_ipc_reply(handle: u64, words: [u64; FAST_IPC_MAX_WORDS]) -> isize {
    unsafe {
        syscall5(SYS_IPC_FAST_REPLY,
            handle,
            words[0], words[1], words[2], words[3])
    }
}

// ===========================================================================
//  Untyped shared memory (F00.4) — SYS_IPC_SHARE / _MAP / _UNSHARE
// ===========================================================================

/// Shared-memory access mode for [`ipc_share`]. Read-only.
pub const SHM_RO: u64 = 0;
/// Shared-memory access mode for [`ipc_share`]. Read-write.
pub const SHM_RW: u64 = 1;

/// Create a shared-memory region of `pages` 4 KiB pages.
///
/// ABI (`SYS_IPC_SHARE`, `dispatch.rs`): a0 = page count, a1 = perms, where
/// **any non-zero value means read-write** (the arm is `if a1 != 0`), not
/// just [`SHM_RW`]. Returns the region id, or `-1`.
///
/// The region is not mapped by this call — see [`ipc_map`].
pub fn ipc_share(pages: u64, perms: u64) -> isize {
    unsafe { syscall2(SYS_IPC_SHARE, pages, perms) }
}

/// Map a region created by [`ipc_share`] into the caller's address space.
///
/// ABI (`SYS_IPC_MAP`): a0 = region id → returns the user VA base, or
/// negative.
///
/// Two gates, both of which return a negative value rather than a distinct
/// errno, so test for `< 0`:
///   * **Creator only.** A task that did not create the region gets
///     [`E_PERM_DISPATCH`]; the id is a small integer into a 16-entry global
///     table, and walking it used to map every live region RW. Cross-task
///     sharing belongs to the typed [`shm_create_typed`] path, where the
///     right to map travels in an explicitly granted capability.
///   * **One mapping per (task, region).** A second `ipc_map` of a region
///     already mapped returns `-1`: the release path has exactly one VA slot
///     to tear down, so an untracked alias would outlive the refcount.
pub fn ipc_map(shm_id: u64) -> isize {
    unsafe { syscall1(SYS_IPC_MAP, shm_id) }
}

/// Delegate a capability this task holds to another live task.
///
/// ABI (`SYS_CAP_GRANT`): a0 = target TID, a1 = the caller's own wire cap
/// handle, a2 = requested permission bits for the copy. Returns the handle
/// **as it appears in the target's table** (> 0) — hand that value to the
/// target over IPC; the caller's own handle does not name the copy. Negative
/// return is `-Errno`:
///
///   * `-ECAPPERMS` — the source lacks `DUP`, or the request would amplify.
///     Re-delegation is opt-in: the copy carries `DUP` only if `want_perms`
///     asks for it, so the default outcome is a leaf cap.
///   * `-ESRCH`    — no live task by that TID.
///   * `-ENOSPC`   — the target's cap table is full, or it hit the
///     inbound-delegation quota.
///   * `-EINVAL`   — empty or out-of-range permission bits.
///
/// The grantor identity is bound in the kernel to the calling task; there is
/// no argument that can name a different grantor.
pub fn cap_grant(target_tid: u32, cap_handle: u32, want_perms: u8) -> isize {
    unsafe {
        syscall3(
            SYS_CAP_GRANT,
            target_tid as u64,
            cap_handle as u64,
            want_perms as u64,
        )
    }
}

/// Drop this task's reference to a shared region, unmapping it first.
///
/// ABI (`SYS_IPC_UNSHARE`): a0 = region id. Returns 0, or `-1` if the caller
/// holds no reference. The kernel tears the caller's own mapping down before
/// releasing the reference — dropping a reference while still holding PTEs
/// into the region is how the pages get handed to someone else underneath a
/// live writer.
///
/// Once the last reference goes, the pages return to the PMM and a later
/// [`ipc_map`] of the same id fails.
pub fn ipc_unshare(shm_id: u64) -> isize {
    unsafe { syscall1(SYS_IPC_UNSHARE, shm_id) }
}

/// Signal the io_ring async-completion path.
///
/// ABI (`SYS_IO_SUBMIT_ASYNC`, `dispatch.rs`): **reads no argument** — the
/// arm is `io_ring_signal_async(); 0`. It is a global nudge, not a per-ring
/// operation, and there is consequently no ownership gate to pass. Always
/// returns 0.
pub fn io_submit_async() -> isize {
    unsafe { syscall0(SYS_IO_SUBMIT_ASYNC) }
}

// ===========================================================================
//  Cap<T> typed syscalls (RFC-0003)
// ===========================================================================
//
// Every wrapper below takes a raw `CapHandle` (`u32`) minted by one of the
// `*_create_typed` calls or granted by the kernel's topology, and returns
// `-Errno` from `robot_os_abi::error::Errno` rather than the bare `-1`/`-99`
// pair the untyped families use. `ECAPSTALE` / `ECAPKIND` / `ECAPPERMS` are
// distinguishable from `EBADF` / `EAGAIN`, which is the whole point of the
// typed path — but the numeric values live in `crates/abi`, which this crate
// deliberately does not depend on, so assert on `< 0` unless you have looked
// the value up.
//
// The capability tables are indexed by **task pool slot**, not by TID
// (`crates/ipc/src/cap_store.rs`). TIDs are monotone and `MAX_TASKS` is 64,
// so a TID-indexed table made every typed syscall fail permanently once a
// long-lived board had created its 64th task. `ipctest` pushes past that
// boundary on purpose.

/// Typed channel write. `cap` needs `WRITE`.
pub fn chan_write_typed(cap: u32, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_CHAN_WRITE_TYPED, cap as u64, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Typed channel read. `cap` needs `READ`. Returns bytes read.
pub fn chan_read_typed(cap: u32, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_CHAN_READ_TYPED, cap as u64, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Allocate a port and mint a `Cap<Port>` for it in the caller's cap table.
///
/// Takes no argument; returns the raw cap handle (positive) or `-Errno`
/// (`EMFILE` when the port pool or the cap table is full).
pub fn port_create_typed() -> isize {
    unsafe { syscall0(SYS_PORT_CREATE_TYPED) }
}

/// Bytes a [`port_poll_typed`] event occupies. The kernel copies exactly this
/// many bytes and returns the count, so a smaller buffer is a fault, not a
/// short read.
pub const PORT_EVENT_BYTES: usize = 16;

/// Dequeue one event from a `Cap<Port>` into `out` (≥ [`PORT_EVENT_BYTES`]).
///
/// Returns [`PORT_EVENT_BYTES`] on success, or `-Errno` — `-EAGAIN` when the
/// queue is empty. Unlike [`port_wait`] this **does not block**.
pub fn port_poll_typed(cap: u32, out: &mut [u8]) -> isize {
    if out.len() < PORT_EVENT_BYTES {
        return E_INVAL;
    }
    unsafe { syscall2(SYS_PORT_POLL_TYPED, cap as u64, out.as_mut_ptr() as u64) }
}

/// Free the port behind a `Cap<Port>`.
///
/// The capability itself is **not** revoked — the handle survives as a stale
/// reference until the caller revokes it, which is what makes `ECAPSTALE`
/// observable rather than a use-after-free.
pub fn port_destroy_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_PORT_DESTROY_TYPED, cap as u64) }
}

/// Create a shared-memory region of `pages` pages with mode `perms`
/// ([`SHM_RO`] / [`SHM_RW`]) and mint a `Cap<Shm>` for it.
///
/// Returns the raw cap handle (positive) or `-Errno`. On cap-table exhaustion
/// the region is rolled back, so a failure never leaves a half-created
/// region behind.
pub fn shm_create_typed(pages: u64, perms: u64) -> isize {
    unsafe { syscall2(SYS_SHM_CREATE_TYPED, pages, perms) }
}

/// Bytes written by [`shm_acquire_typed`]: `page_count u32 LE`, `perms u8`
/// (0 = RO, 1 = RW), 3 bytes of padding.
pub const SHM_INFO_BYTES: usize = 8;

/// Take a reference on the region behind a `Cap<Shm>` and read its geometry
/// into `out` (≥ [`SHM_INFO_BYTES`]). Requires `READ`.
pub fn shm_acquire_typed(cap: u32, out: &mut [u8]) -> isize {
    if out.len() < SHM_INFO_BYTES {
        return E_INVAL;
    }
    unsafe { syscall2(SYS_SHM_ACQUIRE_TYPED, cap as u64, out.as_mut_ptr() as u64) }
}

/// Drop one reference to the region behind a `Cap<Shm>`; the backing pages go
/// back to the PMM when the count reaches zero. Requires `READ` — release is
/// paired with acquire, so any holder may drop its own reference.
pub fn shm_release_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_SHM_RELEASE_TYPED, cap as u64) }
}

/// Allocate an io_ring, mint a `Cap<IoRing>`, and write the ring page's
/// **physical** address as `u64` LE into `phys_out` (≥ 8 bytes).
///
/// Returns the raw cap handle (positive) or `-Errno`.
pub fn ioring_create_typed(phys_out: &mut [u8; 8]) -> isize {
    unsafe { syscall1(SYS_IORING_CREATE_TYPED, phys_out.as_mut_ptr() as u64) }
}

/// Process the pending SQEs on a `Cap<IoRing>`. Requires `WRITE`. Returns the
/// number of entries processed, or `-Errno`.
pub fn ioring_submit_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_IORING_SUBMIT_TYPED, cap as u64) }
}

/// Free an io_ring and its backing page. Requires `WRITE`. The capability is
/// not auto-revoked.
pub fn ioring_destroy_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_IORING_DESTROY_TYPED, cap as u64) }
}

/// Read the GPIO pin behind a `Cap<Gpio>`. Requires `READ`. Returns 0 or 1.
///
/// The cap's resource id *is* the pin number, so which pin a task may touch
/// is decided by the grant, not by an argument it chooses.
pub fn gpio_read_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_GPIO_READ_TYPED, cap as u64) }
}

/// Drive the GPIO pin behind a `Cap<Gpio>` (`val`'s low bit). Requires `WRITE`.
pub fn gpio_write_typed(cap: u32, val: u64) -> isize {
    unsafe { syscall2(SYS_GPIO_WRITE_TYPED, cap as u64, val) }
}

/// Set the direction of a `Cap<Gpio>` pin: 0 = input, 1 = output.
/// Requires `WRITE`.
pub fn gpio_set_dir_typed(cap: u32, output: u64) -> isize {
    unsafe { syscall2(SYS_GPIO_SET_DIR_TYPED, cap as u64, output) }
}

/// Largest transfer accepted by [`i2c_read_typed`] / [`i2c_write_typed`]
/// (`I2C_TYPED_MAX_BYTES`). Longer buffers are rejected, not clamped.
pub const I2C_TYPED_MAX_BYTES: usize = 256;

/// Read `buf.len()` bytes from register `reg` of a `Cap<I2c>` slave.
/// Requires `READ`. Returns bytes read.
pub fn i2c_read_typed(cap: u32, reg: u64, buf: &mut [u8]) -> isize {
    unsafe {
        syscall4(SYS_I2C_READ_TYPED, cap as u64, reg,
                 buf.as_mut_ptr() as u64, buf.len() as u64)
    }
}

/// Write `data` to a `Cap<I2c>` slave. Requires `WRITE`.
/// By I2C convention `data[0]` is the register address.
pub fn i2c_write_typed(cap: u32, data: &[u8]) -> isize {
    unsafe {
        syscall3(SYS_I2C_WRITE_TYPED, cap as u64,
                 data.as_ptr() as u64, data.len() as u64)
    }
}

/// Probe whether the `Cap<I2c>` slave ACKs. Requires `READ`.
/// Returns 1 (present) or 0 (absent).
pub fn i2c_detect_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_I2C_DETECT_TYPED, cap as u64) }
}

/// Start the PWM channel behind a `Cap<Pwm>`. Requires `WRITE`.
pub fn pwm_enable_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_PWM_ENABLE_TYPED, cap as u64) }
}

/// Stop the PWM channel behind a `Cap<Pwm>`. Requires `WRITE`.
pub fn pwm_disable_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_PWM_DISABLE_TYPED, cap as u64) }
}

/// Set the PWM period in **nanoseconds**. Requires `WRITE`.
pub fn pwm_set_period_typed(cap: u32, period_ns: u32) -> isize {
    unsafe { syscall2(SYS_PWM_SET_PERIOD_TYPED, cap as u64, period_ns as u64) }
}

/// Set the PWM duty in **nanoseconds**. Requires `WRITE`.
pub fn pwm_set_duty_typed(cap: u32, duty_ns: u32) -> isize {
    unsafe { syscall2(SYS_PWM_SET_DUTY_TYPED, cap as u64, duty_ns as u64) }
}

/// Set the PWM duty as a percentage (0..=100). Requires `WRITE`.
pub fn pwm_set_duty_pct_typed(cap: u32, pct: u32) -> isize {
    unsafe { syscall2(SYS_PWM_SET_DUTY_PCT_TYPED, cap as u64, pct as u64) }
}

/// Set the target speed of both wheels behind a `Cap<Motor>`.
///
/// ABI: the kernel reads the **low 16 bits** of a1/a2 as `i16`, so a speed
/// outside `i16` is silently truncated by the register, not rejected.
/// Requires `WRITE`.
pub fn motor_set_target_typed(cap: u32, speed_l: i16, speed_r: i16) -> isize {
    unsafe {
        syscall3(SYS_MOTOR_SET_TARGET_TYPED, cap as u64,
                 speed_l as u16 as u64, speed_r as u16 as u64)
    }
}

/// Bytes written by [`motor_tick_typed`]: `pwm_l i32 LE`, `pwm_r i32 LE`.
pub const MOTOR_TICK_BYTES: usize = 8;

/// Run one control tick on a `Cap<Motor>` and read the resulting PWM pair
/// into `out` (≥ [`MOTOR_TICK_BYTES`]). Requires `WRITE`.
/// Returns [`MOTOR_TICK_BYTES`] on success.
pub fn motor_tick_typed(cap: u32, ticks_l: i64, ticks_r: i64, now: u64, out: &mut [u8]) -> isize {
    if out.len() < MOTOR_TICK_BYTES {
        return E_INVAL;
    }
    unsafe {
        syscall5(SYS_MOTOR_TICK_TYPED, cap as u64,
                 ticks_l as u64, ticks_r as u64, now, out.as_mut_ptr() as u64)
    }
}

/// Enable (`1`) or disable (`0`) the motor behind a `Cap<Motor>`.
/// Requires `WRITE`.
pub fn motor_enable_typed(cap: u32, on: u64) -> isize {
    unsafe { syscall2(SYS_MOTOR_ENABLE_TYPED, cap as u64, on) }
}

/// Is the motor behind a `Cap<Motor>` enabled? Requires `READ`.
/// Returns 0 or 1.
pub fn motor_enabled_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_MOTOR_ENABLED_TYPED, cap as u64) }
}

/// Set the PID gains of a `Cap<Motor>`. Requires `WRITE`.
pub fn motor_set_gains_typed(cap: u32, kp: i32, ki: i32, kd: i32) -> isize {
    unsafe {
        syscall4(SYS_MOTOR_SET_GAINS_TYPED, cap as u64,
                 kp as u32 as u64, ki as u32 as u64, kd as u32 as u64)
    }
}

/// Reset the controller state behind a `Cap<Motor>`. Requires `WRITE`.
pub fn motor_reset_typed(cap: u32) -> isize {
    unsafe { syscall1(SYS_MOTOR_RESET_TYPED, cap as u64) }
}
