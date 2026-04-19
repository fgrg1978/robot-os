/// Panic handler for the kernel.
///
/// SAFETY FIRST: stops all actuators (motors + ESCs) before printing
/// the panic message and halting.  A robot must never continue moving
/// after a kernel panic.
///
/// After stopping motors and printing to UART, writes a crash log entry
/// to `/fat/CRASH.LOG` (best-effort, no allocations, no locks).
/// Then optionally reboots after a configurable delay.
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

/// FAT32 path for persistent crash log.
const CRASH_LOG_PATH: &[u8] = b"/fat/CRASH.LOG";
/// Maximum crash log entry size (bytes).
const CRASH_ENTRY_MAX: usize = 512;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // ── Stop all actuators IMMEDIATELY ──────────────────────────────────
    // These functions only write atomics — they cannot panic or deadlock.
    robot_os_robot::motor_stop(0);
    robot_os_robot::motor_stop(1);
    robot_os_drivers::esc::esc_disarm();

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

    // ── Dump last trace events ──────────────────────────────────────────
    robot_os_ipc::trace::trace_dump(16);

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
/// Uses a static buffer — no heap, no locks. Best-effort: if FAT32 is
/// unavailable or corrupt, this silently fails (UART output is the fallback).
fn write_crash_log(info: &PanicInfo, hart: usize, task_name: &str) {
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
