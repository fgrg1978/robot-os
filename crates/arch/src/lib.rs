#![no_std]

pub mod cpu;
pub mod csr;
#[cfg(not(feature = "esp32c3"))]
pub mod mmu;
#[cfg(not(feature = "esp32c3"))]
pub mod pmp;
pub mod rvv;
#[cfg(not(feature = "esp32c3"))]
pub mod sbi;
pub mod trap;

// ESP32-C3: stub sbi module with shutdown/reboot
#[cfg(feature = "esp32c3")]
pub mod sbi {
    pub fn shutdown() -> ! {
        loop { crate::cpu::wfi(); }
    }
    pub fn reboot() -> ! {
        loop { crate::cpu::wfi(); }
    }
}
