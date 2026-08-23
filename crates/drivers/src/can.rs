/// CAN bus driver (F25) — MCP2515 (SPI) + simulated loopback.
///
/// Standard CAN 2.0B (11-bit and 29-bit IDs), ID filtering, ring buffers.
/// VF2/K1: MCP2515 via SPI (skeleton, register map defined).
/// QEMU: loopback mode (TX → RX if filter matches).

use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum CAN data payload bytes (CAN 2.0).
pub const CAN_MAX_DLC: usize = 8;

/// RX ring buffer capacity (frames).
const CAN_RX_CAP: usize = 16;

/// Maximum number of ID filters.
const CAN_MAX_FILTERS: usize = 8;

/// Standard CAN ID mask (11 bits).
const CAN_STD_ID_MASK: u32 = 0x7FF;

/// Extended CAN ID mask (29 bits).
const CAN_EXT_ID_MASK: u32 = 0x1FFF_FFFF;

/// Default bitrate: 500 kbps (automotive/industrial standard).
pub const CAN_DEFAULT_BITRATE: u32 = 500_000;

// ---------------------------------------------------------------------------
// MCP2515 Register Map (SPI-attached CAN controller)
// ---------------------------------------------------------------------------

/// MCP2515 SPI instructions.
#[cfg(any(feature = "vf2", feature = "k1"))]
mod mcp2515 {
    pub const CMD_RESET:       u8 = 0xC0;
    pub const CMD_READ:        u8 = 0x03;
    pub const CMD_WRITE:       u8 = 0x02;
    pub const CMD_BIT_MODIFY:  u8 = 0x05;
    pub const CMD_READ_STATUS: u8 = 0xA0;
    pub const CMD_LOAD_TX0:    u8 = 0x40;
    pub const CMD_RTS_TX0:     u8 = 0x81;
    pub const CMD_READ_RX0:    u8 = 0x90;

    pub const REG_CANCTRL:  u8 = 0x0F;
    pub const REG_CANSTAT:  u8 = 0x0E;
    pub const REG_CNF1:     u8 = 0x2A;
    pub const REG_CNF2:     u8 = 0x29;
    pub const REG_CNF3:     u8 = 0x28;
    pub const REG_CANINTE:  u8 = 0x2B;
    pub const REG_CANINTF:  u8 = 0x2C;

    pub const MODE_CONFIG:   u8 = 0x80;
    pub const MODE_NORMAL:   u8 = 0x00;
    pub const MODE_LOOPBACK: u8 = 0x40;

    pub const INT_RX0: u8 = 0x01;
    pub const INT_TX0: u8 = 0x04;
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// CAN 2.0B frame (standard or extended).
#[derive(Clone, Copy)]
pub struct CanFrame {
    /// Arbitration ID (11-bit standard or 29-bit extended).
    pub id: u32,
    /// Data length code (0-8).
    pub dlc: u8,
    /// Payload data.
    pub data: [u8; CAN_MAX_DLC],
    /// Extended frame flag (29-bit ID).
    pub extended: bool,
    /// Remote Transmission Request.
    pub rtr: bool,
}

impl CanFrame {
    pub const fn new() -> Self {
        CanFrame { id: 0, dlc: 0, data: [0u8; CAN_MAX_DLC], extended: false, rtr: false }
    }

    /// Create a standard frame (11-bit ID).
    pub fn standard(id: u32, data: &[u8]) -> Self {
        let mut f = Self::new();
        f.id = id & CAN_STD_ID_MASK;
        f.dlc = data.len().min(CAN_MAX_DLC) as u8;
        f.data[..f.dlc as usize].copy_from_slice(&data[..f.dlc as usize]);
        f
    }

    /// Create an extended frame (29-bit ID).
    pub fn extended_frame(id: u32, data: &[u8]) -> Self {
        let mut f = Self::standard(id & CAN_EXT_ID_MASK, data);
        f.extended = true;
        f
    }
}

/// CAN ID filter.
#[derive(Clone, Copy)]
struct CanFilter {
    id: u32,
    mask: u32,
    active: bool,
}

impl CanFilter {
    const fn empty() -> Self { Self { id: 0, mask: 0, active: false } }
}

/// CAN bus state.
#[derive(Clone, Copy, PartialEq)]
pub enum CanState {
    Uninit,
    Active,
    BusOff,
}

// ---------------------------------------------------------------------------
// Driver state
// ---------------------------------------------------------------------------

struct CanDriver {
    state: CanState,
    bitrate: u32,
    rx_buf: [CanFrame; CAN_RX_CAP],
    rx_head: usize,
    rx_tail: usize,
    rx_count: u32,
    tx_count: u32,
    err_count: u32,
    filters: [CanFilter; CAN_MAX_FILTERS],
    filter_count: u8,
}

impl CanDriver {
    const fn new() -> Self {
        Self {
            state: CanState::Uninit,
            bitrate: CAN_DEFAULT_BITRATE,
            rx_buf: [CanFrame::new(); CAN_RX_CAP],
            rx_head: 0,
            rx_tail: 0,
            rx_count: 0,
            tx_count: 0,
            err_count: 0,
            filters: [CanFilter::empty(); CAN_MAX_FILTERS],
            filter_count: 0,
        }
    }

    fn rx_push(&mut self, frame: CanFrame) -> bool {
        let next = (self.rx_head + 1) % CAN_RX_CAP;
        if next == self.rx_tail { return false; } // full
        self.rx_buf[self.rx_head] = frame;
        self.rx_head = next;
        true
    }

    fn rx_pop(&mut self) -> Option<CanFrame> {
        if self.rx_head == self.rx_tail { return None; }
        let frame = self.rx_buf[self.rx_tail];
        self.rx_tail = (self.rx_tail + 1) % CAN_RX_CAP;
        Some(frame)
    }

    fn rx_available(&self) -> usize {
        if self.rx_head >= self.rx_tail {
            self.rx_head - self.rx_tail
        } else {
            CAN_RX_CAP - self.rx_tail + self.rx_head
        }
    }

    fn matches_filter(&self, frame: &CanFrame) -> bool {
        if self.filter_count == 0 { return true; } // no filters = accept all
        for i in 0..self.filter_count as usize {
            let f = &self.filters[i];
            if f.active && (frame.id & f.mask) == (f.id & f.mask) {
                return true;
            }
        }
        false
    }
}

static CAN: SpinLock<CanDriver> = SpinLock::new(CanDriver::new());

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialize the CAN controller.
pub fn can_init() {
    can_init_with_bitrate(CAN_DEFAULT_BITRATE);
}

/// Initialize with a specific bitrate.
pub fn can_init_with_bitrate(bitrate: u32) {
    let mut drv = CAN.lock();
    drv.bitrate = bitrate;

    #[cfg(any(feature = "vf2", feature = "k1"))]
    {
        // MCP2515: reset → config mode → set bitrate → normal mode
        // (SPI transactions — skeleton, needs SPI driver wiring)
        crate::kprintln!("[CAN] MCP2515 init at {} bps (SPI)", bitrate);
    }
    #[cfg(not(any(feature = "vf2", feature = "k1")))]
    {
        crate::kprintln!("[CAN] Init at {} bps (simulated loopback)", bitrate);
    }

    drv.state = CanState::Active;
}

/// Send a CAN frame. Returns 0 on success, -1 on error.
pub fn can_send(frame: &CanFrame) -> i32 {
    let mut drv = CAN.lock();
    if drv.state != CanState::Active { return -1; }

    #[cfg(any(feature = "vf2", feature = "k1"))]
    {
        // MCP2515: load TX buffer 0 → request to send
        let _ = frame;
    }
    #[cfg(not(any(feature = "vf2", feature = "k1")))]
    {
        // Loopback: TX appears in RX if filter matches
        if drv.matches_filter(frame) {
            drv.rx_push(*frame);
            drv.rx_count = drv.rx_count.wrapping_add(1);
        }
    }

    drv.tx_count = drv.tx_count.wrapping_add(1);
    0
}

/// Receive a CAN frame. Returns None if buffer empty.
pub fn can_recv() -> Option<CanFrame> {
    CAN.lock().rx_pop()
}

/// Inject a frame into RX buffer (for testing/shell).
pub fn can_inject(frame: &CanFrame) {
    let mut drv = CAN.lock();
    if !drv.rx_push(*frame) {
        crate::kprintln!("[CAN] RX buffer full, frame dropped");
    } else {
        drv.rx_count += 1;
    }
}

/// Add an ID filter. Returns true on success.
pub fn can_add_filter(id: u32, mask: u32) -> bool {
    let mut drv = CAN.lock();
    if drv.filter_count as usize >= CAN_MAX_FILTERS { return false; }
    let idx = drv.filter_count as usize;
    drv.filters[idx] = CanFilter { id, mask, active: true };
    drv.filter_count += 1;
    true
}

/// Clear all filters (accept all frames).
pub fn can_clear_filters() {
    let mut drv = CAN.lock();
    for f in drv.filters.iter_mut() { *f = CanFilter::empty(); }
    drv.filter_count = 0;
}

/// Get number of frames available to receive.
pub fn can_available() -> usize {
    CAN.lock().rx_available()
}

/// Get CAN bus state.
pub fn can_get_state() -> CanState {
    CAN.lock().state
}

/// Get statistics: (rx_count, tx_count, err_count).
pub fn can_stats() -> (u32, u32, u32) {
    let drv = CAN.lock();
    (drv.rx_count, drv.tx_count, drv.err_count)
}

/// Print CAN bus status.
pub fn can_info() {
    let drv = CAN.lock();
    let state_str = match drv.state {
        CanState::Uninit => "Uninitialized",
        CanState::Active => "Active",
        CanState::BusOff => "Bus-Off",
    };
    crate::kprintln!("[CAN] State: {} @ {} bps", state_str, drv.bitrate);
    crate::kprintln!("[CAN] RX: {} frames ({} buffered), TX: {}, ERR: {}",
                      drv.rx_count, drv.rx_available(), drv.tx_count, drv.err_count);
    crate::kprintln!("[CAN] Filters: {}/{}", drv.filter_count, CAN_MAX_FILTERS);
}

/// Poll for incoming frames (VF2/K1: check MCP2515 status via SPI).
pub fn can_poll() {
    #[cfg(any(feature = "vf2", feature = "k1"))]
    {
        // MCP2515: read CANINTF, if RX0 flag set → read frame → push to rx_buf
    }
}
