#![no_std]

// Canonical Driver API (RFC-0002 modular pattern). Existing in-kernel
// drivers below predate the trait; they will migrate to `impl Driver`
// per-driver in follow-up batches.
pub mod api;
pub mod runtime;

pub mod platform;
pub mod uart;
// First Driver trait migration (A3a). The legacy `uart` module
// stays for `kprint!` + early-boot panic path; this provides the
// unified API for client tasks via `runtime::REGISTRY`.
pub mod uart_driver;

// Second Driver trait migration (A3a.2). Validates the trait shape
// against a pin-oriented hardware family — sim on QEMU, real MMIO
// on VF2/K1.
pub mod gpio_driver;

// Third Driver trait migration (A3a.3). Bus-oriented hardware
// (DesignWare I2C). Proves the uniform `(op, input, output)` API
// scales to multi-axis (bus + slave addr + register) addressing.
pub mod i2c_driver;

// Fourth Driver trait migration (A3a.4). Multi-parameter actuator
// (PWM channel + nanosecond period/duty).
pub mod pwm_driver;

// Fifth Driver trait migration (A3a.5). Closed-loop controller
// (motor PID) — pure software, no MMIO of its own.
pub mod motor_driver;

// Kernel-side proxy that adapts a userspace driver (registered via
// `robot_os_driver_server`) to the `Driver` trait. Together with
// `uart_driver` this proves the same trait spans both
// `DriverIsolation` variants.
pub mod user_driver_proxy;
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

pub mod virtio;

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

// USB device-mode controller (DEV02 DFU recovery — DWC2 on VF2/K1).
// Trait + scaffold; DWC2 register programming is post-hardware work.
#[allow(dead_code)]
pub mod usb_device;

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

// WiFi driver — API surface only; no-op stubs (no WiFi hardware).
pub mod wifi;

// PID velocity controller for wheeled robots (4WD differential drive).
pub mod motor_pid;

// ADS1115 16-bit 4-channel I2C ADC (battery voltage, analog sensors).
pub mod ads1115;

// PWM-based piezo buzzer for audio feedback (beeps, tones, alerts).
pub mod buzzer;

// LD19 (LD-06) 2D LiDAR UART driver — 360° scan, 12m range.
pub mod lidar;

// INA219 I2C current/voltage sensor — battery monitoring, sag detection.
pub mod ina219;

// SpacemiT K1 NPU (Neural Processing Unit) — ~2 TOPS INT8 inference engine.
// F14: compiled for all builds; MMIO-mapped only on k1 feature.
pub mod npu;

// Optical flow sensor driver (F26): PMW3901 / PAA5100JE via SPI.
// Used for velocity estimation on drones and slip detection on wheeled robots.
#[allow(dead_code)]
pub mod optical_flow;

// F16: WCET (Worst-Case Execution Time) instrumentation — cycle-accurate
// timing using rdcycle CSR + atomic statistics per named measurement point.
pub mod wcet;
