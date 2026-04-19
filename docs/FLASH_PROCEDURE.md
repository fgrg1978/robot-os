# Flash Procedure — VisionFive 2

How to build, (optionally) sign, and install the `robot-os` kernel on a
VisionFive 2 SBC. Works for first-time flashing and for subsequent
manual updates. For over-the-air updates, see [`DEPLOY.md`](./DEPLOY.md).

---

## 0. Prerequisites

**On host (macOS/Linux):**

- `cargo` + `rustc` with `riscv64imac-unknown-none-elf` target:
  ```
  rustup target add riscv64imac-unknown-none-elf
  ```
- `llvm-objcopy` (via `brew install llvm` on macOS).
- `mkfs.fat` (via `brew install dosfstools` on macOS, or `dosfstools` package).
- `cryptography` Python package (only if using F18 secure boot):
  ```
  pip install cryptography
  ```

**On VisionFive 2:**

- U-Boot flashed to SPI (per board manual).
- USB-UART adapter for console.
- MicroSD card ≥8 GB.

---

## 1. Build the kernel

From the repo root:

```
/bin/zsh scripts/build.sh
```

This produces `target/riscv64imac-unknown-none-elf/release/robot_os_kernel`
(ELF format) for each of the 5 configurations:

| Config | Command | Target |
|--------|---------|--------|
| default | `cargo build --release` | QEMU virt |
| vf2 | `cargo build --release --features vf2` | VisionFive 2 (JH7110) |
| k1 | `cargo build --release --features k1` | SpacemiT K1 |
| no-ml | `cargo build --release --features no-ml` | Boards without ML |
| no-mmu | `cargo build --release --features no-mmu` | ESP32-C3 |

For VisionFive 2 specifically:

```
$HOME/.cargo/bin/cargo build --release --features vf2
```

Output ELF: `target/riscv64imac-unknown-none-elf/release/robot_os_kernel`

Convert to raw binary for U-Boot:

```
/opt/homebrew/opt/llvm/bin/llvm-objcopy -O binary \
  target/riscv64imac-unknown-none-elf/release/robot_os_kernel \
  target/kernel.bin
```

---

## 2. (Optional) Sign with F18 secure boot

Only required if the kernel has `SECURE_BOOT_PUBKEY` embedded (production
builds) and you've set `CFG_SECURE_BOOT_REQUIRE_SIG=1`.

```
# One-time key generation (dev key)
python3 tools/gen_dev_key.py

# Sign the binary
python3 tools/sign_ota.py target/kernel.bin \
  --priv tools/keys/dev_priv.bin \
  --out  target/kernel.sig
```

Paste the contents of `tools/keys/dev_pub.rs` into
`crates/ota/src/secure_boot.rs` (the `SECURE_BOOT_PUBKEY` static array).
Rebuild so the trusted pubkey is embedded.

For production: use a private key stored offline (never in the repo).
Key rotation: sign two images, distribute them together, switch trusted
pubkey via a signed pubkey-rotation OTA.

---

## 3. Prepare the MicroSD card

Partition:
- **Partition 1 (FAT32)**: 512 MB — holds kernel images + BOOTMETA + logs
- Rest: unused for v1

On macOS:

```
# Find the SD (DO NOT guess — use `diskutil list` to confirm)
diskutil list

# Example: the SD is /dev/disk4
/usr/sbin/diskutil eraseDisk FAT32 ROBOT_FAT MBRFormat /dev/disk4
```

On Linux:

```
sudo fdisk /dev/sdX    # create primary partition, type W95 FAT32
sudo mkfs.vfat -F 32 -n ROBOT_FAT /dev/sdX1
```

After partitioning, mount the partition and verify it shows as `/Volumes/ROBOT_FAT`
(macOS) or `/mnt/ROBOT_FAT` (Linux).

---

## 4. Copy files to SD

Copy the kernel and (if signed) the signature:

```
cp target/kernel.bin /Volumes/ROBOT_FAT/KERN_A.BIN
cp target/kernel.sig /Volumes/ROBOT_FAT/KERN_A.SIG    # optional
```

Create the initial `BOOTMETA`:

```
cat > /Volumes/ROBOT_FAT/BOOTMETA <<EOF
active_slot=a
boot_count=0
last_good=a
fw_version_a=1
fw_version_b=0
image_size_a=$(stat -f%z target/kernel.bin)
image_size_b=0
image_crc_a=$(python3 -c "import zlib; print(zlib.crc32(open('target/kernel.bin','rb').read()))")
image_crc_b=0
EOF
```

**Safely eject** the SD card before unplugging:

```
/usr/sbin/diskutil eject /dev/disk4
```

---

## 5. First boot

1. Insert the SD card into VisionFive 2.
2. Connect the USB-UART adapter to the console pins (TX=GPIO44, RX=GPIO45).
3. Open a serial terminal at **115200 baud**:
   - macOS: `screen /dev/tty.usbserial-* 115200`
   - Linux: `screen /dev/ttyUSB0 115200`
4. Power on.
5. Watch for the U-Boot prompt. If DIP switches are set for SD boot, U-Boot
   loads `KERN_A.BIN` automatically.
6. The kernel banner `[KERNEL] robot-os vX.Y.Z ready — mode: idle` should
   appear within 2-3 seconds.
7. At the shell prompt, run:
   - `status` — hardware summary
   - `sensors` — IMU, rangefinders, GPS (if connected)
   - `net info` — MAC / IP
   - `fleet info` — logger state

If the kernel panics or hangs, see [Troubleshooting](#troubleshooting).

---

## 6. Subsequent updates — two paths

### 6.1 Physical (SD card pull-out)

1. Rebuild + sign (steps 1-2 above).
2. Power off the robot.
3. Remove SD card, mount on host.
4. Copy the **inactive slot** (KERN_B.BIN if A is active, vice versa).
5. Update `BOOTMETA`: set `active_slot=b` (or `a`), bump version, compute new
   CRC, set `boot_count=0`.
6. Eject, re-insert into robot, power on.
7. On first boot the kernel runs for `OTA_BOOT_GOOD_DELAY_S` (30 s) then
   marks the new slot as `last_good`. If it panics before then, the
   watchdog + boot counter rolls back automatically.

### 6.2 Over-the-air (OTA)

See [`DEPLOY.md`](./DEPLOY.md).

---

## 7. Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| No console output | Wrong baud or bad pins | Try 115200 baud, verify TX/RX not swapped |
| U-Boot prompt but kernel doesn't load | `KERN_A.BIN` missing or SD corruption | Re-flash SD; check `ls mmc 0:1` from U-Boot |
| Boot loop | `CRASH_BOOT_LOOP_THRESHOLD` reached → safe mode | Kernel panic log is in `/LOG/` on SD; pull card, read log, fix bug |
| Kernel panics "PMP violation" | Memory map mismatch with DTB | Verify DTB matches board (VF2 vs QEMU) |
| Kernel boots but shell unresponsive | UART interrupt not firing | Check PLIC config for feature set (vf2 / k1) |
| `secure boot: failed` | `SECURE_BOOT_PUBKEY` doesn't match signing key | Regenerate sig with matching priv key, or zero `SECURE_BOOT_PUBKEY` to disable |
| Motors don't respond | Safety layer active | `safety status` — check for violations; battery OK? |

---

## 8. Recovery

If the robot won't boot at all and neither slot works:

1. Remove SD card.
2. On host, overwrite `KERN_A.BIN` with a known-good build.
3. Reset `BOOTMETA`:
   ```
   active_slot=a
   boot_count=0
   last_good=a
   ```
4. Re-insert, boot. The kernel should come up in safe mode if the bad image
   crashed repeatedly; otherwise normal boot.

**Last resort**: reflash U-Boot to SPI from a working SD card.

---

## 9. Appendix — Build matrix verification

After any invasive change, run the full build matrix to catch breakage early:

```
/bin/zsh scripts/build.sh
```

Expected output:

```
=== QEMU (default) ===
    Finished `release` profile ... in X.Xs
=== vf2 ===
    Finished `release` profile ... in X.Xs
=== k1 ===
    Finished `release` profile ... in X.Xs
=== no-ml ===
    Finished `release` profile ... in X.Xs
=== no-mmu ===
    Finished `release` profile ... in X.Xs
All 5 configs built.
```

Any errors/warnings must be fixed before flashing.
