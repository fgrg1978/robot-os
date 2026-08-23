/// UART driver.
///
/// NS16550A for QEMU virt / VisionFive 2 / SpacemiT K1.
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
///
/// K-A16: IRQ-safe by construction (mirrors `CpuLockGuard` in
/// `crates/sched/src/scheduler.rs`): disables `sstatus.SIE` for the duration
/// the lock is held and restores the previous interrupt state on drop.
/// `acquire()` used to be a plain spin — a tick firing on the same hart
/// while a task held it (e.g. mid-`kprintln!`) could enter the timer ISR,
/// which itself prints (WCET probes, panic/fault paths), and spin forever
/// on a lock only the preempted holder could release — a same-hart
/// deadlock. Also applied to `try_acquire()`'s successful case: it never
/// spins so it was never *itself* deadlock-prone, but without this a tick
/// firing while a trap/panic handler is mid-print through its guard could
/// still nest into another UART caller — keeping both entry points on the
/// same IRQ-safe discipline is what makes it safe to add a new caller later
/// without re-opening this hazard.
pub struct UartGuard {
    prev_sstatus: usize,
}

impl Drop for UartGuard {
    fn drop(&mut self) {
        if SMP_ACTIVE.load(Ordering::Relaxed) {
            UART_LOCK.store(false, Ordering::Release);
        }
        let current = robot_os_arch::csr::read_sstatus();
        let restored = (current & !robot_os_arch::csr::SSTATUS_SIE)
            | (self.prev_sstatus & robot_os_arch::csr::SSTATUS_SIE);
        robot_os_arch::csr::write_sstatus(restored);
    }
}

/// Acquire exclusive access to the UART.
pub fn acquire() -> UartGuard {
    let prev_sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(prev_sstatus & !robot_os_arch::csr::SSTATUS_SIE);
    if SMP_ACTIVE.load(Ordering::Relaxed) {
        while UART_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }
    UartGuard { prev_sstatus }
}

/// Try to acquire the UART lock without blocking.
/// Returns `None` if the lock is already held (e.g. called from ISR context).
pub fn try_acquire() -> Option<UartGuard> {
    let prev_sstatus = robot_os_arch::csr::read_sstatus();
    robot_os_arch::csr::write_sstatus(prev_sstatus & !robot_os_arch::csr::SSTATUS_SIE);
    if SMP_ACTIVE.load(Ordering::Relaxed) {
        if UART_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Restore interrupts before reporting failure — no guard will be
            // returned to do it for us.
            let current = robot_os_arch::csr::read_sstatus();
            let restored = (current & !robot_os_arch::csr::SSTATUS_SIE)
                | (prev_sstatus & robot_os_arch::csr::SSTATUS_SIE);
            robot_os_arch::csr::write_sstatus(restored);
            return None;
        }
    }
    Some(UartGuard { prev_sstatus })
}

/// Enable the SMP UART lock.
pub fn enable_smp_lock() {
    SMP_ACTIVE.store(true, Ordering::SeqCst);
}

// ============================================================
// NS16550A UART (QEMU / VF2 / K1)
// ============================================================

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

    /// Size of the 16550A transmit FIFO, enabled in [`init`] via
    /// `FCR_ENABLE_FIFO`.
    const TX_FIFO_DEPTH: usize = 16;

    /// Write a whole slice, polling the line-status register once per FIFO
    /// load instead of once per byte.
    ///
    /// `LSR_THR_EMPTY` with the FIFO on means "the transmit FIFO can accept
    /// data", not "one byte fits" — so after a single successful poll it is
    /// safe to push up to [`TX_FIFO_DEPTH`] bytes. Doing it a byte at a time
    /// costs one MMIO read per byte for no reason, and MMIO is exactly what
    /// is expensive here: under QEMU TCG every access traps to the device
    /// model, and on real hardware the poll spins until the shift register
    /// drains at the line rate.
    ///
    /// Measured from ring 3 before this existed (`userspace/latbench`):
    /// `write(fd, 64 bytes)` cost 241 us against a 2.3 us syscall floor —
    /// about 3.7 us per byte, all of it MMIO polling. At 115200 baud on real
    /// hardware the same line is ~5.6 ms, against reflex's 25 ms control
    /// period.
    ///
    /// This does not make console output asynchronous — a full FIFO still
    /// blocks the caller. Fixing that properly means a TX ring drained by the
    /// THR-empty interrupt, which trades away the guarantee that a panic
    /// message reaches the wire before the board resets.
    pub fn write_bytes(bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            while !can_write() {}
            let n = (bytes.len() - i).min(TX_FIFO_DEPTH);
            for &b in &bytes[i..i + n] {
                write_reg(REG_THR, b);
            }
            i += n;
        }
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
// Public API (dispatches to platform module)
// ============================================================

/// UART IRQ number on QEMU virt machine (PLIC external interrupt 10).
pub const UART_IRQ: u32 = 10;

/// Initialize the UART hardware.
pub fn init() {
    ns16550a::init();
}

/// Returns true if the transmitter is ready.
#[inline]
pub fn can_write() -> bool {
    ns16550a::can_write()
}

/// Returns true if there is data ready to read.
#[inline]
pub fn can_read() -> bool {
    if IRQ_MODE.load(Ordering::Relaxed) {
        RX_TAIL.load(Ordering::Relaxed) != RX_HEAD.load(Ordering::Acquire)
    } else {
        ns16550a::can_read_hw()
    }
}

/// Write a single byte to the UART (blocking).
pub fn putc(c: u8) {
    ns16550a::putc_raw(c);
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
        ns16550a::getc_raw()
    }
}

/// Write a string to the UART.
pub fn puts(s: &str) {
    write_str_translated(s.as_bytes());
}

/// Write bytes to the UART, expanding `\n` to `\r\n`, using the FIFO-aware
/// batched path. Splits the input at newlines so the CRLF translation that
/// [`putc`] does per byte is preserved without paying a line-status poll per
/// byte.
pub fn write_str_translated(bytes: &[u8]) {
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            ns16550a::write_bytes(&bytes[start..i]);
            ns16550a::write_bytes(b"\n\r");
            start = i + 1;
        }
    }
    if start < bytes.len() {
        ns16550a::write_bytes(&bytes[start..]);
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
    ns16550a::enable_irq();
}

/// UART IRQ handler — called from the PLIC external interrupt path.
pub fn irq_handler() {
    ns16550a::irq_handler();
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
