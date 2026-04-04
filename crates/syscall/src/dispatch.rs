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
        SYS_IPC_CALL ..= SYS_IPC_UNSHARE => sys_stub(), // advanced IPC

        // Network
        SYS_NET_INFO   => sys_net_info(),
        SYS_NET_GETIP  => sys_net_getip(),
        SYS_NET_SETIP  => sys_net_setip(a0, a1, a2),
        SYS_NET_PING   => sys_net_ping(a0),
        SYS_NET_GETMAC => sys_net_getmac(),
        SYS_NET_STATS  => sys_net_stats(),
        SYS_DNS_RESOLVE ..= SYS_MCAST_JOIN => sys_stub(),  // 266..=269
        // SYS_SHUTDOWN (270) and SYS_REBOOT (271) handled above
        SYS_MCAST_LEAVE ..= SYS_SECURE_RECV => sys_stub(),  // 272..=276

        SYS_FDT_INFO   ..= SYS_FDT_DUMP      => sys_stub(),
        SYS_DRV_REGISTER ..= SYS_DRV_GET_DEVICE => sys_stub(),
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

        _ => -1,
    }
}
