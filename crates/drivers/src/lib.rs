#![no_std]

pub mod platform;
pub mod uart;
pub mod plic;
pub mod clint;
pub mod gpio;
pub mod pwm;
pub mod i2c;

// eMMC / SD driver (real hardware only; not present on QEMU virt).
#[cfg(any(feature = "vf2", feature = "k1"))]
pub mod mmc;

// Block device abstraction: routes to VirtIO (QEMU) or SDHCI (VF2).
pub mod blkdev;

// Re-export Uart at crate root so #[macro_export] macros ($crate::Uart) resolve correctly
pub use uart::Uart;

#[cfg(not(feature = "esp32c3"))]
pub mod virtio;

// ESP32-C3: no VirtIO hardware — provide stubs so net/syscall crates compile.
#[cfg(feature = "esp32c3")]
pub mod virtio {
    pub mod blk {
        pub fn init() -> Result<(), ()> { Err(()) }
        pub fn capacity_sectors() -> u64 { 0 }
        pub fn read(_sector: u64, _count: u32, _buf: &mut [u8]) -> Result<(), ()> { Err(()) }
        pub fn write(_sector: u64, _count: u32, _buf: &[u8]) -> Result<(), ()> { Err(()) }
    }
    pub mod net {
        pub fn init() -> Result<(), ()> { Err(()) }
        pub fn get_mac() -> [u8; 6] { [0; 6] }
        pub fn is_ready() -> bool { false }
        pub fn send(_data: &[u8]) -> Result<(), ()> { Err(()) }
        pub fn poll_recv(_buf: &mut [u8]) -> usize { 0 }
    }
}

// Hardware watchdog timer (DesignWare WDT on VF2/K1; no-op on QEMU).
pub mod wdt;

// SPI master driver (sim on QEMU; Cadence SPI on VF2).
#[allow(dead_code)]
pub mod spi;

// CAN bus driver (simulation only — no CAN hardware on supported platforms).
#[allow(dead_code)]
pub mod can;

// DMA controller (memcpy sim on QEMU; JH7110 PDMA on VF2).
#[allow(dead_code)]
pub mod dma;

// USB host controller (skeleton; xHCI on VF2).
#[allow(dead_code)]
pub mod usb;

// Power management (WFI idle, clock gating).
#[allow(dead_code)]
pub mod pm;

// Real Ethernet MAC driver (Cadence MACB/GEM on VF2; stub on QEMU).
#[allow(dead_code)]
pub mod eth;

// UART bridge driver for ESP32-C3 WiFi relay (VF2 UART1; stub on others).
pub mod uart_bridge;

// ESC (Electronic Speed Controller) PWM output for brushless motors.
pub mod esc;

// RC (Remote Control) receiver — SBUS / PPM input.
pub mod rc;

// Rangefinder sensors — ultrasonic (HC-SR04) + Time-of-Flight (VL53L0X).
pub mod rangefinder;

// MIPI CSI-2 camera driver (simulated on QEMU; JH7110 ISP on VF2; SpacemiT ISP on K1).
pub mod csi;

// WiFi driver (ESP32-C3 only; no-op stub on other targets).
pub mod wifi;
