//! Brain Client — userspace ELF that bridges sensor data to the brain server.
//!
//! Runs as a user-mode process on the VF2. Reads sensors via syscalls,
//! builds brain protocol packets, sends over TCP to the macOS brain server,
//! receives ActuatorCmd packets, and applies motor commands.
//!
//! Flow:
//!   loop {
//!     1. sensor_read(IMU)   → accel/gyro
//!     2. sensor_read(ODOM)  → distance/heading
//!     3. sensor_read(ENC)   → encoder ticks
//!     4. sensor_read(RANGE) → front/right mm
//!     5. sensor_read(BATT)  → battery mV
//!     6. Build SensorPacket (64 bytes)
//!     7. Frame it: MAGIC + TYPE + LEN + PAYLOAD + CRC8
//!     8. TCP send to brain server
//!     9. TCP recv → parse ActuatorCmd
//!    10. motor_speed(L, R)
//!    11. yield / sleep
//!   }

#![no_std]
#![no_main]

use robot_os_libsys as sys;

// ── Protocol constants (must match brain_protocol.rs + protocol.py) ──────────
const MAGIC: [u8; 2] = *b"BR";
const PKT_SENSOR: u8 = 0x01;
const PKT_ACTUATOR: u8 = 0x80;
const PKT_CONFIG: u8 = 0x83;

const SENSOR_PAYLOAD_SIZE: usize = 64;
const FRAME_OVERHEAD: usize = 6;        // MAGIC(2) + TYPE(1) + LEN(2) + CRC(1)
const SENSOR_FRAME_SIZE: usize = SENSOR_PAYLOAD_SIZE + FRAME_OVERHEAD; // 70

// Sensor types — re-exported from libsys
use sys::{
    SENSOR_TYPE_IMU, SENSOR_TYPE_ODOM, SENSOR_TYPE_ENCODER,
    SENSOR_TYPE_RANGE, SENSOR_TYPE_BATTERY, SENSOR_TYPE_GPIO_FLAGS,
};

// Actuator command layout
const ACT_HDR_SIZE: usize = 3;          // type(1) + n_channels(1) + flags(1)
const FLAG_EMERGENCY: u8 = 0x01;

// CRC-8/MAXIM polynomial
const CRC8_POLY: u8 = 0x31;

// Network
const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;

// Timing
const SENSOR_PERIOD_MS: u64 = 50;       // 20 Hz sensor rate
const CONNECT_RETRY_MS: u64 = 2000;     // retry connection every 2s
const RECV_BUF_SIZE: usize = 32;        // enough for ActuatorCmd + ConfigCmd

// Motor IDs
const MOTOR_LEFT: u64 = 0;
const MOTOR_RIGHT: u64 = 1;

// ── SockAddr (matches kernel's read_sockaddr: family LE + port BE + addr) ────
#[repr(C)]
struct SockAddr {
    family: u16,        // AF_INET = 2, little-endian
    port: [u8; 2],      // big-endian
    addr: [u8; 4],      // IPv4
    _pad: [u8; 8],
}

impl SockAddr {
    fn new(ip: [u8; 4], port: u16) -> Self {
        Self {
            family: AF_INET as u16,
            port: port.to_be_bytes(),
            addr: ip,
            _pad: [0; 8],
        }
    }

    fn as_bytes(&self) -> &[u8; 16] {
        unsafe { &*(self as *const Self as *const [u8; 16]) }
    }
}

// ── CRC-8/MAXIM ─────────────────────────────────────────────────────────────

fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ CRC8_POLY;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

// ── Little-endian helpers ────────────────────────────────────────────────────

fn put_u16_le(buf: &mut [u8], off: usize, v: u16) {
    let b = v.to_le_bytes();
    buf[off] = b[0];
    buf[off + 1] = b[1];
}

fn put_i32_le(buf: &mut [u8], off: usize, v: i32) {
    let b = v.to_le_bytes();
    buf[off..off + 4].copy_from_slice(&b);
}

fn put_u64_le(buf: &mut [u8], off: usize, v: u64) {
    let b = v.to_le_bytes();
    buf[off..off + 8].copy_from_slice(&b);
}

fn put_i64_le(buf: &mut [u8], off: usize, v: i64) {
    let b = v.to_le_bytes();
    buf[off..off + 8].copy_from_slice(&b);
}

fn get_i16_le(buf: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([buf[off], buf[off + 1]])
}

// ── Sensor reading ──────────────────────────────────────────────────────────

struct SensorData {
    accel_mg: [i32; 3],
    gyro_mdps: [i32; 3],
    battery_mv: u16,
    odom_dist_mm: i32,
    odom_heading_cdeg: i32,
    enc_left: i64,
    enc_right: i64,
    range_front: u16,
    range_right: u16,
    sensor_flags: u16,
}

impl SensorData {
    fn new() -> Self {
        Self {
            accel_mg: [0; 3],
            gyro_mdps: [0; 3],
            battery_mv: 0,
            odom_dist_mm: 0,
            odom_heading_cdeg: 0,
            enc_left: 0,
            enc_right: 0,
            range_front: 0,
            range_right: 0,
            sensor_flags: 0,
        }
    }

    fn read_all(&mut self) {
        // IMU: 24 bytes = 6 × i32 LE
        let mut imu_buf = [0u8; 24];
        if sys::sensor_read(SENSOR_TYPE_IMU, &mut imu_buf) >= 24 {
            for i in 0..3 {
                self.accel_mg[i] = i32::from_le_bytes([
                    imu_buf[i * 4], imu_buf[i * 4 + 1],
                    imu_buf[i * 4 + 2], imu_buf[i * 4 + 3],
                ]);
                self.gyro_mdps[i] = i32::from_le_bytes([
                    imu_buf[12 + i * 4], imu_buf[13 + i * 4],
                    imu_buf[14 + i * 4], imu_buf[15 + i * 4],
                ]);
            }
        }

        // Odometry: 16 bytes = dist_mm(i64) + heading_cdeg(i64)
        let mut odom_buf = [0u8; 16];
        if sys::sensor_read(SENSOR_TYPE_ODOM, &mut odom_buf) >= 16 {
            let dist = i64::from_le_bytes([
                odom_buf[0], odom_buf[1], odom_buf[2], odom_buf[3],
                odom_buf[4], odom_buf[5], odom_buf[6], odom_buf[7],
            ]);
            let hdg = i64::from_le_bytes([
                odom_buf[8], odom_buf[9], odom_buf[10], odom_buf[11],
                odom_buf[12], odom_buf[13], odom_buf[14], odom_buf[15],
            ]);
            self.odom_dist_mm = dist as i32;
            self.odom_heading_cdeg = hdg as i32;
        }

        // Encoders: 16 bytes = enc_l(i64) + enc_r(i64)
        let mut enc_buf = [0u8; 16];
        if sys::sensor_read(SENSOR_TYPE_ENCODER, &mut enc_buf) >= 16 {
            self.enc_left = i64::from_le_bytes([
                enc_buf[0], enc_buf[1], enc_buf[2], enc_buf[3],
                enc_buf[4], enc_buf[5], enc_buf[6], enc_buf[7],
            ]);
            self.enc_right = i64::from_le_bytes([
                enc_buf[8], enc_buf[9], enc_buf[10], enc_buf[11],
                enc_buf[12], enc_buf[13], enc_buf[14], enc_buf[15],
            ]);
        }

        // Rangefinder: 4 bytes = front_mm(u16) + right_mm(u16)
        let mut range_buf = [0u8; 4];
        if sys::sensor_read(SENSOR_TYPE_RANGE, &mut range_buf) >= 4 {
            self.range_front = u16::from_le_bytes([range_buf[0], range_buf[1]]);
            self.range_right = u16::from_le_bytes([range_buf[2], range_buf[3]]);
        }

        // Battery: 2 bytes = mv(u16)
        let mut batt_buf = [0u8; 2];
        if sys::sensor_read(SENSOR_TYPE_BATTERY, &mut batt_buf) >= 2 {
            self.battery_mv = u16::from_le_bytes([batt_buf[0], batt_buf[1]]);
        }

        // GPIO sensor flags (PIR/sound/IR)
        let mut flags_buf = [0u8; 2];
        if sys::sensor_read(SENSOR_TYPE_GPIO_FLAGS, &mut flags_buf) >= 2 {
            self.sensor_flags = u16::from_le_bytes([flags_buf[0], flags_buf[1]]);
        } else {
            self.sensor_flags = 0;
        }
    }
}

// ── Packet building ─────────────────────────────────────────────────────────

/// Build SensorPacket payload (64 bytes, matches brain_protocol.rs).
fn build_sensor_payload(buf: &mut [u8; SENSOR_PAYLOAD_SIZE], sensors: &SensorData) {
    let ts_ms = sys::uptime() as u64;

    // Common header (34 bytes)
    put_u64_le(buf, 0, ts_ms);                    // timestamp_ms
    put_i32_le(buf, 8, sensors.accel_mg[0]);       // accel_x
    put_i32_le(buf, 12, sensors.accel_mg[1]);      // accel_y
    put_i32_le(buf, 16, sensors.accel_mg[2]);      // accel_z
    put_i32_le(buf, 20, sensors.gyro_mdps[0]);     // gyro_x
    put_i32_le(buf, 24, sensors.gyro_mdps[1]);     // gyro_y
    put_i32_le(buf, 28, sensors.gyro_mdps[2]);     // gyro_z
    put_u16_le(buf, 32, sensors.battery_mv);       // battery_mv

    // Wheeled extra (30 bytes)
    put_i32_le(buf, 34, sensors.odom_dist_mm);     // odom_dist_mm
    put_i32_le(buf, 38, sensors.odom_heading_cdeg); // odom_hdg_cdeg
    put_i64_le(buf, 42, sensors.enc_left);         // encoder_l
    put_i64_le(buf, 50, sensors.enc_right);        // encoder_r
    put_u16_le(buf, 58, sensors.range_front);      // range_front
    put_u16_le(buf, 60, sensors.range_right);      // range_right
    put_u16_le(buf, 62, sensors.sensor_flags);     // sensor_flags
}

/// Frame a payload: MAGIC(2) + TYPE(1) + LEN(2 LE) + PAYLOAD + CRC8(1).
fn build_frame(
    frame: &mut [u8],
    pkt_type: u8,
    payload: &[u8],
) -> usize {
    let payload_len = payload.len();
    let total = FRAME_OVERHEAD + payload_len;

    frame[0] = MAGIC[0];
    frame[1] = MAGIC[1];
    frame[2] = pkt_type;
    put_u16_le(frame, 3, payload_len as u16);

    frame[5..5 + payload_len].copy_from_slice(payload);

    let crc = crc8(&frame[..5 + payload_len]);
    frame[5 + payload_len] = crc;

    total
}

/// Parse a received frame. Returns (pkt_type, payload_start, payload_len) or None.
fn parse_frame(buf: &[u8], len: usize) -> Option<(u8, usize, usize)> {
    if len < FRAME_OVERHEAD {
        return None;
    }
    if buf[0] != MAGIC[0] || buf[1] != MAGIC[1] {
        return None;
    }
    let pkt_type = buf[2];
    let payload_len = u16::from_le_bytes([buf[3], buf[4]]) as usize;
    let expected_total = FRAME_OVERHEAD + payload_len;
    if len < expected_total {
        return None;
    }
    // Verify CRC
    let crc_idx = 5 + payload_len;
    let computed = crc8(&buf[..crc_idx]);
    if computed != buf[crc_idx] {
        return None;
    }
    Some((pkt_type, 5, payload_len))
}

// ── ActuatorCmd decode ──────────────────────────────────────────────────────

fn apply_actuator_cmd(payload: &[u8]) {
    if payload.len() < ACT_HDR_SIZE {
        return;
    }
    let _act_type = payload[0];
    let n_channels = payload[1] as usize;
    let flags = payload[2];

    if flags & FLAG_EMERGENCY != 0 {
        // Emergency stop
        sys::motor_speed(MOTOR_LEFT, 0);
        sys::motor_speed(MOTOR_RIGHT, 0);
        return;
    }

    let ch_data = &payload[ACT_HDR_SIZE..];
    if n_channels >= 2 && ch_data.len() >= 4 {
        // The brain sends signed i16 speeds; the kernel cannot take one.
        //
        // `sys_motor_speed` (crates/syscall/src/handlers.rs:753) reads an
        // UNSIGNED percentage and hard-codes `MotorDir::Forward`;
        // `motor_set` then clamps with `speed_pct.min(100)`. The old code
        // was `get_i16_le(..) as u64`, so a brain command of -50 ("back
        // up") sign-extended to 0xFFFF...CE and clamped to 100 — the robot
        // drove FULL SPEED FORWARD on a reverse command. Same defect as
        // `userspace/reflex`'s motor_backup(); both trusted a libsys doc
        // that claimed the kernel interpreted the sign.
        //
        // Split the sign off explicitly. Direction and speed cannot be sent
        // in one syscall (see the ABI audit report), so a reverse command
        // becomes `motor_set_direction(BACKWARD)` at the kernel's fixed 50%
        // and the brain's magnitude is not honoured on that path. A forward
        // command keeps its exact magnitude.
        apply_motor_cmd(MOTOR_LEFT, get_i16_le(ch_data, 0));
        apply_motor_cmd(MOTOR_RIGHT, get_i16_le(ch_data, 2));
    }
}

/// Drive one motor from a signed brain command, without ever handing the
/// kernel a sign-extended negative. See `apply_actuator_cmd`.
fn apply_motor_cmd(motor: u64, speed: i16) {
    if speed < 0 {
        // Reverse: only reachable through SYS_MOTOR_ENABLE, which fixes the
        // speed at 50% in the kernel.
        sys::motor_set_direction(motor, sys::MOTOR_DIR_BACKWARD);
    } else {
        // Forward at the requested percentage; the kernel clamps to 100.
        sys::motor_speed(motor, speed as u64);
    }
}

// ── Main loop ───────────────────────────────────────────────────────────────

fn connect_to_brain(ip: [u8; 4], port: u16) -> isize {
    let fd = sys::socket(AF_INET, SOCK_STREAM, 0);
    if fd < 0 {
        return -1;
    }

    // `sys::connect` now takes `&[u8; 16]` directly: the kernel's
    // `read_sockaddr` copies exactly 16 bytes and never reads the `addrlen`
    // argument, so the length is not the caller's to get wrong.
    let addr = SockAddr::new(ip, port);
    let ret = sys::connect(fd as u64, addr.as_bytes());
    if ret < 0 {
        sys::close(fd as u64);
        return -1;
    }
    fd
}

fn brain_client_loop(sock_fd: u64) {
    let mut sensors = SensorData::new();
    let mut payload = [0u8; SENSOR_PAYLOAD_SIZE];
    let mut frame = [0u8; SENSOR_FRAME_SIZE];
    let mut recv_buf = [0u8; RECV_BUF_SIZE];

    loop {
        // 1. Read all sensors
        sensors.read_all();

        // 2. Build and send SensorPacket
        build_sensor_payload(&mut payload, &sensors);
        let frame_len = build_frame(&mut frame, PKT_SENSOR, &payload);
        let sent = sys::send(sock_fd, &frame[..frame_len], 0);
        if sent < 0 {
            // Connection lost
            sys::print(b"[brain_client] Send failed, disconnecting\n");
            break;
        }

        // 3. Check for incoming commands (non-blocking receive)
        let n = sys::recv(sock_fd, &mut recv_buf, 0);
        if n > 0 {
            let n = n as usize;
            if let Some((pkt_type, pay_start, pay_len)) = parse_frame(&recv_buf, n) {
                let payload_slice = &recv_buf[pay_start..pay_start + pay_len];
                match pkt_type {
                    PKT_ACTUATOR => apply_actuator_cmd(payload_slice),
                    PKT_CONFIG => {
                        // Config commands handled by kernel behavior_task
                        // (buzzer, LED, etc.) — forward via IPC in future
                    }
                    _ => {}
                }
            }
        } else if n < 0 {
            // Error or connection closed
            sys::print(b"[brain_client] Recv error, disconnecting\n");
            break;
        }

        // 4. Sleep until next sensor period
        sys::sleep(SENSOR_PERIOD_MS);
    }
}

/// Parse `behavior_server_ip` / `behavior_server_port` out of
/// `/fat/CONFIG.INI`. Returns `None` if the file cannot be read or the keys
/// are absent, so the caller can keep its compiled-in default.
/// Format `[brain_client] Brain server A.B.C.D:PORT (CONFIG.INI|default)\n`
/// into `out`, returning the byte count. Built as one buffer so it can go out
/// in a single `write()` and cannot be interleaved mid-line.
fn fmt_addr_line(out: &mut [u8; 96], ip: [u8; 4], port: u16, from_cfg: bool) -> usize {
    let mut n = 0usize;
    let push = |b: u8, n: &mut usize, out: &mut [u8; 96]| {
        if *n < out.len() { out[*n] = b; *n += 1; }
    };
    for &b in b"[brain_client] Brain server " { push(b, &mut n, out); }
    for (i, o) in ip.iter().enumerate() {
        if i > 0 { push(b'.', &mut n, out); }
        let mut v = *o as u32;
        let mut d = [0u8; 3];
        let mut k = 0;
        if v == 0 { d[0] = b'0'; k = 1; }
        while v > 0 { d[k] = b'0' + (v % 10) as u8; v /= 10; k += 1; }
        while k > 0 { k -= 1; push(d[k], &mut n, out); }
    }
    push(b':', &mut n, out);
    let mut v = port as u32;
    let mut d = [0u8; 5];
    let mut k = 0;
    if v == 0 { d[0] = b'0'; k = 1; }
    while v > 0 { d[k] = b'0' + (v % 10) as u8; v /= 10; k += 1; }
    while k > 0 { k -= 1; push(d[k], &mut n, out); }
    let tag: &[u8] = if from_cfg { b" (from CONFIG.INI)\n" } else { b" (compiled-in default)\n" };
    for &b in tag { push(b, &mut n, out); }
    n
}

fn read_brain_addr() -> Option<([u8; 4], u16)> {
    // The trailing NUL is load-bearing: the kernel reads this path with
    // copy_cstr_from_user, i.e. it scans for a terminator rather than using
    // the slice length. Without one the kernel walks past the literal into
    // whatever the linker put next.
    //
    // `sys::cstr!` appends it at compile time so it cannot be forgotten
    // again; `sys::open` also now rejects an unterminated slice outright
    // instead of letting the kernel scan.
    let fd = sys::open(sys::cstr!(b"/fat/CONFIG.INI"), 0);
    if fd < 0 { return None; }
    let mut buf = [0u8; 1024];
    let n = sys::read(fd as u64, &mut buf);
    sys::close(fd as u64);
    if n <= 0 { return None; }
    let data = &buf[..n as usize];

    let ip = find_value(data, b"behavior_server_ip").and_then(parse_ipv4)?;
    // A missing port is not fatal: the IP is the part that was wrong.
    let port = find_value(data, b"behavior_server_port")
        .and_then(parse_u16)
        .unwrap_or(9000);
    Some((ip, port))
}

/// Value of `key=` on its own line, up to end-of-line. Whitespace-trimmed.
fn find_value<'a>(data: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0usize;
    while i < data.len() {
        // Start of a line.
        let line_start = i;
        while i < data.len() && data[i] != b'\n' { i += 1; }
        let mut line_end = i;
        if i < data.len() { i += 1; } // step over the newline
        // Trim trailing CR/space.
        while line_end > line_start
            && (data[line_end - 1] == b'\r' || data[line_end - 1] == b' ')
        {
            line_end -= 1;
        }
        let line = &data[line_start..line_end];
        if line.len() > key.len()
            && &line[..key.len()] == key
            && line[key.len()] == b'='
        {
            return Some(&line[key.len() + 1..]);
        }
    }
    None
}

fn parse_ipv4(v: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut octet = 0usize;
    let mut acc: u32 = 0;
    let mut digits = 0;
    for &b in v {
        if b == b'.' {
            if digits == 0 || octet >= 3 { return None; }
            out[octet] = acc as u8;
            octet += 1; acc = 0; digits = 0;
        } else if b.is_ascii_digit() {
            acc = acc * 10 + (b - b'0') as u32;
            if acc > 255 { return None; }
            digits += 1;
        } else {
            break;
        }
    }
    if octet != 3 || digits == 0 { return None; }
    out[3] = acc as u8;
    Some(out)
}

fn parse_u16(v: &[u8]) -> Option<u16> {
    let mut acc: u32 = 0;
    let mut digits = 0;
    for &b in v {
        if b.is_ascii_digit() {
            acc = acc * 10 + (b - b'0') as u32;
            if acc > 65535 { return None; }
            digits += 1;
        } else { break; }
    }
    if digits == 0 { None } else { Some(acc as u16) }
}



fn run() {
    sys::println(b"[brain_client] Starting brain protocol client");

    // Brain server address, read from /fat/CONFIG.INI.
    //
    // This used to be a hardcoded 192.168.1.2 with a "TODO: read from
    // CONFIG.INI" beside it, which meant the client could never reach
    // anything under QEMU (SLIRP puts the host at 10.0.2.2) and silently
    // burned its retry loop against an address that does not exist. The
    // symptom was misleading in a specific way: the log said "Connected!"
    // and then "Send failed", which reads like a transport bug rather than
    // a client aimed at the wrong place.
    //
    // Falls back to the old literal if the file is missing or unparseable,
    // so a deployment that relied on the compiled-in default still behaves
    // as before.
    let from_cfg = read_brain_addr();
    let (brain_ip, brain_port) = from_cfg.unwrap_or(([192, 168, 1, 2], 9000));
    // One write() for the whole line. The UART guard the kernel takes covers
    // a single write; a line assembled from several print calls can still be
    // sliced by another hart's kprintln between them, which is exactly how
    // this line first came out as "Brain server [IPC] service_heartbeat = 0".
    {
        let mut line = [0u8; 96];
        let n = fmt_addr_line(&mut line, brain_ip, brain_port, from_cfg.is_some());
        sys::print(&line[..n]);
    }

    loop {
        sys::print(b"[brain_client] Connecting to brain server...\n");

        let fd = connect_to_brain(brain_ip, brain_port);
        if fd < 0 {
            sys::print(b"[brain_client] Connection failed, retrying...\n");
            sys::sleep(CONNECT_RETRY_MS);
            continue;
        }

        sys::println(b"[brain_client] Connected!");
        brain_client_loop(fd as u64);

        // Cleanup
        sys::close(fd as u64);

        // Stop motors on disconnect
        sys::motor_speed(MOTOR_LEFT, 0);
        sys::motor_speed(MOTOR_RIGHT, 0);

        sys::print(b"[brain_client] Disconnected, reconnecting...\n");
        sys::sleep(CONNECT_RETRY_MS);
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    run();
    sys::exit(0);
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::print(b"[brain_client] PANIC!\n");
    sys::motor_speed(MOTOR_LEFT, 0);
    sys::motor_speed(MOTOR_RIGHT, 0);
    sys::exit(1);
}
