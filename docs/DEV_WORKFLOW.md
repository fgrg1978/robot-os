# Dev Workflow (DEV01) — Fast iteration on RISC-V boards

This document describes how to iterate on the kernel against a real
VisionFive 2 (JH7110) or BananaPi BPI-F3 (K1) board **without removing
the SD card** for every change.

## TL;DR cycle

```bash
# Once per macOS reboot:
sudo ./scripts/dev_setup_tftp.sh start

# In one terminal (watch the board's serial console live):
./scripts/dev_uart.py

# In another terminal — every code change:
./scripts/dev_deploy.sh           # build + publish + reset.   ~5 seconds.
```

That's it. The board boots the new kernel via TFTP on every reset.

## Why TFTP

With the standard "edit → build → flash SD" loop, each iteration is
3–5 minutes (eject SD, dd to it, re-insert, power on). With ~100
iterations a day during bring-up, that's an entire workday lost to
mechanical handling. The TFTP loop reduces this to ~5 seconds, a
**30–60× speedup**.

The SD card still holds:
- U-Boot proper (immutable from the macOS side)
- `boot.scr` (the boot script — see below)
- `KERN_A.BIN` / `KERN_B.BIN` (OTA slots, fallback if TFTP fails)
- `BOOTMETA`, `CONFIG.INI`

The kernel that *actually runs* during dev comes over the wire.

## One-time setup

### 1. macOS side

#### a) Install pyserial (used by `dev_uart.py` and `dev_reset.sh`)

```bash
pip3 install pyserial
```

#### b) Build U-Boot mkimage (only needed once, to compile `boot.scr`)

```bash
brew install u-boot-tools
```

#### c) Compile `boot.scr` from the source

```bash
mkimage -A riscv -T script -C none -n "robot-os boot" \
        -d boot/boot.scr.cmd boot/boot.scr
```

#### d) Enable macOS TFTP server

```bash
sudo ./scripts/dev_setup_tftp.sh start
```

This loads `com.apple.tftpd` from `/System/Library/LaunchDaemons/tftp.plist`
and ensures `/private/tftpboot` exists with the right perms. Re-run with
`stop` or `status` as needed.

#### e) Confirm Mac IP

The board needs to reach the Mac on UDP/69. If you're using:

- **Ethernet** (recommended) — note the Mac's address with
  `ifconfig en0 inet | grep inet`. The board will get an IP via DHCP if
  there's a router on the segment, or you can configure a static IP on
  the board side.
- **Shared WiFi** — same network as the board's WiFi. Slower, more
  packet loss; use only for casual iteration.

### 2. SD card setup (once per board)

Format the SD card and copy:

```
/                       (FAT32, 1st partition, marked bootable)
├── boot.scr            ← compiled from boot/boot.scr.cmd
├── KERN_A.BIN          ← initial kernel (fallback when TFTP unavailable)
├── KERN_B.BIN          ← second OTA slot
├── BOOTMETA            ← OTA metadata
├── CONFIG.INI          ← runtime config (see crates/config)
└── ... (any U-Boot files for VF2/K1 — see docs/FLASH_PROCEDURE.md)
```

The first time, set the U-Boot environment to read `boot.scr` automatically:

```
=> setenv bootcmd 'load mmc 0:1 ${kernel_addr_r} /boot.scr; source ${kernel_addr_r}'
=> saveenv
```

(Most VF2/K1 distros do this by default — check with `printenv bootcmd` first.)

### 3. UART connection

The board must be reachable via USB-UART for `dev_uart.py` and
`dev_reset.sh`. Pin maps:

#### VisionFive 2 (JH7110)

J1 debug header: pin 6 GND, pin 8 TX (board) → RX (FTDI),
pin 10 RX (board) ← TX (FTDI). 115200 8N1.

#### BPI-F3 (K1)

USB-C debug port — works directly with a USB-C cable to the Mac. Shows
up as `/dev/cu.usbserial-XXXX` automatically.

**Optional: DTR-reset wiring** — solder the FTDI's DTR line to the board's
RESET pin (pulled up). With this in place, `./scripts/dev_reset.sh`
triggers a hardware reset without a power cycle. Without it, use
`--method uboot` to send a `reset` command at the U-Boot prompt (only
works if the board is sitting at the prompt, not running the kernel).

## Daily loop

```bash
# Terminal 1 — log monitor:
./scripts/dev_uart.py

# Terminal 2 — every code change:
./scripts/dev_deploy.sh
```

`dev_deploy.sh` does, in order:

1. `cargo build --release --features vf2` (or k1)
2. `llvm-objcopy -O binary` to a flat kernel binary
3. Atomic copy to `/private/tftpboot/kernel.bin`
4. DTR-pulse reset on the UART

The board, on reset:

1. U-Boot runs `boot.scr`
2. `boot.scr` does `dhcp ${kernel_addr_r} kernel.bin` — this both
   acquires a lease and pulls the kernel via TFTP from the DHCP-supplied
   server (option 66) or from `serverip` env if static.
3. On TFTP failure (3s timeout), falls back to `mmc 0:1 /KERN_A.BIN`.
4. `booti` jumps to the kernel.

`dev_uart.py` shows everything timestamped:

```
[14:32:01.842] dev_uart: connected /dev/cu.usbserial-A50285BI @ 115200
[14:32:02.115] U-Boot SPL ...
[14:32:02.430] [boot.scr] DEV01 — attempting TFTP boot (timeout=3000ms)
[14:32:02.881] Bytes transferred = 487424 (770c0 hex)
[14:32:02.882] [boot.scr] TFTP load OK (487424 bytes)
[14:32:02.883] [boot.scr] booting kernel @ 0x40200000
[14:32:03.012] [KERNEL] Robot OS booting...
```

A copy of the session lives at `build/dev_log/uart_YYYYMMDD_HHMMSS.log`
for post-mortem analysis.

## Troubleshooting

### "TFTP failed AND SD fallback failed"

The board can't reach the Mac. Check:

- `sudo ./scripts/dev_setup_tftp.sh status` — is tftpd loaded?
- `/private/tftpboot/kernel.bin` exists and is readable?
- `tftp localhost` from another Mac terminal works?
- Mac's firewall isn't blocking UDP/69? (System Settings → Network → Firewall)
- Board and Mac on the same L2 segment? (`arp -a` on the Mac should
  show the board's MAC.)
- DHCP is supplying option 66 (TFTP server)? If not, set `serverip` in
  U-Boot env and re-fetch from there.

### "DTR-pulse reset sent" but board doesn't reset

Most boards don't wire DTR to RESET by default. Two options:

1. **Hold a U-Boot prompt:** at startup, hit any key during the auto-boot
   countdown. Then run `./scripts/dev_reset.sh --method uboot`.
2. **Manual power cycle:** unplug + replug power. Slower but always works.
3. **Solder DTR to RESET:** small mod, biggest workflow win for VF2.

### Kernel loads but hangs or panics

The TFTP path doesn't bypass any kernel logic — it's the same binary
you'd flash. Check the UART log for the panic message and stack trace.
For deep debugging, attach `gdb-multiarch` over OpenOCD/JTAG (out of
scope for DEV01).

### "pyserial not installed"

```bash
pip3 install pyserial
```

### TFTP works but boot is slow on first packet

macOS `tftpd` doesn't send TSize/Block-size options by default; the
board falls back to 512-byte blocks. For a 500KB kernel that's ~1000
round trips. Workaround: configure U-Boot env `tftpblocksize=1468` so
the board *requests* large blocks; macOS `tftpd` will honor it.

```
=> setenv tftpblocksize 1468
=> saveenv
```

## See also

- `docs/FLASH_PROCEDURE.md` — first-time SD setup
- `docs/DEPLOY.md` — production OTA flow (different from dev iteration)
- `boot/boot.scr.cmd` — boot script source
- DEV02 (planned) — USB recovery / DFU when even U-Boot is broken
- DEV04 (post-Julio) — fleet OTA deploy with canary + auto-rollback
