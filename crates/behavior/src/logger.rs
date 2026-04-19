//! E06 — Mission logging / replay / analytics.
//!
//! Ring buffer of compact event records in RAM, periodically flushed to FAT32
//! for post-mortem analysis. All events are fixed-size (32 bytes on-wire) so
//! that replay readers don't have to parse variable-length records.
//!
//! File layout on disk:
//!   /LOG/LOGNNNNN.BIN          N = monotonic session serial (5 digits)
//!   Header 16B:  b"RBL1" | version u16 LE | reserved u16 | open_ts u64 LE
//!   Records 32B each: see LogRecord below.
//!
//! File rotation: when the current file exceeds LOG_FILE_ROTATE_BYTES, it is
//! fsynced + closed and a new file is opened with the next serial number.
//!
//! Analytics counters (in RAM, reset at init):
//!   - total_distance_mm      (from odometry events)
//!   - mission_duration_ticks (CLINT ticks since logger_init)
//!   - battery_mah_used       (INA219 integration, caller feeds microamp-hours)
//!   - safety_violations      (incremented per SafetyViolation event)
//!   - events_dropped         (ring-buffer overflow counter)
//!   - flush_errors           (FAT32 errors during flush)

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use robot_os_sync::SpinLock;
use robot_os_fs::{
    fat32_mount_volume, fat32_mkdir, fat32_open, fat32_write, fat32_fsync,
    fat32_close, fat32_file_stat, open_flags, FsError, Volume, Fat32File,
};

// ---------------------------------------------------------------------------
// Event kinds — one u8 per record.
// ---------------------------------------------------------------------------

/// Periodic sensor snapshot (~1 Hz).
pub const LOG_EVT_SENSOR_SNAPSHOT: u8 = 0x01;
/// Actuator command issued to motors.
pub const LOG_EVT_ACTUATOR_CMD: u8 = 0x02;
/// Safety violation detected (see `safety::SafetyViolation`).
pub const LOG_EVT_SAFETY_VIOLATION: u8 = 0x03;
/// Mode change (idle / autonomous / teleop / return-to-home).
pub const LOG_EVT_MODE_CHANGE: u8 = 0x04;
/// Skill started.
pub const LOG_EVT_SKILL_START: u8 = 0x05;
/// Skill ended.
pub const LOG_EVT_SKILL_END: u8 = 0x06;
/// Waypoint reached or updated.
pub const LOG_EVT_WAYPOINT: u8 = 0x07;
/// Error condition (sensor fault, comms loss, etc.).
pub const LOG_EVT_ERROR: u8 = 0x08;

// ---------------------------------------------------------------------------
// Record layout — 32 bytes fixed.
// ---------------------------------------------------------------------------

/// Size of a log record on-disk and in-ring (bytes).
pub const LOG_RECORD_SIZE: usize = 32;
/// Payload bytes per record (after fixed 12-byte header).
pub const LOG_PAYLOAD_BYTES: usize = 20;

/// One log event. The on-wire representation is:
///   [ts u64 LE] [kind u8] [flags u8] [_pad u16] [payload 20B]
#[derive(Clone, Copy)]
pub struct LogRecord {
    pub ts:      u64,
    pub kind:    u8,
    pub flags:   u8,
    pub payload: [u8; LOG_PAYLOAD_BYTES],
}

impl LogRecord {
    pub const fn zeroed() -> Self {
        Self { ts: 0, kind: 0, flags: 0, payload: [0; LOG_PAYLOAD_BYTES] }
    }

    /// Encode into a 32-byte buffer (little-endian).
    pub fn encode(&self, out: &mut [u8; LOG_RECORD_SIZE]) {
        out[0..8].copy_from_slice(&self.ts.to_le_bytes());
        out[8]  = self.kind;
        out[9]  = self.flags;
        out[10] = 0;
        out[11] = 0;
        out[12..32].copy_from_slice(&self.payload);
    }

    /// Decode from a 32-byte buffer.
    pub fn decode(buf: &[u8; LOG_RECORD_SIZE]) -> Self {
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&buf[0..8]);
        let mut payload = [0u8; LOG_PAYLOAD_BYTES];
        payload.copy_from_slice(&buf[12..32]);
        Self { ts: u64::from_le_bytes(ts), kind: buf[8], flags: buf[9], payload }
    }
}

// ---------------------------------------------------------------------------
// Ring buffer (power-of-two capacity for fast masking).
// ---------------------------------------------------------------------------

/// Number of records held in RAM before flushing.
pub const LOG_RING_CAPACITY: usize = 128;

struct LogRing {
    records: [LogRecord; LOG_RING_CAPACITY],
    head:    usize,  // next write index
    tail:    usize,  // next read index
    count:   usize,  // records currently in ring
}

impl LogRing {
    const fn new() -> Self {
        Self {
            records: [LogRecord::zeroed(); LOG_RING_CAPACITY],
            head: 0, tail: 0, count: 0,
        }
    }

    /// Push a record. Drops the oldest if full (and bumps the drop counter).
    fn push(&mut self, rec: LogRecord) -> bool {
        let overflowed = self.count == LOG_RING_CAPACITY;
        if overflowed {
            // Overwrite oldest — advance tail as well.
            self.tail = (self.tail + 1) % LOG_RING_CAPACITY;
            self.count -= 1;
        }
        self.records[self.head] = rec;
        self.head = (self.head + 1) % LOG_RING_CAPACITY;
        self.count += 1;
        !overflowed
    }

    fn drain_into(&mut self, out: &mut [LogRecord]) -> usize {
        let n = core::cmp::min(self.count, out.len());
        for slot in out.iter_mut().take(n) {
            *slot = self.records[self.tail];
            self.tail = (self.tail + 1) % LOG_RING_CAPACITY;
            self.count -= 1;
        }
        n
    }
}

// ---------------------------------------------------------------------------
// Global state (SpinLock-protected ring + atomics for counters/flags).
// ---------------------------------------------------------------------------

static LOG_RING: SpinLock<LogRing> = SpinLock::new(LogRing::new());

/// True after successful `logger_init`.
static LOG_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Monotonic session serial number (persisted state — we simply pick the
/// next free integer at init; no filesystem scan in minimal impl).
static LOG_SERIAL: AtomicU32 = AtomicU32::new(0);

/// Currently-open logfile (None if not yet opened or rotation in progress).
static LOG_FILE: SpinLock<Option<OpenLogFile>> = SpinLock::new(None);

/// Analytics counters.
static LOG_DISTANCE_MM:     AtomicU64 = AtomicU64::new(0);
static LOG_INIT_TS:         AtomicU64 = AtomicU64::new(0);
static LOG_BATTERY_UAH:     AtomicU64 = AtomicU64::new(0);
static LOG_SAFETY_COUNT:    AtomicU32 = AtomicU32::new(0);
static LOG_DROPPED:         AtomicU32 = AtomicU32::new(0);
static LOG_FLUSH_ERRORS:    AtomicU32 = AtomicU32::new(0);

struct OpenLogFile {
    file:          Fat32File,
    vol:           Volume,
    bytes_written: u32,
    serial:        u32,
}

// ---------------------------------------------------------------------------
// Logger configuration.
// ---------------------------------------------------------------------------

/// Rotate the log file when it exceeds this many bytes.
pub const LOG_FILE_ROTATE_BYTES: u32 = 1024 * 1024;
/// Flush the ring to disk when this many records are queued.
pub const LOG_FLUSH_WATERMARK: usize = LOG_RING_CAPACITY / 2;
/// Log directory (always ASCII uppercase for FAT32 8.3).
pub const LOG_DIR_PATH: &[u8] = b"/LOG";
/// File magic identifying a robot-os log file.
pub const LOG_FILE_MAGIC: &[u8; 4] = b"RBL1";
/// Log file format version.
pub const LOG_FILE_VERSION: u16 = 1;
/// Size of the on-disk file header.
pub const LOG_FILE_HEADER_BYTES: usize = 16;
/// Upper bound on records flushed per call (tune for WCET).
pub const LOG_FLUSH_BATCH_MAX: usize = LOG_RING_CAPACITY;

// ---------------------------------------------------------------------------
// Public API — lifecycle.
// ---------------------------------------------------------------------------

/// Initialise the logger. Idempotent: a second call returns immediately.
///
/// Mounts FAT32 (if not mounted), creates `/LOG`, opens the first file, writes
/// the header. Returns `Err` if any of those fail; the logger stays inactive
/// in that case and event calls become no-ops.
pub fn logger_init() -> Result<(), FsError> {
    if LOG_ACTIVE.load(Ordering::Acquire) { return Ok(()); }

    let vol = fat32_mount_volume()?;
    // Best-effort mkdir — already-existing directory is fine.
    let _ = fat32_mkdir(vol, LOG_DIR_PATH);

    let serial = LOG_SERIAL.fetch_add(1, Ordering::Relaxed);
    open_log_file(vol, serial)?;

    LOG_INIT_TS.store(now_ticks(), Ordering::Relaxed);
    LOG_ACTIVE.store(true, Ordering::Release);
    Ok(())
}

/// Gracefully shut down the logger: flush remaining records, fsync+close file.
pub fn logger_shutdown() {
    if !LOG_ACTIVE.swap(false, Ordering::AcqRel) { return; }
    let _ = logger_flush();
    let mut guard = LOG_FILE.lock();
    if let Some(open) = guard.take() {
        let _ = fat32_fsync(open.file);
        let _ = fat32_close(open.file);
    }
}

/// Periodic tick — call from a kernel timer (~1 Hz). Flushes when the ring
/// watermark is reached.
pub fn logger_tick() {
    if !LOG_ACTIVE.load(Ordering::Acquire) { return; }
    let should_flush = LOG_RING.lock().count >= LOG_FLUSH_WATERMARK;
    if should_flush { let _ = logger_flush(); }
}

/// Force a flush of the ring buffer to disk. Returns the number of records
/// written (can be 0 if the ring was empty).
pub fn logger_flush() -> Result<usize, FsError> {
    let mut batch: [LogRecord; LOG_FLUSH_BATCH_MAX] =
        [LogRecord::zeroed(); LOG_FLUSH_BATCH_MAX];
    let n = LOG_RING.lock().drain_into(&mut batch);
    if n == 0 { return Ok(0); }

    // Encode the batch into a contiguous buffer.
    let mut out = [0u8; LOG_FLUSH_BATCH_MAX * LOG_RECORD_SIZE];
    for (i, rec) in batch.iter().take(n).enumerate() {
        let slot = &mut out[i * LOG_RECORD_SIZE .. (i + 1) * LOG_RECORD_SIZE];
        let mut buf = [0u8; LOG_RECORD_SIZE];
        rec.encode(&mut buf);
        slot.copy_from_slice(&buf);
    }

    let mut guard = LOG_FILE.lock();
    let open = match guard.as_mut() {
        Some(o) => o,
        None => {
            LOG_FLUSH_ERRORS.fetch_add(1, Ordering::Relaxed);
            return Err(FsError::InvalidArg);
        }
    };

    match fat32_write(open.file, &out[.. n * LOG_RECORD_SIZE]) {
        Ok(written) => {
            open.bytes_written = open.bytes_written.saturating_add(written as u32);
            let _ = fat32_fsync(open.file);
        }
        Err(e) => {
            LOG_FLUSH_ERRORS.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
    }

    // Rotate if the file grew past the threshold.
    if open.bytes_written >= LOG_FILE_ROTATE_BYTES {
        let vol = open.vol;
        let _ = fat32_fsync(open.file);
        let _ = fat32_close(open.file);
        *guard = None;
        drop(guard);
        let next_serial = LOG_SERIAL.fetch_add(1, Ordering::Relaxed);
        let _ = open_log_file(vol, next_serial);
    }

    Ok(n)
}

// ---------------------------------------------------------------------------
// Event encoders — each wraps `push_event()`.
// ---------------------------------------------------------------------------

/// Log an odometry/sensor snapshot. Also updates the distance counter if
/// `distance_delta_mm > 0`.
pub fn log_sensor_snapshot(
    battery_mv: u16,
    velocity_mm_s: i16,
    distance_delta_mm: u32,
    heading_cdeg: i32,
) {
    if distance_delta_mm > 0 {
        LOG_DISTANCE_MM.fetch_add(distance_delta_mm as u64, Ordering::Relaxed);
    }
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0..2].copy_from_slice(&battery_mv.to_le_bytes());
    payload[2..4].copy_from_slice(&velocity_mm_s.to_le_bytes());
    payload[4..8].copy_from_slice(&distance_delta_mm.to_le_bytes());
    payload[8..12].copy_from_slice(&heading_cdeg.to_le_bytes());
    push_event(LOG_EVT_SENSOR_SNAPSHOT, 0, payload);
}

/// Log an actuator command (speed_l, speed_r for wheeled; channel/value generic).
pub fn log_actuator_cmd(actuator_type: u8, ch0: i16, ch1: i16, ch2: i16, ch3: i16) {
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0] = actuator_type;
    payload[2..4].copy_from_slice(&ch0.to_le_bytes());
    payload[4..6].copy_from_slice(&ch1.to_le_bytes());
    payload[6..8].copy_from_slice(&ch2.to_le_bytes());
    payload[8..10].copy_from_slice(&ch3.to_le_bytes());
    push_event(LOG_EVT_ACTUATOR_CMD, 0, payload);
}

/// Log a safety violation. Bumps the violation counter.
pub fn log_safety_violation(violation_code: u8, action_code: u8, detail: u32) {
    LOG_SAFETY_COUNT.fetch_add(1, Ordering::Relaxed);
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0] = violation_code;
    payload[1] = action_code;
    payload[4..8].copy_from_slice(&detail.to_le_bytes());
    push_event(LOG_EVT_SAFETY_VIOLATION, 0, payload);
}

/// Log a mode transition (old_mode → new_mode).
pub fn log_mode_change(old_mode: u8, new_mode: u8) {
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0] = old_mode;
    payload[1] = new_mode;
    push_event(LOG_EVT_MODE_CHANGE, 0, payload);
}

/// Log skill start.
pub fn log_skill_start(skill_id: u16) {
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0..2].copy_from_slice(&skill_id.to_le_bytes());
    push_event(LOG_EVT_SKILL_START, 0, payload);
}

/// Log skill end with a result code.
pub fn log_skill_end(skill_id: u16, result_code: u8) {
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0..2].copy_from_slice(&skill_id.to_le_bytes());
    payload[2] = result_code;
    push_event(LOG_EVT_SKILL_END, 0, payload);
}

/// Log a waypoint event (reached / updated).
pub fn log_waypoint(x_mm: i32, y_mm: i32, index: u16, kind: u8) {
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0..4].copy_from_slice(&x_mm.to_le_bytes());
    payload[4..8].copy_from_slice(&y_mm.to_le_bytes());
    payload[8..10].copy_from_slice(&index.to_le_bytes());
    payload[10] = kind;
    push_event(LOG_EVT_WAYPOINT, 0, payload);
}

/// Log an error condition.
pub fn log_error(subsystem: u8, error_code: u16, detail: u32) {
    let mut payload = [0u8; LOG_PAYLOAD_BYTES];
    payload[0] = subsystem;
    payload[1..3].copy_from_slice(&error_code.to_le_bytes());
    payload[4..8].copy_from_slice(&detail.to_le_bytes());
    push_event(LOG_EVT_ERROR, 0, payload);
}

// ---------------------------------------------------------------------------
// Analytics view.
// ---------------------------------------------------------------------------

/// Snapshot of the in-memory analytics counters.
#[derive(Clone, Copy, Debug)]
pub struct LoggerAnalytics {
    pub total_distance_mm:     u64,
    pub mission_duration_ticks: u64,
    pub battery_mah_used:      u32,
    pub safety_violations:     u32,
    pub events_dropped:        u32,
    pub flush_errors:          u32,
}

/// Read the current analytics counters.
pub fn logger_analytics() -> LoggerAnalytics {
    let init_ts = LOG_INIT_TS.load(Ordering::Relaxed);
    let duration = now_ticks().saturating_sub(init_ts);
    let battery_uah = LOG_BATTERY_UAH.load(Ordering::Relaxed);
    // mAh = uAh / 1000 (integer division, conservative for low usage).
    let battery_mah = (battery_uah / 1000) as u32;
    LoggerAnalytics {
        total_distance_mm:      LOG_DISTANCE_MM.load(Ordering::Relaxed),
        mission_duration_ticks: duration,
        battery_mah_used:       battery_mah,
        safety_violations:      LOG_SAFETY_COUNT.load(Ordering::Relaxed),
        events_dropped:         LOG_DROPPED.load(Ordering::Relaxed),
        flush_errors:           LOG_FLUSH_ERRORS.load(Ordering::Relaxed),
    }
}

/// Accumulate battery usage in microamp-hours — caller integrates INA219 reads.
pub fn logger_add_battery_uah(uah: u32) {
    LOG_BATTERY_UAH.fetch_add(uah as u64, Ordering::Relaxed);
}

/// Reset the analytics counters (typically called on a new mission start).
pub fn logger_analytics_reset() {
    LOG_DISTANCE_MM.store(0, Ordering::Relaxed);
    LOG_BATTERY_UAH.store(0, Ordering::Relaxed);
    LOG_SAFETY_COUNT.store(0, Ordering::Relaxed);
    LOG_DROPPED.store(0, Ordering::Relaxed);
    LOG_FLUSH_ERRORS.store(0, Ordering::Relaxed);
    LOG_INIT_TS.store(now_ticks(), Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Internals.
// ---------------------------------------------------------------------------

fn push_event(kind: u8, flags: u8, payload: [u8; LOG_PAYLOAD_BYTES]) {
    if !LOG_ACTIVE.load(Ordering::Acquire) { return; }
    let rec = LogRecord { ts: now_ticks(), kind, flags, payload };
    let accepted = LOG_RING.lock().push(rec);
    if !accepted {
        LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

fn now_ticks() -> u64 {
    robot_os_drivers::clint::get_time()
}

/// Build a path `/LOG/LOGNNNNN.BIN` for the given serial.
fn make_log_path(serial: u32, out: &mut [u8; 17]) {
    // "/LOG/LOG00000.BIN"  = 17 bytes.
    out.copy_from_slice(b"/LOG/LOG00000.BIN");
    let mut n = serial % 100_000;
    let digits_start = 8; // "/LOG/LOG" is 8 chars, then 5 digits.
    for i in (0..5).rev() {
        out[digits_start + i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
}

fn open_log_file(vol: Volume, serial: u32) -> Result<(), FsError> {
    let mut path = [0u8; 17];
    make_log_path(serial, &mut path);
    let flags = open_flags::WRITE | open_flags::CREATE | open_flags::TRUNCATE;
    let file = fat32_open(vol, &path, flags)?;

    // Write header.
    let mut hdr = [0u8; LOG_FILE_HEADER_BYTES];
    hdr[0..4].copy_from_slice(LOG_FILE_MAGIC);
    hdr[4..6].copy_from_slice(&LOG_FILE_VERSION.to_le_bytes());
    hdr[6..8].copy_from_slice(&0u16.to_le_bytes());
    hdr[8..16].copy_from_slice(&now_ticks().to_le_bytes());
    let written = fat32_write(file, &hdr)?;
    let _ = fat32_fsync(file);

    let (_pos, size) = fat32_file_stat(file)?;
    let _ = size; // header count already known via `written`.

    *LOG_FILE.lock() = Some(OpenLogFile {
        file,
        vol,
        bytes_written: written as u32,
        serial,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpful test-friendly accessors.
// ---------------------------------------------------------------------------

/// Number of records currently in the ring (for tests / diagnostics).
pub fn logger_ring_len() -> usize {
    LOG_RING.lock().count
}

/// Current session serial number (mostly for tests).
pub fn logger_current_serial() -> u32 {
    LOG_FILE.lock().as_ref().map(|f| f.serial).unwrap_or(0)
}
