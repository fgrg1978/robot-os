/// UART driver.
///
/// NS16550A for QEMU virt / VisionFive 2 / SpacemiT K1.
/// ESP32-C3 UART0 for ESP32-C3.
///
/// Phase 5: Added SMP-safe spinlock via AtomicBool.
/// Before `enable_smp_lock()` is called, no spinning occurs (safe for early boot).
///
/// Ported from kernel/drivers/uart.c
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::platform::hw::UART_BASE;

// ---- IRQ ring buffer (shared across all platforms) ----

/// Ring buffer capacity for IRQ-driven UART RX.
const RX_BUF_CAP: usize = 256;

/// Static ring buffer for interrupt-driven character reception.
static mut RX_BUF: [u8; RX_BUF_CAP] = [0u8; RX_BUF_CAP];
static RX_HEAD: AtomicUsize = AtomicUsize::new(0);
static RX_TAIL: AtomicUsize = AtomicUsize::new(0);
/// Set to true after `uart_enable_irq()` — changes `can_read()`/`getc()` behavior.
static IRQ_MODE: AtomicBool = AtomicBool::new(false);

// ---- SMP lock (shared across all platforms) ----

/// Global UART spinlock — only active after `enable_smp_lock()` is called.
static UART_LOCK: AtomicBool = AtomicBool::new(false);
/// Set to true when secondary CPUs are active to enable the spinlock.
static SMP_ACTIVE: AtomicBool = AtomicBool::new(false);

/// RAII guard that releases the UART lock on drop.
pub struct UartGuard;

impl Drop for UartGuard {
    fn drop(&mut self) {
        if SMP_ACTIVE.load(Ordering::Relaxed) {
            UART_LOCK.store(false, Ordering::Release);
        }
    }
}

/// Acquire exclusive access to the UART.
pub fn acquire() -> UartGuard {
    if SMP_ACTIVE.load(Ordering::Relaxed) {
        while UART_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }
    UartGuard
}

/// Try to acquire the UART lock without blocking.
/// Returns `None` if the lock is already held (e.g. called from ISR context).
pub fn try_acquire() -> Option<UartGuard> {
    if SMP_ACTIVE.load(Ordering::Relaxed) {
        if UART_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
    }
    Some(UartGuard)
}

/// Enable the SMP UART lock.
pub fn enable_smp_lock() {
    SMP_ACTIVE.store(true, Ordering::SeqCst);
}

// ============================================================
// NS16550A UART (QEMU / VF2 / K1)
// ============================================================

#[cfg(not(feature = "esp32c3"))]
mod ns16550a {
    use super::*;

    // UART reference clock and baud-rate divisor.
    #[cfg(not(any(feature = "vf2", feature = "k1")))]
    const UART_DIVISOR: u8 = 3;   // QEMU only
    #[cfg(feature = "vf2")]
    const UART_DIVISOR: u8 = 13;  // VF2 JH7110: ~115200 baud

    // Register offsets
    const REG_THR: usize = 0;
    const REG_RBR: usize = 0;
    const REG_IER: usize = 1;
    const REG_FCR: usize = 2;
    const REG_LCR: usize = 3;
    const REG_MCR: usize = 4;
    const REG_LSR: usize = 5;

    const LSR_DATA_READY: u8 = 1 << 0;
    const LSR_THR_EMPTY: u8 = 1 << 5;

    const LCR_8BITS: u8 = 0x03;
    #[cfg(not(feature = "k1"))]
    const LCR_DLAB: u8 = 1 << 7;

    const FCR_ENABLE_FIFO: u8 = 0x01;
    const FCR_CLEAR_RX: u8 = 0x02;
    const FCR_CLEAR_TX: u8 = 0x04;

    const IER_RX_AVAIL: u8 = 1 << 0;

    #[inline(always)]
    fn read_reg(reg: usize) -> u8 {
        unsafe { core::ptr::read_volatile((UART_BASE + reg) as *const u8) }
    }

    #[inline(always)]
    fn write_reg(reg: usize, val: u8) {
        unsafe { core::ptr::write_volatile((UART_BASE + reg) as *mut u8, val) }
    }

    pub fn init() {
        write_reg(REG_IER, 0x00);

        #[cfg(not(feature = "k1"))]
        {
            write_reg(REG_LCR, LCR_DLAB);
            write_reg(REG_RBR, UART_DIVISOR);
            write_reg(REG_IER, 0x00);
        }

        write_reg(REG_LCR, LCR_8BITS);
        write_reg(REG_FCR, FCR_ENABLE_FIFO | FCR_CLEAR_RX | FCR_CLEAR_TX);
        write_reg(REG_MCR, 0x03);
    }

    #[inline]
    pub fn can_write() -> bool {
        read_reg(REG_LSR) & LSR_THR_EMPTY != 0
    }

    #[inline]
    pub fn can_read_hw() -> bool {
        read_reg(REG_LSR) & LSR_DATA_READY != 0
    }

    pub fn putc_raw(c: u8) {
        while !can_write() {}
        write_reg(REG_THR, c);
    }

    pub fn getc_raw() -> u8 {
        while !can_read_hw() {}
        read_reg(REG_RBR)
    }

    pub fn enable_irq() {
        IRQ_MODE.store(true, Ordering::Release);
        let ier = read_reg(REG_IER);
        write_reg(REG_IER, ier | IER_RX_AVAIL);
    }

    pub fn irq_handler() {
        while read_reg(REG_LSR) & LSR_DATA_READY != 0 {
            let c = read_reg(REG_RBR);
            let head = RX_HEAD.load(Ordering::Relaxed);
            let next = (head + 1) % RX_BUF_CAP;
            if next != RX_TAIL.load(Ordering::Acquire) {
                unsafe { RX_BUF[head] = c; }
                RX_HEAD.store(next, Ordering::Release);
            }
        }
    }
}

// ============================================================
// ESP32-C3 UART0
// ============================================================

#[cfg(feature = "esp32c3")]
mod esp_uart {
    use super::*;

    // ESP32-C3 UART register offsets (from UART_BASE = 0x6000_0000)
    const FIFO_REG: usize = 0x00;
    const STATUS_REG: usize = 0x1C;
    // TXFIFO_CNT in bits [23:16], RXFIFO_CNT in bits [7:0]

    #[inline(always)]
    fn read32(off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((UART_BASE + off) as *const u32) }
    }

    #[inline(always)]
    fn write32(off: usize, val: u32) {
        unsafe { core::ptr::write_volatile((UART_BASE + off) as *mut u32, val) }
    }

    pub fn init() {
        // ROM bootloader already configured 115200 baud — nothing to do.
    }

    #[inline]
    pub fn can_write() -> bool {
        (read32(STATUS_REG) >> 16) & 0xFF < 128
    }

    #[inline]
    pub fn can_read_hw() -> bool {
        read32(STATUS_REG) & 0xFF > 0
    }

    pub fn putc_raw(c: u8) {
        while !can_write() {}
        write32(FIFO_REG, c as u32);
    }

    pub fn getc_raw() -> u8 {
        while !can_read_hw() {}
        (read32(FIFO_REG) & 0xFF) as u8
    }

    pub fn enable_irq() {
        // No PLIC on ESP32-C3 — IRQ mode not supported (polling only)
    }

    pub fn irq_handler() {
        // No-op: no PLIC-driven UART IRQ on ESP32-C3
    }
}

// ============================================================
// Public API (dispatches to platform module)
// ============================================================

/// UART IRQ number on QEMU virt machine (PLIC external interrupt 10).
pub const UART_IRQ: u32 = 10;

/// Initialize the UART hardware.
pub fn init() {
    #[cfg(not(feature = "esp32c3"))]
    ns16550a::init();
    #[cfg(feature = "esp32c3")]
    esp_uart::init();
}

/// Returns true if the transmitter is ready.
#[inline]
pub fn can_write() -> bool {
    #[cfg(not(feature = "esp32c3"))]
    { ns16550a::can_write() }
    #[cfg(feature = "esp32c3")]
    { esp_uart::can_write() }
}

/// Returns true if there is data ready to read.
#[inline]
pub fn can_read() -> bool {
    if IRQ_MODE.load(Ordering::Relaxed) {
        RX_TAIL.load(Ordering::Relaxed) != RX_HEAD.load(Ordering::Acquire)
    } else {
        #[cfg(not(feature = "esp32c3"))]
        { ns16550a::can_read_hw() }
        #[cfg(feature = "esp32c3")]
        { esp_uart::can_read_hw() }
    }
}

/// Write a single byte to the UART (blocking).
pub fn putc(c: u8) {
    #[cfg(not(feature = "esp32c3"))]
    ns16550a::putc_raw(c);
    #[cfg(feature = "esp32c3")]
    esp_uart::putc_raw(c);
    if c == b'\n' {
        putc(b'\r');
    }
}

/// Read a single byte from the UART (blocking).
pub fn getc() -> u8 {
    if IRQ_MODE.load(Ordering::Relaxed) {
        loop {
            if let Some(c) = try_getc() { return c; }
            core::hint::spin_loop();
        }
    } else {
        #[cfg(not(feature = "esp32c3"))]
        { ns16550a::getc_raw() }
        #[cfg(feature = "esp32c3")]
        { esp_uart::getc_raw() }
    }
}

/// Write a string to the UART.
pub fn puts(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}

/// A zero-size writer that implements `core::fmt::Write`.
pub struct Uart;

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        puts(s);
        Ok(())
    }
}

/// Print formatted output to UART (SMP-safe).
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _guard = $crate::uart::acquire();
        let _ = write!($crate::Uart, $($arg)*);
    }};
}

/// Print formatted output to UART with newline (SMP-safe).
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _guard = $crate::uart::acquire();
        let _ = writeln!($crate::Uart, $($arg)*);
    }};
}

// ---- IRQ-driven RX ----

/// Enable UART RX interrupt.
pub fn enable_irq() {
    #[cfg(not(feature = "esp32c3"))]
    ns16550a::enable_irq();
    #[cfg(feature = "esp32c3")]
    esp_uart::enable_irq();
}

/// UART IRQ handler — called from the PLIC external interrupt path.
pub fn irq_handler() {
    #[cfg(not(feature = "esp32c3"))]
    ns16550a::irq_handler();
    #[cfg(feature = "esp32c3")]
    esp_uart::irq_handler();
}

/// Returns the number of characters available in the RX ring buffer.
pub fn rx_available() -> usize {
    let head = RX_HEAD.load(Ordering::Acquire);
    let tail = RX_TAIL.load(Ordering::Relaxed);
    (head + RX_BUF_CAP - tail) % RX_BUF_CAP
}

/// Read one character from the RX ring buffer (non-blocking).
pub fn try_getc() -> Option<u8> {
    let tail = RX_TAIL.load(Ordering::Relaxed);
    if tail == RX_HEAD.load(Ordering::Acquire) {
        return None;
    }
    let c = unsafe { RX_BUF[tail] };
    RX_TAIL.store((tail + 1) % RX_BUF_CAP, Ordering::Release);
    Some(c)
}
