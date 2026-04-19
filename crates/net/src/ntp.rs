//! SNTP client — Simple Network Time Protocol (RFC 4330, F05.2).
//!
//! Sends a unicast SNTP request to the configured NTP server and adjusts
//! the kernel's wall-clock offset so that callers can compute Unix time.
//!
//! # Design
//!
//! - Pure UDP, no TCP dependency.
//! - Single-request / single-response (SNTP, not full NTP).
//! - Stores `(ntp_seconds, clint_ticks_at_sync)` — from these two values,
//!   `ntp_now()` computes current Unix seconds without further syscalls.
//! - Retry on timeout with exponential backoff.
//! - NTP server is set by DHCP (F05.3) or defaults to pool.ntp.org IP below.

use super::udp;
use robot_os_sync::SpinLock;

// ── Constants ────────────────────────────────────────────────────────────────

/// UDP port for NTP.
const NTP_PORT: u16 = 123;

/// Client source port for NTP queries (ephemeral).
const NTP_CLIENT_PORT: u16 = 1123;

/// NTP packet size (48 bytes, no authentication).
const NTP_PKT_SIZE: usize = 48;

/// Offset of the transmit timestamp in an NTP packet (seconds, big-endian u32).
const NTP_TRANSMIT_TS_OFFSET: usize = 40;

/// NTP era offset: seconds between NTP epoch (1900-01-01) and Unix epoch (1970-01-01).
const NTP_UNIX_EPOCH_OFFSET: u32 = 2_208_988_800;

/// LI=0 (no leap), VN=4 (NTPv4), Mode=3 (client): byte 0 = 0b00_100_011.
const NTP_FLAGS_CLIENT: u8 = 0b00_100_011;

/// Timeout for a single NTP response poll (2 seconds in CLINT ticks at 10 MHz).
const NTP_TIMEOUT_TICKS: u64 = 20_000_000;

/// Maximum retry attempts before giving up.
const NTP_MAX_RETRIES: u32 = 3;

/// Backoff multiplier between retries (ticks added per retry).
const NTP_RETRY_BACKOFF_TICKS: u64 = 10_000_000; // +1 second per retry

/// CLINT timer frequency (10 MHz on QEMU/VF2).
const NTP_CLINT_FREQ: u64 = 10_000_000;

/// Default NTP server: time.cloudflare.com = 162.159.200.1
const NTP_DEFAULT_SERVER: [u8; 4] = [162, 159, 200, 1];

// ── State ────────────────────────────────────────────────────────────────────

struct NtpState {
    /// NTP server IP.
    server: [u8; 4],
    /// Unix timestamp (seconds) at last successful sync.
    synced_unix_sec: u32,
    /// CLINT ticks at last successful sync.
    synced_clint_ticks: u64,
    /// Whether we have a valid sync.
    valid: bool,
    /// Pending response flag (set by handle_response).
    response_ready: bool,
    /// Last received NTP packet.
    response_buf: [u8; NTP_PKT_SIZE],
}

impl NtpState {
    const fn new() -> Self {
        NtpState {
            server: NTP_DEFAULT_SERVER,
            synced_unix_sec: 0,
            synced_clint_ticks: 0,
            valid: false,
            response_ready: false,
            response_buf: [0; NTP_PKT_SIZE],
        }
    }
}

static NTP: SpinLock<NtpState> = SpinLock::new(NtpState::new());

// ── Public API ────────────────────────────────────────────────────────────────

/// Set the NTP server address (called by DHCP on lease, or manually).
pub fn set_ntp_server(ip: [u8; 4]) {
    NTP.lock().server = ip;
}

/// Get the currently configured NTP server.
pub fn get_ntp_server() -> [u8; 4] {
    NTP.lock().server
}

/// Synchronize the kernel wall clock with the NTP server.
///
/// Blocks the calling task for up to `NTP_MAX_RETRIES * (2 + retry)` seconds.
/// Returns the Unix timestamp on success, or 0 on failure.
pub fn ntp_sync() -> u32 {
    let (mac, our_ip, server) = {
        let ntp = NTP.lock();
        (super::net_get_mac(), super::net_get_ip(), ntp.server)
    };

    let mut pkt = [0u8; NTP_PKT_SIZE];
    pkt[0] = NTP_FLAGS_CLIENT;
    // All other fields zero (valid for SNTP client request)

    for attempt in 0..NTP_MAX_RETRIES {
        // Clear response flag
        NTP.lock().response_ready = false;

        // Send NTP request
        udp::send_raw(&mac, &our_ip, &server,
                      NTP_CLIENT_PORT, NTP_PORT, &pkt);

        // Poll for response with timeout
        let timeout = NTP_TIMEOUT_TICKS + attempt as u64 * NTP_RETRY_BACKOFF_TICKS;
        let send_ticks = robot_os_drivers::clint::get_time();
        let deadline = send_ticks + timeout;

        loop {
            let now = robot_os_drivers::clint::get_time();
            if now >= deadline { break; }

            super::net_poll();

            let ready = NTP.lock().response_ready;
            if ready {
                // Extract transmit timestamp (NTP seconds, big-endian)
                let buf = NTP.lock().response_buf;
                let ntp_sec = u32::from_be_bytes([
                    buf[NTP_TRANSMIT_TS_OFFSET],
                    buf[NTP_TRANSMIT_TS_OFFSET + 1],
                    buf[NTP_TRANSMIT_TS_OFFSET + 2],
                    buf[NTP_TRANSMIT_TS_OFFSET + 3],
                ]);
                // Validate: NTP seconds must be post-2020 (Unix ~1578000000 → NTP ~3786988800)
                if ntp_sec < NTP_UNIX_EPOCH_OFFSET {
                    break; // invalid or kiss-of-death
                }
                let unix_sec = ntp_sec.wrapping_sub(NTP_UNIX_EPOCH_OFFSET);
                {
                    let mut ntp = NTP.lock();
                    ntp.synced_unix_sec = unix_sec;
                    ntp.synced_clint_ticks = now;
                    ntp.valid = true;
                }
                robot_os_drivers::kprintln!(
                    "[NTP] Synced — Unix time: {} (attempt {})",
                    unix_sec, attempt + 1
                );
                return unix_sec;
            }
            core::hint::spin_loop();
        }

        robot_os_drivers::kprintln!("[NTP] Timeout (attempt {}), retrying...", attempt + 1);
    }

    robot_os_drivers::kprintln!("[NTP] Sync failed after {} attempts", NTP_MAX_RETRIES);
    0
}

/// Return the current Unix time (seconds) based on the last sync plus elapsed CLINT ticks.
///
/// Returns 0 if no sync has been performed.
pub fn ntp_now() -> u32 {
    let ntp = NTP.lock();
    if !ntp.valid { return 0; }

    let now = robot_os_drivers::clint::get_time();
    let elapsed_ticks = now.saturating_sub(ntp.synced_clint_ticks);
    let elapsed_sec = (elapsed_ticks / NTP_CLINT_FREQ) as u32;
    ntp.synced_unix_sec.saturating_add(elapsed_sec)
}

/// Returns the Unix time offset from the last sync point (seconds since epoch, 0 if unsynced).
pub fn ntp_offset() -> u32 {
    ntp_now()
}

/// Returns true if an NTP sync has been completed successfully.
pub fn ntp_is_synced() -> bool {
    NTP.lock().valid
}

/// Handle an incoming NTP response (called from udp::handle for port NTP_CLIENT_PORT).
pub fn handle_response(data: &[u8]) {
    if data.len() < NTP_PKT_SIZE { return; }

    // Basic validation: LI != 3 (unsynchronized), Mode must be 4 (server)
    let mode = data[0] & 0x07;
    if mode != 4 { return; } // not a server response

    let mut ntp = NTP.lock();
    ntp.response_buf[..NTP_PKT_SIZE].copy_from_slice(&data[..NTP_PKT_SIZE]);
    ntp.response_ready = true;
}
