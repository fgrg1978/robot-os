/// CAN bus driver — simulation only (no real CAN hardware on QEMU/VF2/K1).
///
/// Provides a simulated CAN interface with a 16-frame ring buffer for
/// testing robot CAN-based actuator and sensor protocols.


use robot_os_sync::SpinLock;

/// Standard CAN 2.0 frame.
#[derive(Clone, Copy)]
pub struct CanFrame {
    pub id:   u32,
    pub dlc:  u8,
    pub data: [u8; 8],
}

impl CanFrame {
    pub const fn new() -> Self {
        CanFrame { id: 0, dlc: 0, data: [0u8; 8] }
    }
}

const CAN_RX_CAP: usize = 16;

struct CanRxBuf {
    buf:   [CanFrame; CAN_RX_CAP],
    head:  usize,
    tail:  usize,
    count: usize,
    tx_count: u64,
    rx_count: u64,
}

impl CanRxBuf {
    const fn new() -> Self {
        CanRxBuf {
            buf:   [CanFrame::new(); CAN_RX_CAP],
            head:  0,
            tail:  0,
            count: 0,
            tx_count: 0,
            rx_count: 0,
        }
    }

    fn push(&mut self, frame: &CanFrame) -> bool {
        if self.count >= CAN_RX_CAP { return false; }
        self.buf[self.head] = *frame;
        self.head = (self.head + 1) % CAN_RX_CAP;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<CanFrame> {
        if self.count == 0 { return None; }
        let frame = self.buf[self.tail];
        self.tail = (self.tail + 1) % CAN_RX_CAP;
        self.count -= 1;
        Some(frame)
    }
}

static CAN_RX: SpinLock<CanRxBuf> = SpinLock::new(CanRxBuf::new());

/// Initialise the CAN bus (no-op — no real hardware).
pub fn can_init() {
    crate::kprintln!("[CAN] Initialized (simulated, no hardware)");
}

/// Send a CAN frame.  In simulation, the frame is printed via kprintln.
pub fn can_send(frame: &CanFrame) -> i32 {
    let mut state = CAN_RX.lock();
    state.tx_count += 1;
    crate::kprintln!("[CAN] TX id={:#05x} dlc={} data=[{:#04x},{:#04x},{:#04x},{:#04x},{:#04x},{:#04x},{:#04x},{:#04x}]",
        frame.id, frame.dlc,
        frame.data[0], frame.data[1], frame.data[2], frame.data[3],
        frame.data[4], frame.data[5], frame.data[6], frame.data[7]);
    0
}

/// Receive a CAN frame from the ring buffer (returns None if empty).
pub fn can_recv() -> Option<CanFrame> {
    let mut state = CAN_RX.lock();
    let frame = state.pop();
    if frame.is_some() { state.rx_count += 1; }
    frame
}

/// Inject a CAN frame into the RX ring buffer (for testing from shell).
pub fn can_inject(frame: &CanFrame) {
    let mut state = CAN_RX.lock();
    if !state.push(frame) {
        crate::kprintln!("[CAN] RX buffer full, frame dropped");
    }
}

/// Print CAN bus status.
pub fn can_info() {
    let state = CAN_RX.lock();
    crate::kprintln!("[CAN] Simulated CAN bus (no hardware)");
    crate::kprintln!("[CAN]   RX buffer: {}/{} frames", state.count, CAN_RX_CAP);
    crate::kprintln!("[CAN]   TX count:  {}", state.tx_count);
    crate::kprintln!("[CAN]   RX count:  {}", state.rx_count);
}
