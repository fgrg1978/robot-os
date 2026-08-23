/// GPIO driver — port of kernel/drivers/gpio.c + kernel/include/gpio.h
///
/// QEMU:  in-memory simulation (no hardware on QEMU virt).
/// VF2:   StarFive JH7110 sys_iomux GPIO controller — real MMIO.

pub const GPIO_MAX_PINS: usize = 64;

/// GPIO pin direction.
#[derive(Clone, Copy, PartialEq)]
pub enum GpioDir {
    Input  = 0,
    Output = 1,
}

// ── QEMU: in-memory simulation ────────────────────────────────────────────────

#[cfg(not(feature = "vf2"))]
mod sim {
    use super::*;
    use robot_os_sync::SpinLock;

    struct GpioState {
        value:     [u8; GPIO_MAX_PINS],
        direction: [GpioDir; GPIO_MAX_PINS],
        valid:     [bool; GPIO_MAX_PINS],
    }

    impl GpioState {
        const fn new() -> Self {
            GpioState {
                value:     [0u8; GPIO_MAX_PINS],
                direction: [GpioDir::Input; GPIO_MAX_PINS],
                valid:     [false; GPIO_MAX_PINS],
            }
        }
    }

    static GPIO: SpinLock<GpioState> = SpinLock::new(GpioState::new());

    pub fn gpio_init() {}

    pub fn gpio_set_direction(pin: u32, dir: GpioDir) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let mut g = GPIO.lock();
        g.direction[pin as usize] = dir;
        g.valid[pin as usize]     = true;
        0
    }

    pub fn gpio_read(pin: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let g = GPIO.lock();
        if !g.valid[pin as usize] { return -1; }
        g.value[pin as usize] as i32
    }

    pub fn gpio_write(pin: u32, val: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let mut g = GPIO.lock();
        if g.direction[pin as usize] != GpioDir::Output { return -1; }
        g.value[pin as usize] = (val & 1) as u8;
        0
    }

    pub fn gpio_toggle(pin: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let mut g = GPIO.lock();
        if g.direction[pin as usize] != GpioDir::Output { return -1; }
        g.value[pin as usize] ^= 1;
        0
    }

    /// Emergency GPIO write for the panic handler — bypasses the `GPIO`
    /// spinlock entirely instead of calling `.lock()`.
    ///
    /// This deliberately sacrifices mutual exclusion: if another hart is
    /// inside `gpio_write`/`gpio_set_direction`/`gpio_toggle` holding
    /// `GPIO` at the exact moment of a panic, waiting for that lock (as
    /// `gpio_write` does) would spin forever and the panic message would
    /// never reach UART. During a panic, stopping actuators and getting
    /// the crash reason printed matters more than leaving the simulated
    /// GPIO state internally consistent. This is a conscious trade-off,
    /// not an oversight — do not "fix" it by adding a lock back in.
    ///
    /// # Safety
    /// May race with a concurrent `gpio_write`/`gpio_set_direction` on
    /// another hart, producing a torn read-modify-write of `GpioState`.
    /// Only call this from the panic handler.
    pub fn gpio_write_panic(pin: u32, val: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let g = unsafe { GPIO.get_mut_unchecked() };
        if g.direction[pin as usize] != GpioDir::Output { return -1; }
        g.value[pin as usize] = (val & 1) as u8;
        0
    }

    pub fn gpio_info() {
        crate::kprintln!("[GPIO] Simulated GPIO — {} pins", GPIO_MAX_PINS);
        let g = GPIO.lock();
        let mut configured = 0u32;
        for i in 0..GPIO_MAX_PINS {
            if g.valid[i] { configured += 1; }
        }
        crate::kprintln!("[GPIO] Configured: {} pins", configured);
        for i in 0..GPIO_MAX_PINS {
            if g.valid[i] {
                let dir = if g.direction[i] == GpioDir::Output { "OUT" } else { "IN " };
                crate::kprintln!("[GPIO]   pin {:2}: {} = {}", i, dir, g.value[i]);
            }
        }
    }
}

#[cfg(not(feature = "vf2"))]
pub use sim::*;

// ── VisionFive 2 / JH7110: real MMIO GPIO ────────────────────────────────────
//
// JH7110 sys_iomux GPIO controller (base 0x13040000).
// The controller exposes 64 GPIOs split into two 32-bit banks.
//
// Register map (offsets from GPIO_BASE = 0x13040000):
//   0x040  GPIOOUT0    — output value, GPIO 0..31  (1 = high)
//   0x044  GPIOOEN0    — output-enable, GPIO 0..31 (0 = output enabled, 1 = input)
//   0x050  GPIOIN0     — input value,   GPIO 0..31 (read-only)
//   0x048  GPIOOUT1    — output value, GPIO 32..63
//   0x04C  GPIOOEN1    — output-enable, GPIO 32..63
//   0x054  GPIOIN1     — input value,   GPIO 32..63
//
// NOTE: exact offsets must be verified against JH7110 TRM and confirmed on
//       real hardware.  The register layout above is derived from the
//       starfive,jh7110-sys-pinctrl Linux driver.

#[cfg(feature = "vf2")]
mod mmio {
    use super::*;
    use crate::platform::hw::{GPIO_BASE, GPIO_DOUT0, GPIO_OEN0, GPIO_DIN0};
    use robot_os_sync::SpinLock;

    // Bank 1 registers are offset by 0x08 from bank 0.
    const BANK1_OFFSET: usize = 0x08;

    // GPIOOUT0/OEN0 (and their bank-1 counterparts) are read-modify-write
    // registers: set_direction/write/toggle all read the current register,
    // flip one bit, and write it back. Bank 0 is shared by motors, the
    // payload actuator and the camera, so two harts touching different
    // pins in the same bank can race — one hart's read-modify-write
    // clobbers the other's update with a stale value. Mirrors the lock
    // already used by the QEMU `sim` path above (`static GPIO: SpinLock<..>`)
    // to serialize the same class of RMW.
    static GPIO_MMIO_LOCK: SpinLock<()> = SpinLock::new(());

    #[inline(always)]
    fn reg_read32(offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile((GPIO_BASE + offset) as *const u32) }
    }

    #[inline(always)]
    fn reg_write32(offset: usize, val: u32) {
        unsafe { core::ptr::write_volatile((GPIO_BASE + offset) as *mut u32, val) }
    }

    /// Returns (bank_offset, bit) for a given pin index.
    fn bank(pin: u32) -> (usize, u32) {
        if pin < 32 { (0, pin) } else { (BANK1_OFFSET, pin - 32) }
    }

    pub fn gpio_init() {
        // Default: all pins in input mode (OEN = all ones).
        reg_write32(GPIO_OEN0, 0xFFFF_FFFF);
        reg_write32(GPIO_OEN0 + BANK1_OFFSET, 0xFFFF_FFFF);
    }

    pub fn gpio_set_direction(pin: u32, dir: GpioDir) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let (boff, bit) = bank(pin);
        let _guard = GPIO_MMIO_LOCK.lock();
        let mut oen = reg_read32(GPIO_OEN0 + boff);
        match dir {
            GpioDir::Output => oen &= !(1 << bit),  // 0 = output enabled
            GpioDir::Input  => oen |=   1 << bit,   // 1 = input (tri-state)
        }
        reg_write32(GPIO_OEN0 + boff, oen);
        0
    }

    pub fn gpio_read(pin: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let (boff, bit) = bank(pin);
        ((reg_read32(GPIO_DIN0 + boff) >> bit) & 1) as i32
    }

    pub fn gpio_write(pin: u32, val: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let (boff, bit) = bank(pin);
        let _guard = GPIO_MMIO_LOCK.lock();
        let mut out = reg_read32(GPIO_DOUT0 + boff);
        if val & 1 != 0 { out |=  1 << bit; }
        else             { out &= !(1 << bit); }
        reg_write32(GPIO_DOUT0 + boff, out);
        0
    }

    pub fn gpio_toggle(pin: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let (boff, bit) = bank(pin);
        let _guard = GPIO_MMIO_LOCK.lock();
        let out = reg_read32(GPIO_DOUT0 + boff) ^ (1 << bit);
        reg_write32(GPIO_DOUT0 + boff, out);
        0
    }

    /// Emergency GPIO write for the panic handler — bypasses
    /// `GPIO_MMIO_LOCK` entirely instead of calling `.lock()`.
    ///
    /// `GPIO_MMIO_LOCK` was added to serialize the GPIOOUT/GPIOOEN
    /// read-modify-write across harts (see the lock's doc comment above).
    /// That is exactly right for normal operation, but it means a panic
    /// on one hart while another hart holds this lock would spin forever
    /// in the panic handler and the crash reason would never reach UART.
    /// This function deliberately sacrifices RMW exclusion — a torn
    /// bank write that leaves some unrelated pin's output bit wrong is
    /// an acceptable price during a panic; a kernel that hangs silently
    /// instead of printing why it crashed is not. Conscious trade-off,
    /// not an oversight — do not "fix" it by adding the lock back.
    ///
    /// # Safety
    /// May race with a concurrent `gpio_write`/`gpio_set_direction`/
    /// `gpio_toggle` on another hart touching the same register bank,
    /// producing a torn read-modify-write. Only call this from the
    /// panic handler.
    pub fn gpio_write_panic(pin: u32, val: u32) -> i32 {
        if pin as usize >= GPIO_MAX_PINS { return -1; }
        let (boff, bit) = bank(pin);
        let mut out = reg_read32(GPIO_DOUT0 + boff);
        if val & 1 != 0 { out |=  1 << bit; }
        else             { out &= !(1 << bit); }
        reg_write32(GPIO_DOUT0 + boff, out);
        0
    }

    pub fn gpio_info() {
        crate::kprintln!("[GPIO] JH7110 MMIO GPIO @ {:#010x}", GPIO_BASE);
        let out0 = reg_read32(GPIO_DOUT0);
        let oen0 = reg_read32(GPIO_OEN0);
        let in0  = reg_read32(GPIO_DIN0);
        let out1 = reg_read32(GPIO_DOUT0 + BANK1_OFFSET);
        let oen1 = reg_read32(GPIO_OEN0  + BANK1_OFFSET);
        let in1  = reg_read32(GPIO_DIN0  + BANK1_OFFSET);
        crate::kprintln!("[GPIO] Bank0: OUT={:#010x} OEN={:#010x} IN={:#010x}", out0, oen0, in0);
        crate::kprintln!("[GPIO] Bank1: OUT={:#010x} OEN={:#010x} IN={:#010x}", out1, oen1, in1);
    }
}

#[cfg(feature = "vf2")]
pub use mmio::*;
