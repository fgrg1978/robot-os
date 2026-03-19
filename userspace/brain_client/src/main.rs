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
        let speed_l = get_i16_le(ch_data, 0) as u64;
        let speed_r = get_i16_le(ch_data, 2) as u64;
        sys::motor_speed(MOTOR_LEFT, speed_l);
        sys::motor_speed(MOTOR_RIGHT, speed_r);
    }
}

// ── Main loop ───────────────────────────────────────────────────────────────

fn connect_to_brain(ip: [u8; 4], port: u16) -> isize {
    let fd = sys::socket(AF_INET, SOCK_STREAM, 0);
    if fd < 0 {
        return -1;
    }

    let addr = SockAddr::new(ip, port);
    let ret = sys::connect(
        fd as u64,
        addr.as_bytes().as_ptr() as u64,
        16,
    );
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

fn run() {
    sys::println(b"[brain_client] Starting brain protocol client");

    // Brain server address: 192.168.1.x:9000
    // TODO: read from CONFIG.INI via filesystem or IPC
    let brain_ip: [u8; 4] = [192, 168, 1, 2];
    let brain_port: u16 = 9000;

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
