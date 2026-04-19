//! TS03 — driver mock framework.
//!
//! Mocks the hardware-facing surfaces of common drivers so that
//! higher-level logic (state machines, retry loops, parsing) can be
//! unit-tested on the host without QEMU or real hardware.
//!
//! ## Design
//!
//! Each driver exposes a trait. The kernel's "real" implementation is
//! one impl; the test fixture is another. Tests work against the trait,
//! never against MMIO directly.
//!
//! Currently mocked:
//! - `MockI2c`     (IMU MPU-6050, baro BMP280)
//! - `MockGpio`    (PIRs, encoder pulses)
//! - `MockUart`    (GPS NMEA, UART bridge)
//! - `MockClock`   (synthetic monotonic time for retries / timeouts)
//!
//! Add new mocks here as we add tests for the next driver.

#![allow(dead_code)]

use std::collections::VecDeque;

// ── I2C ──────────────────────────────────────────────────────────────────

pub trait I2c {
    fn read_reg (&mut self, addr: u8, reg: u8, out: &mut [u8]) -> Result<(), I2cError>;
    fn write_reg(&mut self, addr: u8, reg: u8, data: &[u8])    -> Result<(), I2cError>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum I2cError { NoAck, Bus, Timeout }

#[derive(Default)]
pub struct MockI2c {
    /// Programmable register table: (addr, reg) → bytes.
    pub regs: std::collections::HashMap<(u8, u8), Vec<u8>>,
    /// If true, simulate a NoAck on every transfer.
    pub bus_dead: bool,
    /// History for assertions in tests.
    pub log: Vec<I2cOp>,
}

#[derive(Debug, Clone)]
pub enum I2cOp {
    Read  { addr: u8, reg: u8, len: usize },
    Write { addr: u8, reg: u8, data: Vec<u8> },
}

impl I2c for MockI2c {
    fn read_reg(&mut self, addr: u8, reg: u8, out: &mut [u8]) -> Result<(), I2cError> {
        self.log.push(I2cOp::Read { addr, reg, len: out.len() });
        if self.bus_dead { return Err(I2cError::NoAck); }
        let key = (addr, reg);
        if let Some(v) = self.regs.get(&key) {
            let n = v.len().min(out.len());
            out[..n].copy_from_slice(&v[..n]);
            Ok(())
        } else {
            Err(I2cError::NoAck)
        }
    }

    fn write_reg(&mut self, addr: u8, reg: u8, data: &[u8]) -> Result<(), I2cError> {
        self.log.push(I2cOp::Write { addr, reg, data: data.to_vec() });
        if self.bus_dead { return Err(I2cError::NoAck); }
        self.regs.insert((addr, reg), data.to_vec());
        Ok(())
    }
}

// ── GPIO ─────────────────────────────────────────────────────────────────

pub trait Gpio {
    fn read(&self, pin: u8) -> bool;
    fn set (&mut self, pin: u8, level: bool);
    fn pulse_count(&self, pin: u8) -> u32;
}

#[derive(Default)]
pub struct MockGpio {
    pub levels:  std::collections::HashMap<u8, bool>,
    pub pulses:  std::collections::HashMap<u8, u32>,
}

impl Gpio for MockGpio {
    fn read(&self, pin: u8) -> bool { *self.levels.get(&pin).unwrap_or(&false) }
    fn set (&mut self, pin: u8, level: bool) { self.levels.insert(pin, level); }
    fn pulse_count(&self, pin: u8) -> u32 { *self.pulses.get(&pin).unwrap_or(&0) }
}

// ── UART ─────────────────────────────────────────────────────────────────

pub trait Uart {
    fn read (&mut self, out: &mut [u8]) -> usize;
    fn write(&mut self, data: &[u8]);
}

#[derive(Default)]
pub struct MockUart {
    pub rx_queue: VecDeque<u8>,
    pub tx_log:   Vec<u8>,
}

impl MockUart {
    pub fn push_rx(&mut self, data: &[u8]) { self.rx_queue.extend(data.iter().copied()); }
}

impl Uart for MockUart {
    fn read(&mut self, out: &mut [u8]) -> usize {
        let mut n = 0;
        while n < out.len() {
            if let Some(b) = self.rx_queue.pop_front() {
                out[n] = b; n += 1;
            } else { break; }
        }
        n
    }
    fn write(&mut self, data: &[u8]) { self.tx_log.extend_from_slice(data); }
}

// ── Clock ────────────────────────────────────────────────────────────────

pub trait Clock {
    fn now_us(&self) -> u64;
    fn advance_us(&mut self, delta: u64);
}

#[derive(Default)]
pub struct MockClock { pub t_us: u64 }

impl Clock for MockClock {
    fn now_us(&self) -> u64 { self.t_us }
    fn advance_us(&mut self, delta: u64) { self.t_us += delta; }
}

// ── Tests of the mocks themselves ─────────────────────────────────────────
// Sanity checks so when a real test fails we know the harness is OK.

#[cfg(test)]
mod sanity {
    use super::*;

    #[test]
    fn mock_i2c_read_returns_programmed_bytes() {
        let mut bus = MockI2c::default();
        bus.regs.insert((0x68, 0x75), vec![0x68]); // MPU-6050 WHO_AM_I
        let mut buf = [0u8; 1];
        bus.read_reg(0x68, 0x75, &mut buf).unwrap();
        assert_eq!(buf[0], 0x68);
        assert_eq!(bus.log.len(), 1);
    }

    #[test]
    fn mock_i2c_no_ack_when_bus_dead() {
        let mut bus = MockI2c { bus_dead: true, ..Default::default() };
        let mut buf = [0u8; 1];
        assert_eq!(bus.read_reg(0x68, 0x75, &mut buf), Err(I2cError::NoAck));
    }

    #[test]
    fn mock_uart_drains_rx_queue() {
        let mut u = MockUart::default();
        u.push_rx(b"$GPRMC");
        let mut buf = [0u8; 16];
        let n = u.read(&mut buf);
        assert_eq!(&buf[..n], b"$GPRMC");
    }

    #[test]
    fn mock_clock_advances_monotonically() {
        let mut c = MockClock::default();
        assert_eq!(c.now_us(), 0);
        c.advance_us(1234);
        assert_eq!(c.now_us(), 1234);
    }
}
