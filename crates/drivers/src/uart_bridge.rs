/// UART bridge driver — secondary UART for ESP32-C3 WiFi bridge.
///
/// VF2: NS16550A UART1 at 0x10010000, 115200 baud.
/// QEMU/K1/ESP32-C3: stubs (no bridge hardware).
///
/// Architecture:
///   VF2 ──UART1 (TX/RX/GND)──→ ESP32-C3 ──WiFi/TCP──→ macOS (brain server)
///
/// The VF2 sends/receives brain protocol packets (MAGIC "BR" framed) over
/// UART1.  The ESP32 firmware is a transparent byte relay between its UART
/// and a TCP socket to the brain server.

// ── QEMU / K1 / ESP32-C3: no bridge hardware ────────────────────────────────

#[cfg(not(feature = "vf2"))]
pub fn bridge_init() -> i32 { -1 }

#[cfg(not(feature = "vf2"))]
pub fn bridge_is_ready() -> bool { false }

#[cfg(not(feature = "vf2"))]
pub fn bridge_send(_data: &[u8]) -> i32 { -1 }

#[cfg(not(feature = "vf2"))]
pub fn bridge_recv(_buf: &mut [u8]) -> i32 { 0 }

#[cfg(not(feature = "vf2"))]
pub fn bridge_info() {
    crate::kprintln!("[BRIDGE] Not available (no VF2 UART1)");
}

// ── VF2: NS16550A UART1 for ESP32 bridge ─────────────────────────────────────

#[cfg(feature = "vf2")]
mod uart1 {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const BASE: usize = crate::platform::hw::UART1_BASE;

    // NS16550A register offsets
    const THR: usize = 0;
    const RBR: usize = 0;
    const IER: usize = 1;
    const FCR: usize = 2;
    const LCR: usize = 3;
    const MCR: usize = 4;
    const LSR: usize = 5;

    const LSR_DATA_READY: u8 = 1 << 0;
    const LSR_THR_EMPTY:  u8 = 1 << 5;
    const LCR_8BITS:      u8 = 0x03;
    const LCR_DLAB:       u8 = 1 << 7;
    const FCR_FIFO:       u8 = 0x07; // enable + clear RX + clear TX

    // JH7110 UART1 clock: same as UART0 → divisor 13 for 115200 baud.
    const DIVISOR: u8 = 13;

    static INIT_DONE: AtomicBool = AtomicBool::new(false);

    // RX ring buffer (polled, not IRQ-driven — bridge runs in its own task)
    const RX_CAP: usize = 512;
    static mut RX_BUF: [u8; RX_CAP] = [0u8; RX_CAP];
    static RX_HEAD: AtomicUsize = AtomicUsize::new(0);
    static RX_TAIL: AtomicUsize = AtomicUsize::new(0);

    #[inline(always)]
    fn rd(reg: usize) -> u8 {
        unsafe { core::ptr::read_volatile((BASE + reg) as *const u8) }
    }

    #[inline(always)]
    fn wr(reg: usize, val: u8) {
        unsafe { core::ptr::write_volatile((BASE + reg) as *mut u8, val) }
    }

    /// Initialize UART1 at 115200 baud, 8N1, FIFO enabled.
    pub fn bridge_init() -> i32 {
        wr(IER, 0x00);           // disable interrupts
        wr(LCR, LCR_DLAB);      // enable DLAB for divisor access
        wr(RBR, DIVISOR);       // divisor low
        wr(IER, 0x00);          // divisor high = 0
        wr(LCR, LCR_8BITS);     // 8N1, DLAB off
        wr(FCR, FCR_FIFO);      // enable + clear FIFOs
        wr(MCR, 0x03);          // DTR + RTS

        // Verify: read LSR to confirm UART is responsive
        let lsr = rd(LSR);
        if lsr == 0xFF {
            // No UART at this address (floating bus)
            crate::kprintln!("[BRIDGE] UART1 @ {:#010x} not responding", BASE);
            return -1;
        }

        INIT_DONE.store(true, Ordering::Release);
        crate::kprintln!("[BRIDGE] UART1 @ {:#010x} ready (115200 8N1)", BASE);
        0
    }

    pub fn bridge_is_ready() -> bool {
        INIT_DONE.load(Ordering::Acquire)
    }

    /// Drain hardware FIFO into software ring buffer (non-blocking).
    fn poll_rx() {
        while rd(LSR) & LSR_DATA_READY != 0 {
            let c = rd(RBR);
            let head = RX_HEAD.load(Ordering::Relaxed);
            let next = (head + 1) % RX_CAP;
            if next != RX_TAIL.load(Ordering::Acquire) {
                unsafe { RX_BUF[head] = c; }
                RX_HEAD.store(next, Ordering::Release);
            }
            // else: ring full — drop byte
        }
    }

    /// Send raw bytes over UART1 (blocking per-byte).
    pub fn bridge_send(data: &[u8]) -> i32 {
        if !INIT_DONE.load(Ordering::Acquire) { return -1; }
        for &b in data {
            while rd(LSR) & LSR_THR_EMPTY == 0 {
                core::hint::spin_loop();
            }
            wr(THR, b);
        }
        data.len() as i32
    }

    /// Receive bytes from UART1 into `buf` (non-blocking).
    /// Returns number of bytes read (0 if none available).
    pub fn bridge_recv(buf: &mut [u8]) -> i32 {
        if !INIT_DONE.load(Ordering::Acquire) { return 0; }

        // Drain hardware FIFO first
        poll_rx();

        let mut count = 0usize;
        while count < buf.len() {
            let tail = RX_TAIL.load(Ordering::Relaxed);
            if tail == RX_HEAD.load(Ordering::Acquire) {
                break; // ring empty
            }
            buf[count] = unsafe { RX_BUF[tail] };
            RX_TAIL.store((tail + 1) % RX_CAP, Ordering::Release);
            count += 1;
        }
        count as i32
    }

    pub fn bridge_info() {
        if !INIT_DONE.load(Ordering::Acquire) {
            crate::kprintln!("[BRIDGE] UART1 not initialized");
            return;
        }
        let lsr = rd(LSR);
        crate::kprintln!("[BRIDGE] UART1 @ {:#010x} (115200 8N1)", BASE);
        crate::kprintln!("[BRIDGE]   LSR={:#04x} (data_ready={}, thr_empty={})",
            lsr,
            if lsr & LSR_DATA_READY != 0 { "yes" } else { "no" },
            if lsr & LSR_THR_EMPTY  != 0 { "yes" } else { "no" });
        let head = RX_HEAD.load(Ordering::Relaxed);
        let tail = RX_TAIL.load(Ordering::Relaxed);
        let used = (head + RX_CAP - tail) % RX_CAP;
        crate::kprintln!("[BRIDGE]   RX buffer: {}/{} bytes", used, RX_CAP);
    }
}

#[cfg(feature = "vf2")]
pub use uart1::*;
