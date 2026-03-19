//! LD19 (LD-06) 2D LiDAR UART driver.
//!
//! The LD19 is a 360° 2D LiDAR with 12m range, connected via UART at 230400 baud.
//! It outputs scan packets continuously at ~10 Hz (full revolution).
//!
//! Packet format (47 bytes):
//!   Header: 0x54 (1B)
//!   VerLen: 0x2C (1B) — version(4b) + point_count(4b), always 12 points
//!   Speed:  u16 LE — rotation speed in degrees/sec × 100
//!   Start angle: u16 LE — start angle in centidegrees
//!   Data:   12 × [distance_mm: u16 LE, intensity: u8] = 36B
//!   End angle: u16 LE — end angle in centidegrees
//!   Timestamp: u16 LE — ms timestamp
//!   CRC8:   u8
//!
//! This driver parses raw UART bytes into scan points and accumulates
//! a full 360° scan buffer that can be read via SYS_SENSOR_READ.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------
const LD19_HEADER: u8 = 0x54;
const LD19_VERLEN: u8 = 0x2C;
const LD19_POINTS_PER_PACKET: usize = 12;
const LD19_PACKET_SIZE: usize = 47;

/// Maximum scan points in a full revolution buffer.
pub const SCAN_BUF_MAX_POINTS: usize = 360;

/// Each scan point is 4 bytes: angle_cdeg(u16 LE) + distance_mm(u16 LE).
pub const SCAN_POINT_SIZE: usize = 4;

/// Maximum scan data size in bytes.
pub const SCAN_DATA_MAX_BYTES: usize = SCAN_BUF_MAX_POINTS * SCAN_POINT_SIZE;

/// CRC-8 table for LD19 (polynomial 0x4D).
const CRC_TABLE: [u8; 256] = crc8_table();

// ---------------------------------------------------------------------------
// Scan buffer (global, lock-free double buffer)
// ---------------------------------------------------------------------------

/// A single scan point.
#[derive(Copy, Clone, Default)]
pub struct ScanPoint {
    pub angle_cdeg: u16,
    pub distance_mm: u16,
}

/// Static scan buffer: producer writes to back, consumer reads from front.
static mut SCAN_FRONT: [ScanPoint; SCAN_BUF_MAX_POINTS] =
    [ScanPoint { angle_cdeg: 0, distance_mm: 0 }; SCAN_BUF_MAX_POINTS];
static mut SCAN_BACK: [ScanPoint; SCAN_BUF_MAX_POINTS] =
    [ScanPoint { angle_cdeg: 0, distance_mm: 0 }; SCAN_BUF_MAX_POINTS];
static SCAN_FRONT_COUNT: AtomicU32 = AtomicU32::new(0);
static SCAN_BACK_COUNT: AtomicU32 = AtomicU32::new(0);
static SCAN_READY: AtomicBool = AtomicBool::new(false);
static LIDAR_INITIALIZED: AtomicBool = AtomicBool::new(false);

// Packet reassembly state
static mut PKT_BUF: [u8; LD19_PACKET_SIZE] = [0; LD19_PACKET_SIZE];
static mut PKT_IDX: usize = 0;
static mut LAST_END_ANGLE: u16 = 0;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialize the LiDAR driver. Called once at boot.
pub fn lidar_init() {
    unsafe {
        PKT_IDX = 0;
        LAST_END_ANGLE = 0;
        SCAN_BACK_COUNT.store(0, Ordering::Relaxed);
        SCAN_FRONT_COUNT.store(0, Ordering::Relaxed);
    }
    SCAN_READY.store(false, Ordering::Release);
    LIDAR_INITIALIZED.store(true, Ordering::Release);
}

/// Check if LiDAR is initialized.
pub fn lidar_is_initialized() -> bool {
    LIDAR_INITIALIZED.load(Ordering::Acquire)
}

/// Feed raw UART bytes from the LiDAR. Call from UART0 IRQ handler or poll loop.
///
/// Parses LD19 packets and accumulates scan points. When a full revolution
/// is detected (angle wraps around), swaps the double buffer.
pub fn lidar_feed(data: &[u8]) {
    if !lidar_is_initialized() { return; }

    for &byte in data {
        unsafe { feed_byte(byte); }
    }
}

/// Read the latest complete scan into a user buffer.
///
/// Writes pairs of (angle_cdeg: u16 LE, distance_mm: u16 LE) into `buf`.
/// Returns the number of bytes written, or 0 if no scan available.
pub fn lidar_read_scan(buf: &mut [u8]) -> usize {
    if !SCAN_READY.load(Ordering::Acquire) {
        return 0;
    }

    let count = SCAN_FRONT_COUNT.load(Ordering::Acquire) as usize;
    let needed = count * SCAN_POINT_SIZE;
    if buf.len() < needed {
        return 0;
    }

    unsafe {
        for i in 0..count {
            let pt = &SCAN_FRONT[i];
            let off = i * SCAN_POINT_SIZE;
            buf[off..off + 2].copy_from_slice(&pt.angle_cdeg.to_le_bytes());
            buf[off + 2..off + 4].copy_from_slice(&pt.distance_mm.to_le_bytes());
        }
    }

    needed
}

/// Number of points in the latest complete scan.
pub fn lidar_scan_count() -> usize {
    SCAN_FRONT_COUNT.load(Ordering::Acquire) as usize
}

// ---------------------------------------------------------------------------
// Internal: packet parsing
// ---------------------------------------------------------------------------

unsafe fn feed_byte(byte: u8) {
    let idx = PKT_IDX;

    // Sync to header
    if idx == 0 {
        if byte == LD19_HEADER {
            PKT_BUF[0] = byte;
            PKT_IDX = 1;
        }
        return;
    }

    // Verify second byte (verlen)
    if idx == 1 && byte != LD19_VERLEN {
        PKT_IDX = 0;
        return;
    }

    PKT_BUF[idx] = byte;
    PKT_IDX = idx + 1;

    if PKT_IDX >= LD19_PACKET_SIZE {
        PKT_IDX = 0;
        process_packet();
    }
}

unsafe fn process_packet() {
    // Verify CRC
    let crc = crc8_compute(&PKT_BUF[..LD19_PACKET_SIZE - 1]);
    if crc != PKT_BUF[LD19_PACKET_SIZE - 1] {
        return; // bad CRC
    }

    // Parse header fields
    let start_angle = u16::from_le_bytes([PKT_BUF[4], PKT_BUF[5]]);
    let end_angle = u16::from_le_bytes([PKT_BUF[42], PKT_BUF[43]]);

    // Detect revolution wrap: end_angle < last_end_angle → new scan
    if end_angle < LAST_END_ANGLE && SCAN_BACK_COUNT.load(Ordering::Relaxed) > 0 {
        // Swap buffers
        let count = SCAN_BACK_COUNT.load(Ordering::Relaxed) as usize;
        let src = &SCAN_BACK[..count];
        SCAN_FRONT[..count].copy_from_slice(src);
        SCAN_FRONT_COUNT.store(count as u32, Ordering::Release);
        SCAN_BACK_COUNT.store(0, Ordering::Relaxed);
        SCAN_READY.store(true, Ordering::Release);
    }
    LAST_END_ANGLE = end_angle;

    // Interpolate angles for the 12 points in this packet
    let angle_step = if end_angle >= start_angle {
        (end_angle - start_angle) / (LD19_POINTS_PER_PACKET as u16)
    } else {
        (36000 + end_angle - start_angle) / (LD19_POINTS_PER_PACKET as u16)
    };

    // Parse 12 data points (each 3 bytes: distance_mm u16 LE + intensity u8)
    let back_idx = SCAN_BACK_COUNT.load(Ordering::Relaxed) as usize;
    let data_start = 6; // offset of first point in packet

    for i in 0..LD19_POINTS_PER_PACKET {
        let pt_idx = back_idx + i;
        if pt_idx >= SCAN_BUF_MAX_POINTS {
            break;
        }

        let off = data_start + i * 3;
        let distance_mm = u16::from_le_bytes([PKT_BUF[off], PKT_BUF[off + 1]]);
        // intensity at PKT_BUF[off + 2] — ignored for now

        let angle_cdeg = (start_angle + (i as u16) * angle_step) % 36000;

        SCAN_BACK[pt_idx] = ScanPoint { angle_cdeg, distance_mm };
    }

    let new_count = (back_idx + LD19_POINTS_PER_PACKET).min(SCAN_BUF_MAX_POINTS);
    SCAN_BACK_COUNT.store(new_count as u32, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// CRC-8 (polynomial 0x4D, used by LD19)
// ---------------------------------------------------------------------------

fn crc8_compute(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &b in data {
        crc = CRC_TABLE[(crc ^ b) as usize];
    }
    crc
}

const fn crc8_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i: usize = 0;
    while i < 256 {
        let mut crc = i as u8;
        let mut j = 0;
        while j < 8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ 0x4D;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}
