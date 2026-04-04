#![no_std]
//! OTA firmware update — A/B slot management, image format, CRC-32.
//!
//! ## On-wire format (TCP transfer)
//!
//! The OTA header (24 bytes) is sent over TCP before the payload.
//! It is **never** stored on disk — only the raw kernel binary is written.
//!
//! ```text
//! Offset  Size  Field
//! 0x00    4     Magic: "ROTA" (0x524F5441)
//! 0x04    4     Header version (1)
//! 0x08    4     Image size (payload only, excl. header)
//! 0x0C    4     CRC-32 of payload (IEEE 802.3)
//! 0x10    4     Firmware version (major.minor.patch packed u32)
//! 0x14    1     Platform ID (0=qemu, 1=vf2, 2=k1, 3=esp32c3)
//! 0x15    1     Flags (bit0=compressed)
//! 0x16    2     Reserved (zero)
//! 0x18    --    Payload (raw kernel binary)
//! ```
//!
//! ## Disk layout
//!
//! ```text
//! /fat/KERN_A.BIN  — raw kernel binary (no header), directly bootable by U-Boot
//! /fat/KERN_B.BIN  — raw kernel binary (no header), directly bootable by U-Boot
//! /fat/BOOTMETA    — INI text with slot info + per-slot CRC/size/version
//! ```
//!
//! ## Boot metadata (`/fat/BOOTMETA`)
//!
//! ```text
//! active_slot=a
//! boot_count=0
//! last_good=a
//! fw_version_a=0
//! fw_version_b=0
//! image_size_a=0
//! image_size_b=0
//! image_crc_a=0
//! image_crc_b=0
//! ```

use core::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// OTA on-wire header
// ---------------------------------------------------------------------------

/// Magic bytes identifying an OTA transfer: "ROTA".
pub const OTA_MAGIC: [u8; 4] = *b"ROTA";

/// Current header format version.
pub const OTA_HEADER_VERSION: u32 = 1;

/// Size of the OTA on-wire header in bytes.
pub const OTA_HEADER_SIZE: usize = 24;

/// Maximum firmware payload size (2 MiB — kernel rarely exceeds 1 MiB).
pub const OTA_MAX_IMAGE_SIZE: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Platform IDs
// ---------------------------------------------------------------------------

pub const OTA_PLATFORM_QEMU:    u8 = 0;
pub const OTA_PLATFORM_VF2:     u8 = 1;
pub const OTA_PLATFORM_K1:      u8 = 2;
pub const OTA_PLATFORM_ESP32C3: u8 = 3;

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

pub const OTA_FLAG_COMPRESSED: u8 = 0x01;

// ---------------------------------------------------------------------------
// File paths on FAT32
// ---------------------------------------------------------------------------

pub const OTA_SLOT_A_PATH: &[u8] = b"/fat/KERN_A.BIN";
pub const OTA_SLOT_B_PATH: &[u8] = b"/fat/KERN_B.BIN";
pub const OTA_META_PATH:   &[u8] = b"/fat/BOOTMETA";

// ---------------------------------------------------------------------------
// Network defaults
// ---------------------------------------------------------------------------

/// Default TCP port for OTA receive.
pub const OTA_DEFAULT_PORT: u16 = 8080;

/// Size of the streaming receive buffer (bytes per chunk).
pub const OTA_RECV_BUF_SIZE: usize = 4096;

// ---------------------------------------------------------------------------
// Boot loop detection
// ---------------------------------------------------------------------------

/// Maximum consecutive boot attempts before rollback.
pub const OTA_DEFAULT_MAX_BOOT_ATTEMPTS: u32 = 3;

/// Seconds of successful uptime before marking boot as good.
pub const OTA_BOOT_GOOD_DELAY_S: u32 = 30;

// ---------------------------------------------------------------------------
// Slot identifiers
// ---------------------------------------------------------------------------

pub const SLOT_A: u8 = 0;
pub const SLOT_B: u8 = 1;

// ---------------------------------------------------------------------------
// Runtime atomics (populated from BOOTMETA at boot)
// ---------------------------------------------------------------------------

/// Active boot slot (0 = A, 1 = B).
pub static CFG_OTA_ACTIVE_SLOT: AtomicU32 = AtomicU32::new(0);

/// Current boot count (incremented each boot, reset on success).
pub static CFG_OTA_BOOT_COUNT: AtomicU32 = AtomicU32::new(0);

/// Maximum boot attempts before automatic rollback.
pub static CFG_OTA_MAX_BOOT_ATTEMPTS: AtomicU32 =
    AtomicU32::new(OTA_DEFAULT_MAX_BOOT_ATTEMPTS);

// ---------------------------------------------------------------------------
// OTA on-wire header structure
// ---------------------------------------------------------------------------

/// Parsed OTA header (24 bytes, on-wire only — NOT stored on disk).
#[derive(Clone, Copy, Debug)]
pub struct OtaHeader {
    pub header_version: u32,
    pub image_size:     u32,
    pub image_crc32:    u32,
    pub fw_version:     u32,
    pub platform_id:    u8,
    pub flags:          u8,
}

/// Parse an OTA header from a 24-byte buffer.
///
/// Returns `None` if the magic or header version is wrong.
pub fn ota_parse_header(buf: &[u8]) -> Option<OtaHeader> {
    if buf.len() < OTA_HEADER_SIZE { return None; }
    if buf[0..4] != OTA_MAGIC { return None; }

    let header_version = get_u32(buf, 4);
    if header_version != OTA_HEADER_VERSION { return None; }

    Some(OtaHeader {
        header_version,
        image_size:  get_u32(buf, 8),
        image_crc32: get_u32(buf, 12),
        fw_version:  get_u32(buf, 16),
        platform_id: buf[20],
        flags:       buf[21],
    })
}

/// Validate an OTA header against the running platform.
pub fn ota_validate_header(h: &OtaHeader, platform: u8) -> bool {
    if h.platform_id != platform { return false; }
    if h.image_size as usize > OTA_MAX_IMAGE_SIZE { return false; }
    if h.flags & OTA_FLAG_COMPRESSED != 0 { return false; } // not yet supported
    true
}

/// Encode an OTA header into a 24-byte buffer.
pub fn ota_encode_header(h: &OtaHeader, out: &mut [u8]) {
    if out.len() < OTA_HEADER_SIZE { return; }
    out[0..4].copy_from_slice(&OTA_MAGIC);
    put_u32(out, 4,  h.header_version);
    put_u32(out, 8,  h.image_size);
    put_u32(out, 12, h.image_crc32);
    put_u32(out, 16, h.fw_version);
    out[20] = h.platform_id;
    out[21] = h.flags;
    out[22] = 0; // reserved
    out[23] = 0;
}

// ---------------------------------------------------------------------------
// Slot management
// ---------------------------------------------------------------------------

/// Return the inactive slot (the slot we write new firmware to).
pub fn ota_inactive_slot() -> u8 {
    if CFG_OTA_ACTIVE_SLOT.load(Ordering::Acquire) == SLOT_A as u32 {
        SLOT_B
    } else {
        SLOT_A
    }
}

/// Return the FAT32 path for the given slot.
pub fn ota_slot_path(slot: u8) -> &'static [u8] {
    if slot == SLOT_A { OTA_SLOT_A_PATH } else { OTA_SLOT_B_PATH }
}

/// Return the active slot index.
pub fn ota_active_slot() -> u8 {
    CFG_OTA_ACTIVE_SLOT.load(Ordering::Acquire) as u8
}

// ---------------------------------------------------------------------------
// Boot metadata (BOOTMETA read/write)
// ---------------------------------------------------------------------------

/// Parsed boot metadata from `/fat/BOOTMETA`.
///
/// Contains slot selection + per-slot CRC/size/version for verification.
/// The .BIN files on disk are raw kernel binaries (no OTA header).
#[derive(Clone, Copy, Debug)]
pub struct BootMeta {
    pub active_slot:  u8,    // 0=A, 1=B
    pub boot_count:   u32,
    pub last_good:    u8,    // 0=A, 1=B
    pub fw_version_a: u32,
    pub fw_version_b: u32,
    pub image_size_a: u32,   // payload size in bytes
    pub image_size_b: u32,
    pub image_crc_a:  u32,   // CRC-32 of raw binary
    pub image_crc_b:  u32,
}

impl BootMeta {
    pub const fn default() -> Self {
        BootMeta {
            active_slot:  SLOT_A,
            boot_count:   0,
            last_good:    SLOT_A,
            fw_version_a: 0,
            fw_version_b: 0,
            image_size_a: 0,
            image_size_b: 0,
            image_crc_a:  0,
            image_crc_b:  0,
        }
    }

    /// Get the stored CRC for a given slot.
    pub fn slot_crc(&self, slot: u8) -> u32 {
        if slot == SLOT_A { self.image_crc_a } else { self.image_crc_b }
    }

    /// Get the stored size for a given slot.
    pub fn slot_size(&self, slot: u8) -> u32 {
        if slot == SLOT_A { self.image_size_a } else { self.image_size_b }
    }

    /// Get the stored firmware version for a given slot.
    pub fn slot_version(&self, slot: u8) -> u32 {
        if slot == SLOT_A { self.fw_version_a } else { self.fw_version_b }
    }
}

/// Read boot metadata from `/fat/BOOTMETA`.
///
/// Returns default metadata if the file does not exist or is corrupt.
pub fn ota_read_boot_meta() -> BootMeta {
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, OTA_META_PATH,
                                    robot_os_fs::O_RDONLY);
    if fd < 0 {
        return BootMeta::default();
    }

    let mut buf = [0u8; 512];
    let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), buf.len());
    robot_os_fs::vfs_close(&mut fd_table, fd);
    if n <= 0 {
        return BootMeta::default();
    }

    parse_boot_meta(&buf[..n as usize])
}

/// Write boot metadata to `/fat/BOOTMETA`.
pub fn ota_write_boot_meta(meta: &BootMeta) {
    let mut buf = [0u8; 512];
    let n = serialize_boot_meta(meta, &mut buf);

    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, OTA_META_PATH,
        robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC);
    if fd >= 0 {
        robot_os_fs::vfs_write(&mut fd_table, fd, buf.as_ptr(), n);
        robot_os_fs::vfs_close(&mut fd_table, fd);
    }
}

/// Apply boot metadata to runtime atomics.
pub fn ota_apply_meta(meta: &BootMeta) {
    CFG_OTA_ACTIVE_SLOT.store(meta.active_slot as u32, Ordering::Release);
    CFG_OTA_BOOT_COUNT.store(meta.boot_count, Ordering::Release);
}

/// Mark the current boot as successful (reset boot_count, set last_good).
pub fn ota_mark_boot_good() {
    let mut meta = ota_read_boot_meta();
    meta.boot_count = 0;
    meta.last_good = meta.active_slot;
    ota_write_boot_meta(&meta);
    CFG_OTA_BOOT_COUNT.store(0, Ordering::Release);
    robot_os_drivers::kprintln!("[OTA] Boot marked good (slot={})",
        if meta.active_slot == SLOT_A { 'A' } else { 'B' });
}

// ---------------------------------------------------------------------------
// CRC-32 (IEEE 802.3, polynomial 0xEDB88320, no lookup table)
// ---------------------------------------------------------------------------

/// Compute CRC-32 of a byte slice (bit-at-a-time, no table).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Incremental CRC-32 state for streaming computation.
pub struct Crc32State {
    crc: u32,
}

impl Crc32State {
    /// Create a new CRC-32 accumulator.
    pub const fn new() -> Self {
        Crc32State { crc: 0xFFFF_FFFF }
    }

    /// Feed a chunk of data into the CRC.
    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.crc ^= byte as u32;
            for _ in 0..8 {
                if self.crc & 1 != 0 {
                    self.crc = (self.crc >> 1) ^ 0xEDB8_8320;
                } else {
                    self.crc >>= 1;
                }
            }
        }
    }

    /// Finalize and return the CRC-32 value.
    pub fn finalize(&self) -> u32 {
        !self.crc
    }
}

// ---------------------------------------------------------------------------
// Verify a slot's raw firmware on disk against BOOTMETA CRC
// ---------------------------------------------------------------------------

/// Verify CRC-32 of a raw firmware file against the expected CRC from BOOTMETA.
///
/// The .BIN file is a raw kernel binary (no header). The expected CRC and
/// size come from BOOTMETA, which was set during the OTA receive.
///
/// Returns `true` if the file exists, size matches, and CRC matches.
pub fn ota_verify_slot(slot: u8) -> bool {
    let meta = ota_read_boot_meta();
    let expected_crc = meta.slot_crc(slot);
    let expected_size = meta.slot_size(slot);

    if expected_size == 0 {
        return false; // no firmware recorded for this slot
    }

    let path = ota_slot_path(slot);
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path, robot_os_fs::O_RDONLY);
    if fd < 0 { return false; }

    // Stream file and compute CRC-32
    let mut crc_state = Crc32State::new();
    let mut total_read = 0u32;
    let mut chunk = [0u8; OTA_RECV_BUF_SIZE];

    loop {
        let got = robot_os_fs::vfs_read(&mut fd_table, fd,
                                         chunk.as_mut_ptr(), chunk.len());
        if got <= 0 { break; }
        crc_state.update(&chunk[..got as usize]);
        total_read += got as u32;
    }
    robot_os_fs::vfs_close(&mut fd_table, fd);

    if total_read != expected_size {
        return false;
    }

    crc_state.finalize() == expected_crc
}

/// Get the slot info (version, size, crc) from BOOTMETA for display.
pub fn ota_slot_info(slot: u8) -> (u32, u32, u32) {
    let meta = ota_read_boot_meta();
    (meta.slot_version(slot), meta.slot_size(slot), meta.slot_crc(slot))
}

// ---------------------------------------------------------------------------
// Boot-time validation (called from kernel_main)
// ---------------------------------------------------------------------------

/// Boot-time OTA validation: increment boot_count, rollback if stuck.
///
/// Call after FAT32 is mounted and config is loaded, before starting tasks.
/// Returns the boot metadata (possibly updated with rollback).
pub fn ota_boot_validate() -> BootMeta {
    let mut meta = ota_read_boot_meta();

    // Increment boot count
    meta.boot_count += 1;

    let max_attempts = CFG_OTA_MAX_BOOT_ATTEMPTS.load(Ordering::Relaxed);

    if meta.boot_count > max_attempts {
        // Boot loop detected — rollback to last known good slot
        robot_os_drivers::kprintln!(
            "[OTA] Boot loop detected (count={} > max={}) — rolling back to slot {}",
            meta.boot_count, max_attempts,
            if meta.last_good == SLOT_A { 'A' } else { 'B' });

        meta.active_slot = meta.last_good;
        meta.boot_count = 1; // reset to 1 (this boot attempt)
    }

    // Persist updated metadata
    ota_write_boot_meta(&meta);
    ota_apply_meta(&meta);

    robot_os_drivers::kprintln!("[OTA] Boot: slot={} count={}/{} last_good={}",
        if meta.active_slot == SLOT_A { 'A' } else { 'B' },
        meta.boot_count, max_attempts,
        if meta.last_good == SLOT_A { 'A' } else { 'B' });

    meta
}

// ---------------------------------------------------------------------------
// Current platform detection
// ---------------------------------------------------------------------------

/// Return the platform ID for the currently running kernel.
pub fn ota_current_platform() -> u8 {
    #[cfg(feature = "vf2")]
    { return OTA_PLATFORM_VF2; }
    #[cfg(feature = "k1")]
    { return OTA_PLATFORM_K1; }
    #[cfg(feature = "esp32c3")]
    { return OTA_PLATFORM_ESP32C3; }
    #[cfg(not(any(feature = "vf2", feature = "k1", feature = "esp32c3")))]
    { OTA_PLATFORM_QEMU }
}

// ---------------------------------------------------------------------------
// Internal helpers — LE encoding
// ---------------------------------------------------------------------------

#[inline]
fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

#[inline]
fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    let b = v.to_le_bytes();
    buf[off]     = b[0];
    buf[off + 1] = b[1];
    buf[off + 2] = b[2];
    buf[off + 3] = b[3];
}

// ---------------------------------------------------------------------------
// Boot metadata serialization
// ---------------------------------------------------------------------------

/// Parse boot metadata from INI-style text.
fn parse_boot_meta(data: &[u8]) -> BootMeta {
    let mut meta = BootMeta::default();

    let mut i = 0;
    while i < data.len() {
        while i < data.len() && (data[i] == b'\n' || data[i] == b'\r'
                                  || data[i] == b' ' || data[i] == b'\t') {
            i += 1;
        }
        if i >= data.len() { break; }
        if data[i] == b'#' {
            while i < data.len() && data[i] != b'\n' { i += 1; }
            continue;
        }

        let key_start = i;
        while i < data.len() && data[i] != b'=' && data[i] != b'\n' { i += 1; }
        if i >= data.len() || data[i] != b'=' { continue; }
        let key = &data[key_start..i];
        i += 1;

        let val_start = i;
        while i < data.len() && data[i] != b'\n' && data[i] != b'\r' { i += 1; }
        let val = &data[val_start..i];

        if key == b"active_slot" {
            meta.active_slot = if val == b"b" || val == b"B" { SLOT_B } else { SLOT_A };
        } else if key == b"boot_count" {
            meta.boot_count = parse_u32_simple(val);
        } else if key == b"last_good" {
            meta.last_good = if val == b"b" || val == b"B" { SLOT_B } else { SLOT_A };
        } else if key == b"fw_version_a" {
            meta.fw_version_a = parse_u32_simple(val);
        } else if key == b"fw_version_b" {
            meta.fw_version_b = parse_u32_simple(val);
        } else if key == b"image_size_a" {
            meta.image_size_a = parse_u32_simple(val);
        } else if key == b"image_size_b" {
            meta.image_size_b = parse_u32_simple(val);
        } else if key == b"image_crc_a" {
            meta.image_crc_a = parse_u32_simple(val);
        } else if key == b"image_crc_b" {
            meta.image_crc_b = parse_u32_simple(val);
        }
    }

    meta
}

/// Serialize boot metadata to INI text. Returns bytes written.
fn serialize_boot_meta(meta: &BootMeta, buf: &mut [u8]) -> usize {
    let mut pos = 0;

    pos += write_kv(buf, pos, b"active_slot=",
                    if meta.active_slot == SLOT_A { b"a" } else { b"b" });
    pos += write_kv_u32(buf, pos, b"boot_count=", meta.boot_count);
    pos += write_kv(buf, pos, b"last_good=",
                    if meta.last_good == SLOT_A { b"a" } else { b"b" });
    pos += write_kv_u32(buf, pos, b"fw_version_a=", meta.fw_version_a);
    pos += write_kv_u32(buf, pos, b"fw_version_b=", meta.fw_version_b);
    pos += write_kv_u32(buf, pos, b"image_size_a=", meta.image_size_a);
    pos += write_kv_u32(buf, pos, b"image_size_b=", meta.image_size_b);
    pos += write_kv_u32(buf, pos, b"image_crc_a=", meta.image_crc_a);
    pos += write_kv_u32(buf, pos, b"image_crc_b=", meta.image_crc_b);

    pos
}

fn write_kv(buf: &mut [u8], pos: usize, key: &[u8], val: &[u8]) -> usize {
    let needed = key.len() + val.len() + 1;
    if pos + needed > buf.len() { return 0; }
    buf[pos..pos + key.len()].copy_from_slice(key);
    let p = pos + key.len();
    buf[p..p + val.len()].copy_from_slice(val);
    buf[p + val.len()] = b'\n';
    needed
}

fn write_kv_u32(buf: &mut [u8], pos: usize, key: &[u8], val: u32) -> usize {
    let mut num_buf = [0u8; 10];
    let num_len = fmt_u32_to_buf(val, &mut num_buf);
    write_kv(buf, pos, key, &num_buf[..num_len])
}

fn fmt_u32_to_buf(mut val: u32, buf: &mut [u8; 10]) -> usize {
    if val == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut i = 10;
    while val > 0 {
        i -= 1;
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    let len = 10 - i;
    buf.copy_within(i..10, 0);
    len
}

fn parse_u32_simple(s: &[u8]) -> u32 {
    let mut v = 0u32;
    for &b in s {
        if b < b'0' || b > b'9' { break; }
        v = v.saturating_mul(10).saturating_add((b - b'0') as u32);
    }
    v
}
