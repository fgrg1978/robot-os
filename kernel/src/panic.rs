/// Panic handler for the kernel.
///
/// SAFETY FIRST: stops all actuators (motors + ESCs) before printing
/// the panic message and halting.  A robot must never continue moving
/// after a kernel panic.
///
/// After stopping motors and printing to UART, writes a crash log entry
/// to `/fat/CRASH.LOG` (best-effort, no allocations).
///
/// The panic message itself goes out via `uart::puts`, which is lock-free
/// (see `uart::putc`/`ns16550a::putc_raw` — no software lock, only a
/// hardware busy-wait on the transmitter-ready bit). The actuator-stop
/// calls at the top (`motor_stop_panic`, `esc_disarm_panic`) are also
/// lock-free: they bypass the `MOTORS`/`GPIO`/`PWM` spinlocks and the
/// UART lock respectively, on purpose, so nothing before the first
/// `uart::puts` call below can spin forever on a lock held by another
/// hart. See the doc comments on `motor::motor_stop_panic`,
/// `drivers::gpio::gpio_write_panic`, `drivers::pwm::pwm_set_duty_pct_panic`
/// and `drivers::esc::esc_disarm_panic` for why that trade-off (torn
/// actuator state instead of a hung panic handler) is deliberate.
/// The crash-log write (VFS `FS` lock + FAT32 volume/sector-cache locks)
/// and the trace dump (UART lock, via `kprintln!`) — both further down,
/// after the panic message is already out — are each gated by a
/// non-blocking peek first and skipped with a UART note if the lock isn't
/// free — see `write_crash_log()`.
/// Then optionally reboots after a configurable delay.
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

/// FAT32 path for persistent crash log.
const CRASH_LOG_PATH: &[u8] = b"/fat/CRASH.LOG";
/// Maximum crash log entry size (bytes).
const CRASH_ENTRY_MAX: usize = 512;
/// Number of most recent trace events to dump on panic (best-effort).
const TRACE_DUMP_EVENTS: usize = 16;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // ── Freeze this hart and flag the panic globally ────────────────────
    // Clear SSTATUS.SIE so a timer tick cannot re-enter the scheduler and
    // resume normal execution on this hart, and publish the panic flag so
    // the other harts halt on their next tick (see the timer ISR) and no
    // control path re-commands the motors after the stop below.
    let sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(sstatus & !robot_os_arch::csr::SSTATUS_SIE);
    robot_os_common::set_panicked();

    // ── Stop all actuators IMMEDIATELY ──────────────────────────────────
    // Uses the lock-free `_panic` variants, not `motor_stop`/`esc_disarm`:
    // those take the `MOTORS`/`GPIO`/`PWM` and UART spinlocks respectively,
    // so a hart holding any of them at panic time would wedge this call
    // before any UART output ever went out. The `_panic` variants bypass
    // those locks on purpose — see the module doc comment above and the
    // doc comments on `motor_stop_panic`/`esc_disarm_panic` themselves.
    robot_os_robot::motor_stop_panic(0);
    robot_os_robot::motor_stop_panic(1);
    robot_os_drivers::esc::esc_disarm_panic();

    // ── Print panic info (no locks — we're crashing) ────────────────────
    robot_os_drivers::uart::puts("\n!!! KERNEL PANIC !!!\n");

    if let Some(location) = info.location() {
        robot_os_drivers::uart::puts("  at ");
        robot_os_drivers::uart::puts(location.file());
        robot_os_drivers::uart::puts(":");
        let mut buf = [0u8; 10];
        robot_os_drivers::uart::puts(fmt_u32(location.line(), &mut buf));
        robot_os_drivers::uart::puts("\n");
    }

    if let Some(msg) = info.message().as_str() {
        robot_os_drivers::uart::puts("  ");
        robot_os_drivers::uart::puts(msg);
        robot_os_drivers::uart::puts("\n");
    }

    // ── Print CPU and task context ──────────────────────────────────────
    let hart = robot_os_arch::cpu::hart_id();
    let task_name = robot_os_sched::current_task_name();
    robot_os_drivers::uart::puts("  hart=");
    let mut buf = [0u8; 20];
    robot_os_drivers::uart::puts(fmt_usize(hart, &mut buf));
    robot_os_drivers::uart::puts(" task=");
    robot_os_drivers::uart::puts(task_name);
    robot_os_drivers::uart::puts("\n");

    // ── F11.3: Increment crash counter (boot-loop detection) ────────────
    let crashes = robot_os_drivers::wdt::crash_counter_increment();
    robot_os_drivers::uart::puts("[PANIC] Crash counter = ");
    let mut cbuf = [0u8; 10];
    robot_os_drivers::uart::puts(fmt_u32(crashes, &mut cbuf));
    if robot_os_drivers::wdt::crash_counter_is_boot_loop() {
        robot_os_drivers::uart::puts(" [BOOT LOOP DETECTED — safe mode on next boot]\n");
    } else {
        robot_os_drivers::uart::puts("\n");
    }

    // ── Persist crash log to FAT32 (best-effort) ────────────────────────
    write_crash_log(info, hart, task_name);

    // ── Dump last trace events (best-effort — never block on UART) ──────
    // `trace_dump` prints via `kprintln!`, which takes the UART spinlock
    // (`uart::acquire()`). Peek with `try_acquire()` first so a hart
    // holding that lock can never wedge the panic handler.
    let uart_free_for_trace = robot_os_drivers::uart::try_acquire().is_some();
    if uart_free_for_trace {
        robot_os_ipc::trace::trace_dump(TRACE_DUMP_EVENTS);
    } else {
        robot_os_drivers::uart::puts("[PANIC] UART lock busy — skipping trace dump\n");
    }

    // ── Auto-reboot or halt ─────────────────────────────────────────────
    let delay_ms = robot_os_config::CFG_PANIC_REBOOT_DELAY_MS.load(Ordering::Relaxed);
    if delay_ms > 0 {
        robot_os_drivers::uart::puts("[PANIC] Rebooting in ");
        let mut dbuf = [0u8; 10];
        robot_os_drivers::uart::puts(fmt_u32(delay_ms, &mut dbuf));
        robot_os_drivers::uart::puts(" ms...\n");

        // Busy-wait delay (no scheduler available during panic)
        let start = robot_os_drivers::clint::get_time();
        let ticks_per_ms = robot_os_drivers::clint::TIMER_FREQ / 1000;
        let wait_ticks = delay_ms as u64 * ticks_per_ms;
        while robot_os_drivers::clint::get_time().wrapping_sub(start) < wait_ticks {
            core::hint::spin_loop();
        }

        robot_os_arch::sbi::reboot();
    }

    loop {
        robot_os_arch::cpu::wfi();
    }
}

/// Write a crash log entry to `/fat/CRASH.LOG` (append).
///
/// Uses a static buffer — no heap allocations. Best-effort: if FAT32 is
/// unavailable or corrupt, this silently fails (UART output is the
/// fallback). The panic handler must never block, so before touching the
/// VFS this checks — without spinning — whether the locks the write path
/// needs are currently free: the VFS-level `FS` lock
/// (`vfs_fs_lock_available()`) and, since `/fat/CRASH.LOG` is always
/// FAT32-backed, the FAT32 volume/sector-cache locks
/// (`fat32_locks_available()`) that `vfs_open`/`vfs_close` reach into for
/// FAT32 paths. If any of them is held (e.g. some hart panicked while
/// holding it, or is otherwise stuck with it held), the on-disk dump is
/// skipped entirely and that is reported over UART.
///
/// Note these checks are a best-effort guard, not a hard guarantee:
/// another hart can still grab one of these locks in the narrow window
/// between the check and the `vfs_open`/`vfs_write`/`vfs_close` calls
/// below, since those functions acquire and release each lock several
/// times internally rather than holding it for the whole operation.
/// Closing that residual race fully would require converting the VFS's
/// and FAT32 driver's internal locking to non-blocking acquisition
/// throughout, which is out of scope here.
fn write_crash_log(info: &PanicInfo, hart: usize, task_name: &str) {
    if !robot_os_fs::vfs_fs_lock_available() {
        robot_os_drivers::uart::puts(
            "[PANIC] FS lock busy — skipping crash log dump to /fat/CRASH.LOG\n");
        return;
    }
    if !robot_os_fs::fat32_locks_available() {
        robot_os_drivers::uart::puts(
            "[PANIC] FAT32 lock busy — skipping crash log dump to /fat/CRASH.LOG\n");
        return;
    }

    static mut CRASH_BUF: [u8; CRASH_ENTRY_MAX] = [0u8; CRASH_ENTRY_MAX];
    let buf = unsafe { &mut *(&raw mut CRASH_BUF) };
    let mut pos = 0usize;

    // Timestamp (CLINT ticks)
    let ts = robot_os_drivers::clint::get_time();
    pos += copy_str(buf, pos, b"[t=");
    pos += copy_u64(buf, pos, ts);
    pos += copy_str(buf, pos, b"] hart=");
    pos += copy_usize(buf, pos, hart);
    pos += copy_str(buf, pos, b" task=");
    pos += copy_str(buf, pos, task_name.as_bytes());

    if let Some(loc) = info.location() {
        pos += copy_str(buf, pos, b" at ");
        pos += copy_str(buf, pos, loc.file().as_bytes());
        pos += copy_str(buf, pos, b":");
        pos += copy_u32(buf, pos, loc.line());
    }

    if let Some(msg) = info.message().as_str() {
        pos += copy_str(buf, pos, b" ");
        let max = (CRASH_ENTRY_MAX - pos).saturating_sub(2); // room for \n
        let mlen = msg.len().min(max);
        pos += copy_str(buf, pos, &msg.as_bytes()[..mlen]);
    }

    if pos < CRASH_ENTRY_MAX {
        buf[pos] = b'\n';
        pos += 1;
    }

    // Write to FAT32 (best-effort)
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, CRASH_LOG_PATH,
        robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_APPEND);
    if fd >= 0 {
        robot_os_fs::vfs_write(&mut fd_table, fd, buf.as_ptr(), pos);
        robot_os_fs::vfs_close(&mut fd_table, fd);
        robot_os_drivers::uart::puts("[PANIC] Crash log written to /fat/CRASH.LOG\n");
    }
}

fn copy_str(buf: &mut [u8], pos: usize, s: &[u8]) -> usize {
    let n = s.len().min(buf.len().saturating_sub(pos));
    buf[pos..pos + n].copy_from_slice(&s[..n]);
    n
}

fn copy_u32(buf: &mut [u8], pos: usize, val: u32) -> usize {
    let mut tmp = [0u8; 10];
    let s = fmt_u32(val, &mut tmp);
    copy_str(buf, pos, s.as_bytes())
}

fn copy_u64(buf: &mut [u8], pos: usize, mut val: u64) -> usize {
    let mut tmp = [0u8; 20];
    if val == 0 {
        return copy_str(buf, pos, b"0");
    }
    let mut i = 20;
    while val > 0 {
        i -= 1;
        tmp[i] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    copy_str(buf, pos, &tmp[i..])
}

fn copy_usize(buf: &mut [u8], pos: usize, mut val: usize) -> usize {
    let mut tmp = [0u8; 20];
    if val == 0 {
        return copy_str(buf, pos, b"0");
    }
    let mut i = 20;
    while val > 0 {
        i -= 1;
        tmp[i] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    copy_str(buf, pos, &tmp[i..])
}

/// Format a u32 into a decimal string. Returns the written slice of `buf`.
fn fmt_u32(mut val: u32, buf: &mut [u8; 10]) -> &str {
    if val == 0 {
        buf[0] = b'0';
        return unsafe { core::str::from_utf8_unchecked(&buf[..1]) };
    }
    let mut i = 10;
    while val > 0 {
        i -= 1;
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    unsafe { core::str::from_utf8_unchecked(&buf[i..]) }
}

/// Format a usize into a decimal string. Returns the written slice of `buf`.
fn fmt_usize(mut val: usize, buf: &mut [u8; 20]) -> &str {
    if val == 0 {
        buf[0] = b'0';
        return unsafe { core::str::from_utf8_unchecked(&buf[..1]) };
    }
    let mut i = 20;
    while val > 0 {
        i -= 1;
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    unsafe { core::str::from_utf8_unchecked(&buf[i..]) }
}
