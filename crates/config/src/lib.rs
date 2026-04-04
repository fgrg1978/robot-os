#![no_std]
//! Persistent key-value configuration store — Phase 18 + G2.
//!
//! Parses and serialises simple `KEY=VALUE` settings.
//!
//! ## Runtime atomics
//!
//! `cfg_apply()` transfers parsed values into `AtomicBool` / `AtomicU32`
//! globals that can be read safely from concurrent tasks without locking.
//!
//! # File format
//!
//! ```text
//! # Robot OS Configuration
//! ml_enabled=1
//! sched_hz=100
//! watchdog_ms=500
//! motor_max_speed=100
//! ```
//!
//! Lines starting with `#` and blank lines are ignored.
//! Keys and values are byte strings; leading/trailing spaces are stripped.
//!
//! # Limits
//!
//! - Up to `MAX_ENTRIES` (32) entries.
//! - Keys  ≤ `MAX_KEY`  (24) bytes.
//! - Values ≤ `MAX_VAL` (16) bytes.
//!
//! # Thread safety
//!
//! `cfg_load` is called once during single-threaded boot (before tasks start).
//! `cfg_get` / `cfg_set` are only used from the shell task (single writer).
//! Consumers that need safe concurrent access should copy values into an
//! `AtomicBool` / `AtomicU32` after `cfg_load` (see `ML_ENABLED` etc.).

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// ── Runtime atomics (Phase 18 + G2) ──────────────────────────────────────────

/// Runtime flag — true if the ML deliberative pipeline is enabled.
pub static ML_ENABLED: AtomicBool = AtomicBool::new(true);

/// Runtime: behavior VLA server IP packed as `(a<<24)|(b<<16)|(c<<8)|d`.
pub static BEHAVIOR_SERVER_IP: AtomicU32 = AtomicU32::new(0);

/// Runtime: behavior VLA server port (default 0 = disabled).
pub static BEHAVIOR_SERVER_PORT: AtomicU32 = AtomicU32::new(0);

/// Runtime: behavior layer 1 (avoid-obstacle) enabled.
pub static BEHAVIOR_L1_ENABLED: AtomicBool = AtomicBool::new(true);

/// Runtime: behavior layer 2 (remote-vla) enabled.
pub static BEHAVIOR_L2_ENABLED: AtomicBool = AtomicBool::new(true);

/// Runtime: behavior layer 3 (explore) enabled.
pub static BEHAVIOR_L3_ENABLED: AtomicBool = AtomicBool::new(true);

/// Runtime: PID proportional gain (×1000 fixed-point: 1000 = 1.0).
pub static CFG_PID_KP: AtomicU32 = AtomicU32::new(1);

/// Runtime: PID integral gain.
pub static CFG_PID_KI: AtomicU32 = AtomicU32::new(0);

/// Runtime: PID derivative gain.
pub static CFG_PID_KD: AtomicU32 = AtomicU32::new(0);

/// Runtime: encoder ticks per metre.
pub static CFG_TICKS_PER_M: AtomicU32 = AtomicU32::new(1000);

/// Runtime: wheel base in millimetres.
pub static CFG_WHEEL_BASE_MM: AtomicU32 = AtomicU32::new(200);

/// Runtime: network IP packed as `(a<<24)|(b<<16)|(c<<8)|d`.
pub static CFG_NET_IP: AtomicU32 = AtomicU32::new(0x0A00_020F); // 10.0.2.15

/// Runtime: network gateway packed.
pub static CFG_NET_GATEWAY: AtomicU32 = AtomicU32::new(0x0A00_0202); // 10.0.2.2

/// Runtime: network mask packed.
pub static CFG_NET_MASK: AtomicU32 = AtomicU32::new(0xFFFF_FF00); // 255.255.255.0

/// Runtime: DHCP enabled (1 = auto-discover IP at boot, 0 = use static config).
pub static CFG_NET_DHCP: AtomicU32 = AtomicU32::new(0);

/// Runtime: IMU calibration offset — accel X (signed, milli-g units).
pub static CFG_IMU_OFFSET_AX: AtomicU32 = AtomicU32::new(0);

/// Runtime: IMU calibration offset — accel Y.
pub static CFG_IMU_OFFSET_AY: AtomicU32 = AtomicU32::new(0);

/// Runtime: IMU calibration offset — accel Z.
pub static CFG_IMU_OFFSET_AZ: AtomicU32 = AtomicU32::new(0);

/// Runtime: hardware watchdog timeout in milliseconds.
pub static CFG_WATCHDOG_MS: AtomicU32 = AtomicU32::new(500);

/// Runtime: motor max speed (0-100).
pub static CFG_MOTOR_MAX_SPEED: AtomicU32 = AtomicU32::new(100);

/// Runtime: auto-reboot delay after panic (ms). 0 = disabled (infinite WFI).
pub static CFG_PANIC_REBOOT_DELAY_MS: AtomicU32 = AtomicU32::new(0);

/// Runtime: GPIO pin for hardware kill-switch (255 = disabled).
pub static CFG_ESTOP_GPIO_PIN: AtomicU32 = AtomicU32::new(255);

/// Unpack an IP from packed u32 to `[u8; 4]`.
pub fn unpack_ip(packed: u32) -> [u8; 4] {
    [
        (packed >> 24) as u8,
        (packed >> 16) as u8,
        (packed >>  8) as u8,
        packed as u8,
    ]
}

/// Unpack server IP from packed u32 to `[u8; 4]`.
pub fn behavior_server_ip_bytes() -> [u8; 4] {
    unpack_ip(BEHAVIOR_SERVER_IP.load(Ordering::Relaxed))
}

/// Apply loaded configuration to runtime atomic values.
///
/// Call once after `cfg_load`, still in single-threaded boot.
/// Subsequent calls (e.g. after `config load` in the shell) are safe because
/// `AtomicBool::store` with `Release` ordering is visible to any `Acquire` load.
pub fn cfg_apply() {
    ML_ENABLED.store(cfg_get_u32(b"ml_enabled", 1) != 0, Ordering::Release);
    BEHAVIOR_SERVER_PORT.store(cfg_get_u32(b"behavior_server_port", 0), Ordering::Release);

    // Parse behavior_server_ip as dotted-decimal → packed u32.
    if let Some(ip_val) = cfg_get(b"behavior_server_ip") {
        if let Some(packed) = parse_ip_packed(ip_val) {
            BEHAVIOR_SERVER_IP.store(packed, Ordering::Release);
        }
    }

    // Phase G2: behavior layers
    BEHAVIOR_L1_ENABLED.store(cfg_get_u32(b"behavior_l1_enabled", 1) != 0, Ordering::Release);
    BEHAVIOR_L2_ENABLED.store(cfg_get_u32(b"behavior_l2_enabled", 1) != 0, Ordering::Release);
    BEHAVIOR_L3_ENABLED.store(cfg_get_u32(b"behavior_l3_enabled", 1) != 0, Ordering::Release);

    // PID tuning
    CFG_PID_KP.store(cfg_get_u32(b"pid_kp", 1), Ordering::Release);
    CFG_PID_KI.store(cfg_get_u32(b"pid_ki", 0), Ordering::Release);
    CFG_PID_KD.store(cfg_get_u32(b"pid_kd", 0), Ordering::Release);

    // Robot physical
    CFG_TICKS_PER_M.store(cfg_get_u32(b"ticks_per_m", 1000), Ordering::Release);
    CFG_WHEEL_BASE_MM.store(cfg_get_u32(b"wheel_base_mm", 200), Ordering::Release);

    // Network
    if let Some(v) = cfg_get(b"net_ip") {
        if let Some(p) = parse_ip_packed(v) { CFG_NET_IP.store(p, Ordering::Release); }
    }
    if let Some(v) = cfg_get(b"net_gateway") {
        if let Some(p) = parse_ip_packed(v) { CFG_NET_GATEWAY.store(p, Ordering::Release); }
    }
    if let Some(v) = cfg_get(b"net_mask") {
        if let Some(p) = parse_ip_packed(v) { CFG_NET_MASK.store(p, Ordering::Release); }
    }
    CFG_NET_DHCP.store(cfg_get_u32(b"dhcp", 0), Ordering::Release);

    // IMU calibration (signed offsets stored as u32 bit-patterns)
    CFG_IMU_OFFSET_AX.store(cfg_get_i32(b"imu_offset_ax", 0) as u32, Ordering::Release);
    CFG_IMU_OFFSET_AY.store(cfg_get_i32(b"imu_offset_ay", 0) as u32, Ordering::Release);
    CFG_IMU_OFFSET_AZ.store(cfg_get_i32(b"imu_offset_az", 0) as u32, Ordering::Release);

    // Watchdog + motor
    CFG_WATCHDOG_MS.store(cfg_get_u32(b"watchdog_ms", 500), Ordering::Release);
    CFG_MOTOR_MAX_SPEED.store(cfg_get_u32(b"motor_max_speed", 100), Ordering::Release);

    // Panic + safety
    CFG_PANIC_REBOOT_DELAY_MS.store(cfg_get_u32(b"panic_reboot_ms", 0), Ordering::Release);
    CFG_ESTOP_GPIO_PIN.store(cfg_get_u32(b"estop_gpio_pin", 255), Ordering::Release);
}

/// Parse "a.b.c.d" into packed u32 `(a<<24)|(b<<16)|(c<<8)|d`.
fn parse_ip_packed(s: &[u8]) -> Option<u32> {
    let mut octets = [0u8; 4];
    let mut octet  = 0u32;
    let mut idx    = 0usize;
    let mut any    = false;

    for &b in s {
        if b >= b'0' && b <= b'9' {
            octet = octet * 10 + (b - b'0') as u32;
            any = true;
        } else if b == b'.' {
            if !any || idx >= 3 || octet > 255 { return None; }
            octets[idx] = octet as u8;
            idx += 1;
            octet = 0;
            any = false;
        } else {
            break;
        }
    }
    if any && idx == 3 && octet <= 255 {
        octets[3] = octet as u8;
        Some((octets[0] as u32) << 24
           | (octets[1] as u32) << 16
           | (octets[2] as u32) << 8
           |  octets[3] as u32)
    } else {
        None
    }
}

/// Maximum number of configuration entries stored.
pub const MAX_ENTRIES: usize = 32;
/// Maximum key length in bytes.
pub const MAX_KEY: usize = 24;
/// Maximum value length in bytes.
pub const MAX_VAL: usize = 16;

#[derive(Copy, Clone)]
struct Entry {
    key:     [u8; MAX_KEY],
    val:     [u8; MAX_VAL],
    key_len: u8,
    val_len: u8,
    used:    bool,
}

impl Entry {
    const fn new() -> Self {
        Entry {
            key:     [0; MAX_KEY],
            val:     [0; MAX_VAL],
            key_len: 0,
            val_len: 0,
            used:    false,
        }
    }
}

static mut ENTRIES: [Entry; MAX_ENTRIES] = [Entry::new(); MAX_ENTRIES];
static mut COUNT:   usize                = 0;

// ── Parser ────────────────────────────────────────────────────────────────────

/// Load configuration from an INI-format byte slice.
///
/// Overwrites all previous entries.
/// Lines starting with `#` and blank lines are silently ignored.
pub fn cfg_load(data: &[u8]) {
    // Safety: called once during single-threaded boot.
    unsafe {
        ENTRIES = [Entry::new(); MAX_ENTRIES];
        COUNT   = 0;
    }
    let mut i = 0;
    while i < data.len() {
        // Skip blank lines and leading whitespace.
        while i < data.len()
            && (data[i] == b'\r' || data[i] == b'\n'
                || data[i] == b' '  || data[i] == b'\t')
        {
            i += 1;
        }
        if i >= data.len() { break; }

        // Skip comment lines.
        if data[i] == b'#' {
            while i < data.len() && data[i] != b'\n' { i += 1; }
            continue;
        }

        // Scan key up to '=' or end-of-line.
        let key_start = i;
        while i < data.len() && data[i] != b'=' && data[i] != b'\n' { i += 1; }
        if i >= data.len() || data[i] != b'=' { continue; }
        let key_end = rtrim(data, key_start, i);
        i += 1; // skip '='

        // Scan value up to end-of-line.
        let val_start = i;
        while i < data.len() && data[i] != b'\n' && data[i] != b'\r' { i += 1; }
        let val_end = rtrim(data, val_start, i);

        let klen = (key_end - key_start).min(MAX_KEY);
        let vlen = (val_end - val_start).min(MAX_VAL);
        if klen == 0 { continue; }

        // Safety: single-threaded boot, COUNT < MAX_ENTRIES checked.
        unsafe {
            if COUNT < MAX_ENTRIES {
                let e = &mut ENTRIES[COUNT];
                e.key[..klen].copy_from_slice(&data[key_start..key_start + klen]);
                e.key_len = klen as u8;
                e.val[..vlen].copy_from_slice(&data[val_start..val_start + vlen]);
                e.val_len = vlen as u8;
                e.used    = true;
                COUNT    += 1;
            }
        }
    }
}

// ── Accessors ─────────────────────────────────────────────────────────────────

/// Return the value bytes for `key`, or `None` if the key is not found.
pub fn cfg_get(key: &[u8]) -> Option<&'static [u8]> {
    unsafe {
        for i in 0..COUNT {
            let e = &ENTRIES[i];
            if e.used && &e.key[..e.key_len as usize] == key {
                return Some(&e.val[..e.val_len as usize]);
            }
        }
    }
    None
}

/// Return value parsed as `u32`, or `default` if the key is absent / not numeric.
pub fn cfg_get_u32(key: &[u8], default: u32) -> u32 {
    match cfg_get(key) {
        None    => default,
        Some(v) => parse_u32(v).unwrap_or(default),
    }
}

/// Return value parsed as `i32`, or `default` if the key is absent / not numeric.
/// Handles leading `-` for negative values.
pub fn cfg_get_i32(key: &[u8], default: i32) -> i32 {
    match cfg_get(key) {
        None    => default,
        Some(v) => parse_i32(v).unwrap_or(default),
    }
}

/// Set (or insert) a key-value pair in memory.
///
/// Returns `false` if the key or value exceeds the maximum length, or the
/// table is already full and the key is new.
pub fn cfg_set(key: &[u8], val: &[u8]) -> bool {
    if key.len() > MAX_KEY || val.len() > MAX_VAL { return false; }
    let klen = key.len();
    let vlen = val.len();
    unsafe {
        // Update existing entry if key already present.
        for i in 0..COUNT {
            let e = &mut ENTRIES[i];
            if e.used && &e.key[..e.key_len as usize] == key {
                e.val = [0; MAX_VAL];
                e.val[..vlen].copy_from_slice(&val[..vlen]);
                e.val_len = vlen as u8;
                return true;
            }
        }
        // Insert new entry.
        if COUNT < MAX_ENTRIES {
            let e = &mut ENTRIES[COUNT];
            e.key[..klen].copy_from_slice(&key[..klen]);
            e.key_len = klen as u8;
            e.val[..vlen].copy_from_slice(&val[..vlen]);
            e.val_len = vlen as u8;
            e.used    = true;
            COUNT    += 1;
            return true;
        }
    }
    false
}

/// Returns the current number of loaded entries.
pub fn cfg_count() -> usize {
    unsafe { COUNT }
}

/// Retrieve entry at index `i` (0 = first). Returns `(key, value)` slices.
/// Returns `None` if `i >= cfg_count()`.
pub fn cfg_iter(i: usize) -> Option<(&'static [u8], &'static [u8])> {
    unsafe {
        if i >= COUNT { return None; }
        let e = &ENTRIES[i];
        if !e.used { return None; }
        Some((&e.key[..e.key_len as usize], &e.val[..e.val_len as usize]))
    }
}

/// Serialise all entries into `buf` as `KEY=VALUE\n` text.
/// Returns the number of bytes written.
pub fn cfg_serialize(buf: &mut [u8]) -> usize {
    let mut pos = 0usize;
    unsafe {
        for i in 0..COUNT {
            let e = &ENTRIES[i];
            if !e.used { continue; }
            let kl = e.key_len as usize;
            let vl = e.val_len as usize;
            if pos + kl + 1 + vl + 1 > buf.len() { break; }
            buf[pos..pos + kl].copy_from_slice(&e.key[..kl]); pos += kl;
            buf[pos] = b'=';                                    pos += 1;
            buf[pos..pos + vl].copy_from_slice(&e.val[..vl]); pos += vl;
            buf[pos] = b'\n';                                   pos += 1;
        }
    }
    pos
}

// ── Phase G2: Factory defaults ───────────────────────────────────────────────

/// Populate the config store with factory-default values (22 keys).
///
/// Call when CONFIG.INI is missing (first boot) or on `config defaults`.
/// Overwrites all previous entries.
pub fn cfg_defaults() {
    // Safety: called during single-threaded boot or from shell (single writer).
    unsafe {
        ENTRIES = [Entry::new(); MAX_ENTRIES];
        COUNT   = 0;
    }
    // General
    cfg_set(b"ml_enabled", b"1");
    cfg_set(b"sched_hz", b"100");
    cfg_set(b"watchdog_ms", b"500");
    cfg_set(b"motor_max_speed", b"100");
    // Behavior engine
    cfg_set(b"behavior_server_ip", b"0.0.0.0");
    cfg_set(b"behavior_server_port", b"0");
    cfg_set(b"behavior_l1_enabled", b"1");
    cfg_set(b"behavior_l2_enabled", b"1");
    cfg_set(b"behavior_l3_enabled", b"1");
    // PID tuning
    cfg_set(b"pid_kp", b"1");
    cfg_set(b"pid_ki", b"0");
    cfg_set(b"pid_kd", b"0");
    // Robot physical
    cfg_set(b"ticks_per_m", b"1000");
    cfg_set(b"wheel_base_mm", b"200");
    // Network
    cfg_set(b"net_ip", b"10.0.2.15");
    cfg_set(b"net_gateway", b"10.0.2.2");
    cfg_set(b"net_mask", b"255.255.255.0");
    // IMU calibration
    cfg_set(b"imu_offset_ax", b"0");
    cfg_set(b"imu_offset_ay", b"0");
    cfg_set(b"imu_offset_az", b"0");
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn rtrim(data: &[u8], start: usize, end: usize) -> usize {
    let mut e = end;
    while e > start && (data[e - 1] == b' ' || data[e - 1] == b'\t') { e -= 1; }
    e
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    let mut v   = 0u32;
    let mut any = false;
    for &b in s {
        if b < b'0' || b > b'9' { break; }
        v   = v.saturating_mul(10).saturating_add((b - b'0') as u32);
        any = true;
    }
    if any { Some(v) } else { None }
}

fn parse_i32(s: &[u8]) -> Option<i32> {
    if s.is_empty() { return None; }
    let (neg, start) = if s[0] == b'-' { (true, 1) } else { (false, 0) };
    let mut v   = 0i32;
    let mut any = false;
    for &b in &s[start..] {
        if b < b'0' || b > b'9' { break; }
        v   = v.saturating_mul(10).saturating_add((b - b'0') as i32);
        any = true;
    }
    if !any { return None; }
    if neg { Some(-v) } else { Some(v) }
}
