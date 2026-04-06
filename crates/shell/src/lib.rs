#![no_std]

//! Kernel shell — port of kernel/core/shell.c
//! Interactive UART command shell.  Runs as a kernel task.

const MAX_LINE:  usize = 256;
const MAX_ARGS:  usize = 8;
const PROMPT:    &str  = "robot> ";

// ── Line reader ───────────────────────────────────────────────────────────────

/// Read one line from UART into `buf`.  Returns byte count (without newline).
/// Supports backspace editing.
fn readline(buf: &mut [u8; MAX_LINE]) -> usize {
    let mut pos = 0usize;
    loop {
        // Poll until a character is available
        loop {
            if robot_os_drivers::uart::can_read() { break; }
            robot_os_sched::task_yield();
        }
        let c = robot_os_drivers::uart::getc();
        match c {
            b'\r' | b'\n' => {
                robot_os_drivers::uart::putc(b'\n');
                break;
            }
            // Backspace / DEL
            0x08 | 0x7F => {
                if pos > 0 {
                    pos -= 1;
                    robot_os_drivers::uart::puts("\x08 \x08");
                }
            }
            // Printable ASCII
            0x20..=0x7E => {
                if pos < MAX_LINE - 1 {
                    buf[pos] = c;
                    pos += 1;
                    robot_os_drivers::uart::putc(c);
                }
            }
            _ => {}
        }
    }
    pos
}

// ── Argument parser ───────────────────────────────────────────────────────────

fn parse_args<'a>(line: &'a [u8], args: &mut [&'a [u8]; MAX_ARGS]) -> usize {
    let mut argc = 0;
    let mut start = 0;
    let mut in_word = false;

    for i in 0..line.len() {
        let b = line[i];
        if b == b' ' || b == b'\t' {
            if in_word {
                if argc < MAX_ARGS { args[argc] = &line[start..i]; argc += 1; }
                in_word = false;
            }
        } else {
            if !in_word { start = i; in_word = true; }
        }
    }
    if in_word && argc < MAX_ARGS {
        args[argc] = &line[start..line.len()];
        argc += 1;
    }
    argc
}

// ── Individual commands ───────────────────────────────────────────────────────

fn cmd_help() {
    robot_os_drivers::kprintln!("Available commands:");
    robot_os_drivers::kprintln!("  help              - this message");
    #[cfg(not(feature = "no-mmu"))]
    robot_os_drivers::kprintln!("  exec <path>       - load and run ELF from FAT32");
    robot_os_drivers::kprintln!("  ps                - list current task");
    robot_os_drivers::kprintln!("  mem               - memory info");
    robot_os_drivers::kprintln!("  uptime            - system uptime ticks");
    robot_os_drivers::kprintln!("  ls [path]         - list directory");
    robot_os_drivers::kprintln!("  cat <path>        - print file");
    robot_os_drivers::kprintln!("  write <path> <t>  - write text to FAT32 file");
    robot_os_drivers::kprintln!("  rm <path>         - remove FAT32 file");
    robot_os_drivers::kprintln!("  mkdir <path>      - create directory");
    robot_os_drivers::kprintln!("  echo <text>       - echo arguments");
    robot_os_drivers::kprintln!("  disk              - disk capacity");
    robot_os_drivers::kprintln!("  ifconfig          - network interface info");
    robot_os_drivers::kprintln!("  ping <ip>         - send ICMP ping");
    robot_os_drivers::kprintln!("  arp               - ARP cache");
    robot_os_drivers::kprintln!("  tcpecho <port>    - TCP echo server");
    robot_os_drivers::kprintln!("  gpio info         - GPIO state");
    robot_os_drivers::kprintln!("  pwm info          - PWM state");
    robot_os_drivers::kprintln!("  i2c scan [bus]    - I2C bus scan");
    robot_os_drivers::kprintln!("  motor info        - motor state");
    robot_os_drivers::kprintln!("  rvv               - RVV 1.0 f32 benchmark (qemu-rvv only)");
    #[cfg(not(feature = "no-ml"))]
    {
        robot_os_drivers::kprintln!("  ml                - MLP inference demo (4→8→3, f32)");
        robot_os_drivers::kprintln!("  pipeline          - sensor→ML→motor pipeline status");
        robot_os_drivers::kprintln!("  cam [capture|info] [0-2]  - virtual camera driver");
        robot_os_drivers::kprintln!("  model [info|load <path>]  - RMLP model loader");
    }
    robot_os_drivers::kprintln!("  security          - Phase 16: security overview");
    robot_os_drivers::kprintln!("  odom              - Phase 17: odometry (dist, heading)");
    robot_os_drivers::kprintln!("  traj [status|dump [N]|flush|reset]  - trajectory ring");
    #[cfg(not(feature = "no-ml"))]
    robot_os_drivers::kprintln!("  ota recv <port>   - Phase 17: OTA model update via TCP");
    robot_os_drivers::kprintln!("  config [list|get|set|save|load|defaults|export] - Phase G2: persistent config");
    robot_os_drivers::kprintln!("  behavior [status|enable|disable|remote|goal]  - Phase G1: subsumption + VLA");
    robot_os_drivers::kprintln!("  pmp               - Phase D: PMP memory-protection policy");
    robot_os_drivers::kprintln!("  wdt               - Phase D: hardware watchdog status");
    robot_os_drivers::kprintln!("  fuzz              - Phase D: basic memory fuzz test");
    robot_os_drivers::kprintln!("  sched_hz [<hz>]   - Phase E1: show/set scheduler rate");
    robot_os_drivers::kprintln!("  imu [info|read]   - Phase E2: MPU-6050 IMU sensor");
    robot_os_drivers::kprintln!("  baro [info|read]  - Phase G1: BMP280 barometer");
    robot_os_drivers::kprintln!("  attitude          - Phase I1: AHRS attitude (roll/pitch/yaw/alt)");
    robot_os_drivers::kprintln!("  gps [info|read]   - Phase I2: GPS position (lat/lon/alt)");
    robot_os_drivers::kprintln!("  flight [status|arm|disarm|mode <m>] - Phase J: flight controller");
    robot_os_drivers::kprintln!("  rc [info]         - Phase K: RC receiver channels");
    robot_os_drivers::kprintln!("  esc [info]        - Phase J: ESC motor outputs");
    robot_os_drivers::kprintln!("  telem [status|start <port>|stop] - Phase L: telemetry");
    robot_os_drivers::kprintln!("  range             - Phase M: rangefinder sensors (US+ToF)");
    robot_os_drivers::kprintln!("  nav [info]        - Phase N: navigation + waypoints");
    robot_os_drivers::kprintln!("  csi               - Phase M2: CSI camera info");
    robot_os_drivers::kprintln!("  wifi [info|connect|disconnect] - Phase O: WiFi (ESP32-C3)");
    robot_os_drivers::kprintln!("  spi info          - SPI bus info");
    robot_os_drivers::kprintln!("  can [info|send|recv] - CAN bus");
    robot_os_drivers::kprintln!("  dma info          - DMA controller info");
    robot_os_drivers::kprintln!("  usb info          - USB host info");
    robot_os_drivers::kprintln!("  pm [info|idle|suspend|resume] - power management");
    robot_os_drivers::kprintln!("  eth info          - Ethernet MAC info");
    robot_os_drivers::kprintln!("  dhcp              - DHCP client (acquire IP)");
    #[cfg(not(feature = "no-mmu"))]
    robot_os_drivers::kprintln!("  fork              - fork current process (test)");
    robot_os_drivers::kprintln!("  shutdown          - shutdown system");
    robot_os_drivers::kprintln!("  reboot            - reboot system");
}

fn cmd_ps() {
    robot_os_drivers::kprintln!("[SCHED] TID: {}", robot_os_sched::current_task_tid());
    robot_os_drivers::kprintln!("[SCHED] Task: {}", robot_os_sched::current_task_name());
}

fn cmd_mem() {
    let free  = robot_os_mm::pmm::free_pages();
    let total = robot_os_mm::pmm::total_pages();
    let used  = robot_os_mm::pmm::used_pages();
    robot_os_drivers::kprintln!("[MEM] Total: {} pages ({} KiB)", total, total * 4);
    robot_os_drivers::kprintln!("[MEM] Used:  {} pages ({} KiB)", used,  used  * 4);
    robot_os_drivers::kprintln!("[MEM] Free:  {} pages ({} KiB)", free,  free  * 4);
}

fn cmd_uptime() {
    let ticks = robot_os_drivers::clint::get_time();
    robot_os_drivers::kprintln!("[UPTIME] {} ticks (~{} ms)", ticks, ticks / 10000);
}

fn cmd_ls(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let path: &[u8] = if argc >= 2 { args[1] } else { b"/" };

    // FAT32 paths: /fat or /fat/...
    let is_fat = path == b"/fat" || path.starts_with(b"/fat/");
    if is_fat {
        robot_os_drivers::kprint!("[FS] ls ");
        for &b in path { robot_os_drivers::uart::putc(b); }
        robot_os_drivers::kprintln!(" (FAT32):");
        robot_os_fs::fat32_ls_root(|name, size, is_dir| {
            if is_dir {
                robot_os_drivers::kprint!("  [DIR]  ");
                for &b in name { robot_os_drivers::uart::putc(b); }
                robot_os_drivers::kprintln!();
            } else {
                robot_os_drivers::kprint!("  [FILE] ");
                for &b in name { robot_os_drivers::uart::putc(b); }
                robot_os_drivers::kprintln!("  ({} B)", size);
            }
        });
        return;
    }

    // ramfs path
    let idx = robot_os_fs::path_lookup(path);
    if idx == robot_os_fs::NO_IDX {
        robot_os_drivers::kprint!("[FS] Not found: ");
        for &b in path { robot_os_drivers::uart::putc(b); }
        robot_os_drivers::kprintln!();
        return;
    }
    robot_os_drivers::kprint!("[FS] ls ");
    for &b in path { robot_os_drivers::uart::putc(b); }
    robot_os_drivers::kprintln!(":");
    robot_os_fs::dir_list(idx, |name, itype| {
        let kind = match itype {
            robot_os_fs::INODE_DIR    => "[DIR] ",
            robot_os_fs::INODE_DEVICE => "[DEV] ",
            _                         => "[FILE]",
        };
        robot_os_drivers::kprint!("  {} ", kind);
        for &b in name { robot_os_drivers::uart::putc(b); }
        robot_os_drivers::kprintln!();
    });
}

fn cmd_cat(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 2 {
        robot_os_drivers::kprintln!("Usage: cat <path>");
        return;
    }
    let path = args[1];
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path, robot_os_fs::O_RDONLY);
    if fd < 0 {
        robot_os_drivers::kprint!("[FS] Cannot open: ");
        for &b in path { robot_os_drivers::uart::putc(b); }
        robot_os_drivers::kprintln!();
        return;
    }
    let mut buf = [0u8; 256];
    loop {
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), 256);
        if n <= 0 { break; }
        for i in 0..n as usize {
            robot_os_drivers::uart::putc(buf[i]);
        }
    }
    robot_os_fs::vfs_close(&mut fd_table, fd);
    robot_os_drivers::uart::putc(b'\n');
}

fn cmd_mkdir(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 2 {
        robot_os_drivers::kprintln!("Usage: mkdir <path>");
        return;
    }
    let path = args[1];
    let (parent_idx, name) = robot_os_fs::path_parent(path);
    if parent_idx == robot_os_fs::NO_IDX || name.is_empty() {
        robot_os_drivers::kprintln!("[FS] Invalid path");
        return;
    }
    let dir_idx = robot_os_fs::inode_alloc(
        robot_os_fs::INODE_DIR,
        robot_os_fs::PERM_READ | robot_os_fs::PERM_WRITE | robot_os_fs::PERM_EXEC,
    );
    if dir_idx == robot_os_fs::NO_IDX {
        robot_os_drivers::kprintln!("[FS] No free inodes");
        return;
    }
    match robot_os_fs::dir_add_entry(parent_idx, name, dir_idx) {
        Ok(())  => robot_os_drivers::kprintln!("[FS] mkdir ok"),
        Err(()) => {
            robot_os_fs::inode_free(dir_idx);
            robot_os_drivers::kprintln!("[FS] mkdir failed");
        }
    }
}

fn cmd_echo(args: &[&[u8]; MAX_ARGS], argc: usize) {
    for i in 1..argc {
        if i > 1 { robot_os_drivers::uart::putc(b' '); }
        for &b in args[i] { robot_os_drivers::uart::putc(b); }
    }
    robot_os_drivers::uart::putc(b'\n');
}

fn cmd_disk() {
    let secs = robot_os_drivers::virtio::blk::capacity_sectors();
    robot_os_drivers::kprintln!("[DISK] Capacity: {} sectors ({} MiB)", secs, secs / 2048);
}

fn cmd_ifconfig() {
    robot_os_net::net_info();
}

fn cmd_ping(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 2 {
        robot_os_drivers::kprintln!("Usage: ping <ip>");
        return;
    }
    match parse_ip(args[1]) {
        Some(addr) => {
            robot_os_drivers::kprintln!("PING {}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]);
            for seq in 1u32..=4 {
                let r = robot_os_net::net_ping(addr);
                if r == 0 {
                    robot_os_drivers::kprintln!("  seq={}: sent", seq);
                } else {
                    robot_os_drivers::kprintln!("  seq={}: no route (ARP miss)", seq);
                }
                for _ in 0..50000 { core::hint::spin_loop(); }
                robot_os_net::net_poll();
            }
        }
        None => robot_os_drivers::kprintln!("Invalid IP address"),
    }
}

fn cmd_arp() {
    robot_os_net::arp::dump();
}

fn cmd_gpio(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc >= 2 && args[1] == b"info" {
        robot_os_drivers::gpio::gpio_info();
    } else {
        robot_os_drivers::kprintln!("Usage: gpio info");
    }
}

fn cmd_pwm(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc >= 2 && args[1] == b"info" {
        robot_os_drivers::pwm::pwm_info();
    } else {
        robot_os_drivers::kprintln!("Usage: pwm info");
    }
}

fn cmd_i2c(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc >= 2 && args[1] == b"scan" {
        let bus = if argc >= 3 { parse_u8(args[2]) } else { 0 };
        robot_os_drivers::i2c::i2c_scan(bus);
    } else if argc >= 2 && args[1] == b"info" {
        robot_os_drivers::i2c::i2c_info();
    } else {
        robot_os_drivers::kprintln!("Usage: i2c scan [bus] | i2c info");
    }
}

fn cmd_motor(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc >= 2 && args[1] == b"info" {
        robot_os_robot::motor_info();
    } else {
        robot_os_drivers::kprintln!("Usage: motor info");
    }
}

/// Write text arguments (joined by spaces + newline) to a FAT32 file.
fn cmd_write(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 3 {
        robot_os_drivers::kprintln!("Usage: write <path> <text...>");
        return;
    }
    let path = args[1];
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(
        &mut fd_table, path,
        robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC,
    );
    if fd < 0 {
        robot_os_drivers::kprint!("[FS] Cannot create: ");
        for &b in path { robot_os_drivers::uart::putc(b); }
        robot_os_drivers::kprintln!();
        return;
    }
    for i in 2..argc {
        if i > 2 {
            robot_os_fs::vfs_write(&mut fd_table, fd, b" ".as_ptr(), 1);
        }
        robot_os_fs::vfs_write(&mut fd_table, fd, args[i].as_ptr(), args[i].len());
    }
    robot_os_fs::vfs_write(&mut fd_table, fd, b"\n".as_ptr(), 1);
    robot_os_fs::vfs_close(&mut fd_table, fd);
    robot_os_drivers::kprintln!("[FS] Written");
}

/// Remove a file from the FAT32 root directory.
fn cmd_rm(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 2 {
        robot_os_drivers::kprintln!("Usage: rm <path>");
        return;
    }
    let path = args[1];
    if !path.starts_with(b"/fat/") {
        robot_os_drivers::kprintln!("[FS] rm only supports /fat/<file> paths");
        return;
    }
    let name = &path[5..]; // strip "/fat/"
    if name.is_empty() || name.contains(&b'/') {
        robot_os_drivers::kprintln!("[FS] Only root-level FAT32 files supported");
        return;
    }
    match robot_os_fs::fat32_unlink_path(name) {
        Ok(())  => robot_os_drivers::kprintln!("[FS] Removed"),
        Err(()) => robot_os_drivers::kprintln!("[FS] Not found or cannot remove"),
    }
}

#[cfg(not(feature = "no-mmu"))]
fn cmd_exec(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 2 {
        robot_os_drivers::kprintln!("Usage: exec <path>");
        return;
    }
    let path = args[1];

    // Read the ELF from FAT32 using VFS.
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path, robot_os_fs::O_RDONLY);
    if fd < 0 {
        robot_os_drivers::kprint!("[EXEC] Cannot open: ");
        for &b in path { robot_os_drivers::uart::putc(b); }
        robot_os_drivers::kprintln!();
        return;
    }

    // Read into a static buffer (max 256 KiB — enough for small test ELFs).
    static mut ELF_BUF: [u8; 256 * 1024] = [0u8; 256 * 1024];
    let mut total = 0usize;
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(ELF_BUF) };

    loop {
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf[total..].as_mut_ptr(), 512);
        if n <= 0 { break; }
        total += n as usize;
        if total >= buf.len() { break; }
    }
    robot_os_fs::vfs_close(&mut fd_table, fd);

    if total == 0 {
        robot_os_drivers::kprintln!("[EXEC] Empty file");
        return;
    }

    robot_os_drivers::kprintln!("[EXEC] Loading {} bytes...", total);
    let r = robot_os_sched::exec_user(&buf[..total]);
    if r != 0 {
        robot_os_drivers::kprintln!("[EXEC] exec_user failed (bad ELF?)");
        return;
    }
    // The shell is a kernel task — it cannot rely on the ecall/SRET mechanism.
    // Instead, we take the prepared ExecContext and SRET to U-mode directly.
    if let Some(ctx) = robot_os_sched::take_pending_exec() {
        robot_os_drivers::kprintln!("[EXEC] SRET to user-space entry={:#x}", ctx.entry);
        unsafe {
            robot_os_sched::sret_to_user(
                ctx.entry   as usize,
                ctx.user_sp as usize,
                ctx.satp    as usize,
            );
        }
        // sret_to_user() is -> ! — unreachable
    }
}

/// RVV 1.0 interactive benchmark — runs scalar vs RVV dot product + matmul.
///
/// When built without `--features rvv`, prints a hint and returns immediately.
/// Timer interrupt is disabled during RVV operations (vector context save is
/// a Phase 12 TODO; preemption mid-RVV would corrupt vector registers).
fn cmd_rvv() {
    #[cfg(not(feature = "rvv"))]
    {
        robot_os_drivers::kprintln!("[RVV] not available — build with: make qemu-rvv");
        return;
    }

    #[cfg(feature = "rvv")]
    {
        use robot_os_arch::{csr, rvv};

        robot_os_drivers::kprintln!("[RVV] RISC-V Vector Extension 1.0 benchmark");
        robot_os_drivers::kprintln!("[RVV] VLEN=128, LMUL=m4, f32 precision");
        robot_os_drivers::kprintln!();

        // Disable timer IRQ — prevent vector register corruption during bench.
        let saved_sie = csr::read_sie();
        csr::write_sie(saved_sie & !csr::SIE_STIE);

        // ── Dot product: 256 f32 ────────────────────────────────────────────
        const N: usize = 256;
        let mut a = [0.0f32; N];
        let mut b = [0.0f32; N];
        for i in 0..N {
            a[i] = (i as f32) * 0.001;
            b[i] = 1.0_f32;
        }

        let (sc, vc, _, _) = rvv::bench_dot(&a, &b);
        let sp = if vc > 0 { sc * 100 / vc } else { 0 };
        robot_os_drivers::kprintln!("[RVV] dot({} f32):", N);
        robot_os_drivers::kprintln!("[RVV]   scalar : {} cycles", sc);
        robot_os_drivers::kprintln!("[RVV]   rvv    : {} cycles", vc);
        robot_os_drivers::kprintln!("[RVV]   speedup: {}.{}x", sp / 100, sp % 100);
        robot_os_drivers::kprintln!();

        // ── Matmul: 8×8×8 f32 ──────────────────────────────────────────────
        const MM: usize = 8;
        const KK: usize = 8;
        const NN: usize = 8;
        let mut ma   = [0.0f32; MM * KK];
        let mut mb   = [0.0f32; KK * NN];
        let mut mc_s = [0.0f32; MM * NN];
        let mut mc_v = [0.0f32; MM * NN];
        for i in 0..MM * KK { ma[i] = (i as f32) * 0.001; }
        for i in 0..KK * NN { mb[i] = (i as f32) * 0.001; }

        let (ms, mv) = rvv::bench_matmul(&mut mc_s, &mut mc_v, &ma, &mb, MM, KK, NN);
        let msp = if mv > 0 { ms * 100 / mv } else { 0 };
        robot_os_drivers::kprintln!("[RVV] matmul({}x{}x{} f32):", MM, KK, NN);
        robot_os_drivers::kprintln!("[RVV]   scalar : {} cycles", ms);
        robot_os_drivers::kprintln!("[RVV]   rvv    : {} cycles", mv);
        robot_os_drivers::kprintln!("[RVV]   speedup: {}.{}x", msp / 100, msp % 100);

        csr::write_sie(saved_sie);
        robot_os_drivers::kprintln!();
        robot_os_drivers::kprintln!("[RVV] done");
    }
}

#[cfg(not(feature = "no-ml"))]
/// Phase 13: print the current sensor→ML→motor pipeline status.
fn cmd_pipeline() {
    let cmd   = robot_os_robot::motor_cmd_read();
    let age   = robot_os_robot::motor_cmd_age_ticks();
    let fired = robot_os_robot::motor_watchdog_fired();

    robot_os_drivers::kprintln!("[PIPELINE] ========================================");
    robot_os_drivers::kprintln!("[PIPELINE]  Phase 13: Robot pipeline status");
    robot_os_drivers::kprintln!("[PIPELINE] ========================================");

    if !robot_os_robot::CH_MOTOR_CMD.is_valid() {
        robot_os_drivers::kprintln!("[PIPELINE] No command published yet");
    } else {
        robot_os_drivers::kprintln!("[PIPELINE] Last cmd: L={} R={}", cmd.speed_l, cmd.speed_r);
        // Convert ticks to ms (QEMU CLINT = 10 MHz → 10 000 ticks/ms).
        let age_ms = age / 10_000;
        robot_os_drivers::kprintln!("[PIPELINE] Command age: {} ticks (~{} ms)", age, age_ms);
    }

    if fired {
        robot_os_drivers::kprintln!("[PIPELINE] Watchdog: FIRED (safe stop active)");
    } else {
        let timeout_ms = robot_os_robot::watchdog_timeout_ticks() / (robot_os_drivers::clint::TIMER_FREQ / 1000);
        robot_os_drivers::kprintln!("[PIPELINE] Watchdog: OK (timeout={} ms)", timeout_ms);
    }

    robot_os_drivers::kprintln!("[PIPELINE] ---- Motor state ----");
    robot_os_robot::motor_info();
    robot_os_drivers::kprintln!("[PIPELINE] ========================================");
}

#[cfg(not(feature = "no-ml"))]
/// Run the 4→8→3 MLP demo inference and print results.
fn cmd_ml() {
    use robot_os_ml::{mlp_infer, argmax3, CLASS_NAMES, DEMO_INPUT};

    robot_os_drivers::kprintln!("[ML] ========================================");
    robot_os_drivers::kprintln!("[ML]  Phase 12: MLP Inference Demo");
    robot_os_drivers::kprintln!("[ML]  Model: 4 → 8 (ReLU) → 3 (logits)");
    robot_os_drivers::kprintln!("[ML] ========================================");

    let input = DEMO_INPUT;
    robot_os_drivers::kprintln!("[ML] Input (normalised 0..1):");
    robot_os_drivers::kprintln!("[ML]   dist_front = {}", fmt_f32(input[0]));
    robot_os_drivers::kprintln!("[ML]   dist_right = {}", fmt_f32(input[1]));
    robot_os_drivers::kprintln!("[ML]   velocity   = {}", fmt_f32(input[2]));
    robot_os_drivers::kprintln!("[ML]   battery    = {}", fmt_f32(input[3]));
    robot_os_drivers::kprintln!();

    let logits = mlp_infer(&input);
    let class  = argmax3(&logits);

    robot_os_drivers::kprintln!("[ML] Logits:");
    for i in 0..3 {
        let marker = if i == class { " ←" } else { "" };
        robot_os_drivers::kprintln!("[ML]   [{}] {:12} : {}{}",
            i, CLASS_NAMES[i], fmt_f32(logits[i]), marker);
    }
    robot_os_drivers::kprintln!();
    robot_os_drivers::kprintln!("[ML] Prediction: {} (class {})", CLASS_NAMES[class], class);

    #[cfg(feature = "rvv")]
    robot_os_drivers::kprintln!("[ML] Backend: RVV 1.0 dot product");
    #[cfg(not(feature = "rvv"))]
    robot_os_drivers::kprintln!("[ML] Backend: scalar (build with make qemu-rvv for RVV)");

    robot_os_drivers::kprintln!("[ML] ========================================");
}

#[cfg(not(feature = "no-ml"))]
/// Format f32 as "±X.XXX" (no libc, no alloc).
fn fmt_f32(v: f32) -> FmtF32 { FmtF32(v) }

#[cfg(not(feature = "no-ml"))]
struct FmtF32(f32);
#[cfg(not(feature = "no-ml"))]
impl core::fmt::Display for FmtF32 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let v = self.0;
        let neg = v < 0.0;
        let abs = if neg { -v } else { v };
        let int  = abs as u32;
        let frac = ((abs - int as f32) * 1000.0 + 0.5) as u32;
        if neg { f.write_str("-")?; }
        write!(f, "{}.{:03}", int, frac)
    }
}

#[cfg(not(feature = "no-ml"))]
/// Phase 14: virtual camera command — capture a frame or print driver info.
///
/// Usage:
///   cam info           — print camera resolution, regions, backend
///   cam capture [0-2]  — capture pattern 0/1/2, print pixels + features
///   cam              — defaults to `cam info`
fn cmd_cam(args: &[&[u8]; MAX_ARGS], argc: usize) {
    use robot_os_camera::{
        cam_capture, cam_extract_features, cam_info,
        CAM_W, CAM_H, PATTERN_COUNT,
    };
    use robot_os_ml::{mlp_infer, argmax3, CLASS_NAMES};

    let sub = if argc >= 2 { args[1] } else { b"info" };

    if sub == b"info" {
        cam_info();
        return;
    }

    if sub == b"capture" {
        // Default to pattern 0 if not specified.
        let pattern_num = if argc >= 3 { parse_u8(args[2]) } else { 0 };
        let pattern = if pattern_num < PATTERN_COUNT { pattern_num } else { 0 };

        let pattern_name = match pattern {
            1 => "right_wall",
            2 => "obstacle",
            _ => "clear",
        };

        robot_os_drivers::kprintln!("[CAM] Capturing pattern {} ({})", pattern, pattern_name);

        let frame = cam_capture(pattern);
        let feat  = cam_extract_features(&frame);

        // Print frame as a grid of 3-digit pixel values.
        robot_os_drivers::kprintln!("[CAM] Frame ({}x{} pixels):", CAM_W, CAM_H);
        for row in 0..CAM_H {
            robot_os_drivers::kprint!("[CAM] ");
            for col in 0..CAM_W {
                let px = frame.pixels[row * CAM_W + col];
                robot_os_drivers::kprint!(" {:3}", px);
            }
            robot_os_drivers::kprintln!();
        }

        // Print extracted features.
        let df = (feat.dist_front * 1000.0) as u32;
        let dr = (feat.dist_right * 1000.0) as u32;
        robot_os_drivers::kprintln!("[CAM] dist_front = {}.{:03} (center region mean)",
            df / 1000, df % 1000);
        robot_os_drivers::kprintln!("[CAM] dist_right = {}.{:03} (right  region mean)",
            dr / 1000, dr % 1000);

        // Run the MLP on the extracted features.
        let input: [f32; 4] = [feat.dist_front, feat.dist_right, 0.5, 0.9];
        let logits = mlp_infer(&input);
        let class  = argmax3(&logits);

        robot_os_drivers::kprintln!("[CAM] MLP logits (x1000):");
        for i in 0..3 {
            let l = (logits[i] * 1000.0) as i32;
            let marker = if i == class { " <--" } else { "" };
            robot_os_drivers::kprintln!("[CAM]   [{}] {:12} : {}{}", i, CLASS_NAMES[i], l, marker);
        }
        robot_os_drivers::kprintln!("[CAM] Prediction: {} (class {})", CLASS_NAMES[class], class);
        return;
    }

    robot_os_drivers::kprintln!("Usage: cam info | cam capture [0-2]");
}

#[cfg(not(feature = "no-ml"))]
/// Phase 15: model command — inspect or reload RMLP weights at runtime.
///
/// Usage:
///   model info         — print model status (loaded/hardcoded, dimensions)
///   model load <path>  — read .rmlp file from FAT32 and activate its weights
///   model              — defaults to `model info`
fn cmd_model(args: &[&[u8]; MAX_ARGS], argc: usize) {
    use robot_os_ml::{model_is_loaded, model_load_bytes, CLASS_NAMES,
                      mlp_infer, argmax3, DEMO_INPUT, RMLP_FILE_SIZE};

    let sub = if argc >= 2 { args[1] } else { b"info" };

    if sub == b"info" {
        robot_os_drivers::kprintln!("[MODEL] ========================================");
        robot_os_drivers::kprintln!("[MODEL]  Phase 15: RMLP Model Loader");
        robot_os_drivers::kprintln!("[MODEL]  Architecture: 4 → 8 (ReLU) → 3");
        robot_os_drivers::kprintln!("[MODEL]  Format: .rmlp ({} bytes)", RMLP_FILE_SIZE);
        if model_is_loaded() {
            robot_os_drivers::kprintln!("[MODEL]  Weights: DYNAMIC (loaded from FAT32)");
        } else {
            robot_os_drivers::kprintln!("[MODEL]  Weights: HARDCODED (compile-time)");
        }
        // Run inference on DEMO_INPUT to show current model behaviour.
        let logits = mlp_infer(&DEMO_INPUT);
        let class  = argmax3(&logits);
        let l0 = (logits[0] * 1000.0) as i32;
        let l1 = (logits[1] * 1000.0) as i32;
        let l2 = (logits[2] * 1000.0) as i32;
        robot_os_drivers::kprintln!("[MODEL]  Demo logits (x1000): go_forward={} turn_right={} stop={}",
            l0, l1, l2);
        robot_os_drivers::kprintln!("[MODEL]  Demo prediction: {} (class {})", CLASS_NAMES[class], class);
        robot_os_drivers::kprintln!("[MODEL] ========================================");
        return;
    }

    if sub == b"load" {
        if argc < 3 {
            robot_os_drivers::kprintln!("Usage: model load <path>");
            return;
        }
        let path = args[2];

        // Read the .rmlp file from FAT32 into a static buffer.
        static mut RMLP_BUF: [u8; 512] = [0u8; 512];
        let buf = unsafe { &mut *(&raw mut RMLP_BUF) };

        let mut fd_table = robot_os_fs::FdTable::new();
        let fd = robot_os_fs::vfs_open(&mut fd_table, path, robot_os_fs::O_RDONLY);
        if fd < 0 {
            robot_os_drivers::kprint!("[MODEL] Cannot open: ");
            for &b in path { robot_os_drivers::uart::putc(b); }
            robot_os_drivers::kprintln!();
            return;
        }
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), buf.len());
        robot_os_fs::vfs_close(&mut fd_table, fd);

        if n <= 0 {
            robot_os_drivers::kprintln!("[MODEL] Empty file");
            return;
        }

        if model_load_bytes(&buf[..n as usize]) {
            robot_os_drivers::kprintln!("[MODEL] Loaded {} bytes — dynamic weights active", n);
            // Verify with DEMO_INPUT.
            let logits = mlp_infer(&DEMO_INPUT);
            let class  = argmax3(&logits);
            robot_os_drivers::kprintln!("[MODEL] Verification: DEMO_INPUT → {} (class {})",
                CLASS_NAMES[class], class);
        } else {
            robot_os_drivers::kprintln!("[MODEL] Load failed — bad magic, version, or dimensions");
            robot_os_drivers::kprintln!("[MODEL] Expected {} bytes, got {}", RMLP_FILE_SIZE, n);
        }
        return;
    }

    robot_os_drivers::kprintln!("Usage: model info | model load <path>");
}

// ── Integer formatting helpers (for CSV trajectory flush) ────────────────────

/// Write decimal digits of `v` into `buf[pos..]`. Returns new position.
fn write_u64(buf: &mut [u8], pos: usize, v: u64) -> usize {
    if pos >= buf.len() { return pos; }
    if v == 0 {
        buf[pos] = b'0';
        return pos + 1;
    }
    let mut tmp = [0u8; 20];
    let mut n = v;
    let mut len = 0usize;
    while n > 0 && len < 20 {
        tmp[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
    }
    tmp[..len].reverse();
    let end = (pos + len).min(buf.len());
    buf[pos..end].copy_from_slice(&tmp[..end - pos]);
    end
}

fn write_i64(buf: &mut [u8], mut pos: usize, v: i64) -> usize {
    if pos >= buf.len() { return pos; }
    if v < 0 {
        buf[pos] = b'-';
        pos += 1;
        // Avoid i64::MIN negation overflow: use wrapping_neg cast to u64.
        write_u64(buf, pos, (v as u64).wrapping_neg())
    } else {
        write_u64(buf, pos, v as u64)
    }
}

fn write_i32(buf: &mut [u8], pos: usize, v: i32) -> usize {
    write_i64(buf, pos, v as i64)
}

// ── Phase 17 commands ─────────────────────────────────────────────────────────

/// Phase 17: print dead-reckoning odometry state.
fn cmd_odom() {
    let (tl, tr)             = robot_os_robot::encoder_read();
    let (dist_mm, hdg_cdeg)  = robot_os_robot::odom_get();

    // Convert heading_cdeg to deg + centideg remainder for display.
    let neg = hdg_cdeg < 0;
    let abs_cdeg = if neg { hdg_cdeg.wrapping_neg() } else { hdg_cdeg };
    let deg  = abs_cdeg / 100;
    let frac = abs_cdeg % 100;

    robot_os_drivers::kprintln!("[ODOM] ========================================");
    robot_os_drivers::kprintln!("[ODOM]  Phase 17: Dead-reckoning odometry");
    robot_os_drivers::kprintln!("[ODOM] ========================================");
    robot_os_drivers::kprintln!("[ODOM]  Encoder ticks : L={}  R={}", tl, tr);
    robot_os_drivers::kprintln!("[ODOM]  Total distance: {} mm", dist_mm);
    if neg {
        robot_os_drivers::kprintln!("[ODOM]  Heading change: -{}.{:02} deg", deg, frac);
    } else {
        robot_os_drivers::kprintln!("[ODOM]  Heading change: +{}.{:02} deg", deg, frac);
    }
    robot_os_drivers::kprintln!("[ODOM] ========================================");
}

/// Phase 17: trajectory ring buffer command.
///
/// Subcommands:
///   traj status    — show buffer fill level.
///   traj dump [N]  — print last N points to UART (default: all).
///   traj flush     — write all points as CSV to /fat/TRAJ.CSV.
///   traj reset     — clear the ring buffer.
fn cmd_traj(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub = if argc >= 2 { args[1] } else { b"status" };

    if sub == b"status" {
        let n = robot_os_robot::traj_len();
        robot_os_drivers::kprintln!("[TRAJ] Ring buffer: {}/{} points recorded",
            n, robot_os_robot::TRAJ_CAP);
        return;
    }

    if sub == b"reset" {
        robot_os_robot::traj_reset();
        robot_os_drivers::kprintln!("[TRAJ] Ring buffer cleared");
        return;
    }

    if sub == b"dump" {
        let total = robot_os_robot::traj_len();
        let want  = if argc >= 3 { parse_u8(args[2]) as usize } else { total };
        let start = if want < total { total - want } else { 0 };
        robot_os_drivers::kprintln!("[TRAJ] ts_ms | spd_L | spd_R | class | dist_mm | hdg_cdeg");
        for i in start..total {
            if let Some(p) = robot_os_robot::traj_get(i) {
                robot_os_drivers::kprintln!("[TRAJ] {} {} {} {} {} {}",
                    p.timestamp_ms, p.speed_l, p.speed_r,
                    p.ml_class as u32, p.dist_mm, p.heading_cdeg);
            }
        }
        return;
    }

    if sub == b"flush" {
        cmd_traj_flush();
        return;
    }

    robot_os_drivers::kprintln!("Usage: traj status | dump [N] | flush | reset");
}

/// Write trajectory ring buffer to /fat/TRAJ.CSV.
fn cmd_traj_flush() {
    static mut TRAJ_CSV: [u8; 8192] = [0u8; 8192];
    let buf = unsafe { &mut *(&raw mut TRAJ_CSV) };

    let n = robot_os_robot::traj_len();
    if n == 0 {
        robot_os_drivers::kprintln!("[TRAJ] No points to flush");
        return;
    }

    // Build CSV content.
    let header = b"timestamp_ms,speed_l,speed_r,ml_class,dist_mm,heading_cdeg\n";
    let mut pos = 0usize;
    if pos + header.len() <= buf.len() {
        buf[pos..pos + header.len()].copy_from_slice(header);
        pos += header.len();
    }

    let mut written = 0usize;
    for i in 0..n {
        if let Some(p) = robot_os_robot::traj_get(i) {
            if pos + 100 > buf.len() { break; } // buffer safety margin
            pos = write_u64(buf, pos, p.timestamp_ms); buf[pos] = b','; pos += 1;
            pos = write_i32(buf, pos, p.speed_l);      buf[pos] = b','; pos += 1;
            pos = write_i32(buf, pos, p.speed_r);      buf[pos] = b','; pos += 1;
            buf[pos] = b'0' + (p.ml_class % 10);      buf[pos + 1] = b','; pos += 2;
            pos = write_i64(buf, pos, p.dist_mm);      buf[pos] = b','; pos += 1;
            pos = write_i64(buf, pos, p.heading_cdeg); buf[pos] = b'\n'; pos += 1;
            written += 1;
        }
    }

    // Write to FAT32.
    let path = b"/fat/TRAJ.CSV";
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path,
        robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC);
    if fd < 0 {
        robot_os_drivers::kprintln!("[TRAJ] Cannot create /fat/TRAJ.CSV (mount FAT32 first)");
        return;
    }
    robot_os_fs::vfs_write(&mut fd_table, fd, buf.as_ptr(), pos);
    robot_os_fs::vfs_close(&mut fd_table, fd);
    robot_os_drivers::kprintln!("[TRAJ] Flushed {} points ({} bytes) → /fat/TRAJ.CSV",
        written, pos);
}

/// OTA firmware update — A/B slot management over TCP.
///
/// Subcommands:
///   ota recv <port>    — receive firmware image over TCP, write to inactive slot
///   ota status         — show current slot, boot count, versions
///   ota verify         — CRC-32 check both firmware slots
///   ota rollback       — switch active slot to last known good
fn cmd_ota(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 2 {
        robot_os_drivers::kprintln!("Usage: ota <recv|status|verify|rollback>");
        robot_os_drivers::kprintln!("  recv <port>  — receive firmware over TCP");
        robot_os_drivers::kprintln!("  status       — show OTA slot info");
        robot_os_drivers::kprintln!("  verify       — CRC-32 check firmware slots");
        robot_os_drivers::kprintln!("  rollback     — revert to last good slot");
        return;
    }

    if args[1] == b"status" {
        cmd_ota_status();
    } else if args[1] == b"verify" {
        cmd_ota_verify();
    } else if args[1] == b"rollback" {
        cmd_ota_rollback();
    } else if args[1] == b"recv" {
        if argc < 3 {
            robot_os_drivers::kprintln!("Usage: ota recv <port>");
            return;
        }
        let port = parse_u16(args[2]);
        if port == 0 {
            robot_os_drivers::kprintln!("[OTA] Invalid port");
            return;
        }
        cmd_ota_recv(port);
    } else {
        robot_os_drivers::kprintln!("[OTA] Unknown subcommand. Use: recv, status, verify, rollback");
    }
}

fn cmd_ota_status() {
    let meta = robot_os_ota::ota_read_boot_meta();
    let slot_char = |s: u8| if s == robot_os_ota::SLOT_A { 'A' } else { 'B' };
    robot_os_drivers::kprintln!("[OTA] Active slot:  {}", slot_char(meta.active_slot));
    robot_os_drivers::kprintln!("[OTA] Boot count:   {}/{}", meta.boot_count,
        robot_os_ota::CFG_OTA_MAX_BOOT_ATTEMPTS.load(core::sync::atomic::Ordering::Relaxed));
    robot_os_drivers::kprintln!("[OTA] Last good:    {}", slot_char(meta.last_good));
    robot_os_drivers::kprintln!("[OTA] Slot A: fw={} size={} crc={:#010x}",
        meta.fw_version_a, meta.image_size_a, meta.image_crc_a);
    robot_os_drivers::kprintln!("[OTA] Slot B: fw={} size={} crc={:#010x}",
        meta.fw_version_b, meta.image_size_b, meta.image_crc_b);
    robot_os_drivers::kprintln!("[OTA] Platform:     {}", match robot_os_ota::ota_current_platform() {
        robot_os_ota::OTA_PLATFORM_QEMU => "QEMU",
        robot_os_ota::OTA_PLATFORM_VF2  => "VisionFive 2",
        robot_os_ota::OTA_PLATFORM_K1   => "SpacemiT K1",
        _ => "ESP32-C3",
    });
}

fn cmd_ota_verify() {
    for (label, slot) in [
        ("A", robot_os_ota::SLOT_A),
        ("B", robot_os_ota::SLOT_B),
    ] {
        let (ver, size, crc) = robot_os_ota::ota_slot_info(slot);
        if size == 0 {
            robot_os_drivers::kprintln!("[OTA] Slot {} — empty (no firmware)", label);
            continue;
        }
        if robot_os_ota::ota_verify_slot(slot) {
            robot_os_drivers::kprintln!(
                "[OTA] Slot {} — OK  (fw={}, size={}, crc={:#010x})",
                label, ver, size, crc);
        } else {
            robot_os_drivers::kprintln!(
                "[OTA] Slot {} — CRC FAIL or missing file (expected size={}, crc={:#010x})",
                label, size, crc);
        }
    }
}

fn cmd_ota_rollback() {
    let mut meta = robot_os_ota::ota_read_boot_meta();
    if meta.active_slot == meta.last_good {
        robot_os_drivers::kprintln!("[OTA] Already on last good slot ({})",
            if meta.active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });
        return;
    }
    let old = meta.active_slot;
    meta.active_slot = meta.last_good;
    meta.boot_count = 0;
    robot_os_ota::ota_write_boot_meta(&meta);
    robot_os_ota::ota_apply_meta(&meta);
    robot_os_drivers::kprintln!("[OTA] Rolled back: {} → {} (reboot to apply)",
        if old == robot_os_ota::SLOT_A { 'A' } else { 'B' },
        if meta.active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });
}

/// Receive firmware over TCP, write to inactive slot, validate CRC-32.
fn cmd_ota_recv(port: u16) {
    let target_slot = robot_os_ota::ota_inactive_slot();
    let target_path = robot_os_ota::ota_slot_path(target_slot);
    let platform = robot_os_ota::ota_current_platform();

    robot_os_drivers::kprintln!("[OTA] Listening on port {} — target slot {}",
        port, if target_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });

    // Create TCP listener
    let listen_fd = robot_os_net::socket_create(
        robot_os_net::AF_INET, robot_os_net::SOCK_STREAM, 0);
    if listen_fd < 0 {
        robot_os_drivers::kprintln!("[OTA] socket_create failed");
        return;
    }

    let mut addr = robot_os_net::SockAddr::new();
    addr.family = robot_os_net::AF_INET as u16;
    addr.port   = port;

    if robot_os_net::socket_bind(listen_fd, &addr) < 0 ||
       robot_os_net::socket_listen_bound(listen_fd) < 0 {
        robot_os_drivers::kprintln!("[OTA] bind/listen failed");
        robot_os_net::socket_close(listen_fd);
        return;
    }

    // Accept connection
    let client_fd = loop {
        robot_os_net::net_poll();
        let r = robot_os_net::socket_accept(listen_fd);
        if r >= 0 { break r; }
        robot_os_sched::task_yield();
    };
    robot_os_drivers::kprintln!("[OTA] Client connected — receiving header...");

    // Receive OTA header (24 bytes)
    static mut OTA_HDR_BUF: [u8; 24] = [0u8; 24];
    let hdr_buf = unsafe { &mut *(&raw mut OTA_HDR_BUF) };
    let mut hdr_got = 0usize;

    while hdr_got < robot_os_ota::OTA_HEADER_SIZE {
        robot_os_net::net_poll();
        let n = robot_os_net::socket_recv(client_fd, &mut hdr_buf[hdr_got..]);
        if n > 0 {
            hdr_got += n as usize;
        } else if n < 0 {
            robot_os_drivers::kprintln!("[OTA] Connection lost during header");
            robot_os_net::socket_close(client_fd);
            robot_os_net::socket_close(listen_fd);
            return;
        }
        robot_os_sched::task_yield();
    }

    // Parse and validate header
    let header = match robot_os_ota::ota_parse_header(hdr_buf) {
        Some(h) => h,
        None => {
            robot_os_drivers::kprintln!("[OTA] Invalid header (bad magic or version)");
            robot_os_net::socket_close(client_fd);
            robot_os_net::socket_close(listen_fd);
            return;
        }
    };

    if !robot_os_ota::ota_validate_header(&header, platform) {
        robot_os_drivers::kprintln!("[OTA] Header validation failed (platform={}, size={})",
            header.platform_id, header.image_size);
        robot_os_net::socket_close(client_fd);
        robot_os_net::socket_close(listen_fd);
        return;
    }

    robot_os_drivers::kprintln!("[OTA] Header OK — fw={} size={} crc={:#010x}",
        header.fw_version, header.image_size, header.image_crc32);

    // Open target file
    let mut fd_table = robot_os_fs::FdTable::new();
    let file_fd = robot_os_fs::vfs_open(&mut fd_table, target_path,
        robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC);
    if file_fd < 0 {
        robot_os_drivers::kprintln!("[OTA] Cannot create target file");
        robot_os_net::socket_close(client_fd);
        robot_os_net::socket_close(listen_fd);
        return;
    }

    // Stream payload from TCP to FAT32 as raw binary (no header on disk).
    // The .BIN file is directly bootable by U-Boot.
    static mut OTA_CHUNK: [u8; 4096] = [0u8; 4096];
    let chunk = unsafe { &mut *(&raw mut OTA_CHUNK) };
    let mut crc_state = robot_os_ota::Crc32State::new();
    let mut remaining = header.image_size as usize;
    let mut total_written = 0usize;

    robot_os_drivers::kprintln!("[OTA] Receiving {} bytes...", header.image_size);

    while remaining > 0 {
        robot_os_net::net_poll();
        let max_recv = remaining.min(chunk.len());
        let n = robot_os_net::socket_recv(client_fd, &mut chunk[..max_recv]);
        if n > 0 {
            let got = n as usize;
            crc_state.update(&chunk[..got]);
            robot_os_fs::vfs_write(&mut fd_table, file_fd, chunk.as_ptr(), got);
            remaining -= got;
            total_written += got;
        } else if n < 0 {
            robot_os_drivers::kprintln!("[OTA] Connection lost ({}/{} bytes received)",
                total_written, header.image_size);
            break;
        }
        robot_os_sched::task_yield();
    }

    robot_os_fs::vfs_close(&mut fd_table, file_fd);
    robot_os_net::socket_close(client_fd);
    robot_os_net::socket_close(listen_fd);

    if remaining > 0 {
        robot_os_drivers::kprintln!("[OTA] INCOMPLETE — deleting partial image");
        let _ = robot_os_fs::fat32_unlink_path(target_path);
        return;
    }

    // Verify CRC-32
    let computed_crc = crc_state.finalize();
    if computed_crc != header.image_crc32 {
        robot_os_drivers::kprintln!("[OTA] CRC MISMATCH: computed={:#010x} expected={:#010x}",
            computed_crc, header.image_crc32);
        robot_os_drivers::kprintln!("[OTA] Deleting corrupt image");
        let _ = robot_os_fs::fat32_unlink_path(target_path);
        return;
    }

    robot_os_drivers::kprintln!("[OTA] CRC OK — {} bytes written to slot {}",
        total_written, if target_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });

    // Update boot metadata: switch to new slot, record CRC + size for verify
    let mut meta = robot_os_ota::ota_read_boot_meta();
    meta.active_slot = target_slot;
    meta.boot_count = 0;
    if target_slot == robot_os_ota::SLOT_A {
        meta.fw_version_a = header.fw_version;
        meta.image_size_a = header.image_size;
        meta.image_crc_a  = header.image_crc32;
    } else {
        meta.fw_version_b = header.fw_version;
        meta.image_size_b = header.image_size;
        meta.image_crc_b  = header.image_crc32;
    }
    robot_os_ota::ota_write_boot_meta(&meta);
    robot_os_ota::ota_apply_meta(&meta);

    robot_os_drivers::kprintln!("[OTA] Active slot → {} (fw={}). Reboot to apply.",
        if target_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' },
        header.fw_version);
}

/// Phase 16: print a summary of all active security layers.
fn cmd_security() {
    robot_os_drivers::kprintln!("[SEC] ========================================");
    robot_os_drivers::kprintln!("[SEC]  Phase 16: Security overview");
    robot_os_drivers::kprintln!("[SEC] ========================================");

    // Sv39 paging
    let satp = robot_os_arch::csr::read_satp();
    if satp != 0 {
        robot_os_drivers::kprintln!("[SEC]  Sv39 paging:     ACTIVE  (satp={:#x})", satp);
    } else {
        robot_os_drivers::kprintln!("[SEC]  Sv39 paging:     DISABLED");
    }

    // Stack canaries
    let (ok, total) = robot_os_sched::stack_canary_check();
    robot_os_drivers::kprintln!(
        "[SEC]  Stack canaries:  {}/{} intact  (magic=0xDEADBEEFCAFE1234)", ok, total);

    // RT motor watchdog
    let wdt_fired = robot_os_robot::motor_watchdog_fired();
    if wdt_fired {
        robot_os_drivers::kprintln!("[SEC]  RT watchdog:     FIRED   (motors stopped)");
    } else {
        let t = robot_os_robot::watchdog_timeout_ticks() / (robot_os_drivers::clint::TIMER_FREQ / 1000);
        robot_os_drivers::kprintln!("[SEC]  RT watchdog:     OK      (timeout={} ms)", t);
    }

    // System watchdog task
    robot_os_drivers::kprintln!("[SEC]  System watchdog: RUNNING (sys-wdt task, ~500 ms)");

    // RMLP model
    #[cfg(not(feature = "no-ml"))]
    if robot_os_ml::model_is_loaded() {
        robot_os_drivers::kprintln!("[SEC]  RMLP model:      DYNAMIC (loaded from FAT32)");
    } else {
        robot_os_drivers::kprintln!("[SEC]  RMLP model:      HARDCODED (compile-time)");
    }
    #[cfg(feature = "no-ml")]
    robot_os_drivers::kprintln!("[SEC]  RMLP model:      DISABLED (no-ml)");

    robot_os_drivers::kprintln!("[SEC] ========================================");
}

/// Crash log management: `crash log` / `crash clear`.
fn cmd_crash(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"log" };

    if sub == b"log" {
        let mut fd_table = robot_os_fs::FdTable::new();
        let fd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/CRASH.LOG",
                                        robot_os_fs::O_RDONLY);
        if fd < 0 {
            robot_os_drivers::kprintln!("[CRASH] No crash log found");
            return;
        }
        static mut CRASH_READ_BUF: [u8; 2048] = [0u8; 2048];
        let buf = unsafe { &mut *(&raw mut CRASH_READ_BUF) };
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), buf.len());
        robot_os_fs::vfs_close(&mut fd_table, fd);
        if n <= 0 {
            robot_os_drivers::kprintln!("[CRASH] Crash log empty");
            return;
        }
        robot_os_drivers::kprintln!("[CRASH] === /fat/CRASH.LOG ({} bytes) ===", n);
        for &b in &buf[..n as usize] {
            robot_os_drivers::uart::putc(b);
        }
        robot_os_drivers::kprintln!("[CRASH] === end ===");
    } else if sub == b"clear" {
        let _ = robot_os_fs::fat32_unlink_path(b"/fat/CRASH.LOG");
        robot_os_drivers::kprintln!("[CRASH] Crash log cleared");
    } else {
        robot_os_drivers::kprintln!("Usage: crash [log|clear]");
    }
}

fn cmd_shutdown() -> ! {
    robot_os_drivers::kprintln!("[SHELL] System shutdown...");
    robot_os_arch::sbi::shutdown()
}

fn cmd_reboot() -> ! {
    robot_os_drivers::kprintln!("[SHELL] System reboot...");
    robot_os_arch::sbi::reboot()
}

// ── Parsing utilities ─────────────────────────────────────────────────────────

fn parse_ip(s: &[u8]) -> Option<[u8; 4]> {
    let mut result = [0u8; 4];
    let mut octet  = 0u32;
    let mut idx    = 0usize;
    let mut digits = 0usize;

    for &b in s {
        if b >= b'0' && b <= b'9' {
            octet  = octet * 10 + (b - b'0') as u32;
            digits += 1;
            if octet > 255 { return None; }
        } else if b == b'.' {
            if digits == 0 || idx >= 3 { return None; }
            result[idx] = octet as u8;
            idx   += 1;
            octet  = 0;
            digits = 0;
        } else {
            return None;
        }
    }
    if idx == 3 && digits > 0 {
        result[3] = octet as u8;
        Some(result)
    } else {
        None
    }
}

fn parse_u8(s: &[u8]) -> u8 {
    let mut v = 0u32;
    for &b in s {
        if b < b'0' || b > b'9' { break; }
        v = v * 10 + (b - b'0') as u32;
    }
    v.min(255) as u8
}

fn parse_u16(s: &[u8]) -> u16 {
    let mut v = 0u32;
    for &b in s {
        if b < b'0' || b > b'9' { break; }
        v = v * 10 + (b - b'0') as u32;
    }
    v.min(65535) as u16
}

fn parse_u32(s: &[u8]) -> u32 {
    let mut v = 0u64;
    for &b in s {
        if b < b'0' || b > b'9' { break; }
        v = v * 10 + (b - b'0') as u64;
    }
    v.min(u32::MAX as u64) as u32
}

/// TCP echo server: listen on given port, echo back received data.
/// Usage: tcpecho <port>
fn cmd_tcpecho(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc < 2 {
        robot_os_drivers::kprintln!("Usage: tcpecho <port>");
        return;
    }
    let port = parse_u16(args[1]);
    if port == 0 {
        robot_os_drivers::kprintln!("[NET] Invalid port");
        return;
    }

    robot_os_drivers::kprintln!("[NET] TCP echo server on port {} (Ctrl+C to stop)", port);

    // Create and bind listener socket
    let listen_fd = robot_os_net::socket_create(
        robot_os_net::AF_INET, robot_os_net::SOCK_STREAM, 0,
    );
    if listen_fd < 0 {
        robot_os_drivers::kprintln!("[NET] socket_create failed");
        return;
    }

    let mut addr = robot_os_net::SockAddr::new();
    addr.family = robot_os_net::AF_INET as u16;
    addr.port   = port;

    if robot_os_net::socket_bind(listen_fd, &addr) < 0 {
        robot_os_drivers::kprintln!("[NET] bind failed");
        robot_os_net::socket_close(listen_fd);
        return;
    }
    if robot_os_net::socket_listen_bound(listen_fd) < 0 {
        robot_os_drivers::kprintln!("[NET] listen failed");
        robot_os_net::socket_close(listen_fd);
        return;
    }

    robot_os_drivers::kprintln!("[NET] Waiting for connection...");

    // Poll until a client connects
    let client_fd = loop {
        robot_os_net::net_poll();
        let r = robot_os_net::socket_accept(listen_fd);
        if r >= 0 { break r; }
        robot_os_sched::task_yield();
    };

    robot_os_drivers::kprintln!("[NET] Client connected (fd={})", client_fd);

    // Echo loop: receive data and send it back
    let mut buf = [0u8; 256];
    loop {
        robot_os_net::net_poll();
        let n = robot_os_net::socket_recv(client_fd, &mut buf);
        if n > 0 {
            let sent = robot_os_net::socket_send(client_fd, &buf[..n as usize]);
            robot_os_drivers::kprintln!("[NET] echoed {} bytes (sent={})", n, sent);
        } else if n < 0 {
            robot_os_drivers::kprintln!("[NET] Connection closed");
            break;
        }
        robot_os_sched::task_yield();
    }

    robot_os_net::socket_close(client_fd);
    robot_os_net::socket_close(listen_fd);
    robot_os_drivers::kprintln!("[NET] Echo server done");
}

// ── Phase 18 + G2: Persistent configuration ──────────────────────────────────

/// Push config atomics to every subsystem (net, sched, behavior, encoder, wdt).
/// Called after `cfg_apply()` from `config set`, `config load`, or `config defaults`.
fn apply_config_to_subsystems() {
    use core::sync::atomic::Ordering;

    // Network
    let ip   = robot_os_config::unpack_ip(
        robot_os_config::CFG_NET_IP.load(Ordering::Relaxed));
    let mask = robot_os_config::unpack_ip(
        robot_os_config::CFG_NET_MASK.load(Ordering::Relaxed));
    let gw   = robot_os_config::unpack_ip(
        robot_os_config::CFG_NET_GATEWAY.load(Ordering::Relaxed));
    robot_os_net::net_set_ip(ip, mask, gw);

    // Scheduler Hz
    let hz = robot_os_config::cfg_get_u32(b"sched_hz", 100);
    if hz >= 10 {
        robot_os_drivers::clint::sched_hz_set(hz as u64);
    }

    // Behavior layers
    robot_os_behavior::layer_set_enabled(1,
        robot_os_config::BEHAVIOR_L1_ENABLED.load(Ordering::Relaxed));
    robot_os_behavior::layer_set_enabled(2,
        robot_os_config::BEHAVIOR_L2_ENABLED.load(Ordering::Relaxed));
    robot_os_behavior::layer_set_enabled(3,
        robot_os_config::BEHAVIOR_L3_ENABLED.load(Ordering::Relaxed));

    // Behavior VLA server
    let bport = robot_os_config::BEHAVIOR_SERVER_PORT.load(Ordering::Relaxed);
    if bport > 0 {
        let bip = robot_os_config::behavior_server_ip_bytes();
        robot_os_behavior::remote_configure(bip, bport as u16);
    }

    // Encoder physical params
    robot_os_robot::set_ticks_per_m(
        robot_os_config::CFG_TICKS_PER_M.load(Ordering::Relaxed));
    robot_os_robot::set_wheel_base_mm(
        robot_os_config::CFG_WHEEL_BASE_MM.load(Ordering::Relaxed));

    // Watchdog (note: wdt_init re-programs the hardware timer)
    robot_os_drivers::wdt::wdt_init(
        robot_os_config::CFG_WATCHDOG_MS.load(Ordering::Relaxed));
}

/// `config [list | get <key> | set <key> <val> | save | load | defaults | export]`
///
/// Manages the in-memory key-value config store and persists it to
/// `/fat/CONFIG.INI` on the FAT32 volume.
fn cmd_config(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"list" };

    if sub == b"list" {
        let count = robot_os_config::cfg_count();
        robot_os_drivers::kprintln!("[CFG] {} entries:", count);
        for i in 0..count {
            if let Some((k, v)) = robot_os_config::cfg_iter(i) {
                for &b in k { robot_os_drivers::uart::putc(b); }
                robot_os_drivers::uart::putc(b'=');
                for &b in v { robot_os_drivers::uart::putc(b); }
                robot_os_drivers::kprintln!();
            }
        }
        robot_os_drivers::kprintln!("[CFG] ml_enabled (runtime)={}",
            robot_os_config::ML_ENABLED.load(core::sync::atomic::Ordering::Relaxed) as u8);
        return;
    }

    if sub == b"get" {
        if argc < 3 {
            robot_os_drivers::kprintln!("Usage: config get <key>");
            return;
        }
        let key = args[2];
        match robot_os_config::cfg_get(key) {
            None => {
                robot_os_drivers::kprint!("[CFG] not found: ");
                for &b in key { robot_os_drivers::uart::putc(b); }
                robot_os_drivers::kprintln!();
            }
            Some(v) => {
                for &b in key { robot_os_drivers::uart::putc(b); }
                robot_os_drivers::uart::putc(b'=');
                for &b in v { robot_os_drivers::uart::putc(b); }
                robot_os_drivers::kprintln!();
            }
        }
        return;
    }

    if sub == b"set" {
        if argc < 4 {
            robot_os_drivers::kprintln!("Usage: config set <key> <val>");
            return;
        }
        let key = args[2];
        let val = args[3];
        if robot_os_config::cfg_set(key, val) {
            robot_os_config::cfg_apply();
            apply_config_to_subsystems();
            robot_os_drivers::kprintln!("[CFG] set OK");
        } else {
            robot_os_drivers::kprintln!(
                "[CFG] FAILED: key>{} or val>{} bytes, or table full ({})",
                robot_os_config::MAX_KEY, robot_os_config::MAX_VAL,
                robot_os_config::MAX_ENTRIES);
        }
        return;
    }

    if sub == b"save" {
        static mut SAVE_BUF: [u8; 1024] = [0u8; 1024];
        let buf = unsafe { &mut *(&raw mut SAVE_BUF) };
        let n = robot_os_config::cfg_serialize(buf);
        if n == 0 {
            robot_os_drivers::kprintln!("[CFG] nothing to save");
            return;
        }
        let mut fd_table = robot_os_fs::FdTable::new();
        let fd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/CONFIG.INI",
            robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC);
        if fd < 0 {
            robot_os_drivers::kprintln!("[CFG] cannot open /fat/CONFIG.INI for write");
            return;
        }
        let written = robot_os_fs::vfs_write(&mut fd_table, fd, buf.as_ptr(), n);
        robot_os_fs::vfs_close(&mut fd_table, fd);
        robot_os_drivers::kprintln!("[CFG] saved {} bytes to /fat/CONFIG.INI", written);
        return;
    }

    if sub == b"load" {
        static mut LOAD_BUF: [u8; 1024] = [0u8; 1024];
        let buf = unsafe { &mut *(&raw mut LOAD_BUF) };
        let mut fd_table = robot_os_fs::FdTable::new();
        let fd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/CONFIG.INI",
                                        robot_os_fs::O_RDONLY);
        if fd < 0 {
            robot_os_drivers::kprintln!("[CFG] /fat/CONFIG.INI not found (mount FAT32 first)");
            return;
        }
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), buf.len());
        robot_os_fs::vfs_close(&mut fd_table, fd);
        if n > 0 {
            robot_os_config::cfg_load(&buf[..n as usize]);
            robot_os_config::cfg_apply();
            apply_config_to_subsystems();
            robot_os_drivers::kprintln!("[CFG] loaded {} entries",
                robot_os_config::cfg_count());
        } else {
            robot_os_drivers::kprintln!("[CFG] empty or read error");
        }
        return;
    }

    // Phase G2: reset to factory defaults (in-memory only, use `config save` to persist).
    if sub == b"defaults" {
        robot_os_config::cfg_defaults();
        robot_os_config::cfg_apply();
        apply_config_to_subsystems();
        robot_os_drivers::kprintln!("[CFG] factory defaults applied ({} entries)",
            robot_os_config::cfg_count());
        return;
    }

    // Phase G2: export all config as KEY=VALUE over UART (copy/paste backup).
    if sub == b"export" {
        static mut EXPORT_BUF: [u8; 1024] = [0u8; 1024];
        let buf = unsafe { &mut *(&raw mut EXPORT_BUF) };
        let n = robot_os_config::cfg_serialize(buf);
        robot_os_drivers::kprintln!("# Robot OS CONFIG.INI ({} bytes)", n);
        for &b in &buf[..n] {
            robot_os_drivers::uart::putc(b);
        }
        return;
    }

    robot_os_drivers::kprintln!(
        "Usage: config [list|get <key>|set <k> <v>|save|load|defaults|export]");
}

// ── Phase G1: Behavior Engine + VLA Protocol ─────────────────────────────────

fn cmd_behavior(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"status" };

    if sub == b"status" {
        robot_os_drivers::kprintln!("[BEHAVIOR] Subsumption layers:");
        let statuses = robot_os_behavior::layer_statuses();
        for ls in &statuses {
            let mark = if ls.winning { " <-- WINNING" } else { "" };
            robot_os_drivers::kprintln!("  L{}: {:16} enabled={} {}",
                ls.layer, ls.name,
                ls.enabled as u8, mark);
        }

        // Remote info
        let ri = robot_os_behavior::remote_info();
        if ri.enabled {
            robot_os_drivers::kprintln!("[BEHAVIOR] Remote VLA: {}.{}.{}.{}:{} connected={} tx={} rx={}",
                ri.server_ip[0], ri.server_ip[1], ri.server_ip[2], ri.server_ip[3],
                ri.server_port, ri.connected as u8,
                ri.packets_sent, ri.packets_recv);
        } else {
            robot_os_drivers::kprintln!("[BEHAVIOR] Remote VLA: disabled");
        }

        // Current goal
        let goal = robot_os_behavior::current_goal();
        if goal.valid {
            robot_os_drivers::kprint!("[BEHAVIOR] Goal #{}: ", goal.goal_id);
            for i in 0..goal.text_len as usize {
                robot_os_drivers::uart::putc(goal.text[i]);
            }
            robot_os_drivers::kprintln!();
        } else {
            robot_os_drivers::kprintln!("[BEHAVIOR] Goal: (none)");
        }
        return;
    }

    if sub == b"enable" {
        if argc < 3 {
            robot_os_drivers::kprintln!("Usage: behavior enable <layer>");
            return;
        }
        let layer = parse_u32_shell(args[2]) as usize;
        if layer == 0 {
            robot_os_drivers::kprintln!("[BEHAVIOR] Layer 0 cannot be disabled");
            return;
        }
        if layer >= robot_os_behavior::NUM_LAYERS {
            robot_os_drivers::kprintln!("[BEHAVIOR] Invalid layer (0-3)");
            return;
        }
        robot_os_behavior::layer_set_enabled(layer, true);
        robot_os_drivers::kprintln!("[BEHAVIOR] L{} enabled", layer);
        return;
    }

    if sub == b"disable" {
        if argc < 3 {
            robot_os_drivers::kprintln!("Usage: behavior disable <layer>");
            return;
        }
        let layer = parse_u32_shell(args[2]) as usize;
        if layer == 0 {
            robot_os_drivers::kprintln!("[BEHAVIOR] Layer 0 cannot be disabled");
            return;
        }
        if layer >= robot_os_behavior::NUM_LAYERS {
            robot_os_drivers::kprintln!("[BEHAVIOR] Invalid layer (0-3)");
            return;
        }
        robot_os_behavior::layer_set_enabled(layer, false);
        robot_os_drivers::kprintln!("[BEHAVIOR] L{} disabled", layer);
        return;
    }

    if sub == b"remote" {
        if argc < 4 {
            robot_os_drivers::kprintln!("Usage: behavior remote <ip> <port>");
            return;
        }
        if let Some(ip) = parse_ip_shell(args[2]) {
            let port = parse_u32_shell(args[3]) as u16;
            robot_os_behavior::remote_configure(ip, port);
            robot_os_drivers::kprintln!("[BEHAVIOR] VLA server: {}.{}.{}.{}:{}",
                ip[0], ip[1], ip[2], ip[3], port);
        } else {
            robot_os_drivers::kprintln!("[BEHAVIOR] Invalid IP format (a.b.c.d)");
        }
        return;
    }

    if sub == b"goal" {
        let goal = robot_os_behavior::current_goal();
        if goal.valid {
            robot_os_drivers::kprint!("[BEHAVIOR] Goal #{}: ", goal.goal_id);
            for i in 0..goal.text_len as usize {
                robot_os_drivers::uart::putc(goal.text[i]);
            }
            robot_os_drivers::kprintln!();
        } else {
            robot_os_drivers::kprintln!("[BEHAVIOR] No active goal from VLA server");
        }
        return;
    }

    robot_os_drivers::kprintln!("Usage: behavior [status|enable <n>|disable <n>|remote <ip> <port>|goal]");
}

/// Parse a u32 from a byte slice (shell helper).
fn parse_u32_shell(s: &[u8]) -> u32 {
    let mut v   = 0u32;
    for &b in s {
        if b < b'0' || b > b'9' { break; }
        v = v.saturating_mul(10).saturating_add((b - b'0') as u32);
    }
    v
}

/// Parse an IPv4 address from "a.b.c.d" byte slice.
fn parse_ip_shell(s: &[u8]) -> Option<[u8; 4]> {
    let mut ip = [0u8; 4];
    let mut octet = 0u32;
    let mut idx = 0usize;
    let mut any = false;

    for &b in s {
        if b >= b'0' && b <= b'9' {
            octet = octet * 10 + (b - b'0') as u32;
            any = true;
        } else if b == b'.' {
            if !any || idx >= 3 || octet > 255 { return None; }
            ip[idx] = octet as u8;
            idx += 1;
            octet = 0;
            any = false;
        } else {
            break;
        }
    }
    if any && idx == 3 && octet <= 255 {
        ip[3] = octet as u8;
        Some(ip)
    } else {
        None
    }
}

// ── Phase D: PMP + WDT + fuzz commands ────────────────────────────────────────

/// Show the Robot OS PMP memory-protection policy (informational; M-mode only to enforce).
#[cfg(not(feature = "esp32c3"))]
fn cmd_pmp() {
    use robot_os_arch::pmp;
    // Use platform kernel-load address as firmware_end; PMM watermark as proxy for heap.
    let fw_end    = robot_os_drivers::platform::hw::KERNEL_LOAD;
    let heap_mark = robot_os_mm::pmm::next_free_addr();
    let regions   = pmp::pmp_regions(fw_end, heap_mark, heap_mark, 4 * 1024 * 1024);
    robot_os_drivers::kprintln!("[PMP] Memory-protection policy ({} TOR regions):", pmp::N_PMP_REGIONS);
    robot_os_drivers::kprintln!("[PMP]   Note: CSRs are M-mode only; enforce from boot stub.");
    for r in &regions {
        robot_os_drivers::kprintln!("[PMP]   {:20}  base={:#010x}  size={:#010x}  {}{}{}",
            r.name,
            r.base, r.size,
            if r.perm.r { "R" } else { "-" },
            if r.perm.w { "W" } else { "-" },
            if r.perm.x { "X" } else { "-" });
    }
}
#[cfg(feature = "esp32c3")]
fn cmd_pmp() {
    robot_os_drivers::kprintln!("[PMP] Not available on ESP32-C3 (no PMP in M-mode stub)");
}

/// Show hardware watchdog status.
fn cmd_wdt() {
    use robot_os_drivers::wdt;
    if wdt::wdt_has_hardware() {
        robot_os_drivers::kprintln!("[WDT] Hardware WDT present (DesignWare)");
        robot_os_drivers::kprintln!("[WDT] Counter = {}", wdt::wdt_counter());
        robot_os_drivers::kprintln!("[WDT] Kick is called every timer tick (~1 ms)");
    } else {
        robot_os_drivers::kprintln!("[WDT] No hardware WDT (QEMU) — software watchdog only");
        robot_os_drivers::kprintln!("[WDT] Software WDT: sys-wdt task checks canaries + timer");
    }
}

/// Basic memory write+read fuzz test over a stack buffer.
fn cmd_fuzz() {
    const N: usize = 256;
    let mut buf = [0u32; N];
    let magic: u32 = 0xCAFE_BEEF;
    for (i, v) in buf.iter_mut().enumerate() {
        *v = magic ^ (i as u32);
    }
    let mut ok = 0usize;
    for (i, v) in buf.iter().enumerate() {
        if *v == magic ^ (i as u32) { ok += 1; }
    }
    robot_os_drivers::kprintln!("[FUZZ] Stack memory test: {}/{} cells correct", ok, N);
    if ok == N {
        robot_os_drivers::kprintln!("[FUZZ] PASS");
    } else {
        robot_os_drivers::kprintln!("[FUZZ] FAIL ({} errors)", N - ok);
    }
}

// ── Phase E1: Scheduler Hz ────────────────────────────────────────────────────

/// `sched_hz [<hz>]` — show or set the scheduler tick rate.
fn cmd_sched_hz(args: &[&[u8]; MAX_ARGS], argc: usize) {
    if argc >= 2 {
        let hz = parse_u32(args[1]) as u64;
        if hz >= 10 && hz <= 10_000 {
            robot_os_drivers::clint::sched_hz_set(hz);
            robot_os_drivers::kprintln!("[SCHED] Scheduler rate set to {} Hz", hz);
        } else {
            robot_os_drivers::kprintln!("[SCHED] Invalid Hz (range 10..10000)");
        }
    } else {
        let hz = robot_os_drivers::clint::sched_hz_get();
        robot_os_drivers::kprintln!("[SCHED] Scheduler: {} Hz (TIMER_FREQ={})",
            hz, robot_os_drivers::clint::TIMER_FREQ);
    }
}

// ── Phase E2: IMU ─────────────────────────────────────────────────────────────

/// `imu [info|read]` — MPU-6050 IMU sensor.
fn cmd_imu(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"read" };

    if sub == b"info" {
        robot_os_imu::imu_info();
    } else if sub == b"read" {
        match robot_os_imu::imu_read_scaled() {
            Some(d) => {
                robot_os_drivers::kprintln!(
                    "[IMU] Accel: X={} Y={} Z={} mg",
                    d.accel_mg[0], d.accel_mg[1], d.accel_mg[2]);
                robot_os_drivers::kprintln!(
                    "[IMU] Gyro:  X={} Y={} Z={} mdps",
                    d.gyro_mdps[0], d.gyro_mdps[1], d.gyro_mdps[2]);
                let deg = d.temp_cdeg / 100;
                let frac = (d.temp_cdeg % 100).unsigned_abs();
                robot_os_drivers::kprintln!("[IMU] Temp:  {}.{:02} C", deg, frac);
            }
            None => {
                robot_os_drivers::kprintln!("[IMU] Read failed (not initialized?)");
            }
        }
    } else {
        robot_os_drivers::kprintln!("Usage: imu [info|read]");
    }
}

fn cmd_baro(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"read" };

    if sub == b"info" {
        robot_os_baro::baro_info();
    } else if sub == b"read" {
        match robot_os_baro::baro_read() {
            Some(d) => {
                let hpa = d.pressure_pa / 100;
                let hpa_frac = d.pressure_pa % 100;
                let deg = d.temp_cdeg / 100;
                let frac = (d.temp_cdeg % 100).unsigned_abs();
                robot_os_drivers::kprintln!("[BARO] Pressure: {}.{:02} hPa ({} Pa)",
                    hpa, hpa_frac, d.pressure_pa);
                robot_os_drivers::kprintln!("[BARO] Temp:     {}.{:02} C", deg, frac);
            }
            None => {
                robot_os_drivers::kprintln!("[BARO] Read failed (not initialized?)");
            }
        }
    } else {
        robot_os_drivers::kprintln!("Usage: baro [info|read]");
    }
}

// ── Phase I1: AHRS attitude ──────────────────────────────────────────────────

fn cmd_attitude() {
    robot_os_ahrs::attitude_info();
}

// ── Phase I2: GPS ────────────────────────────────────────────────────────────

fn cmd_gps(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"info" };

    if sub == b"info" {
        robot_os_gps::gps_info();
        // Also show channel age.
        let snap = robot_os_gps::CH_GPS.read();
        if snap.seq > 0 {
            robot_os_drivers::kprintln!("[GPS] ch seq={} age={} ticks",
                snap.seq,
                robot_os_gps::CH_GPS.age(robot_os_drivers::clint::get_time()));
        }
    } else if sub == b"read" {
        match robot_os_gps::gps_read() {
            Some(pos) => {
                robot_os_drivers::kprintln!("[GPS] fix={} sats={} hdop={}.{:02}",
                    pos.fix, pos.sats, pos.hdop / 100, pos.hdop % 100);
                let (lat_sign, lat_abs) = if pos.lat_deg7 < 0 { ("-", (-pos.lat_deg7) as u32) } else { ("", pos.lat_deg7 as u32) };
                let (lon_sign, lon_abs) = if pos.lon_deg7 < 0 { ("-", (-pos.lon_deg7) as u32) } else { ("", pos.lon_deg7 as u32) };
                robot_os_drivers::kprintln!("[GPS] lat={}{}.{:07} lon={}{}.{:07}",
                    lat_sign, lat_abs / 10_000_000, lat_abs % 10_000_000,
                    lon_sign, lon_abs / 10_000_000, lon_abs % 10_000_000);
                let alt_sign = if pos.alt_mm < 0 { "-" } else { "" };
                let alt_abs = pos.alt_mm.unsigned_abs();
                robot_os_drivers::kprintln!("[GPS] alt={}{}.{:03}m speed={}.{:02}m/s course={}.{:02}deg",
                    alt_sign, alt_abs / 1000, alt_abs % 1000,
                    pos.speed_cms / 100, pos.speed_cms % 100,
                    pos.course_cdeg / 100, pos.course_cdeg % 100);
            }
            None => {
                robot_os_drivers::kprintln!("[GPS] Not initialized");
            }
        }
    } else {
        robot_os_drivers::kprintln!("Usage: gps [info|read]");
    }
}

// ── Phase J: flight controller ───────────────────────────────────────────────

fn cmd_flight(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"status" };

    if sub == b"status" {
        robot_os_flight::flight_info();
    } else if sub == b"arm" {
        robot_os_flight::flight_arm();
        robot_os_drivers::esc::esc_arm();
    } else if sub == b"disarm" {
        robot_os_flight::flight_disarm();
        robot_os_drivers::esc::esc_disarm();
    } else if sub == b"mode" {
        if argc < 3 {
            robot_os_drivers::kprintln!("[FLIGHT] Current mode: {}",
                robot_os_flight::flight_mode().name());
            robot_os_drivers::kprintln!("Usage: flight mode <disarmed|manual|stabilize|althold|poshold|auto|rtl|land>");
            return;
        }
        match robot_os_flight::FlightMode::from_str(args[2]) {
            Some(mode) => {
                robot_os_flight::set_flight_mode(mode);
                robot_os_drivers::kprintln!("[FLIGHT] Mode set: {}", mode.name());
            }
            None => {
                robot_os_drivers::kprintln!("[FLIGHT] Unknown mode");
            }
        }
    } else {
        robot_os_drivers::kprintln!("Usage: flight [status|arm|disarm|mode <mode>]");
    }
}

fn cmd_rc() {
    robot_os_drivers::rc::rc_info();
}

fn cmd_esc() {
    robot_os_drivers::esc::esc_info();
}

// ── Phase L: telemetry ──────────────────────────────────────────────────────

fn cmd_telem(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"status" };

    if sub == b"status" {
        robot_os_telemetry::telem_info();
    } else if sub == b"start" {
        let port: u16 = if argc >= 3 {
            parse_u32(args[2]) as u16
        } else {
            5000
        };
        robot_os_telemetry::telem_start(port);
    } else if sub == b"stop" {
        robot_os_telemetry::telem_stop();
    } else {
        robot_os_drivers::kprintln!("Usage: telem [status|start <port>|stop]");
    }
}

// ── Phase M+N: perception + navigation ────────────────────────────────────────

fn cmd_range() {
    robot_os_drivers::rangefinder::range_info();
}

fn cmd_nav(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"info" };

    if sub == b"info" {
        robot_os_nav::nav_info();
    } else {
        robot_os_drivers::kprintln!("Usage: nav [info]");
    }
}

fn cmd_csi() {
    robot_os_drivers::csi::csi_info();
}

fn cmd_wifi(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"info" };

    if sub == b"info" {
        robot_os_drivers::wifi::wifi_info();
    } else if sub == b"connect" {
        let ssid: &[u8] = if argc >= 3 { args[2] } else { b"RobotAP" };
        let pass: &[u8] = if argc >= 4 { args[3] } else { b"" };
        robot_os_drivers::wifi::wifi_connect(ssid, pass);
    } else if sub == b"disconnect" {
        robot_os_drivers::wifi::wifi_disconnect();
    } else {
        robot_os_drivers::kprintln!("Usage: wifi [info|connect <ssid> [pass]|disconnect]");
    }
}

// ── Phase H: new driver + subsystem commands ─────────────────────────────────

fn cmd_spi() {
    robot_os_drivers::spi::spi_info();
}

fn cmd_can(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"info" };

    if sub == b"info" {
        robot_os_drivers::can::can_info();
    } else if sub == b"send" {
        let frame = robot_os_drivers::can::CanFrame::standard(
            0x123, &[0xDE, 0xAD, 0xBE, 0xEF],
        );
        let rc = robot_os_drivers::can::can_send(&frame);
        robot_os_drivers::kprintln!("[CAN] send(id=0x123, 4B) = {}", rc);
    } else if sub == b"recv" {
        match robot_os_drivers::can::can_recv() {
            Some(f) => robot_os_drivers::kprintln!("[CAN] recv: id=0x{:03x} dlc={} data={:02x?}",
                f.id, f.dlc, &f.data[..f.dlc as usize]),
            None => robot_os_drivers::kprintln!("[CAN] No frames in buffer"),
        }
    } else {
        robot_os_drivers::kprintln!("Usage: can [info|send|recv]");
    }
}

fn cmd_dma() {
    robot_os_drivers::dma::dma_info();
}

fn cmd_usb() {
    robot_os_drivers::usb::usb_info();
}

fn cmd_pm(args: &[&[u8]; MAX_ARGS], argc: usize) {
    let sub: &[u8] = if argc >= 2 { args[1] } else { b"info" };

    if sub == b"info" {
        robot_os_drivers::pm::pm_info();
    } else if sub == b"idle" {
        robot_os_drivers::kprintln!("[PM] Entering idle...");
        robot_os_drivers::pm::pm_idle();
        robot_os_drivers::kprintln!("[PM] Resumed from idle");
    } else if sub == b"suspend" {
        robot_os_drivers::kprintln!("[PM] Entering suspend...");
        robot_os_drivers::pm::pm_suspend();
        robot_os_drivers::kprintln!("[PM] Resumed from suspend");
    } else if sub == b"resume" {
        robot_os_drivers::pm::pm_resume();
        robot_os_drivers::kprintln!("[PM] Forced resume");
    } else {
        robot_os_drivers::kprintln!("Usage: pm [info|idle|suspend|resume]");
    }
}

fn cmd_eth() {
    robot_os_drivers::eth::eth_info();
}

fn cmd_dhcp() {
    robot_os_drivers::kprintln!("[DHCP] Starting DHCP discovery...");
    let ok = robot_os_net::dhcp::dhcp_start(robot_os_sched::task_yield);
    if ok {
        robot_os_drivers::kprintln!("[DHCP] Bound successfully");
    } else {
        robot_os_drivers::kprintln!("[DHCP] Failed to obtain IP");
    }
}

#[cfg(not(feature = "no-mmu"))]
fn cmd_fork() {
    robot_os_drivers::kprintln!("[FORK] Calling sys_fork_impl()...");
    let rc = robot_os_sched::process::sys_fork_impl();
    robot_os_drivers::kprintln!("[FORK] Result: {}", rc);
}

// ── Main shell loop ───────────────────────────────────────────────────────────

/// Main shell entry point.  Runs forever, reading and executing commands.
/// Should be called from a kernel task.
pub fn shell_run() -> ! {
    robot_os_drivers::kprintln!();
    robot_os_drivers::kprintln!("Robot OS shell — type 'help' for commands");
    robot_os_drivers::kprintln!();

    let mut line_buf = [0u8; MAX_LINE];

    loop {
        robot_os_drivers::uart::puts(PROMPT);

        let len = readline(&mut line_buf);
        robot_os_net::net_poll();

        if len > 0 {
            let mut args: [&[u8]; MAX_ARGS] = [b""; MAX_ARGS];
            let argc = parse_args(&line_buf[..len], &mut args);

            if argc > 0 {
                let cmd = args[0];
                if      cmd == b"help"     { cmd_help(); }
                else if cmd == b"exec"     {
                    #[cfg(not(feature = "no-mmu"))]
                    cmd_exec(&args, argc);
                    #[cfg(feature = "no-mmu")]
                    robot_os_drivers::kprintln!("[SHELL] exec disabled (compiled with --features no-mmu)");
                }
                else if cmd == b"ps"       { cmd_ps(); }
                else if cmd == b"mem"      { cmd_mem(); }
                else if cmd == b"uptime"   { cmd_uptime(); }
                else if cmd == b"ls"       { cmd_ls(&args, argc); }
                else if cmd == b"cat"      { cmd_cat(&args, argc); }
                else if cmd == b"write"    { cmd_write(&args, argc); }
                else if cmd == b"rm"       { cmd_rm(&args, argc); }
                else if cmd == b"mkdir"    { cmd_mkdir(&args, argc); }
                else if cmd == b"echo"     { cmd_echo(&args, argc); }
                else if cmd == b"disk"     { cmd_disk(); }
                else if cmd == b"ifconfig" { cmd_ifconfig(); }
                else if cmd == b"ping"     { cmd_ping(&args, argc); }
                else if cmd == b"arp"      { cmd_arp(); }
                else if cmd == b"tcpecho"  { cmd_tcpecho(&args, argc); }
                else if cmd == b"gpio"     { cmd_gpio(&args, argc); }
                else if cmd == b"pwm"      { cmd_pwm(&args, argc); }
                else if cmd == b"i2c"      { cmd_i2c(&args, argc); }
                else if cmd == b"motor"    { cmd_motor(&args, argc); }
                else if cmd == b"rvv"      { cmd_rvv(); }
                else if cmd == b"ml" || cmd == b"pipeline" || cmd == b"cam"
                     || cmd == b"model" {
                    #[cfg(not(feature = "no-ml"))]
                    {
                        if      cmd == b"ml"       { cmd_ml(); }
                        else if cmd == b"pipeline" { cmd_pipeline(); }
                        else if cmd == b"cam"      { cmd_cam(&args, argc); }
                        else if cmd == b"model"    { cmd_model(&args, argc); }
                    }
                    #[cfg(feature = "no-ml")]
                    robot_os_drivers::kprintln!("[SHELL] ML disabled (compiled with --features no-ml)");
                }
                else if cmd == b"ota" { cmd_ota(&args, argc); }
                else if cmd == b"security" { cmd_security(); }
                else if cmd == b"odom"     { cmd_odom(); }
                else if cmd == b"traj"     { cmd_traj(&args, argc); }
                else if cmd == b"config"   { cmd_config(&args, argc); }
                else if cmd == b"behavior" { cmd_behavior(&args, argc); }
                else if cmd == b"pmp"      { cmd_pmp(); }
                else if cmd == b"wdt"      { cmd_wdt(); }
                else if cmd == b"fuzz"     { cmd_fuzz(); }
                else if cmd == b"sched_hz" { cmd_sched_hz(&args, argc); }
                else if cmd == b"imu"      { cmd_imu(&args, argc); }
                else if cmd == b"baro"     { cmd_baro(&args, argc); }
                else if cmd == b"attitude" { cmd_attitude(); }
                else if cmd == b"gps"      { cmd_gps(&args, argc); }
                else if cmd == b"flight"   { cmd_flight(&args, argc); }
                else if cmd == b"rc"       { cmd_rc(); }
                else if cmd == b"esc"      { cmd_esc(); }
                else if cmd == b"telem"    { cmd_telem(&args, argc); }
                else if cmd == b"range"    { cmd_range(); }
                else if cmd == b"nav"      { cmd_nav(&args, argc); }
                else if cmd == b"csi"      { cmd_csi(); }
                else if cmd == b"wifi"     { cmd_wifi(&args, argc); }
                else if cmd == b"spi"      { cmd_spi(); }
                else if cmd == b"can"      { cmd_can(&args, argc); }
                else if cmd == b"dma"      { cmd_dma(); }
                else if cmd == b"usb"      { cmd_usb(); }
                else if cmd == b"pm"       { cmd_pm(&args, argc); }
                else if cmd == b"eth"      { cmd_eth(); }
                else if cmd == b"dhcp"     { cmd_dhcp(); }
                else if cmd == b"fork"     {
                    #[cfg(not(feature = "no-mmu"))]
                    cmd_fork();
                    #[cfg(feature = "no-mmu")]
                    robot_os_drivers::kprintln!("[SHELL] fork disabled (compiled with --features no-mmu)");
                }
                else if cmd == b"crash"    { cmd_crash(&args, argc); }
                else if cmd == b"shutdown" { cmd_shutdown(); }
                else if cmd == b"reboot"   { cmd_reboot(); }
                else {
                    robot_os_drivers::kprint!("[SHELL] Unknown: ");
                    for &b in cmd { robot_os_drivers::uart::putc(b); }
                    robot_os_drivers::kprintln!(" (type 'help')");
                }
            }
        }

        for b in line_buf.iter_mut() { *b = 0; }
    }
}
