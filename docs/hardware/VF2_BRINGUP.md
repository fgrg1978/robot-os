# VisionFive 2 — Bring-up Data (KernOS)

Hardware reference for the post-QEMU phase. Official StarFive sources
(rvspace.org), collected 2026-08-23.

## Documents

- **40-Pin GPIO Header User Guide** — local copy:
  [`VisionFive2_40-Pin_GPIO_Header_UG.pdf`](VisionFive2_40-Pin_GPIO_Header_UG.pdf)
  (source: <https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_40-Pin_GPIO_Header_UG.pdf>).
  40-pin header pinout: this is the source for mapping gpio/pwm/i2c drivers
  to physical pins when we leave QEMU.
- **Boot Mode Settings** —
  <https://doc-en.rvspace.org/VisionFive2/Boot_UG/VisionFive2_SDK_QSG/boot_mode_settings.html>
  (table replicated below in case the page moves).

## Boot Modes (Switch RGPIO_1 / RGPIO_0)

| # | Boot Mode             | RGPIO_1 | RGPIO_0 |
|---|-----------------------|---------|---------|
| 1 | 1-bit QSPI Nor Flash  | 0 (L)   | 0 (L)   |
| 2 | SDIO 3.0 (SD card)    | 0 (L)   | 1 (H)   |
| 3 | eMMC                  | 1 (H)   | 0 (L)   |
| 4 | UART                  | 1 (H)   | 1 (H)   |

StarFive Notes:

- They recommend **QSPI Nor Flash** (mode 1): eMMC and SDIO have "low
  probability" of boot failure; if it happens, restart the board.
- Switch silkscreen **varies between board revisions** — verify
  against the figures on the page, not blind against silkscreen.

## Known Reminders From Tree (Don't Repeat Diagnosis)

- **Hart enumeration**: S7 (without application MMU) as hart 0, the four
  U74s as 1..4 — 5 harts for `MAX_CPUS = 4`. The clamps from 23-08 make it
  safe (boot hart out of range = FATAL+halt with message; secondaries
  parked), but physical→logical mapping is still work for this phase
  (`KERNEL_REVIEW_NOTES`).
- **Console UART** and VF2 linker script go via `RUSTFLAGS` in the
  Makefile (not via `.cargo/config.toml`).
- WCET dimensioned to VF2 DVFS floor (375 MHz) — see
  `IPC_AUDIT_2026-08-22.md` § lease_tick.
