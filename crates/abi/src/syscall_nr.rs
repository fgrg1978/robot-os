//! Frozen syscall numbers.
//!
//! These are the **canonical** definitions from W1 onwards. The kernel
//! crate `robot_os_syscall` will re-export these in W3; for now they are
//! mirrored verbatim from `crates/syscall/src/numbers.rs` so that builds
//! continue to work while consumers migrate.
//!
//! Within ABI v1, no number can move and no new number is added without an
//! RFC.
//!
//! ## Number-space conventions
//!
//! | Range     | Subsystem                            |
//! |-----------|--------------------------------------|
//! | 0..=19    | Process control                      |
//! | 20..=29   | File I/O (basic)                     |
//! | 100..=119 | IPC (channels, fast-path, lease, SHM)|
//! | 200..=229 | GPIO / PWM / I2C                     |
//! | 230..=249 | Motor + system info                  |
//! | 250..=269 | Filesystem + network                 |
//! | 270..=299 | System control / disk / FDT          |
//! | 300..=319 | Driver-server (E11.AQ3)              |
//! | 320..=349 | Robot control + platform             |
//! | 350..=369 | Signals + pipes                      |
//! | 370..=389 | Sockets                              |
//! | 390..=399 | Service manager                      |
//! | 400..=429 | Memory mgmt + ADC + buzzer           |
//! | 430..=499 | Security (seccomp, future)           |
//! | 500..=529 | IO ring, channels, MMIO/IRQ, ports, handles, trace, drivers |

#![allow(missing_docs)]

// ── Process control (0..=19) ────────────────────────────────────────────
pub const SYS_TEST: u64 = 0;
pub const SYS_PUTCHAR: u64 = 1;
pub const SYS_GETCHAR: u64 = 2;
pub const SYS_EXIT: u64 = 3;
pub const SYS_GETPID: u64 = 10;
pub const SYS_YIELD: u64 = 11;
pub const SYS_FORK: u64 = 12;
pub const SYS_EXEC: u64 = 13;
pub const SYS_WAIT: u64 = 14;
pub const SYS_SLEEP: u64 = 15;
pub const SYS_EXECPATH: u64 = 16;

// ── File I/O (20..=29) ──────────────────────────────────────────────────
pub const SYS_OPEN: u64 = 20;
pub const SYS_CLOSE: u64 = 21;
pub const SYS_READ: u64 = 22;
pub const SYS_WRITE: u64 = 23;
pub const SYS_LSEEK: u64 = 24;

// ── IPC (100..=119) ─────────────────────────────────────────────────────
pub const SYS_IPC_CREATE: u64 = 100;
pub const SYS_IPC_SEND: u64 = 101;
pub const SYS_IPC_RECEIVE: u64 = 102;
pub const SYS_IPC_CALL: u64 = 103;
pub const SYS_IPC_REPLY: u64 = 104;
pub const SYS_IPC_SHARE: u64 = 105;
pub const SYS_IPC_UNSHARE: u64 = 106;
pub const SYS_IPC_DESTROY: u64 = 107;
pub const SYS_IPC_FAST_CALL: u64 = 108;
pub const SYS_IPC_FAST_REPLY: u64 = 109;
pub const SYS_IPC_FAST_ACCEPT: u64 = 110;
pub const SYS_IPC_LEASE_GRANT: u64 = 111;
pub const SYS_IPC_LEASE_ACCEPT: u64 = 112;
pub const SYS_IPC_LEASE_RETURN: u64 = 113;
pub const SYS_IPC_LEASE_FREE: u64 = 114;
pub const SYS_IPC_MAP: u64 = 115;
/// Delegate a capability the caller holds to another live task (K-C5 audit
/// tanda, ABI change approved 2026-08-22). a0=target_tid, a1=cap_handle,
/// a2=want_perms bits. Returns the wire handle as seen in the TARGET's table
/// (>0), or -Errno. Never amplifies; re-delegation only if the source carries
/// DUP and the caller asks for it. See `robot_os_ipc::cap_store::delegate`.
pub const SYS_CAP_GRANT: u64 = 116;

// ── GPIO / PWM / I2C / Motor / Sysinfo / FS / Net (200..=299) ──────────
pub const SYS_GPIO_READ: u64 = 200;
pub const SYS_GPIO_WRITE: u64 = 201;
pub const SYS_GPIO_MODE: u64 = 202;
pub const SYS_GPIO_INFO: u64 = 203;
pub const SYS_PWM_ENABLE: u64 = 210;
pub const SYS_PWM_DISABLE: u64 = 211;
pub const SYS_PWM_SET_FREQ: u64 = 212;
pub const SYS_PWM_SET_DUTY: u64 = 213;
pub const SYS_PWM_INFO: u64 = 214;
pub const SYS_I2C_READ: u64 = 220;
pub const SYS_I2C_WRITE: u64 = 221;
pub const SYS_I2C_SCAN: u64 = 222;
pub const SYS_I2C_INFO: u64 = 223;
pub const SYS_MOTOR_CREATE: u64 = 230;
pub const SYS_MOTOR_ENABLE: u64 = 231;
pub const SYS_MOTOR_SPEED: u64 = 232;
pub const SYS_MOTOR_ANGLE: u64 = 233;
pub const SYS_MOTOR_INFO: u64 = 234;
pub const SYS_MEMINFO: u64 = 240;
pub const SYS_TASKINFO: u64 = 241;
pub const SYS_UPTIME: u64 = 242;
pub const SYS_STAT: u64 = 250;
pub const SYS_READDIR: u64 = 251;
pub const SYS_MKDIR: u64 = 252;
pub const SYS_UNLINK: u64 = 253;
pub const SYS_CHDIR: u64 = 254;
pub const SYS_GETCWD: u64 = 255;
pub const SYS_MOUNT: u64 = 256;
pub const SYS_UMOUNT: u64 = 257;
pub const SYS_SYNC: u64 = 258;
pub const SYS_NET_INFO: u64 = 260;
pub const SYS_NET_GETIP: u64 = 261;
pub const SYS_NET_SETIP: u64 = 262;
pub const SYS_NET_PING: u64 = 263;
pub const SYS_NET_GETMAC: u64 = 264;
pub const SYS_NET_STATS: u64 = 265;
pub const SYS_DNS_RESOLVE: u64 = 266;
pub const SYS_NTP_SYNC: u64 = 267;
pub const SYS_NTP_OFFSET: u64 = 268;
pub const SYS_MCAST_JOIN: u64 = 269;
pub const SYS_SHUTDOWN: u64 = 270;
pub const SYS_REBOOT: u64 = 271;
pub const SYS_MCAST_LEAVE: u64 = 272;
pub const SYS_MCAST_SEND: u64 = 273;
pub const SYS_SECURE_INIT: u64 = 274;
pub const SYS_SECURE_SEND: u64 = 275;
pub const SYS_SECURE_RECV: u64 = 276;
pub const SYS_DISK_INFO: u64 = 280;
pub const SYS_DISK_READ: u64 = 281;
pub const SYS_DISK_WRITE: u64 = 282;
pub const SYS_DISK_SIZE: u64 = 283;
pub const SYS_FDT_INFO: u64 = 290;
pub const SYS_FDT_DUMP: u64 = 291;

// ── Driver-server / Robot / Platform / Signals / Pipes (300..=369) ─────
pub const SYS_DRV_REGISTER: u64 = 300;
pub const SYS_DRV_UNREGISTER: u64 = 301;
pub const SYS_DRV_MMAP: u64 = 302;
pub const SYS_DRV_MUNMAP: u64 = 303;
pub const SYS_DRV_IRQ_WAIT: u64 = 304;
pub const SYS_DRV_IRQ_ACK: u64 = 305;
pub const SYS_DRV_DMA_ALLOC: u64 = 306;
pub const SYS_DRV_DMA_FREE: u64 = 307;
pub const SYS_DRV_DMA_SYNC: u64 = 308;
pub const SYS_DRV_HEARTBEAT: u64 = 309;
pub const SYS_DRV_GET_DEVICE: u64 = 310;
/// `SYS_DRV_INVOKE` — userspace bridge into the RFC-0002 Driver
/// registry. Looks up the driver registered for `kind`, then calls
/// `handle_request(op, input, output)` on it via the `dyn Driver`
/// trait object.
///
/// Args:
/// - `a0 = kind` (DRV_KIND_*; matches `robot_os_driver_server`)
/// - `a1 = op`   (driver-defined op code, e.g. `UART_OP_WRITE`)
/// - `a2 = input_ptr`   (userspace; may be 0 if `input_len == 0`)
/// - `a3 = input_len`   (≤ [`DRIVER_INVOKE_MAX_INPUT_BYTES`])
/// - `a4 = output_ptr`  (userspace; may be 0 if `output_cap == 0`)
/// - `a5 = output_cap`  (≤ [`DRIVER_INVOKE_MAX_OUTPUT_BYTES`])
///
/// Returns the number of bytes written to `output` (≥ 0) on
/// success, or `-Errno`. The driver's [`DriverError`] is mapped
/// to the standard errno table (e.g. `BadOp → ENOSYS`,
/// `NotInitialized → ENODEV`, `BadInput → EINVAL`,
/// `BadOutput → ERANGE`, `Busy → EAGAIN`, `IoFault → EIO`,
/// `Unsupported → ENOSYS`, `NoMem → ENOMEM`).
pub const SYS_DRV_INVOKE: u64 = 311;
/// Upper bound on the input payload for [`SYS_DRV_INVOKE`].
/// Per-call stack buffer in the kernel handler — keep small so we
/// don't blow the syscall stack frame. Large transfers should use
/// the F15 zero-copy pipeline (separate syscall family).
pub const DRIVER_INVOKE_MAX_INPUT_BYTES: usize = 256;
/// Upper bound on the output payload for [`SYS_DRV_INVOKE`].
pub const DRIVER_INVOKE_MAX_OUTPUT_BYTES: usize = 256;
pub const SYS_ROBOT_INIT: u64 = 320;
pub const SYS_ROBOT_START: u64 = 321;
pub const SYS_ROBOT_STOP: u64 = 322;
pub const SYS_ROBOT_PAUSE: u64 = 323;
pub const SYS_ROBOT_RESUME: u64 = 324;
pub const SYS_ROBOT_ESTOP: u64 = 325;
pub const SYS_ROBOT_MOVE: u64 = 326;
pub const SYS_ROBOT_FORWARD: u64 = 327;
pub const SYS_ROBOT_ROTATE: u64 = 328;
pub const SYS_ROBOT_INFO: u64 = 329;
pub const SYS_SENSOR_INFO: u64 = 330;
pub const SYS_SENSOR_ADD: u64 = 331;
pub const SYS_SENSOR_READ: u64 = 332;
pub const SYS_PLATFORM_INFO: u64 = 340;
pub const SYS_PLATFORM_TYPE: u64 = 341;
pub const SYS_KILL: u64 = 350;
pub const SYS_SIGNAL: u64 = 351;
pub const SYS_SIGRETURN: u64 = 352;
pub const SYS_SIGPENDING: u64 = 353;
pub const SYS_SIGPROCMASK: u64 = 354;
pub const SYS_PAUSE: u64 = 355;
pub const SYS_ALARM: u64 = 356;
pub const SYS_PIPE: u64 = 360;
pub const SYS_DUP: u64 = 361;
pub const SYS_DUP2: u64 = 362;

// ── Sockets (370..=389) ─────────────────────────────────────────────────
pub const SYS_SOCKET: u64 = 370;
pub const SYS_BIND: u64 = 371;
pub const SYS_LISTEN: u64 = 372;
pub const SYS_ACCEPT: u64 = 373;
pub const SYS_CONNECT: u64 = 374;
pub const SYS_SEND: u64 = 375;
pub const SYS_RECV: u64 = 376;
pub const SYS_SENDTO: u64 = 377;
pub const SYS_RECVFROM: u64 = 378;
pub const SYS_SOCK_SHUTDOWN: u64 = 379;
pub const SYS_GETSOCKNAME: u64 = 380;
pub const SYS_GETPEERNAME: u64 = 381;

// ── Service manager (390..=399) ─────────────────────────────────────────
pub const SYS_SERVICE_REGISTER: u64 = 390;
pub const SYS_SERVICE_UNREGISTER: u64 = 391;
pub const SYS_SERVICE_DISCOVER: u64 = 392;
pub const SYS_SERVICE_HEARTBEAT: u64 = 393;
pub const SYS_SERVICE_LIST: u64 = 394;
pub const SYS_SERVICE_INFO: u64 = 395;
pub const SYS_SERVICE_START: u64 = 396;
pub const SYS_SERVICE_STOP: u64 = 397;

// ── Memory + misc (400..=429) ───────────────────────────────────────────
pub const SYS_BRK: u64 = 400;
pub const SYS_MMAP: u64 = 401;
pub const SYS_MUNMAP: u64 = 402;
pub const SYS_FORK_COW: u64 = 403;
pub const SYS_ALLOC_DEMAND: u64 = 404;
pub const SYS_ADC_READ: u64 = 410;
pub const SYS_BUZZER_TONE: u64 = 420;
pub const SYS_BUZZER_OFF: u64 = 421;

// ── Security (430..=499) ────────────────────────────────────────────────
pub const SYS_SECCOMP: u64 = 430;

// ── IO ring / channels / MMIO / IRQ / ports / handles / trace (500..=529)
pub const SYS_IO_SETUP: u64 = 503;
pub const SYS_IO_SUBMIT: u64 = 504;
pub const SYS_IO_WAIT: u64 = 505;
pub const SYS_CHAN_CREATE: u64 = 506;
pub const SYS_CHAN_WRITE: u64 = 507;
pub const SYS_CHAN_READ: u64 = 508;
pub const SYS_MMIO_MAP: u64 = 509;
pub const SYS_IRQ_BIND: u64 = 510;
pub const SYS_PORT_CREATE: u64 = 511;
pub const SYS_PORT_BIND: u64 = 512;
pub const SYS_PORT_WAIT: u64 = 513;
pub const SYS_PORT_UNBIND: u64 = 514;
pub const SYS_HANDLE_GRANT: u64 = 515;
pub const SYS_HANDLE_REVOKE: u64 = 516;
pub const SYS_HANDLE_DUP: u64 = 517;
pub const SYS_TRACE_DUMP: u64 = 518;
pub const SYS_IO_SUBMIT_ASYNC: u64 = 519;

// ── Driver framework (520..=527) ────────────────────────────────────────
pub const SYS_DRIVER_REGISTER: u64 = 520;
pub const SYS_DRIVER_UNREGISTER: u64 = 521;
pub const SYS_DRIVER_POLL_EVENT: u64 = 522;
pub const SYS_DRIVER_FETCH_REQ: u64 = 523;
pub const SYS_DRIVER_REPLY: u64 = 524;
pub const SYS_DRIVER_REQUEST: u64 = 525;
pub const SYS_DRIVER_TRY_REPLY: u64 = 526;
pub const SYS_DRIVER_STATS: u64 = 527;

// ── Cap-typed IPC (528..=549) — RFC-0003 W3+ ────────────────────────────
/// `SYS_CHAN_WRITE_TYPED` — `Cap<Channel>` typed channel write.
///
/// `a0 = cap_handle (u32)`, `a1 = data_ptr`, `a2 = len`.
///
/// Returns `0` on success or `-Errno`. Distinguishes
/// `ECAPSTALE` / `ECAPKIND` / `ECAPPERMS` from `EBADF` / `EAGAIN`.
pub const SYS_CHAN_WRITE_TYPED: u64 = 528;
/// `SYS_CHAN_READ_TYPED` — `Cap<Channel>` typed channel read.
pub const SYS_CHAN_READ_TYPED: u64 = 529;

/// `SYS_PORT_CREATE_TYPED` — allocates a port + grants a `Cap<Port>`
/// into the calling task's cap-table.
///
/// No args. Returns the raw `CapHandle` as `i64` (positive) on
/// success, or `-Errno` on failure.
pub const SYS_PORT_CREATE_TYPED: u64 = 530;
/// `SYS_PORT_POLL_TYPED` — `Cap<Port>` typed event dequeue.
///
/// `a0 = cap_handle (u32)`, `a1 = out_ptr (PortEvent buffer, 16 B)`.
/// Returns the number of bytes copied (16) on success, or `-Errno`.
pub const SYS_PORT_POLL_TYPED: u64 = 531;
/// `SYS_PORT_DESTROY_TYPED` — `Cap<Port>` typed port destruction.
///
/// `a0 = cap_handle`. Frees the port slot but does **not**
/// auto-revoke the cap — userspace must do that separately if it
/// wants the slot back.
pub const SYS_PORT_DESTROY_TYPED: u64 = 532;

/// `SYS_SHM_CREATE_TYPED` — allocates a shared-memory region with
/// `a0 = page_count` pages and access mode `a1` (0=ReadOnly,
/// 1=ReadWrite). Mints a `Cap<Shm>` into the caller's cap-table.
///
/// Returns the raw `CapHandle` as `i64` (positive) on success, or
/// `-Errno` (`ENOMEM`, `EINVAL`, `EMFILE`) on failure. On cap-table
/// exhaustion the region is rolled back so callers never observe a
/// half-created region.
pub const SYS_SHM_CREATE_TYPED: u64 = 533;
/// `SYS_SHM_ACQUIRE_TYPED` — bumps the refcount of the region
/// referenced by `a0 = cap_handle`. `a1 = out_ptr` receives an 8-byte
/// blob: `page_count u32 LE`, `perms u8` (0=RO,1=RW), 3 bytes pad.
///
/// Requires `READ` on the cap.
pub const SYS_SHM_ACQUIRE_TYPED: u64 = 534;
/// `SYS_SHM_RELEASE_TYPED` — decrements the region refcount;
/// frees all backing pages when it reaches zero.
///
/// `a0 = cap_handle`. Requires `READ` (release is paired with
/// acquire — any holder may drop its own ref). The cap is **not**
/// auto-revoked; call `cap_store::revoke` separately to free the slot.
pub const SYS_SHM_RELEASE_TYPED: u64 = 535;

/// `SYS_IORING_CREATE_TYPED` — allocates an io_ring and mints a
/// `Cap<IoRing>` into the caller's cap-table.
///
/// `a0 = phys_out_ptr` (userspace pointer, 8 bytes — receives the
/// physical address of the ring page as `u64 LE`; userspace maps it
/// via `sys_mmap` or equivalent).
///
/// Returns the raw `CapHandle` as `i64` (positive) on success, or
/// `-Errno` (`ENOMEM`, `EMFILE`, `EFAULT`). On cap-table
/// exhaustion the ring is rolled back so callers never observe a
/// half-created ring.
pub const SYS_IORING_CREATE_TYPED: u64 = 536;
/// `SYS_IORING_SUBMIT_TYPED` — process SQEs on the ring referenced
/// by `a0 = cap_handle`. Returns the number of SQEs processed
/// (`u32`, sign-extended into `i64` ≥ 0) or `-Errno`. Requires
/// `WRITE` on the cap.
pub const SYS_IORING_SUBMIT_TYPED: u64 = 537;
/// `SYS_IORING_DESTROY_TYPED` — frees the io_ring + its backing
/// page. `a0 = cap_handle`. Requires `WRITE`. The cap is **not**
/// auto-revoked.
pub const SYS_IORING_DESTROY_TYPED: u64 = 538;

/// `SYS_GPIO_READ_TYPED` — read a GPIO pin via `Cap<Gpio>`.
/// `a0 = cap_handle`. Returns 0 or 1 on success, or `-Errno`.
/// Requires `READ` permission on the cap. The cap's resource_id
/// is the pin number; topology grant determines which pin a task
/// may touch.
pub const SYS_GPIO_READ_TYPED: u64 = 539;
/// `SYS_GPIO_WRITE_TYPED` — drive a GPIO pin. `a0 = cap_handle`,
/// `a1 = val` (low bit only). Returns 0 or `-Errno`. Requires `WRITE`.
pub const SYS_GPIO_WRITE_TYPED: u64 = 540;
/// `SYS_GPIO_SET_DIR_TYPED` — set pin direction. `a0 = cap_handle`,
/// `a1 = 0` (input) or `1` (output). Returns 0 or `-Errno`.
/// Requires `WRITE`.
pub const SYS_GPIO_SET_DIR_TYPED: u64 = 541;

/// `SYS_I2C_READ_TYPED` — read from a Cap<I2c> slave.
/// `a0 = cap`, `a1 = reg`, `a2 = buf_ptr (user)`, `a3 = buf_len`.
/// Returns bytes read (≥ 0) or `-Errno`. Requires `READ`.
pub const SYS_I2C_READ_TYPED: u64 = 542;
/// `SYS_I2C_WRITE_TYPED` — write to a Cap<I2c> slave.
/// `a0 = cap`, `a1 = data_ptr (user)`, `a2 = data_len`.
/// `data[0]` is by I2C convention the register address.
/// Returns 0 or `-Errno`. Requires `WRITE`.
pub const SYS_I2C_WRITE_TYPED: u64 = 543;
/// `SYS_I2C_DETECT_TYPED` — probe whether the slave ACKs.
/// `a0 = cap`. Returns 1 (present) / 0 (absent), or `-Errno`.
/// Requires `READ`.
pub const SYS_I2C_DETECT_TYPED: u64 = 544;

/// Per-call buffer cap for [`SYS_I2C_READ_TYPED`] and
/// [`SYS_I2C_WRITE_TYPED`]. Mirrors the DRIVER_INVOKE caps —
/// larger transfers should use the F15 zero-copy pipeline.
pub const I2C_TYPED_MAX_BYTES: usize = 256;

/// `SYS_PWM_ENABLE_TYPED` — start the PWM channel referenced by
/// `Cap<Pwm>`. `a0 = cap`. Returns 0 or `-Errno`. Requires `WRITE`.
pub const SYS_PWM_ENABLE_TYPED: u64 = 545;
/// `SYS_PWM_DISABLE_TYPED` — stop. `a0 = cap`. Requires `WRITE`.
pub const SYS_PWM_DISABLE_TYPED: u64 = 546;
/// `SYS_PWM_SET_PERIOD_TYPED` — `a0 = cap`, `a1 = period_ns`
/// (u32 nanoseconds). Requires `WRITE`.
pub const SYS_PWM_SET_PERIOD_TYPED: u64 = 547;
/// `SYS_PWM_SET_DUTY_TYPED` — `a0 = cap`, `a1 = duty_ns` (u32).
/// Requires `WRITE`.
pub const SYS_PWM_SET_DUTY_TYPED: u64 = 548;
/// `SYS_PWM_SET_DUTY_PCT_TYPED` — `a0 = cap`, `a1 = pct`
/// (0..=100). Requires `WRITE`.
pub const SYS_PWM_SET_DUTY_PCT_TYPED: u64 = 549;

// ── Cap-typed Phase-1 extension (550..=569) — RFC-0003 W5 batch 5.4+
// The original cap-typed range 528..=549 filled at PWM (5.3). The
// hardware-cap families that don't fit (Motor, Sensor, future
// ESC/Lidar) take this second slot. Strictly speaking allocating
// new typed-syscall ranges needs an RFC-0003 amendment; we
// document it here as the de-facto reservation and the amendment
// is queued as a follow-up.
/// `SYS_MOTOR_SET_TARGET_TYPED` — `a0 = cap`, `a1 = speed_l u16`
/// (low half = signed i16), `a2 = speed_r u16`. Requires `WRITE` — and,
/// since 2026-08-24 (`Cap<Motor>` per-motor granularity, RFC-0003 P1), the
/// caller's cap table must hold `WRITE` on **both** `Motor(0)` and
/// `Motor(1)`, not just the wheel named by `cap`: this syscall actuates the
/// shared drivetrain PID loop for both wheels at once. See
/// `crates/ipc/src/motor_cap.rs::require_pair_write`.
pub const SYS_MOTOR_SET_TARGET_TYPED: u64 = 550;
/// `SYS_MOTOR_TICK_TYPED` — `a0 = cap`, `a1 = ticks_l u64` (cast
/// from i64), `a2 = ticks_r u64`, `a3 = now u64`,
/// `a4 = out_ptr` (8 bytes: pwm_l i32 LE, pwm_r i32 LE).
/// Returns 8 (bytes written) or `-Errno`. Requires `WRITE` on both
/// `Motor(0)` and `Motor(1)` — same pair rule as 550; see
/// `crates/ipc/src/motor_cap.rs::require_pair_write`.
pub const SYS_MOTOR_TICK_TYPED: u64 = 551;
/// `SYS_MOTOR_ENABLE_TYPED` — `a0 = cap`, `a1 = 0|1`. Requires `WRITE` on
/// both `Motor(0)` and `Motor(1)` — same pair rule as 550; see
/// `crates/ipc/src/motor_cap.rs::require_pair_write`.
pub const SYS_MOTOR_ENABLE_TYPED: u64 = 552;
/// `SYS_MOTOR_ENABLED_TYPED` — `a0 = cap`. Returns 0|1 or `-Errno`.
/// Requires `READ`. NOT pair-wide: unlike its WRITE siblings, this is a
/// read of shared state and only needs the single cap named by `a0` — see
/// `crates/ipc/src/motor_cap.rs`'s module doc for why this one syscall is
/// the exception.
pub const SYS_MOTOR_ENABLED_TYPED: u64 = 553;
/// `SYS_MOTOR_SET_GAINS_TYPED` — `a0 = cap`, `a1 = kp` (i32 in
/// low u32), `a2 = ki`, `a3 = kd`. Requires `WRITE` on both `Motor(0)` and
/// `Motor(1)` — same pair rule as 550; see
/// `crates/ipc/src/motor_cap.rs::require_pair_write`.
pub const SYS_MOTOR_SET_GAINS_TYPED: u64 = 554;
/// `SYS_MOTOR_RESET_TYPED` — `a0 = cap`. Requires `WRITE` on both
/// `Motor(0)` and `Motor(1)` — same pair rule as 550; see
/// `crates/ipc/src/motor_cap.rs::require_pair_write`.
pub const SYS_MOTOR_RESET_TYPED: u64 = 555;

/// Width in bytes of the SYS_MOTOR_TICK_TYPED output blob
/// (pwm_l i32 LE + pwm_r i32 LE).
pub const MOTOR_TICK_OUT_BYTES: usize = 8;

/// Reserved upper bound. Numbers ≥ this are unallocated; new syscalls
/// must increment this and request an RFC. The 528..=549 range is
/// reserved for cap-typed migrations of the IPC family (W5);
/// 550..=569 extends it for the hardware caps that didn't fit
/// (W5 batches 5.4+).
pub const SYS_NR_RESERVED_UPPER: u64 = 600;
