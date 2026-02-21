/// I2C driver — port of kernel/drivers/i2c.c + kernel/include/i2c.h
///
/// QEMU: in-memory simulation with fake IMU + barometer.
/// VF2:  DesignWare APB I2C — real MMIO transactions.

pub const I2C_MAX_DEVICES: usize = 16;
pub const I2C_BUS_COUNT:   usize = 4;

#[derive(Clone, Copy)]
pub struct I2cDevice {
    pub bus:     u8,
    pub addr:    u8,
    pub present: bool,
    pub regs:    [u8; 256],
}

impl I2cDevice {
    pub const fn new() -> Self {
        I2cDevice { bus: 0, addr: 0, present: false, regs: [0u8; 256] }
    }
}

// ── QEMU: in-memory simulation ────────────────────────────────────────────────

#[cfg(not(feature = "vf2"))]
mod sim {
    use super::*;
    use robot_os_sync::SpinLock;

    struct I2cState {
        devices: [I2cDevice; I2C_MAX_DEVICES],
        count:   usize,
    }

    impl I2cState {
        const fn new() -> Self {
            I2cState { devices: [I2cDevice::new(); I2C_MAX_DEVICES], count: 0 }
        }

        fn find(&self, bus: u8, addr: u8) -> Option<usize> {
            for i in 0..self.count {
                if self.devices[i].bus == bus && self.devices[i].addr == addr {
                    return Some(i);
                }
            }
            None
        }
    }

    static I2C: SpinLock<I2cState> = SpinLock::new(I2cState::new());

    pub fn i2c_init() {
        let mut state = I2C.lock();
        // Simulated IMU (MPU-6050) at bus 0, address 0x68
        if state.count < I2C_MAX_DEVICES {
            let i = state.count;
            state.devices[i].bus     = 0;
            state.devices[i].addr    = 0x68;
            state.devices[i].present = true;
            state.devices[i].regs[0x75] = 0x68; // WHO_AM_I
            // Phase E2: populate simulated accel/gyro/temp data registers.
            // Simulates the sensor sitting flat: accel = (0, 0, +1g), gyro = 0, temp ≈ 25 °C.
            // Format: big-endian i16, starting at register 0x3B (14 bytes).
            // Accel X = 0
            state.devices[i].regs[0x3B] = 0x00;
            state.devices[i].regs[0x3C] = 0x00;
            // Accel Y = 0
            state.devices[i].regs[0x3D] = 0x00;
            state.devices[i].regs[0x3E] = 0x00;
            // Accel Z = +16384 (= +1g at ±2g range)
            state.devices[i].regs[0x3F] = 0x40; // 16384 >> 8
            state.devices[i].regs[0x40] = 0x00; // 16384 & 0xFF
            // Temp raw = -2198 → (−2198/340)+36.53 ≈ 30.07 °C
            // Use raw = 0 → 0/340 + 36.53 = 36.53 °C (room temp sim)
            state.devices[i].regs[0x41] = 0x00;
            state.devices[i].regs[0x42] = 0x00;
            // Gyro X = 0
            state.devices[i].regs[0x43] = 0x00;
            state.devices[i].regs[0x44] = 0x00;
            // Gyro Y = 0
            state.devices[i].regs[0x45] = 0x00;
            state.devices[i].regs[0x46] = 0x00;
            // Gyro Z = 0
            state.devices[i].regs[0x47] = 0x00;
            state.devices[i].regs[0x48] = 0x00;
            state.count += 1;
        }
        // Simulated barometer (BMP280) at bus 0, address 0x76
        if state.count < I2C_MAX_DEVICES {
            let i = state.count;
            state.devices[i].bus     = 0;
            state.devices[i].addr    = 0x76;
            state.devices[i].present = true;
            state.devices[i].regs[0xD0] = 0x58; // chip_id (BMP280)
            // Phase G1: populate simulated calibration + measurement data.
            // Calibration registers 0x88-0xA1 (26 bytes) — realistic trimming values.
            // dig_T1=27504 dig_T2=26435 dig_T3=-1000
            state.devices[i].regs[0x88] = 0x70; state.devices[i].regs[0x89] = 0x6B; // dig_T1 LE
            state.devices[i].regs[0x8A] = 0x43; state.devices[i].regs[0x8B] = 0x67; // dig_T2 LE
            state.devices[i].regs[0x8C] = 0x18; state.devices[i].regs[0x8D] = 0xFC; // dig_T3 LE
            // dig_P1=36477 dig_P2=-10685 dig_P3=3024 dig_P4=2855
            state.devices[i].regs[0x8E] = 0x7D; state.devices[i].regs[0x8F] = 0x8E; // dig_P1
            state.devices[i].regs[0x90] = 0x43; state.devices[i].regs[0x91] = 0xD6; // dig_P2
            state.devices[i].regs[0x92] = 0xD0; state.devices[i].regs[0x93] = 0x0B; // dig_P3
            state.devices[i].regs[0x94] = 0x27; state.devices[i].regs[0x95] = 0x0B; // dig_P4
            // dig_P5=140 dig_P6=-7 dig_P7=15500 dig_P8=-14600 dig_P9=6000
            state.devices[i].regs[0x96] = 0x8C; state.devices[i].regs[0x97] = 0x00; // dig_P5
            state.devices[i].regs[0x98] = 0xF9; state.devices[i].regs[0x99] = 0xFF; // dig_P6
            state.devices[i].regs[0x9A] = 0x8C; state.devices[i].regs[0x9B] = 0x3C; // dig_P7
            state.devices[i].regs[0x9C] = 0xF8; state.devices[i].regs[0x9D] = 0xC6; // dig_P8
            state.devices[i].regs[0x9E] = 0x70; state.devices[i].regs[0x9F] = 0x17; // dig_P9
            // Measurement registers 0xF7-0xFC (6 bytes).
            // Simulates ~25 C, ~101325 Pa (sea level standard).
            // adc_t = 519888 (0x7EED0) → temp raw: MSB=0x7E LSB=0xED XLSB=0x00
            state.devices[i].regs[0xFA] = 0x7E; // temp MSB
            state.devices[i].regs[0xFB] = 0xED; // temp LSB
            state.devices[i].regs[0xFC] = 0x00; // temp XLSB
            // adc_p = 415148 (0x6572C) → press raw: MSB=0x65 LSB=0x72 XLSB=0xC0
            state.devices[i].regs[0xF7] = 0x65; // press MSB
            state.devices[i].regs[0xF8] = 0x72; // press LSB
            state.devices[i].regs[0xF9] = 0xC0; // press XLSB
            state.count += 1;
        }
    }

    pub fn i2c_write(bus: u8, addr: u8, data: &[u8]) -> i32 {
        if data.is_empty() { return -1; }
        let mut state = I2C.lock();
        match state.find(bus, addr) {
            Some(i) => {
                let reg = data[0] as usize;
                for (j, &b) in data[1..].iter().enumerate() {
                    if reg + j < 256 { state.devices[i].regs[reg + j] = b; }
                }
                0
            }
            None => -1,
        }
    }

    pub fn i2c_read(bus: u8, addr: u8, reg: u8, buf: &mut [u8]) -> i32 {
        let state = I2C.lock();
        match state.find(bus, addr) {
            Some(i) => {
                for (j, b) in buf.iter_mut().enumerate() {
                    *b = state.devices[i].regs[(reg as usize + j) & 0xFF];
                }
                buf.len() as i32
            }
            None => -1,
        }
    }

    pub fn i2c_detect(bus: u8, addr: u8) -> bool {
        I2C.lock().find(bus, addr).is_some()
    }

    pub fn i2c_scan(bus: u8) {
        let state = I2C.lock();
        crate::kprintln!("[I2C] Scanning bus {}:", bus);
        crate::kprintln!("[I2C]      0  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f");
        for row in 0..8u8 {
            crate::kprint!("[I2C] {:02x}: ", row * 16);
            for col in 0..16u8 {
                let addr = row * 16 + col;
                if addr < 8 || addr > 0x77 { crate::kprint!("   "); }
                else {
                    let found = state.devices[..state.count].iter()
                        .any(|d| d.bus == bus && d.addr == addr && d.present);
                    if found { crate::kprint!("{:02x} ", addr); }
                    else     { crate::kprint!("-- "); }
                }
            }
            crate::kprintln!();
        }
    }

    pub fn i2c_info() {
        let state = I2C.lock();
        crate::kprintln!("[I2C] {} device(s) registered (simulated)", state.count);
        for i in 0..state.count {
            let d = &state.devices[i];
            crate::kprintln!("[I2C]   bus={} addr=0x{:02x} present={}", d.bus, d.addr, d.present);
        }
    }
}

#[cfg(not(feature = "vf2"))]
pub use sim::*;

// ── VisionFive 2 / JH7110: DesignWare APB I2C ────────────────────────────────
//
// JH7110 has 6 I2C controllers (I2C0..I2C5), all DesignWare APB I2C v1 compatible.
// We expose I2C0 (0x10010000) and I2C1 (0x10020000) mapped to bus 0 and 1.
//
// DesignWare APB I2C register map (well-documented, used in Linux dwi2c driver):
//   0x00 IC_CON         — control: master/slave, speed (0=std, 1=fast)
//   0x04 IC_TAR         — 7-bit target address
//   0x10 IC_DATA_CMD    — data + read/write command (bit 8: 1=read, 0=write)
//   0x14 IC_SS_SCL_HCNT — standard speed SCL high count
//   0x18 IC_SS_SCL_LCNT — standard speed SCL low count
//   0x1C IC_FS_SCL_HCNT — fast speed SCL high count
//   0x20 IC_FS_SCL_LCNT — fast speed SCL low count
//   0x3C IC_INTR_STAT   — interrupt status
//   0x6C IC_ENABLE      — 1=enable controller
//   0x70 IC_STATUS      — bit 1: master active, bit 3: TXFIFO not full, bit 6: SDA stalled
//   0x74 IC_TXFLR       — TX FIFO level
//   0x78 IC_RXFLR       — RX FIFO level
//   0x80 IC_CLR_INTR    — clear all interrupts (read)
//   0xA0 IC_COMP_PARAM_1 — parameters
//
// SCL freq = IC_CLK / (HCNT + LCNT + 8)
// At IC_CLK=100 MHz, 100 kHz std-mode: HCNT=487, LCNT=512 (typical Linux values).

#[cfg(feature = "vf2")]
mod dw_i2c {
    use crate::platform::hw::{I2C0_BASE, I2C1_BASE};

    // Register offsets
    const IC_CON:        usize = 0x00;
    const IC_TAR:        usize = 0x04;
    const IC_DATA_CMD:   usize = 0x10;
    const IC_SS_SCL_HCNT: usize = 0x14;
    const IC_SS_SCL_LCNT: usize = 0x18;
    const IC_ENABLE:     usize = 0x6C;
    const IC_STATUS:     usize = 0x70;
    #[allow(dead_code)]
    const IC_RXFLR:      usize = 0x78;
    const IC_CLR_INTR:   usize = 0x80;

    // IC_CON bits
    const CON_MASTER:    u32 = 1 << 0;
    const CON_SPEED_STD: u32 = 1 << 1;
    const CON_10BIT_OFF: u32 = 0;       // 7-bit addressing
    const CON_RESTART:   u32 = 1 << 5;
    const CON_SLAVE_DIS: u32 = 1 << 6;

    // IC_STATUS bits
    const STATUS_RFNE: u32 = 1 << 3;   // RX FIFO not empty
    const STATUS_TFE:  u32 = 1 << 2;   // TX FIFO empty (transfer done)
    const STATUS_MA:   u32 = 1 << 5;   // master activity

    // IC_DATA_CMD bits
    const CMD_READ: u32 = 1 << 8;
    const CMD_STOP: u32 = 1 << 9;

    fn bus_base(bus: u8) -> Option<usize> {
        match bus {
            0 => Some(I2C0_BASE),
            1 => Some(I2C1_BASE),
            _ => None,
        }
    }

    #[inline(always)]
    fn rd(base: usize, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((base + off) as *const u32) }
    }

    #[inline(always)]
    fn wr(base: usize, off: usize, val: u32) {
        unsafe { core::ptr::write_volatile((base + off) as *mut u32, val) }
    }

    fn wait_tfne(base: usize) -> bool {
        // Wait for TX FIFO empty (transfer complete), with timeout.
        for _ in 0..100_000u32 {
            if rd(base, IC_STATUS) & STATUS_TFE != 0
                && rd(base, IC_STATUS) & STATUS_MA == 0 {
                return true;
            }
        }
        false
    }

    fn init_bus(base: usize) {
        // Disable controller before programming
        wr(base, IC_ENABLE, 0);
        // Master mode, standard speed (100 kHz), 7-bit, disable slave
        wr(base, IC_CON, CON_MASTER | CON_SPEED_STD | CON_10BIT_OFF | CON_RESTART | CON_SLAVE_DIS);
        // SCL timing for 100 kHz at 100 MHz IC_CLK: HCNT=487, LCNT=512
        wr(base, IC_SS_SCL_HCNT, 487);
        wr(base, IC_SS_SCL_LCNT, 512);
        // Re-enable
        wr(base, IC_ENABLE, 1);
        // Clear any pending interrupts
        let _ = rd(base, IC_CLR_INTR);
    }

    pub fn i2c_init() {
        init_bus(I2C0_BASE);
        init_bus(I2C1_BASE);
    }

    pub fn i2c_write(bus: u8, addr: u8, data: &[u8]) -> i32 {
        if data.is_empty() { return -1; }
        let base = match bus_base(bus) { Some(b) => b, None => return -1 };
        wr(base, IC_TAR, addr as u32);
        wr(base, IC_ENABLE, 1);
        for (i, &b) in data.iter().enumerate() {
            let stop = if i + 1 == data.len() { CMD_STOP } else { 0 };
            wr(base, IC_DATA_CMD, b as u32 | stop);
        }
        if !wait_tfne(base) { return -1; }
        0
    }

    pub fn i2c_read(bus: u8, addr: u8, reg: u8, buf: &mut [u8]) -> i32 {
        if buf.is_empty() { return -1; }
        let base = match bus_base(bus) { Some(b) => b, None => return -1 };
        // Write register address
        wr(base, IC_TAR, addr as u32);
        wr(base, IC_ENABLE, 1);
        wr(base, IC_DATA_CMD, reg as u32); // write register address
        // Issue read commands
        for i in 0..buf.len() {
            let stop = if i + 1 == buf.len() { CMD_STOP } else { 0 };
            wr(base, IC_DATA_CMD, CMD_READ | stop);
        }
        // Collect received bytes
        let mut received = 0usize;
        for _ in 0..1_000_000u32 {
            if rd(base, IC_STATUS) & STATUS_RFNE != 0 {
                buf[received] = (rd(base, IC_DATA_CMD) & 0xFF) as u8;
                received += 1;
                if received == buf.len() { break; }
            }
        }
        received as i32
    }

    pub fn i2c_detect(bus: u8, addr: u8) -> bool {
        // Send a 0-byte write and check for ACK (quick-write probe)
        let base = match bus_base(bus) { Some(b) => b, None => return false };
        wr(base, IC_TAR, addr as u32);
        wr(base, IC_ENABLE, 1);
        wr(base, IC_DATA_CMD, CMD_STOP); // zero-length write → address-only
        wait_tfne(base)
    }

    pub fn i2c_scan(bus: u8) {
        crate::kprintln!("[I2C] Scanning bus {} (JH7110 DW-I2C):", bus);
        crate::kprintln!("[I2C]      0  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f");
        for row in 0..8u8 {
            crate::kprint!("[I2C] {:02x}: ", row * 16);
            for col in 0..16u8 {
                let a = row * 16 + col;
                if a < 8 || a > 0x77 { crate::kprint!("   "); }
                else if i2c_detect(bus, a) { crate::kprint!("{:02x} ", a); }
                else                       { crate::kprint!("-- "); }
            }
            crate::kprintln!();
        }
    }

    pub fn i2c_info() {
        crate::kprintln!("[I2C] JH7110 DesignWare APB I2C");
        crate::kprintln!("[I2C]   I2C0 @ {:#010x}", I2C0_BASE);
        crate::kprintln!("[I2C]   I2C1 @ {:#010x}", I2C1_BASE);
    }
}

#[cfg(feature = "vf2")]
pub use dw_i2c::*;
