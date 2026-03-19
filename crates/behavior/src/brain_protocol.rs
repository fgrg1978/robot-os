//! Brain Protocol — binary wire format for Robot ↔ Brain Server communication.
//!
//! Packet frame:
//!   MAGIC[2] | TYPE[1] | LEN[2 LE] | PAYLOAD[0..1400] | CRC8[1]
//!
//! Robot → Server (0x01-0x7F):
//!   0x01  SENSOR_PACKET   64 bytes (wheeled: 34B header + 28B payload + 2B flags)
//!   0x02  CAMERA_FRAME    5B header + JPEG
//!   0x03  STATUS          8 bytes
//!
//! Server → Robot (0x80-0xFF):
//!   0x80  ACTUATOR_CMD    3 + 2*N bytes
//!   0x81  MODE_CMD        1 byte
//!   0x82  WAYPOINT_CMD    14 bytes
//!   0x83  CONFIG_CMD      4 bytes (key + value + reserved)
//!
//! SensorPacket common header (34 bytes, little-endian):
//!   timestamp_ms: u64   (8B)
//!   accel_mg[3]:  i32×3 (12B)
//!   gyro_mdps[3]: i32×3 (12B)
//!   battery_mv:   u16   (2B)
//!
//! SensorPacket wheeled extra (30 bytes):
//!   odom_dist_mm:  i32  (4B)
//!   odom_hdg_cdeg: i32  (4B)
//!   encoder_l:     i64  (8B)
//!   encoder_r:     i64  (8B)
//!   range_front:   u16  (2B)
//!   range_right:   u16  (2B)
//!   sensor_flags:  u16  (2B)  — PIR/sound/IR digital trigger flags
//!
//! StatusPacket (8 bytes):
//!   mode:       u8
//!   tasks_ok:   u8
//!   canary_ok:  u8
//!   uptime_s:   u32 LE
//!   robot_type: u8
//!
//! ActuatorCmd payload (3 + 2*N bytes):
//!   actuator_type: u8  (0=diff_drive, 1=quad_rotor, 2=humanoid)
//!   n_channels:    u8
//!   flags:         u8  (bit0=emergency, bit1=alert)
//!   channels:      [i16; N] LE
//!
//! ConfigCmd payload (4 bytes):
//!   config_key: u8   — subsystem selector (LED, buzzer, power, etc.)
//!   value:      u8   — command value
//!   reserved:   u16  — reserved for future use

pub const MAGIC: [u8; 2]       = *b"BR";

// Packet types — Robot → Server
pub const PKT_SENSOR:  u8 = 0x01;
pub const PKT_CAMERA:  u8 = 0x02;
pub const PKT_STATUS:  u8 = 0x03;

// Packet types — Server → Robot
pub const PKT_ACTUATOR: u8 = 0x80;
pub const PKT_MODE:     u8 = 0x81;
pub const PKT_WAYPOINT: u8 = 0x82;
pub const PKT_CONFIG:   u8 = 0x83;

// Robot types
pub const ROBOT_WHEELED:   u8 = 0;
pub const ROBOT_DRONE:     u8 = 1;
pub const ROBOT_HUMANOID:  u8 = 2;
pub const ROBOT_ACKERMANN: u8 = 3;

// ActuatorCmd flags
pub const FLAG_EMERGENCY: u8 = 0x01;
pub const FLAG_ALERT:     u8 = 0x02;

// Actuator types
pub const ACT_DIFF_DRIVE: u8 = 0;

// Camera header: width(u16 LE) + height(u16 LE) + format(u8) = 5 bytes
pub const CAMERA_HDR_SIZE: usize = 5;
pub const CAMERA_FMT_GRAY8: u8 = 0;
pub const CAMERA_FMT_JPEG:  u8 = 1;

// ── Config keys (CONFIG_CMD config_key field) ───────────────────────────────

/// LED state control.
pub const CFG_KEY_LED:       u8 = 0x10;
/// Power mode (ECO / ALERT).
pub const CFG_KEY_POWER:     u8 = 0x11;
/// Camera power GPIO (on / off).
pub const CFG_KEY_CAMERA:    u8 = 0x12;
/// ESP32 WiFi sleep mode (batch / continuous).
pub const CFG_KEY_WIFI:      u8 = 0x13;
/// LiDAR scan rate in Hz.
pub const CFG_KEY_LIDAR_HZ:  u8 = 0x14;
/// Buzzer pattern.
pub const CFG_KEY_BUZZER:    u8 = 0x15;
/// Siren module (12V MOSFET).
pub const CFG_KEY_SIREN:     u8 = 0x16;
/// LED 10W COB spotlight (MOSFET).
pub const CFG_KEY_SPOTLIGHT: u8 = 0x17;
/// Green laser 532nm (MOSFET).
pub const CFG_KEY_LASER:     u8 = 0x18;
/// Pan servo angle (0-180 degrees).
pub const CFG_KEY_SERVO_PAN: u8 = 0x19;
/// Tilt servo angle (0-180 degrees).
pub const CFG_KEY_SERVO_TILT: u8 = 0x1A;
/// Speaker / amplifier audio file ID.
pub const CFG_KEY_SPEAKER:   u8 = 0x1B;

// ── LED state codes ─────────────────────────────────────────────────────────

#[allow(dead_code)] pub const LED_OFF:          u8 = 0x00;
#[allow(dead_code)] pub const LED_GREEN:        u8 = 0x01;
#[allow(dead_code)] pub const LED_GREEN_BLINK:  u8 = 0x02;
#[allow(dead_code)] pub const LED_YELLOW:       u8 = 0x03;
#[allow(dead_code)] pub const LED_YELLOW_BLINK: u8 = 0x04;
#[allow(dead_code)] pub const LED_RED:          u8 = 0x05;
#[allow(dead_code)] pub const LED_RED_BLINK:    u8 = 0x06;
#[allow(dead_code)] pub const LED_RED_STROBE:   u8 = 0x07;
#[allow(dead_code)] pub const LED_BLUE:         u8 = 0x08;
#[allow(dead_code)] pub const LED_BLUE_BLINK:   u8 = 0x09;
#[allow(dead_code)] pub const LED_WHITE_FLASH:  u8 = 0x0A;

// ── Buzzer patterns ─────────────────────────────────────────────────────────

pub const BUZZER_OFF:   u8 = 0x00;
pub const BUZZER_BEEP:  u8 = 0x01;
pub const BUZZER_SIREN: u8 = 0x02;
#[allow(dead_code)] pub const BUZZER_CHIRP: u8 = 0x03;

// ── Power modes ─────────────────────────────────────────────────────────────

#[allow(dead_code)] pub const POWER_ECO:   u8 = 0x00;
#[allow(dead_code)] pub const POWER_ALERT: u8 = 0x01;

// ── Camera power ────────────────────────────────────────────────────────────

#[allow(dead_code)] pub const CAMERA_PWR_OFF: u8 = 0x00;
#[allow(dead_code)] pub const CAMERA_PWR_ON:  u8 = 0x01;

// ── WiFi modes ──────────────────────────────────────────────────────────────

#[allow(dead_code)] pub const WIFI_BATCH:      u8 = 0x00;
#[allow(dead_code)] pub const WIFI_CONTINUOUS: u8 = 0x01;

// ── Device on/off (siren, spotlight, laser) ─────────────────────────────────

#[allow(dead_code)] pub const DEVICE_OFF:       u8 = 0x00;
#[allow(dead_code)] pub const DEVICE_ON:        u8 = 0x01;
#[allow(dead_code)] pub const SPOTLIGHT_STROBE: u8 = 0x02;

// ── Speaker audio IDs ───────────────────────────────────────────────────────

#[allow(dead_code)] pub const SPEAKER_STOP:     u8 = 0x00;
#[allow(dead_code)] pub const SPEAKER_WARNING:  u8 = 0x01;
#[allow(dead_code)] pub const SPEAKER_DOG_BARK: u8 = 0x02;
#[allow(dead_code)] pub const SPEAKER_SIREN_FX: u8 = 0x03;

// ── Digital sensor flags (u16 bit flags in sensor_flags field) ──────────────

#[allow(dead_code)] pub const SENSOR_FLAG_PIR:   u16 = 0x0001;
#[allow(dead_code)] pub const SENSOR_FLAG_SOUND: u16 = 0x0002;
#[allow(dead_code)] pub const SENSOR_FLAG_IR:    u16 = 0x0004;

// Payload sizes
pub const SENSOR_PAYLOAD_SIZE: usize = 64;   // 34 header + 28 wheeled + 2 flags
pub const STATUS_PAYLOAD_SIZE: usize = 8;
// Frame overhead: MAGIC(2) + TYPE(1) + LEN(2) + CRC(1) = 6
pub const FRAME_OVERHEAD: usize = 6;

pub const SENSOR_FRAME_SIZE: usize = SENSOR_PAYLOAD_SIZE + FRAME_OVERHEAD;  // 70
pub const STATUS_FRAME_SIZE: usize = STATUS_PAYLOAD_SIZE + FRAME_OVERHEAD;  // 14

/// Maximum channels in an ActuatorCmd.
pub const MAX_CHANNELS: usize = 8;

// ── ActuatorCmd ───────────────────────────────────────────────────────────────

/// Decoded actuator command from the Brain Server.
#[derive(Clone, Copy)]
pub struct ActuatorCmd {
    pub actuator_type: u8,
    pub n_channels:    u8,
    pub flags:         u8,
    pub channels:      [i16; MAX_CHANNELS],
}

impl ActuatorCmd {
    pub const fn zeroed() -> Self {
        ActuatorCmd {
            actuator_type: ACT_DIFF_DRIVE,
            n_channels:    0,
            flags:         0,
            channels:      [0i16; MAX_CHANNELS],
        }
    }

    pub fn is_emergency(&self) -> bool {
        self.flags & FLAG_EMERGENCY != 0
    }

    /// For differential drive: (speed_l, speed_r) as i32, clamped -100..100.
    pub fn diff_drive(&self) -> (i32, i32) {
        if self.n_channels >= 2 {
            let l = (self.channels[0] as i32).clamp(-100, 100);
            let r = (self.channels[1] as i32).clamp(-100, 100);
            (l, r)
        } else {
            (0, 0)
        }
    }
}

// ── CRC-8/MAXIM (polynomial 0x31) ────────────────────────────────────────────

pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0x00;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = crc.wrapping_shl(1) ^ 0x31;
            } else {
                crc = crc.wrapping_shl(1);
            }
        }
    }
    crc
}

// ── Frame builder ─────────────────────────────────────────────────────────────

/// Build a framed packet into `out`.
///
/// `out` must be at least `payload.len() + 6` bytes.
/// Returns the total bytes written.
pub fn build_packet(pkt_type: u8, payload: &[u8], out: &mut [u8]) -> usize {
    let len = payload.len();
    // Header: MAGIC + TYPE + LEN(LE)
    out[0] = MAGIC[0];
    out[1] = MAGIC[1];
    out[2] = pkt_type;
    out[3] = (len & 0xFF) as u8;
    out[4] = (len >> 8) as u8;
    // Payload
    out[5..5 + len].copy_from_slice(payload);
    // CRC over header + payload
    let crc = crc8(&out[..5 + len]);
    out[5 + len] = crc;
    6 + len
}

// ── Frame parser ──────────────────────────────────────────────────────────────

/// Parse one packet from `buf`.
///
/// Returns `Some((pkt_type, payload_start, payload_len, total_consumed))`
/// or `None` if the buffer is incomplete or corrupt.
pub fn parse_packet(buf: &[u8]) -> Option<(u8, usize, usize, usize)> {
    if buf.len() < 6 { return None; }
    if buf[0] != MAGIC[0] || buf[1] != MAGIC[1] { return None; }
    let pkt_type = buf[2];
    let length   = (buf[3] as usize) | ((buf[4] as usize) << 8);
    let total    = 5 + length + 1;
    if buf.len() < total { return None; }
    // CRC check: header (5B) + payload (lengthB)
    let expected = crc8(&buf[..5 + length]);
    if buf[5 + length] != expected { return None; }
    // Payload starts at offset 5
    Some((pkt_type, 5, length, total))
}

// ── SensorPacket encoder ──────────────────────────────────────────────────────

/// Encode a wheeled SensorPacket payload (64 bytes) into `buf`.
#[allow(clippy::too_many_arguments)]
pub fn encode_sensor_packet(
    buf:            &mut [u8; SENSOR_PAYLOAD_SIZE],
    timestamp_ms:   u64,
    accel_mg:       [i32; 3],
    gyro_mdps:      [i32; 3],
    battery_mv:     u16,
    odom_dist_mm:   i32,
    odom_hdg_cdeg:  i32,
    encoder_l:      i64,
    encoder_r:      i64,
    range_front_mm: u16,
    range_right_mm: u16,
    sensor_flags:   u16,
) {
    // ── Common header (34 bytes) ──────────────────────────────────────────────
    put_u64(buf, 0,  timestamp_ms);
    put_i32(buf, 8,  accel_mg[0]);
    put_i32(buf, 12, accel_mg[1]);
    put_i32(buf, 16, accel_mg[2]);
    put_i32(buf, 20, gyro_mdps[0]);
    put_i32(buf, 24, gyro_mdps[1]);
    put_i32(buf, 28, gyro_mdps[2]);
    put_u16(buf, 32, battery_mv);
    // ── Wheeled payload (28 bytes at offset 34) ───────────────────────────────
    put_i32(buf, 34, odom_dist_mm);
    put_i32(buf, 38, odom_hdg_cdeg);
    put_i64(buf, 42, encoder_l);
    put_i64(buf, 50, encoder_r);
    put_u16(buf, 58, range_front_mm);
    put_u16(buf, 60, range_right_mm);
    // ── Digital sensor flags (2 bytes at offset 62) ───────────────────────────
    put_u16(buf, 62, sensor_flags);
}

// ── StatusPacket encoder ──────────────────────────────────────────────────────

/// Encode a StatusPacket payload (8 bytes) into `buf`.
pub fn encode_status_packet(
    buf:        &mut [u8; STATUS_PAYLOAD_SIZE],
    mode:       u8,
    tasks_ok:   u8,
    canary_ok:  u8,
    uptime_s:   u32,
    robot_type: u8,
) {
    buf[0] = mode;
    buf[1] = tasks_ok;
    buf[2] = canary_ok;
    put_u32(buf, 3, uptime_s);
    buf[7] = robot_type;
}

// ── ActuatorCmd decoder ───────────────────────────────────────────────────────

/// Decode an ActuatorCmd from the payload bytes of a `PKT_ACTUATOR` packet.
///
/// Returns `None` if payload is too short.
pub fn decode_actuator_cmd(payload: &[u8]) -> Option<ActuatorCmd> {
    if payload.len() < 3 { return None; }
    let actuator_type = payload[0];
    let n             = payload[1] as usize;
    let flags         = payload[2];
    if payload.len() < 3 + n * 2 { return None; }

    let mut channels = [0i16; MAX_CHANNELS];
    let n_clamped    = n.min(MAX_CHANNELS);
    for i in 0..n_clamped {
        channels[i] = i16::from_le_bytes([payload[3 + i * 2], payload[3 + i * 2 + 1]]);
    }

    Some(ActuatorCmd {
        actuator_type,
        n_channels: n_clamped as u8,
        flags,
        channels,
    })
}

// ── Camera frame encoder ─────────────────────────────────────────────────

/// Encode the 5-byte camera header into `hdr_buf`.
///
/// Format: width(u16 LE) + height(u16 LE) + pixel_format(u8)
pub fn encode_camera_header(hdr_buf: &mut [u8; CAMERA_HDR_SIZE], width: u16, height: u16, fmt: u8) {
    put_u16(hdr_buf, 0, width);
    put_u16(hdr_buf, 2, height);
    hdr_buf[4] = fmt;
}

// ── ModeCmd ─────────────────────────────────────────────────────────────────

/// Payload size for MODE_CMD: 1 byte (mode_id).
pub const MODE_PAYLOAD_SIZE: usize = 1;

/// Decoded mode command from the Brain Server.
#[derive(Clone, Copy, Debug)]
pub struct ModeCmd {
    pub mode_id: u8,
}

/// Decode a ModeCmd from the payload bytes of a `PKT_MODE` packet.
///
/// Returns `None` if payload is too short.
pub fn decode_mode_cmd(payload: &[u8]) -> Option<ModeCmd> {
    if payload.len() < MODE_PAYLOAD_SIZE { return None; }
    Some(ModeCmd { mode_id: payload[0] })
}

// ── WaypointCmd ─────────────────────────────────────────────────────────────

/// Payload size for WAYPOINT_CMD: 14 bytes.
///   lat_deg7(i32) + lon_deg7(i32) + alt_cm(u16) + speed_cms(u16) + action(u8) + flags(u8)
pub const WAYPOINT_PAYLOAD_SIZE: usize = 14;

// Field offsets within WaypointCmd payload
const WP_OFF_LAT:       usize = 0;
const WP_OFF_LON:       usize = 4;
const WP_OFF_ALT:       usize = 8;
const WP_OFF_SPEED:     usize = 10;
const WP_OFF_ACTION:    usize = 12;
const WP_OFF_FLAGS:     usize = 13;

/// Decoded waypoint command from the Brain Server.
#[derive(Clone, Copy, Debug)]
pub struct WaypointCmd {
    pub lat_deg7:  i32,   // latitude × 1e7
    pub lon_deg7:  i32,   // longitude × 1e7
    pub alt_cm:    u16,   // altitude in cm
    pub speed_cms: u16,   // speed in cm/s
    pub action:    u8,    // action at waypoint
    pub flags:     u8,    // waypoint flags
}

/// Decode a WaypointCmd from the payload bytes of a `PKT_WAYPOINT` packet.
///
/// Returns `None` if payload is too short.
pub fn decode_waypoint_cmd(payload: &[u8]) -> Option<WaypointCmd> {
    if payload.len() < WAYPOINT_PAYLOAD_SIZE { return None; }
    Some(WaypointCmd {
        lat_deg7:  get_i32(payload, WP_OFF_LAT),
        lon_deg7:  get_i32(payload, WP_OFF_LON),
        alt_cm:    get_u16(payload, WP_OFF_ALT),
        speed_cms: get_u16(payload, WP_OFF_SPEED),
        action:    payload[WP_OFF_ACTION],
        flags:     payload[WP_OFF_FLAGS],
    })
}

// ── ConfigCmd ───────────────────────────────────────────────────────────────

/// Total payload size for CONFIG_CMD: key(1) + value(1) + reserved(2) = 4 bytes.
pub const CONFIG_PAYLOAD_SIZE: usize = 4;

// Field offsets within ConfigCmd payload
const CFG_OFF_KEY:      usize = 0;
const CFG_OFF_VALUE:    usize = 1;
const CFG_OFF_RESERVED: usize = 2;

/// Decoded config command from the Brain Server.
///
/// 4-byte format: config_key(u8) + value(u8) + reserved(u16 LE).
#[derive(Clone, Copy, Debug)]
pub struct ConfigCmd {
    pub config_key: u8,
    pub value:      u8,
    pub reserved:   u16,
}

/// Decode a ConfigCmd from the payload bytes of a `PKT_CONFIG` packet.
///
/// Returns `None` if payload is too short.
pub fn decode_config_cmd(payload: &[u8]) -> Option<ConfigCmd> {
    if payload.len() < CONFIG_PAYLOAD_SIZE { return None; }
    Some(ConfigCmd {
        config_key: payload[CFG_OFF_KEY],
        value:      payload[CFG_OFF_VALUE],
        reserved:   get_u16(payload, CFG_OFF_RESERVED),
    })
}

// ── Little-endian helpers ─────────────────────────────────────────────────────

#[inline]
fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    let b = v.to_le_bytes();
    buf[off]     = b[0];
    buf[off + 1] = b[1];
}

#[inline]
fn put_i32(buf: &mut [u8], off: usize, v: i32) {
    let b = v.to_le_bytes();
    buf[off]     = b[0];
    buf[off + 1] = b[1];
    buf[off + 2] = b[2];
    buf[off + 3] = b[3];
}

#[inline]
fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    let b = v.to_le_bytes();
    buf[off]     = b[0];
    buf[off + 1] = b[1];
    buf[off + 2] = b[2];
    buf[off + 3] = b[3];
}

#[inline]
fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    let b = v.to_le_bytes();
    for i in 0..8 { buf[off + i] = b[i]; }
}

#[inline]
fn put_i64(buf: &mut [u8], off: usize, v: i64) {
    let b = v.to_le_bytes();
    for i in 0..8 { buf[off + i] = b[i]; }
}

// ── Little-endian reader helpers ─────────────────────────────────────────────

#[inline]
fn get_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

#[inline]
fn get_i32(buf: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}
