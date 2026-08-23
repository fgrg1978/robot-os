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
#[cfg(not(any(feature = "vf2", feature = "k1")))]
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

    /// QEMU `fw_cfg` MMIO-DMA interface, RISC-V `virt` machine. Confirmed
    /// against QEMU mainline `hw/riscv/virt.c`'s `virt_memmap[VIRT_FW_CFG]`
    /// (`{ 0x10100000, 0x18 }`) — QEMU's own emulated device, not JH7110
    /// hardware; used only by the `crates/display` `ramfb` module (behind
    /// `--features qemu` there) to test the general "can this kernel drive
    /// *a* framebuffer" question in QEMU, entirely separate from the real
    /// DC8200/HDMI TX driver, which QEMU cannot simulate at all.
    pub const FW_CFG_BASE: usize = 0x1010_0000;
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
    /// DW-WDT "core" clock (`wdt->core_clk` in Linux mainline
    /// `drivers/watchdog/starfive-wdt.c`) — the one that actually feeds the
    /// timeout-to-ticks computation (`count = timeout * clk_get_rate(core_clk)`).
    /// NOT the same as TIMER_FREQ (RISC-V mtime clock), and NOT the WDT's
    /// "apb" clock either (that one is register-access-only, per the same
    /// driver — irrelevant to timing). Confirmed via
    /// `clk-starfive-jh7110-sys.c`'s clock tree: `wdt_core` is gated
    /// directly off `JH7110_SYSCLK_OSC` (the 24 MHz crystal oscillator),
    /// bypassing the APB_BUS/STG_AXIAHB divider chain entirely — so unlike
    /// a generic "APB clock ≈ 24 MHz" guess, this is a direct,
    /// undivided connection with no PLL/divider uncertainty in between.
    pub const WDT_CLK_HZ:  u64   = 24_000_000;
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
    /// JH7110 PWM8 controller (sifive,pwm-v0 compatible, 4 real channels).
    /// Was `0x1703_0000` — wrong by a full 64 KiB region (that address is
    /// actually where DC8200's NOC/clock-reset block lives, per
    /// `DC8200_NOC_BASE` below — a real, previously-undetected mismapping,
    /// not just "PWM doesn't work" but "PWM writes land in the display
    /// controller's clock config on real silicon"). Corrected against
    /// Linux mainline `jh7110.dtsi` (`pwm@120d0000`, confirmed 2026-08 —
    /// same fetch that grounded the register-model fix in
    /// `crates/drivers/src/pwm.rs`, which fixed the register LAYOUT but
    /// never re-verified this base address). `PWM_STRIDE`/`PWM_DUTY`/
    /// `PWM_PERIOD`/`PWM_ENABLE` removed — dead since that same fix
    /// replaced the per-channel-block addressing model they described
    /// with the real shared-PWMCFG/per-channel-PWMCMP one (see
    /// `crates/drivers/src/pwm.rs`'s `mmio` module, which defines its own
    /// register offsets now, not sourced from `platform::hw`).
    pub const PWM_BASE:    usize = 0x120D_0000;

    // ── I2C (DesignWare APB I2C) ──────────────────────────────────────────────
    /// I2C0 base (DesignWare APB I2C, 400 kHz).
    // JH7110 i2c0 @ 0x10030000 (Linux mainline jh7110.dtsi) — was aliased to UART1_BASE, fixed 2026-08.
    pub const I2C0_BASE:   usize = 0x1003_0000;
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

    // ── Display (Verisilicon DC8200 + Innosilicon HDMI TX) ──────────────────
    // Confirmed against Linux mainline device tree source
    // (jh7110-common.dtsi / jh7110.dtsi, starfive-tech vendor fork,
    // JH7110_VisionFive2_devel branch), 2026-08. HDMI-only path — the SoC
    // also has MIPI DSI/D-PHY outputs, not used here, not wired.
    /// DC8200 top-level/config block (chip ID, top-level IRQ ack/enable).
    pub const DC8200_TOP_BASE:   usize = 0x2940_0000;
    /// DC8200 main register block — CRTC timing, plane/framebuffer config.
    /// This is the block `DC8200_*` register offsets in `crates/display`
    /// are relative to.
    pub const DC8200_MAIN_BASE:  usize = 0x2940_0800;
    /// DC8200 NOC/clock-reset block (not used by this driver directly —
    /// clock gating for the display pipeline is assumed already enabled by
    /// U-Boot/OpenSBI at boot, same assumption this whole driver makes
    /// about not owning JH7110's clock tree — see the module doc comment
    /// in `crates/display/src/lib.rs`).
    pub const DC8200_NOC_BASE:   usize = 0x1703_0000;
    /// Innosilicon HDMI TX — register interface is byte-addressed (8-bit
    /// registers), unlike DC8200's word-aligned layout — see
    /// `crates/display/src/hdmi.rs`.
    pub const HDMI_TX_BASE:      usize = 0x2959_0000;
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
    /// SpacemiT K1 is a different vendor/SoC from the JH7110 (StarFive) —
    /// the JH7110 clock-tree research that confirmed `WDT_CLK_HZ` for the
    /// vf2 block above (see its doc comment) does NOT transfer here; K1's
    /// own WDT IP and clock tree haven't been traced from primary source.
    /// This value is provisional (matches TIMER_FREQ, which is a real DTS
    /// value, but that's not evidence the WDT's own core clock is the
    /// same rate) pending real hardware/TRM access for K1 specifically.
    pub const WDT_CLK_HZ:  u64   = 24_000_000;
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
