# Deploy / OTA Workflow

How to push an updated kernel to a running robot without touching the SD card.

Uses the existing OTA infrastructure (`crates/ota/`) + F18 secure boot signing.

---

## 0. One-time setup

**On the robot (first flash only):**

1. Flash the initial kernel per [`FLASH_PROCEDURE.md`](./FLASH_PROCEDURE.md).
2. Ensure WiFi/ESP32 bridge is connected and the robot can reach the brain server.
3. Verify the OTA slot layout is present on SD:
   - `/fat/KERN_A.BIN` — current active kernel
   - `/fat/KERN_B.BIN` — empty or previous version
   - `/fat/BOOTMETA` — slot metadata

**On the host:**

1. Generate a signing key pair (once):
   ```
   python3 tools/gen_dev_key.py
   ```
2. Paste the contents of `tools/keys/dev_pub.rs` into
   `crates/ota/src/secure_boot.rs` (replace the `SECURE_BOOT_PUBKEY` static).
3. Rebuild the kernel with the pubkey embedded.
4. Flash this signed-aware build to the robot once.

Subsequent OTA updates will be signed and verified automatically.

---

## 1. Quick workflow (normal update)

From the repo root, using the deploy script:

```
/bin/zsh scripts/deploy.sh <robot-ip>
```

This:
1. Builds the `vf2` target.
2. Strips to raw `.bin` via `llvm-objcopy`.
3. Signs with the dev private key → `.sig` sidecar.
4. Pushes the signed image + sig to the robot via OTA protocol.
5. Watches the robot's boot log — if it reboots cleanly, marks slot as good.
6. If the robot boot-loops, rolls back automatically after `boot_count >= OTA_DEFAULT_MAX_BOOT_ATTEMPTS` (3).

Typical run time: ~45 s (build ~15 s, transfer ~5 s, watch-boot ~25 s).

---

## 2. Manual workflow (step-by-step)

Use this when debugging or when `deploy.sh` isn't suitable.

### Step A — Build

```
cd $REPO_ROOT
$HOME/.cargo/bin/cargo build --release --features vf2
/opt/homebrew/opt/llvm/bin/llvm-objcopy -O binary \
  target/riscv64imac-unknown-none-elf/release/robot_os_kernel \
  target/kernel.bin
```

### Step B — Sign (F18)

```
python3 tools/sign_ota.py target/kernel.bin \
  --priv tools/keys/dev_priv.bin \
  --out  target/kernel.sig
```

### Step C — Upload via brain server

The brain server exposes an OTA endpoint (see `api.py`):

```
curl -X POST http://$ROBOT_IP:8080/ota/upload \
  -F "kernel=@target/kernel.bin" \
  -F "signature=@target/kernel.sig" \
  -F "platform=vf2"
```

The brain forwards the image to the robot over TCP using the packet sequence:
`OTA_BEGIN` (0x84) → N × `OTA_CHUNK` (0x85) → `OTA_END` (0x86).

The robot:
1. Writes chunks to the **inactive** slot (if A is active → write to B).
2. Verifies the full-file CRC-32 from `OTA_END`.
3. If signature attached: verifies Ed25519 against `SECURE_BOOT_PUBKEY`.
4. On success, updates `BOOTMETA` to make the new slot active, resets
   `boot_count=0`.
5. Sends `OTA_ACK` (0x04) → brain logs result.
6. Kernel reboots (via soft reset or watchdog).

### Step D — Watch the boot

Tail the brain log:

```
tail -f /var/log/brain/server.log
```

Expect:
```
[OTA] robot R1 slot A → B complete
[OTA] robot R1 rebooting
[OTA] robot R1 reconnected, slot B active
[OTA] robot R1 boot_count=0 (marked good after 30s)
```

If the robot fails to reconnect within `OTA_DEFAULT_MAX_BOOT_ATTEMPTS × WDT_TIMEOUT_S`,
it auto-rolls back to the previous `last_good` slot.

---

## 3. Rollback (manual)

If the new image is bad but the robot is still reachable:

```
curl -X POST http://$ROBOT_IP:8080/ota/rollback
```

This forces `BOOTMETA.active_slot = last_good` and reboots.

If the robot is bricked (doesn't reconnect), pull the SD card and edit
`BOOTMETA` manually per [`FLASH_PROCEDURE.md § 8`](./FLASH_PROCEDURE.md#8-recovery).

---

## 4. Key management

**Dev key** (`tools/keys/dev_priv.bin`):
- Generated once, committed to `.gitignore` (never pushed).
- Paired pubkey compiled into `SECURE_BOOT_PUBKEY`.
- Valid for all dev deploys; replace before production.

**Production key**:
- Generate on an **offline** host.
- Store the private key on a hardware security module (HSM) or offline USB.
- Replace `SECURE_BOOT_PUBKEY` in the kernel source with the production pubkey.
- Rebuild + flash the kernel once (this is the "trust anchor" flash).
- All subsequent OTAs must be signed with this key.

**Key rotation**:
- Build a transitional kernel that accepts EITHER the old or the new pubkey.
- Deploy the transitional kernel via OTA (signed with the OLD key).
- Once confirmed running, deploy the final kernel (signed with the NEW key only).
- Sweep the old key after a grace period.

---

## 5. CI integration

The `.github/workflows/deploy.yml` (future) can automate:

1. On merge to `main`:
   - Run `scripts/build.sh` (5 configs) — must pass.
   - Run `cargo test` — must pass (host targets).
   - Run brain pytest — must pass.
2. On tag `vX.Y.Z`:
   - Build `vf2` release.
   - Sign with production key (stored as GitHub secret).
   - Publish `.bin` + `.sig` as release artifacts.

Manual deploys to test robots pull from the artifact URL.

---

## 6. Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `OTA_ACK` error code `1` (bad CRC) | Transfer corruption | Re-upload; check network quality |
| `OTA_ACK` error code `2` (bad signature) | Key mismatch or unsigned | Regenerate sig with matching priv key |
| Robot doesn't reconnect after OTA | New kernel panics | Wait 3× boot cycles → auto-rollback kicks in |
| Auto-rollback didn't trigger | Watchdog disabled in build | Check `wdt_init` is called; verify `WDT_TIMEOUT_MS` |
| `BOOTMETA` corrupted | Power loss during OTA write | Boot from pinned `last_good`; rewrite `BOOTMETA` |
