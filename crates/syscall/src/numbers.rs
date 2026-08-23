/// Syscall numbers — direct port of kernel/include/syscall.h

pub const SYS_TEST:       u64 = 0;
pub const SYS_PUTCHAR:    u64 = 1;
pub const SYS_GETCHAR:    u64 = 2;
pub const SYS_EXIT:       u64 = 3;
pub const SYS_GETPID:     u64 = 10;
pub const SYS_YIELD:      u64 = 11;
pub const SYS_FORK:       u64 = 12;
pub const SYS_EXEC:       u64 = 13;
pub const SYS_WAIT:       u64 = 14;
pub const SYS_SLEEP:      u64 = 15;
/// Execute ELF from a path on the filesystem (a0 = path_ptr, a1 = 0).
pub const SYS_EXECPATH:   u64 = 16;

// File I/O
pub const SYS_OPEN:       u64 = 20;
pub const SYS_CLOSE:      u64 = 21;
pub const SYS_READ:       u64 = 22;
pub const SYS_WRITE:      u64 = 23;
pub const SYS_LSEEK:      u64 = 24;

// IPC
pub const SYS_IPC_CREATE:  u64 = 100;
pub const SYS_IPC_SEND:    u64 = 101;
pub const SYS_IPC_RECEIVE: u64 = 102;
pub const SYS_IPC_CALL:    u64 = 103;
pub const SYS_IPC_REPLY:   u64 = 104;
pub const SYS_IPC_SHARE:   u64 = 105;
pub const SYS_IPC_UNSHARE: u64 = 106;
pub const SYS_IPC_DESTROY: u64 = 107;
/// Map a shared memory region (created by SYS_IPC_SHARE) into the calling
/// process's address space.  a0 = shm_id → returns VA base or -1.
pub const SYS_IPC_MAP:     u64 = 115;
// Cross-task cap delegation (frozen in crates/abi/src/syscall_nr.rs).
pub const SYS_CAP_GRANT:   u64 = 116;

// Fast-path IPC (M02): register-passing, ≤32 bytes, zero-copy
pub const SYS_IPC_FAST_CALL:   u64 = 108; // client: a0=server_tid, a1..a4=data words
pub const SYS_IPC_FAST_REPLY:  u64 = 109; // server: a0=slot_idx, a1..a4=reply words
pub const SYS_IPC_FAST_ACCEPT: u64 = 110; // server: blocks until FAST_CALL arrives

// Lease-based IPC (M04): zero-copy large transfer via SHM time-bounded grant
pub const SYS_IPC_LEASE_GRANT:  u64 = 111; // a0=shm_id, a1=lessee_tid, a2=expire_ticks (0=∞)
pub const SYS_IPC_LEASE_ACCEPT: u64 = 112; // a0=lessor_tid → returns lease_id,shm_id
pub const SYS_IPC_LEASE_RETURN: u64 = 113; // a0=lease_id
pub const SYS_IPC_LEASE_FREE:   u64 = 114; // a0=lease_id (lessor cleanup)

// GPIO
pub const SYS_GPIO_READ:   u64 = 200;
pub const SYS_GPIO_WRITE:  u64 = 201;
pub const SYS_GPIO_MODE:   u64 = 202;
pub const SYS_GPIO_INFO:   u64 = 203;

// PWM
pub const SYS_PWM_ENABLE:   u64 = 210;
pub const SYS_PWM_DISABLE:  u64 = 211;
pub const SYS_PWM_SET_FREQ: u64 = 212;
pub const SYS_PWM_SET_DUTY: u64 = 213;
pub const SYS_PWM_INFO:     u64 = 214;

// I2C
pub const SYS_I2C_READ:  u64 = 220;
pub const SYS_I2C_WRITE: u64 = 221;
pub const SYS_I2C_SCAN:  u64 = 222;
pub const SYS_I2C_INFO:  u64 = 223;

// Motor
pub const SYS_MOTOR_CREATE: u64 = 230;
pub const SYS_MOTOR_ENABLE: u64 = 231;
pub const SYS_MOTOR_SPEED:  u64 = 232;
pub const SYS_MOTOR_ANGLE:  u64 = 233;
pub const SYS_MOTOR_INFO:   u64 = 234;

// System info
pub const SYS_MEMINFO:  u64 = 240;
pub const SYS_TASKINFO: u64 = 241;
pub const SYS_UPTIME:   u64 = 242;

// Filesystem
pub const SYS_STAT:    u64 = 250;
pub const SYS_READDIR: u64 = 251;
pub const SYS_MKDIR:   u64 = 252;
pub const SYS_UNLINK:  u64 = 253;
pub const SYS_CHDIR:   u64 = 254;
pub const SYS_GETCWD:  u64 = 255;
pub const SYS_MOUNT:   u64 = 256;
pub const SYS_UMOUNT:  u64 = 257;
pub const SYS_SYNC:    u64 = 258;

// Network
pub const SYS_NET_INFO:   u64 = 260;
pub const SYS_NET_GETIP:  u64 = 261;
pub const SYS_NET_SETIP:  u64 = 262;
pub const SYS_NET_PING:   u64 = 263;
pub const SYS_NET_GETMAC: u64 = 264;
pub const SYS_NET_STATS:  u64 = 265;
pub const SYS_DNS_RESOLVE: u64 = 266;
pub const SYS_NTP_SYNC:   u64 = 267;
pub const SYS_NTP_OFFSET: u64 = 268;
pub const SYS_MCAST_JOIN: u64 = 269;

// System control
pub const SYS_SHUTDOWN:   u64 = 270;
pub const SYS_REBOOT:     u64 = 271;
pub const SYS_MCAST_LEAVE: u64 = 272;
pub const SYS_MCAST_SEND: u64 = 273;
pub const SYS_SECURE_INIT: u64 = 274;
pub const SYS_SECURE_SEND: u64 = 275;
pub const SYS_SECURE_RECV: u64 = 276;

// Disk
pub const SYS_DISK_INFO:  u64 = 280;
pub const SYS_DISK_READ:  u64 = 281;
pub const SYS_DISK_WRITE: u64 = 282;
pub const SYS_DISK_SIZE:  u64 = 283;

// FDT
pub const SYS_FDT_INFO: u64 = 290;
pub const SYS_FDT_DUMP: u64 = 291;

// Driver server
pub const SYS_DRV_REGISTER:   u64 = 300;
pub const SYS_DRV_UNREGISTER: u64 = 301;
pub const SYS_DRV_MMAP:       u64 = 302;
pub const SYS_DRV_MUNMAP:     u64 = 303;
pub const SYS_DRV_IRQ_WAIT:   u64 = 304;
pub const SYS_DRV_IRQ_ACK:    u64 = 305;
pub const SYS_DRV_DMA_ALLOC:  u64 = 306;
pub const SYS_DRV_DMA_FREE:   u64 = 307;
pub const SYS_DRV_DMA_SYNC:   u64 = 308;
pub const SYS_DRV_HEARTBEAT:  u64 = 309;
pub const SYS_DRV_GET_DEVICE: u64 = 310;

// Robot control
pub const SYS_ROBOT_INIT:   u64 = 320;
pub const SYS_ROBOT_START:  u64 = 321;
pub const SYS_ROBOT_STOP:   u64 = 322;
pub const SYS_ROBOT_PAUSE:  u64 = 323;
pub const SYS_ROBOT_RESUME: u64 = 324;
pub const SYS_ROBOT_ESTOP:  u64 = 325;
pub const SYS_ROBOT_MOVE:   u64 = 326;
pub const SYS_ROBOT_FORWARD: u64 = 327;
pub const SYS_ROBOT_ROTATE: u64 = 328;
pub const SYS_ROBOT_INFO:   u64 = 329;
pub const SYS_SENSOR_INFO:  u64 = 330;
pub const SYS_SENSOR_ADD:   u64 = 331;
pub const SYS_SENSOR_READ:  u64 = 332;

// Platform
pub const SYS_PLATFORM_INFO: u64 = 340;
pub const SYS_PLATFORM_TYPE: u64 = 341;

// Signals
pub const SYS_KILL:        u64 = 350;
pub const SYS_SIGNAL:      u64 = 351;
pub const SYS_SIGRETURN:   u64 = 352;
pub const SYS_SIGPENDING:  u64 = 353;
pub const SYS_SIGPROCMASK: u64 = 354;
pub const SYS_PAUSE:       u64 = 355;
pub const SYS_ALARM:       u64 = 356;

// Pipes
pub const SYS_PIPE: u64 = 360;
pub const SYS_DUP:  u64 = 361;
pub const SYS_DUP2: u64 = 362;

// Sockets
pub const SYS_SOCKET:      u64 = 370;
pub const SYS_BIND:        u64 = 371;
pub const SYS_LISTEN:      u64 = 372;
pub const SYS_ACCEPT:      u64 = 373;
pub const SYS_CONNECT:     u64 = 374;
pub const SYS_SEND:        u64 = 375;
pub const SYS_RECV:        u64 = 376;
pub const SYS_SENDTO:      u64 = 377;
pub const SYS_RECVFROM:    u64 = 378;
pub const SYS_SOCK_SHUTDOWN: u64 = 379;
pub const SYS_GETSOCKNAME: u64 = 380;
pub const SYS_GETPEERNAME: u64 = 381;

// Memory management (Phase 7+)
pub const SYS_BRK:    u64 = 400;
pub const SYS_MMAP:   u64 = 401;
pub const SYS_MUNMAP: u64 = 402;
// E11 / AQ9 — explicit Copy-on-Write fork.  Semantically identical to
// SYS_FORK today (SYS_FORK already uses fork_cow under the hood), but
// exposed separately so userspace can opt-in or probe for COW support.
pub const SYS_FORK_COW: u64 = 403;
// E11 / AQ10 — reserve a virtual range without allocating physical pages.
// a0 = size in bytes; returns base VA or -1.  Pages materialize on first
// access via the demand-paging fault handler.
pub const SYS_ALLOC_DEMAND: u64 = 404;

// ADC
pub const SYS_ADC_READ: u64 = 410;

// Buzzer
pub const SYS_BUZZER_TONE: u64 = 420;
pub const SYS_BUZZER_OFF:  u64 = 421;

// Security (AQ11)
/// Activate syscall filter for current task (one-way — cannot be disabled).
pub const SYS_SECCOMP: u64 = 430;

// IO Ring (AQ1 + M05)
pub const SYS_IO_SETUP:        u64 = 503;
pub const SYS_IO_SUBMIT:       u64 = 504; // synchronous: process SQEs in syscall
pub const SYS_IO_WAIT:         u64 = 505;
pub const SYS_IO_SUBMIT_ASYNC: u64 = 519; // M05: non-blocking submit, worker processes

// Channels (AQ1)
pub const SYS_CHAN_CREATE: u64 = 506;
pub const SYS_CHAN_WRITE:  u64 = 507;
pub const SYS_CHAN_READ:   u64 = 508;

// MMIO/IRQ mapping (AQ1)
pub const SYS_MMIO_MAP: u64 = 509;
pub const SYS_IRQ_BIND: u64 = 510;

// Ports (AQ5)
pub const SYS_PORT_CREATE: u64 = 511;
pub const SYS_PORT_BIND:   u64 = 512;
pub const SYS_PORT_WAIT:   u64 = 513;
pub const SYS_PORT_UNBIND: u64 = 514;

// Handles (AQ6)
pub const SYS_HANDLE_GRANT:  u64 = 515;
pub const SYS_HANDLE_REVOKE: u64 = 516;
pub const SYS_HANDLE_DUP:    u64 = 517;

// Trace (AQ8)
pub const SYS_TRACE_DUMP: u64 = 518;

// Service manager
pub const SYS_SERVICE_REGISTER:   u64 = 390;
pub const SYS_SERVICE_UNREGISTER: u64 = 391;
pub const SYS_SERVICE_DISCOVER:   u64 = 392;
pub const SYS_SERVICE_HEARTBEAT:  u64 = 393;
pub const SYS_SERVICE_LIST:       u64 = 394;
pub const SYS_SERVICE_INFO:       u64 = 395;
pub const SYS_SERVICE_START:      u64 = 396;
pub const SYS_SERVICE_STOP:       u64 = 397;

// E11.AQ3 — userspace driver framework (520+ — above existing handle/port range).
pub const SYS_DRIVER_REGISTER:    u64 = 520;
pub const SYS_DRIVER_UNREGISTER:  u64 = 521;
pub const SYS_DRIVER_POLL_EVENT:  u64 = 522;
pub const SYS_DRIVER_FETCH_REQ:   u64 = 523;
pub const SYS_DRIVER_REPLY:       u64 = 524;
pub const SYS_DRIVER_REQUEST:     u64 = 525;
pub const SYS_DRIVER_TRY_REPLY:   u64 = 526;
pub const SYS_DRIVER_STATS:       u64 = 527;

// PHANES Phase 1 W3 — Cap<T> typed IPC (RFC-0003).
// Numbers must match `robot_os_abi::syscall_nr::SYS_*_TYPED`.
pub const SYS_CHAN_WRITE_TYPED:   u64 = 528;
pub const SYS_CHAN_READ_TYPED:    u64 = 529;

// PHANES Phase 1 W5 — Cap<Port> typed port API.
// RFC-0002 Driver registry bridge — userspace invokes a driver by
// (kind, op) and the kernel routes through `dyn Driver`.
pub const SYS_DRV_INVOKE:         u64 = 311;

pub const SYS_PORT_CREATE_TYPED:  u64 = 530;
pub const SYS_PORT_POLL_TYPED:    u64 = 531;
pub const SYS_PORT_DESTROY_TYPED: u64 = 532;

// PHANES Phase 1 W5 batch 2 — Cap<Shm> typed shared-memory API.
pub const SYS_SHM_CREATE_TYPED:   u64 = 533;
pub const SYS_SHM_ACQUIRE_TYPED:  u64 = 534;
pub const SYS_SHM_RELEASE_TYPED:  u64 = 535;

// PHANES Phase 1 W5 batch 3 — Cap<IoRing> typed io_ring API.
pub const SYS_IORING_CREATE_TYPED:  u64 = 536;
pub const SYS_IORING_SUBMIT_TYPED:  u64 = 537;
pub const SYS_IORING_DESTROY_TYPED: u64 = 538;

// PHANES Phase 1 W5 batch 5.1 — Cap<Gpio> typed hardware API.
pub const SYS_GPIO_READ_TYPED:      u64 = 539;
pub const SYS_GPIO_WRITE_TYPED:     u64 = 540;
pub const SYS_GPIO_SET_DIR_TYPED:   u64 = 541;

// PHANES Phase 1 W5 batch 5.2 — Cap<I2c> typed hardware API.
pub const SYS_I2C_READ_TYPED:       u64 = 542;
pub const SYS_I2C_WRITE_TYPED:      u64 = 543;
pub const SYS_I2C_DETECT_TYPED:     u64 = 544;

// PHANES Phase 1 W5 batch 5.3 — Cap<Pwm> typed hardware API.
// Fills the last 5 slots of the 528..=549 cap-typed range.
pub const SYS_PWM_ENABLE_TYPED:       u64 = 545;
pub const SYS_PWM_DISABLE_TYPED:      u64 = 546;
pub const SYS_PWM_SET_PERIOD_TYPED:   u64 = 547;
pub const SYS_PWM_SET_DUTY_TYPED:     u64 = 548;
pub const SYS_PWM_SET_DUTY_PCT_TYPED: u64 = 549;

// PHANES Phase 1 W5 batch 5.4 — Cap<Motor> typed hardware API.
// Opens the cap-typed extension range 550..=569.
pub const SYS_MOTOR_SET_TARGET_TYPED: u64 = 550;
pub const SYS_MOTOR_TICK_TYPED:       u64 = 551;
pub const SYS_MOTOR_ENABLE_TYPED:     u64 = 552;
pub const SYS_MOTOR_ENABLED_TYPED:    u64 = 553;
pub const SYS_MOTOR_SET_GAINS_TYPED:  u64 = 554;
pub const SYS_MOTOR_RESET_TYPED:      u64 = 555;
