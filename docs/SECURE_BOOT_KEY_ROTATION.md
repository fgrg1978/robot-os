# Secure-boot Ed25519 key rotation runbook (OT05)

Operational procedures for the Ed25519 signing key that gates OTA
images via `crates/ota/src/secure_boot.rs`.

## Threat model

- **Private key (`tools/keys/prod_priv.bin`)**: 32-byte raw Ed25519
  seed. Anyone with the file can mint a signature that the kernel
  will trust as long as the matching public key is embedded. Treat
  like an SSH key.  Lives on the maintainer's offline backup +
  whatever signing host you authorise (locally or in a CI vault).
- **Public key (`tools/keys/prod_pub.bin`)**: 32 bytes. Embedded
  into every kernel image at build time via `crates/ota/build.rs`.
  Safe to share publicly — knowing the pubkey doesn't let an
  attacker sign anything.  `.gitignored` only as a hygiene
  convention (so we never accidentally commit the matching
  `_priv.bin`); paste the pubkey hex into release notes.

## Files

| Path                          | Sensitivity     | Backed up?            |
|-------------------------------|-----------------|-----------------------|
| `tools/keys/prod_priv.bin`    | **SECRET**      | offline copy required |
| `tools/keys/prod_pub.bin`     | public          | embedded in kernel    |
| `tools/keys/dev_priv.bin`     | low (lab only)  | regen any time        |
| `tools/keys/dev_pub.bin`      | low             | embedded if no prod   |

## First-deploy: generate the prod key

Run once, on the host you trust to hold the private key:

```sh
python3 tools/gen_prod_key.py
```

Outputs:
- `tools/keys/prod_priv.bin` (mode 0600 — back this up offline NOW).
- `tools/keys/prod_pub.bin` (build.rs embeds this on next build).

The script refuses to overwrite an existing `prod_priv.bin`.  If
you genuinely intend to rotate, see the rotation procedure below.

After generation, paste the public-key hex (printed by the script)
into your release notes / commit message so every consumer of the
kernel knows which key is trusted.

## Build picks it up automatically

`crates/ota/build.rs` reads `tools/keys/prod_pub.bin` on every
build and emits the bytes into `SECURE_BOOT_PUBKEY_BYTES`.  Force
a rebuild with:

```sh
cargo clean -p robot_os_ota
cargo build --release
```

Verify the warning shows the path:

```text
warning: robot_os_ota@0.1.0: secure_boot: embedding prod pubkey from .../tools/keys/prod_pub.bin
```

If you see that warning the prod key is in.  No warning at all
(quiet fallback) means `prod_pub.bin` was missing and the kernel
embedded the all-zero dev key — `BootTrust::Unverified`.

## Signing firmware images

```sh
python3 tools/sign_ota.py --priv tools/keys/prod_priv.bin path/to/kernel.bin
```

Emits `path/to/kernel.bin.sig`.  Deploy both files into the OTA
slot; the kernel looks for `KERN_A.SIG` / `KERN_B.SIG` next to the
image.

## Rotation procedure

**Why rotate**: scheduled hygiene (e.g. yearly), suspected
compromise, change of maintainer, change of signing infrastructure.

**Cost of rotating**: every device in the field that hasn't yet
updated to a kernel embedding the new pubkey will reject firmware
signed with the new key.  Rolling rotation is mandatory — do not
flag-day it.

### Steps

1. **Verify no in-flight rollback dependency.**  Confirm every
   device's `last_good` slot is signed with the CURRENT (old) key
   AND that BOOTMETA points at a "primary" slot also signed with
   the old key.  If half the fleet has the old key in `last_good`
   only, rotating without re-signing those images means a future
   rollback can leave them un-bootable.

2. **Generate the new keypair** on a side path so the old key
   still exists:

   ```sh
   mv tools/keys/prod_priv.bin tools/keys/prod_priv.bin.OLD
   mv tools/keys/prod_pub.bin  tools/keys/prod_pub.bin.OLD
   python3 tools/gen_prod_key.py
   ```

3. **Build a "key-rollover" kernel** — embeds the new pubkey but
   also accepts signatures from `prod_pub.bin.OLD` for a transition
   window.  This requires a small patch to `secure_boot.rs` to
   accept two pubkeys; not implemented as standard tooling because
   it should only ever live in the rollover release.  Document
   which release was the rollover.

4. **OTA the rollover kernel** to the entire fleet via the
   existing Fleet OTA (DEV04) flow.  Wait for 100 % uptake (use
   the brain's `/fleet/status` endpoint).

5. **Build a "new-key-only" kernel** — embeds only the new pubkey.

6. **OTA the new-key-only kernel** to the fleet.  Any device that
   missed step 4 will now reject this update at signature check
   and stay on the old kernel; investigate manually.

7. **Re-sign and re-deploy `KERN_R.BIN` (recovery slot)** with the
   new key.  Until you do, the recovery path is signed with the
   old key and you can't reach it with `prod_priv.bin.OLD` deleted.

8. **Delete or archive `prod_priv.bin.OLD`.** If you suspect the
   old key was compromised, securely shred it (`shred -uvz`).  If
   you're rotating for hygiene, keep the old key offline in case a
   pre-rollover device shows up later (rare but possible — robot
   that's been in a drawer for a year).

### Emergency rotation (private key suspected compromised)

Skip the transition window (steps 3-4).  Risk: any device that
isn't network-reachable becomes a brick until you can physically
reflash the recovery slot.  Acceptable only if the attack risk
exceeds the brick risk.

1. `shred -uvz tools/keys/prod_priv.bin` (the compromised key).
2. Generate the new keypair (`gen_prod_key.py`).
3. Force-push the new pubkey to all reachable devices via OTA.
4. Any unreachable device: physical reflash with USB recovery
   (DEV02) or SD swap.

## Restoring from backup

If the dev/build host loses `prod_priv.bin` (disk failure, etc.):

1. Restore `prod_priv.bin` AND `prod_pub.bin` together from your
   offline backup.  The pubkey backup is what proves you're
   restoring the same key the fleet trusts — a fresh `gen_prod_key`
   produces a different pubkey and bricks every device.
2. `chmod 600 tools/keys/prod_priv.bin`.
3. Rebuild the kernel; verify the `cargo:warning` shows the
   restored pubkey path.

## Key fingerprint discipline

Every release should record the public key fingerprint so future
incident response can tell which key was authoritative at which
point.  Easy form:

```sh
shasum -a 256 tools/keys/prod_pub.bin
```

Paste the first 16 hex chars into `CHANGELOG.md` next to the
version tag.  Cheap, immutable, and lets a kernel binary be
traced back to its signing key without unpacking the embedded
bytes.
