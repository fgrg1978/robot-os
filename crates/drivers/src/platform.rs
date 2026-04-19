/// Platform constants — compile-time hardware address selection.
///
/// Default (no feature): QEMU virt machine.
/// Feature "vf2": StarFive VisionFive 2 (JH7110 SoC, SiFive U74 × 4).
/// Feature "k1":  SpacemiT K1 (BananaPi BPI-F3, SpacemiT X60 × 8, RV64GCVB).
///
/// Key differences QEMU virt vs VF2 vs K1:
///   - Timer frequency: 10 MHz (QEMU) / 4 MHz (JH7110) / 24 MHz (K1)
///   - RAM base:        0x8000_0000 (QEMU) / 0x4000_0000 (VF2) / 0x0000_0000 (K1)
///   - UART base:       0x1000_0000 (QEMU/VF2) / 0xD401_7000 (K1)
///   - PLIC base:       0x0C00_0000 (QEMU/VF2) / 0xE000_0000 (K1)
///   - GPIO/PWM/I2C:    simulated (QEMU) vs real MMIO (VF2/K1)

// ── QEMU virt ─────────────────────────────────────────────────────────────────
#[cfg(not(any(feature = "vf2", feature = "k1", feature = "esp32c3")))]
pub mod hw {
    pub const PLATFORM_NAME: &str = "QEMU virt";

    /// NS16550A UART0 base address.
    pub const UART_BASE:   usize = 0x1000_0000;
    /// Platform-Level Interrupt Controller base.
    pub const PLIC_BASE:   usize = 0x0C00_0000;
    /// `mtime` timer frequency reported by the hardware (Hz).
    pub const TIMER_FREQ:  u64   = 10_000_000;   // 10 MHz
    /// Maximum usable CPU cores.
    pub const NUM_CPUS:    usize = 4;
    /// Physical RAM base (below OpenSBI).
    pub const RAM_BASE:    usize = 0x8000_0000;
    /// Kernel load address (above OpenSBI 2 MiB reservation).
    pub const KERNEL_LOAD: usize = 0x8020_0000;

    // GPIO/PWM/I2C are simulated in QEMU; no MMIO addresses needed.
}

// ── StarFive VisionFive 2 (JH7110) ───────────────────────────────────────────
#[cfg(feature = "vf2")]
pub mod hw {
    pub const PLATFORM_NAME: &str = "StarFive VisionFive 2 (JH7110)";

    /// NS16550A UART0 — SAME base as QEMU virt (by design).
    pub const UART_BASE:   usize = 0x1000_0000;
    /// PLIC — SAME base as QEMU virt (by design).
    pub const PLIC_BASE:   usize = 0x0C00_0000;
    /// JH7110 `mtime` timebase (from DTS: timebase-frequency = <4000000>).
    pub const TIMER_FREQ:  u64   = 4_000_000;    // 4 MHz  ← critical difference
    /// 4× SiFive U74 application cores (+ 1× S7 monitor, not managed by our kernel).
    pub const NUM_CPUS:    usize = 4;
    /// JH7110 DDR physical base.
    pub const RAM_BASE:    usize = 0x4000_0000;
    /// Kernel load address on VF2 (above OpenSBI 2 MiB reservation).
    pub const KERNEL_LOAD: usize = 0x4020_0000;

    // ── GPIO ──────────────────────────────────────────────────────────────────
    /// StarFive JH7110 sys_iomux / GPIO controller base.
    /// Reference: JH7110 TRM, Chapter GPIO, sys_iomux_cfgsaif_syscfg*.
    pub const GPIO_BASE:   usize = 0x1304_0000;
    /// Offset of the 64-bit output value register (GPIO 0-31 in low word).
    pub const GPIO_DOUT0:  usize = 0x040;
    /// Offset of the 64-bit output enable register (0 = output enabled).
    pub const GPIO_OEN0:   usize = 0x044;
    /// Offset of the 64-bit data input register.
    pub const GPIO_DIN0:   usize = 0x050;

    // ── PWM ───────────────────────────────────────────────────────────────────
    /// JH7110 PWM controller (8 channels via sifive,pwm-v0).
    /// Reference: JH7110 TRM, Chapter PWM.
    pub const PWM_BASE:    usize = 0x1703_0000;
    /// Per-channel register stride (bytes).
    pub const PWM_STRIDE:  usize = 0x10;
    /// PWM channel duty cycle offset.
    pub const PWM_DUTY:    usize = 0x04;
    /// PWM channel period offset.
    pub const PWM_PERIOD:  usize = 0x08;
    /// PWM enable register offset.
    pub const PWM_ENABLE:  usize = 0x0C;

    // ── I2C (DesignWare APB I2C) ──────────────────────────────────────────────
    /// I2C0 base (DesignWare APB I2C, 400 kHz).
    pub const I2C0_BASE:   usize = 0x1001_0000;
    /// I2C1 base.
    pub const I2C1_BASE:   usize = 0x1002_0000;

    // ── eMMC / SD (DesignWare SDHCI) ─────────────────────────────────────────
    /// SDIO0 base (eMMC — JH7110 mmc@16010000).
    pub const MMC0_BASE:   usize = 0x1601_0000;
    /// SDIO1 base (microSD slot — JH7110 mmc@16020000).
    pub const MMC1_BASE:   usize = 0x1602_0000;

    // ── Ethernet (Cadence MACB/GEM) ─────────────────────────────────────────
    /// GMAC0 base — Cadence GEM Gigabit Ethernet MAC (RGMII to external PHY).
    /// JH7110 ethernet@16030000.
    pub const ETH0_BASE:   usize = 0x1603_0000;

    // ── UART1 (for ESP32 WiFi bridge) ───────────────────────────────────────
    /// JH7110 UART1 — NS16550A, used for ESP32-C3 bridge (serial@10010000).
    pub const UART1_BASE:  usize = 0x1001_0000;

    // ── Watchdog (DesignWare WDT) ─────────────────────────────────────────────
    /// JH7110 DesignWare WDT base address.
    /// Reference: JH7110 TRM, Chapter WDT; APB clock ≈ 24 MHz.
    pub const WDT_BASE:    usize = 0x1301_0000;
}

// ── SpacemiT K1 (BananaPi BPI-F3) ─────────────────────────────────────────────
#[cfg(feature = "k1")]
pub mod hw {
    pub const PLATFORM_NAME: &str = "SpacemiT K1 (BananaPi BPI-F3)";

    /// SpacemiT K1 UART0 — "intel,xscale-uart" compatible (NS16550A register layout).
    /// Clock: 14 MHz.  U-Boot/OpenSBI configures 115200 baud before kernel hand-off.
    pub const UART_BASE:   usize = 0xD401_7000;
    /// SpacemiT K1 PLIC (Platform-Level Interrupt Controller).
    /// 256 external interrupt sources, M-mode + S-mode per hart.
    pub const PLIC_BASE:   usize = 0xE000_0000;
    /// K1 `mtime` timebase (from DTS: timebase-frequency = <24000000>).
    pub const TIMER_FREQ:  u64   = 24_000_000;   // 24 MHz
    /// 8× SpacemiT X60 application cores.
    pub const NUM_CPUS:    usize = 8;
    /// K1 physical DDR base (LPDDR4X starts at address 0x0).
    pub const RAM_BASE:    usize = 0x0000_0000;
    /// Kernel load address (OpenSBI reserves first 2 MiB; kernel follows).
    pub const KERNEL_LOAD: usize = 0x0020_0000;

    // ── GPIO / Pinctrl ────────────────────────────────────────────────────────
    /// SpacemiT K1 pin-control / GPIO base (pinctrl-single).
    pub const GPIO_BASE:   usize = 0xD401_E000;
    /// Output data register offset (32-bit, GPIOs 0-31).
    pub const GPIO_DOUT0:  usize = 0x000;
    /// Output enable register offset (0 = output).
    pub const GPIO_OEN0:   usize = 0x004;
    /// Input data register offset.
    pub const GPIO_DIN0:   usize = 0x010;

    // ── PWM ───────────────────────────────────────────────────────────────────
    /// SpacemiT K1 PWM0 base ("spacemit,k1x-pwm").
    pub const PWM_BASE:    usize = 0xD401_A000;
    /// Per-channel stride.
    pub const PWM_STRIDE:  usize = 0x10;
    /// Channel duty cycle offset.
    pub const PWM_DUTY:    usize = 0x04;
    /// Channel period offset.
    pub const PWM_PERIOD:  usize = 0x08;
    /// Channel enable offset.
    pub const PWM_ENABLE:  usize = 0x0C;

    // ── I2C (DesignWare APB I2C) ──────────────────────────────────────────────
    /// I2C6 base — general-purpose I2C for robot sensors.
    pub const I2C0_BASE:   usize = 0xD401_8800;
    /// I2C7 base.
    pub const I2C1_BASE:   usize = 0xD401_8C00;

    // ── eMMC / SD (DesignWare SDHCI) ─────────────────────────────────────────
    /// SDHCI0 base — removable SD card slot on BPI-F3.
    /// Used as the boot storage device (MmcSlot::Emmc = 0 → MMC0_BASE).
    pub const MMC0_BASE:   usize = 0xD428_0000;
    /// SDHCI2 base — onboard eMMC (HS400, non-removable).
    pub const MMC1_BASE:   usize = 0xD428_1000;

    // ── Watchdog (DesignWare WDT) ─────────────────────────────────────────────
    /// SpacemiT K1 WDT base address.
    /// Reference: K1 BSP DTS (spacemit,wdt); WDT clock = 24 MHz.
    /// Note: address 0xD401_5000 from BSP — verify against final silicon DTS.
    pub const WDT_BASE:    usize = 0xD401_5000;

    // ── NPU (Neural Processing Unit) ─────────────────────────────────────────
    /// SpacemiT K1 NPU MMIO base.
    /// Compatible: "spacemit,k1x-npu".  Reference: K1 BSP DTS (bpi-f3.dts).
    /// Performance: ~2 TOPS INT8; supports conv, pool, activation, eltwise.
    pub const NPU_BASE:    usize = 0xC080_0000;
    /// NPU MMIO region size (1 MiB covers all command/data registers).
    pub const NPU_SIZE:    usize = 0x0010_0000;
}

// ── Espressif ESP32-C3 (RISC-V RV32IMC) ──────────────────────────────────────
//
// Single-core RV32IMC @ 160 MHz.  400 KB SRAM.  WiFi + BLE 5.
// No MMU, no FPU.  Peripherals at APB base 0x6000_0000.
// Reference: ESP32-C3 Technical Reference Manual (Espressif TRM v1.1).
//
// NOTE: This is a port skeleton.  Actual driver implementations (UART, GPIO,
// I2C, Timer) use ESP-specific register layouts, NOT NS16550A / DesignWare.
// Build with: cargo build --release --features esp32c3
#[cfg(feature = "esp32c3")]
pub mod hw {
    pub const PLATFORM_NAME: &str = "Espressif ESP32-C3 (RV32IMC)";

    /// ESP32-C3 UART0 (ESP-specific, NOT NS16550A).
    /// UART_FIFO_REG at base+0x00, UART_STATUS_REG at base+0x1C.
    pub const UART_BASE:   usize = 0x6000_0000;
    /// ESP32-C3 has no traditional PLIC.  Uses an interrupt matrix + CLIC.
    /// This constant is unused; interrupt setup is ESP-specific.
    pub const PLIC_BASE:   usize = 0x0000_0000;
    /// SYSTIMER clock: 16 MHz (XTAL_CLK / 2.5 at 40 MHz XTAL).
    pub const TIMER_FREQ:  u64   = 16_000_000;   // 16 MHz
    /// Single core.
    pub const NUM_CPUS:    usize = 1;
    /// SRAM base (instruction bus address; data bus mirrors at 0x3FC8_0000).
    pub const RAM_BASE:    usize = 0x3FC8_0000;
    /// No OpenSBI — code starts at reset vector via bootloader.
    pub const KERNEL_LOAD: usize = 0x3FC8_0000;

    // ── GPIO ──────────────────────────────────────────────────────────────────
    /// ESP32-C3 GPIO controller base.
    pub const GPIO_BASE:   usize = 0x6000_4000;
    /// GPIO output register offset (GPIO_OUT_REG).
    pub const GPIO_DOUT0:  usize = 0x004;
    /// GPIO output enable register offset (GPIO_ENABLE_REG).
    pub const GPIO_OEN0:   usize = 0x020;
    /// GPIO input register offset (GPIO_IN_REG).
    pub const GPIO_DIN0:   usize = 0x03C;

    // ── PWM (LEDC) ───────────────────────────────────────────────────────────
    /// ESP32-C3 LEDC (LED PWM Controller) base.
    /// 6 channels, used for motor PWM.
    pub const PWM_BASE:    usize = 0x6001_9000;
    /// Per-channel register stride.
    pub const PWM_STRIDE:  usize = 0x14;
    /// Channel duty cycle offset (LEDC_CH_DUTY_REG).
    pub const PWM_DUTY:    usize = 0x04;
    /// Channel period/config offset (LEDC_CH_CONF0_REG).
    pub const PWM_PERIOD:  usize = 0x00;
    /// Channel update/enable trigger.
    pub const PWM_ENABLE:  usize = 0x08;

    // ── I2C ──────────────────────────────────────────────────────────────────
    /// ESP32-C3 I2C0 controller base (ESP I2C, not DesignWare).
    pub const I2C0_BASE:   usize = 0x6001_3000;
    /// I2C1 does not exist on ESP32-C3; alias to I2C0.
    pub const I2C1_BASE:   usize = 0x6001_3000;

    // ── SYSTIMER ─────────────────────────────────────────────────────────────
    /// ESP32-C3 SYSTIMER base (replaces CLINT mtime).
    pub const SYSTIMER_BASE: usize = 0x6002_3000;

    // ── Watchdog ─────────────────────────────────────────────────────────────
    /// ESP32-C3 Timer Group 0 WDT base (TIMG0 WDT).
    pub const WDT_BASE:    usize = 0x6001_F048;
}
