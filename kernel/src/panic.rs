/// Panic handler for the kernel.
///
/// SAFETY FIRST: stops all actuators (motors + ESCs) before printing
/// the panic message and halting.  A robot must never continue moving
/// after a kernel panic.
use core::panic::PanicInfo;

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
    robot_os_drivers::uart::puts("  hart=");
    let mut buf = [0u8; 20];
    robot_os_drivers::uart::puts(fmt_usize(robot_os_arch::cpu::hart_id(), &mut buf));
    robot_os_drivers::uart::puts(" task=");
    robot_os_drivers::uart::puts(robot_os_sched::current_task_name());
    robot_os_drivers::uart::puts("\n");

    loop {
        robot_os_arch::cpu::wfi();
    }
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
