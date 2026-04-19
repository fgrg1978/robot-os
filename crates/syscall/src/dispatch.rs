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

/// Main syscall dispatch.  Arguments are raw register values (u64).
/// Returns the result that will be written back into a0.
pub fn syscall_dispatch(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, _a5: u64) -> i64 {
    // AQ11: Syscall filter — reject if current task's whitelist doesn't include this syscall.
    // Works on both RV64 and RV32 (ESP32-C3).
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
        SYS_FORK    => sys_fork(),
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
        SYS_MMAP   => sys_mmap(a0, a1, a2, a3, a4, _a5),
        SYS_MUNMAP => sys_munmap(a0, a1),
        // E11/AQ9: explicit COW fork (equivalent to SYS_FORK today).
        SYS_FORK_COW    => sys_fork_cow(),
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
                unsafe { core::ptr::copy_nonoverlapping(a1 as *const u8, msg_buf.as_mut_ptr(), msg_len); }
            }
            // Send message to server channel
            if robot_os_ipc::channel_send(ch, &msg_buf[..msg_len]) != 0 {
                return -1; // channel full or invalid
            }
            // Register pending RPC
            match robot_os_ipc::rpc_register(tid, a0 as u32) {
                Some(_rpc_id) => {
                    // Block caller until IPC_REPLY wakes us
                    robot_os_sched::task_block(robot_os_sched::WaitReason::Rpc(tid));
                    // After wake-up, retrieve reply
                    let mut reply_tmp = [0u8; robot_os_ipc::RPC_MSG_MAX_LEN];
                    match robot_os_ipc::rpc_get_reply(tid, &mut reply_tmp) {
                        Some(reply_len) => {
                            // Copy reply to caller's buffer if provided
                            if a3 != 0 {
                                let copy_len = (reply_len as usize).min(a4 as usize);
                                unsafe { core::ptr::copy_nonoverlapping(reply_tmp.as_ptr(), a3 as *mut u8, copy_len); }
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
            let mut reply_buf = [0u8; robot_os_ipc::RPC_MSG_MAX_LEN];
            let reply_len = (a2 as usize).min(robot_os_ipc::RPC_MSG_MAX_LEN);
            if reply_len > 0 && a1 != 0 {
                unsafe { core::ptr::copy_nonoverlapping(a1 as *const u8, reply_buf.as_mut_ptr(), reply_len); }
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
        SYS_IPC_UNSHARE => {
            robot_os_ipc::shm_release(a0 as u32);
            0
        }
        // IPC_MAP (F00.4): map a shared memory region into caller's address space.
        // a0 = shm_id → returns VA base (user pointer), or -1 on failure.
        // Increments shm ref_count; caller must IPC_UNSHARE when done.
        SYS_IPC_MAP => {
            let shm_id = a0 as u32;
            // Acquire ref and get page count + perms.
            match robot_os_ipc::shm_acquire(shm_id) {
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
                            Some(va) => va as i64,
                            None     => { robot_os_ipc::shm_release(shm_id); -1 }
                        }
                    } else {
                        robot_os_ipc::shm_release(shm_id);
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
            match robot_os_ipc::fast_ipc_call(caller_tid, server_tid, words) {
                Some(slot_idx) => {
                    // Wake server if it is blocked in FAST_ACCEPT.
                    robot_os_sched::wake_fast_ipc_server(server_tid);
                    // Block caller until server replies.
                    robot_os_sched::task_block(
                        robot_os_sched::WaitReason::FastIpcClient(slot_idx as u32)
                    );
                    // After wake: collect reply words.
                    match robot_os_ipc::fast_ipc_collect(caller_tid) {
                        Some(reply) => reply[0] as i64, // a0 = first word; rest via frame
                        None        => -1,
                    }
                }
                None => -1, // no free slots — fall back to channel IPC
            }
        }

        // SYS_IPC_FAST_ACCEPT: server side.
        // Blocks until a client calls FAST_CALL targeting this TID.
        // Returns caller_tid in a0; data words in a1..a4 (written via TrapFrame by waker).
        SYS_IPC_FAST_ACCEPT => {
            let server_tid = robot_os_sched::current_task_tid();
            // Check if a call is already waiting.
            match robot_os_ipc::fast_ipc_accept(server_tid) {
                Some((slot_idx, _caller_tid, _words)) => {
                    // Return slot_idx as the "handle" for the subsequent FAST_REPLY.
                    slot_idx as i64
                }
                None => {
                    // No call pending — block until one arrives.
                    robot_os_sched::task_block(
                        robot_os_sched::WaitReason::FastIpcServer(server_tid)
                    );
                    // After wake: accept the call that just arrived.
                    match robot_os_ipc::fast_ipc_accept(server_tid) {
                        Some((slot_idx, _caller_tid, _words)) => slot_idx as i64,
                        None => -1,
                    }
                }
            }
        }

        // SYS_IPC_FAST_REPLY: server side.
        // a0 = slot_idx (from FAST_ACCEPT return value).
        // a1..a4 = reply data words.
        // Wakes the client. Returns 0 on success.
        SYS_IPC_FAST_REPLY => {
            let slot_idx = a0 as usize;
            let words = [a1 as u64, a2 as u64, a3 as u64, a4 as u64];
            match robot_os_ipc::fast_ipc_reply(slot_idx, words) {
                Some(_caller_tid) => {
                    robot_os_sched::wake_fast_ipc_client(slot_idx as u32);
                    0
                }
                None => -1,
            }
        }

        // M04: Lease-based IPC — zero-copy large transfer via time-bounded SHM grant.

        // SYS_IPC_LEASE_GRANT: lessor grants a SHM region to lessee.
        // a0=shm_id, a1=lessee_tid, a2=expire_ticks (0=no expiry).
        // Returns lease_id on success, -1 on error.
        SYS_IPC_LEASE_GRANT => {
            let lessor = robot_os_sched::current_task_tid();
            match robot_os_ipc::lease_grant(a0 as usize, lessor, a1 as u32, a2) {
                Some(lease_id) => lease_id as i64,
                None           => -1,
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
        SYS_IPC_LEASE_RETURN => {
            match robot_os_ipc::lease_return(a0 as usize) {
                Some(lessor_tid) => {
                    // Wake the lessor (may be blocked in lease_wait_return).
                    robot_os_sched::wake_fast_ipc_server(lessor_tid);
                    0
                }
                None => -1,
            }
        }

        // SYS_IPC_LEASE_FREE: lessor frees the lease entry after reclaiming buffer.
        // a0=lease_id.
        SYS_IPC_LEASE_FREE => {
            robot_os_ipc::lease_free(a0 as usize);
            0
        }

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
            unsafe { core::ptr::copy_nonoverlapping(a0 as *const u8, name_buf.as_mut_ptr(), name_len); }
            let hostname = match core::str::from_utf8(&name_buf[..name_len]) {
                Ok(s) => s,
                Err(_) => return -1,
            };
            match robot_os_net::dns::resolve(hostname) {
                Some(ip) => {
                    if a2 != 0 {
                        unsafe { core::ptr::copy_nonoverlapping(ip.as_ptr(), a2 as *mut u8, 4); }
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
        #[cfg(target_pointer_width = "64")]
        SYS_DRV_MMAP => {
            let drv_id   = a0 as usize;
            let mmio_idx = a1 as usize;
            match robot_os_sched::driver_info(drv_id) {
                Some(info) if mmio_idx < info.mmio_count as usize => {
                    let region = info.mmio[mmio_idx];
                    // Map the MMIO region into the calling process's page table.
                    match robot_os_sched::mmio_map_user(region.base, region.size) {
                        Some(va) => va as i64,
                        None     => -1,
                    }
                }
                _ => -1,
            }
        }

        #[cfg(target_pointer_width = "32")]
        SYS_DRV_MMAP => -1,  // No MMU on RV32

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
            // PLIC is only present on 64-bit platforms (not ESP32-C3 / no-mmu).
            #[cfg(target_pointer_width = "64")]
            {
                let hart = robot_os_arch::cpu::hart_id() as u32;
                robot_os_drivers::plic::complete(hart, a0 as u32);
            }
            0
        }

        // F06.4: SYS_DRV_DMA_ALLOC — a0=size_bytes → phys addr or -1
        // Allocates one physically contiguous page (4 KiB minimum unit).
        // Drivers requesting larger DMA buffers should call multiple times.
        SYS_DRV_DMA_ALLOC => {
            const DMA_MAX_SINGLE_ALLOC: usize = 65536; // 64 KiB = 16 pages
            let size = a0 as usize;
            if size == 0 || size > DMA_MAX_SINGLE_ALLOC {
                -1
            } else {
                match robot_os_mm::pmm::alloc_page() {
                    Ok(phys) => phys.0 as i64,
                    Err(_)   => -1,
                }
            }
        }

        // F06.4: SYS_DRV_DMA_FREE — a0=phys_addr
        SYS_DRV_DMA_FREE => {
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
            robot_os_ipc::io_ring_submit(a0 as u32) as i64
        }
        SYS_IO_WAIT => {
            // a0 = ring_id — return number of pending completions.
            robot_os_ipc::io_ring_pending(a0 as u32) as i64
        }

        // M05: Async IO Ring submit — non-blocking, worker task processes SQEs.
        // a0 = ring_id (currently unused — worker polls all rings).
        // Returns 0 immediately; completions appear in CQ when worker runs.
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

        // MMIO mapping (F00.2): a0 = phys_base, a1 = size_bytes
        // Maps a physical MMIO region into the calling task's userspace page table.
        // Requires the caller to hold a Handle(MmioRegion(phys_base, size)).
        SYS_MMIO_MAP => {
            #[cfg(target_pointer_width = "64")]
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
            #[cfg(not(target_pointer_width = "64"))]
            {
                // No MMU on RV32 — MMIO is directly accessible
                let _ = (a0, a1);
                a0 as i64
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
            robot_os_ipc::port_destroy(a0 as u32);
            0
        }

        // Handles (AQ6 + F00.6: generalized grant)
        // a0 = owner_tid, a1 = kind_type, a2 = param0, a3 = param1, a4 = perms_bits
        // kind_type: 0=Sensor, 1=Gpio, 2=I2c, 3=Pwm, 4=Motor,
        //            5=Channel, 6=Ring, 7=Port, 8=Irq, 9=MmioRegion
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
            const HANDLE_KIND_CHANNEL:     u64 = 5;
            const HANDLE_KIND_RING:        u64 = 6;
            const HANDLE_KIND_PORT:        u64 = 7;
            const HANDLE_KIND_IRQ:         u64 = 8;
            const HANDLE_KIND_MMIO_REGION: u64 = 9;

            let kind = match a1 {
                HANDLE_KIND_SENSOR      => robot_os_ipc::HandleKind::Sensor(a2 as u8),
                HANDLE_KIND_GPIO        => robot_os_ipc::HandleKind::Gpio(a2 as u32),
                HANDLE_KIND_I2C         => robot_os_ipc::HandleKind::I2c(a2 as u8, a3 as u8),
                HANDLE_KIND_PWM         => robot_os_ipc::HandleKind::Pwm(a2 as u8),
                HANDLE_KIND_MOTOR       => robot_os_ipc::HandleKind::Motor(a2 as u32),
                HANDLE_KIND_CHANNEL     => robot_os_ipc::HandleKind::Channel(a2 as u32),
                HANDLE_KIND_RING        => robot_os_ipc::HandleKind::Ring(a2 as u32),
                HANDLE_KIND_PORT        => robot_os_ipc::HandleKind::Port(a2 as u32),
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
            robot_os_ipc::handle_revoke(a0 as u32);
            0
        }
        SYS_HANDLE_DUP => {
            // a0 = handle_id, a1 = new_owner_tid
            match robot_os_ipc::handle_dup(a0 as u32, a1 as u32) {
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
