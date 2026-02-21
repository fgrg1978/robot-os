/// WiFi driver — ESP32-C3 companion support (Phase O1+O2).
///
/// The ESP32-C3 has built-in WiFi (802.11 b/g/n).
/// This driver provides a UDP-based link to the ground station server,
/// replacing the wired Ethernet path used on VF2/K1/QEMU.
///
/// On non-ESP32 targets, this module provides no-op stubs.
///
/// # Architecture
///
/// ```text
/// ESP32-C3                         Server
/// ┌────────────┐    WiFi/UDP     ┌─────────────┐
/// │ Flight ctrl│ ──────────────> │ Ground Stn  │
/// │ AHRS       │ <────────────── │ Perception  │
/// │ Safety     │                 │ SLAM        │
/// └────────────┘                 └─────────────┘
/// ```
///
/// The ESP32-C3 has only 400 KB RAM, so no ML, no MMU, no FAT32.
/// All heavy processing is delegated to the server over WiFi.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

// ── WiFi state ───────────────────────────────────────────────────────────────

/// WiFi connection status.
#[derive(Clone, Copy, PartialEq)]
pub enum WifiState {
    /// Not initialized.
    Off,
    /// Scanning / connecting.
    Connecting,
    /// Connected to AP, IP assigned.
    Connected,
    /// Connection lost.
    Disconnected,
}

static WIFI_STATE: AtomicU8 = AtomicU8::new(0); // 0=Off, 1=Connecting, 2=Connected, 3=Disconnected
static WIFI_READY: AtomicBool = AtomicBool::new(false);
#[allow(dead_code)]
static WIFI_TX_PACKETS: AtomicU32 = AtomicU32::new(0);
#[allow(dead_code)]
static WIFI_RX_PACKETS: AtomicU32 = AtomicU32::new(0);

// SSID stored as fixed bytes (max 32).
#[allow(dead_code)]
static mut WIFI_SSID: [u8; 32] = [0u8; 32];
#[allow(dead_code)]
static mut WIFI_SSID_LEN: u8 = 0;

fn state_to_u8(s: WifiState) -> u8 {
    match s {
        WifiState::Off => 0,
        WifiState::Connecting => 1,
        WifiState::Connected => 2,
        WifiState::Disconnected => 3,
    }
}

fn u8_to_state(v: u8) -> WifiState {
    match v {
        1 => WifiState::Connecting,
        2 => WifiState::Connected,
        3 => WifiState::Disconnected,
        _ => WifiState::Off,
    }
}

// ── Init ─────────────────────────────────────────────────────────────────────

/// Initialize WiFi subsystem.
///
/// On ESP32-C3: configures the WiFi peripheral, sets station mode.
/// On other targets: no-op (uses Ethernet instead).
pub fn wifi_init() {
    #[cfg(feature = "esp32c3")]
    {
        // TODO: ESP32-C3 WiFi peripheral init:
        // 1. Enable WiFi clock
        // 2. Configure PHY
        // 3. Set station mode
        // 4. Start scan/connect
        WIFI_STATE.store(state_to_u8(WifiState::Off), Ordering::Relaxed);
        crate::kprintln!("[WIFI] ESP32-C3 WiFi initialized (stub)");
    }
    #[cfg(not(feature = "esp32c3"))]
    {
        // No WiFi on non-ESP32 targets — use Ethernet.
    }
}

/// Connect to a WiFi access point.
///
/// `ssid`: network name (max 32 bytes).
/// `pass`: password (max 64 bytes).
///
/// On ESP32-C3: starts connection process (async, check `wifi_state()`).
/// On other targets: no-op.
pub fn wifi_connect(ssid: &[u8], _pass: &[u8]) {
    #[cfg(feature = "esp32c3")]
    {
        let len = if ssid.len() > 32 { 32 } else { ssid.len() };
        unsafe {
            WIFI_SSID[..len].copy_from_slice(&ssid[..len]);
            WIFI_SSID_LEN = len as u8;
        }
        WIFI_STATE.store(state_to_u8(WifiState::Connecting), Ordering::Release);
        crate::kprintln!("[WIFI] Connecting to AP...");

        // Simulated: immediately "connect" for testing.
        WIFI_STATE.store(state_to_u8(WifiState::Connected), Ordering::Release);
        WIFI_READY.store(true, Ordering::Release);
        crate::kprintln!("[WIFI] Connected (simulated)");
    }
    #[cfg(not(feature = "esp32c3"))]
    {
        let _ = ssid;
    }
}

/// Disconnect from WiFi.
pub fn wifi_disconnect() {
    WIFI_STATE.store(state_to_u8(WifiState::Disconnected), Ordering::Release);
    WIFI_READY.store(false, Ordering::Release);
}

/// Get current WiFi state.
pub fn wifi_state() -> WifiState {
    u8_to_state(WIFI_STATE.load(Ordering::Acquire))
}

/// Check if WiFi is connected and ready.
pub fn wifi_is_ready() -> bool {
    WIFI_READY.load(Ordering::Acquire)
}

// ── Send / Receive (UDP) ─────────────────────────────────────────────────────

/// Send a UDP packet over WiFi.
///
/// On ESP32-C3: uses the WiFi stack's UDP socket.
/// On other targets: no-op (returns 0).
pub fn wifi_send_udp(ip: &[u8; 4], port: u16, data: &[u8]) -> usize {
    if !WIFI_READY.load(Ordering::Acquire) { return 0; }

    #[cfg(feature = "esp32c3")]
    {
        // TODO: ESP32-C3 lwIP UDP sendto.
        // For now, simulate success.
        let _ = (ip, port);
        WIFI_TX_PACKETS.fetch_add(1, Ordering::Relaxed);
        data.len()
    }
    #[cfg(not(feature = "esp32c3"))]
    {
        let _ = (ip, port, data);
        0
    }
}

/// Receive a UDP packet over WiFi.
///
/// Returns number of bytes received, or 0 if nothing available.
pub fn wifi_recv_udp(buf: &mut [u8]) -> usize {
    if !WIFI_READY.load(Ordering::Acquire) { return 0; }

    #[cfg(feature = "esp32c3")]
    {
        // TODO: ESP32-C3 lwIP UDP recvfrom.
        let _ = buf;
        WIFI_RX_PACKETS.fetch_add(1, Ordering::Relaxed);
        0 // nothing available in stub
    }
    #[cfg(not(feature = "esp32c3"))]
    {
        let _ = buf;
        0
    }
}

// ── Info ─────────────────────────────────────────────────────────────────────

/// Print WiFi status.
pub fn wifi_info() {
    let state = wifi_state();
    let state_name = match state {
        WifiState::Off => "OFF",
        WifiState::Connecting => "CONNECTING",
        WifiState::Connected => "CONNECTED",
        WifiState::Disconnected => "DISCONNECTED",
    };
    crate::kprintln!("[WIFI] State: {}", state_name);

    #[cfg(feature = "esp32c3")]
    {
        let ssid_len = unsafe { WIFI_SSID_LEN } as usize;
        if ssid_len > 0 {
            let ssid = unsafe { &WIFI_SSID[..ssid_len] };
            crate::kprint!("[WIFI] SSID: ");
            for &b in ssid { crate::uart::putc(b); }
            crate::kprintln!();
        }
        crate::kprintln!("[WIFI] TX: {} packets  RX: {} packets",
            WIFI_TX_PACKETS.load(Ordering::Relaxed),
            WIFI_RX_PACKETS.load(Ordering::Relaxed));
    }
    #[cfg(not(feature = "esp32c3"))]
    crate::kprintln!("[WIFI] Not available (use Ethernet)");
}
