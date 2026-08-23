# RFC-0011: Secure Boot & Anti-Tamper (Hardware Root of Trust)

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-08-20


> **Status audit 2026-08-20.** Confirmed ACCURATE, and worth restating because
> it has been misread more than once: the Ed25519 software verification this
> RFC lists as "Already in place" really is in place
> (`crates/ota/src/secure_boot.rs`, pubkey embedded at build time by
> `crates/ota/build.rs`). What this RFC actually leaves undelivered is the
> **hardware** root of trust — Layers A–G: boot-ROM verify, TF-A/U-Boot chain,
> OTP/eFuse anchoring, encrypted flash, secure element, tamper response,
> remote attestation. None of that exists in the tree, which is what `accepted`
> means here. Treating "secure boot" as one undifferentiated item is what
> caused the earlier confusion.

## Summary

PHANES adopts a hardware-rooted chain of trust from boot ROM through
the application kernel, with anti-tamper protections that make
trivial reflashing of a deployed robot impractical without physical
access to the trusted-key material. Seven layers (A–G) compose: SoC
boot ROM signature verification, signed boot chain (TF-A → U-Boot
SPL → U-Boot → kernel), OTP-stored keys, encrypted flash, secure
element, tamper response, and remote attestation.

## Motivation

Software-only secure boot (the current `crates/ota/src/secure_boot.rs`)
is necessary but not sufficient: an attacker with physical access can
overwrite the kernel binary and replace the embedded public key.
Defence in depth requires hardware participation.

For PHANES to be:

- **Cert-eligible** (ISO 26262 + ISO/SAE 21434 require HW-ROT).
- **Field-deployable** (robots leave the lab; physical tamper is real).
- **EU CRA / EU AI Act compliant** (HW-ROT becomes mandatory 2027+).
- **Trusted by automotive Tier-2** (they audit the boot chain).

we need each link of the chain rooted in immutable silicon, not in
mutable flash.

## Detailed design — seven layers

### Layer A — SoC boot ROM verification

The first executed code is the SoC's mask ROM (immutable; cannot be
modified post-fabrication). It verifies the next stage's signature
against a key stored in OTP / eFuse.

| SoC | Mechanism | Status |
|-----|-----------|--------|
| NXP i.MX 8M family | HABv4 (well-documented; reference impl) | **Phase 1 reference** |
| StarFive JH7110 (VF2) | StarFive Secure Boot via OTP | Phase 2 (docs limited) |
| SpacemiT K1 (BPI-F3) | SpacemiT secure boot via OTP burns | Phase 2 |
| Rockchip RK3588 | Rockchip secure boot loader | Phase 2 |
| STM32MP1 | OEM secure boot + ROP fuses | Phase 3 (if customer) |
| ARM (generic Cortex-A) | TF-A is the canonical reference | Phase 1 (alongside NXP) |

We adopt **NXP i.MX 8M Plus** as the Phase 1 reference platform
because: HABv4 is the most-documented implementation; Yocto and
Buildroot recipes already exist; broad enterprise familiarity.

### Layer B — Authenticated boot chain

Each link verifies the next before transferring control:

```
HW BROM (mask ROM, immutable)
  └─ verifies → TF-A / OpenSBI hardened
                   └─ verifies → U-Boot SPL
                                    └─ verifies → U-Boot
                                                     └─ verifies → PHANES kernel.bin
                                                                        └─ falls back → KERN_R.BIN (if A/B fail)
```

PHANES contributes:

- **TF-A integration** for ARM platforms (we don't write TF-A; we
  configure it with our root keys and signed FIP container).
- **Hardened OpenSBI** for RV64 platforms (with PMP early-init from
  RFC where we may use no-OpenSBI direct boot).
- **Kernel signature verification** at each transition (already in
  `secure_boot.rs`; extended to the chain).
- **Recovery slot** (`KERN_R.BIN`, RFC-0005's `[OTA]` integration):
  if both A and B slot kernels fail signature, U-Boot tries the
  immutable recovery slot.

### Layer C — OTP / eFuse storage (one-way write)

Burned at factory line, immutable thereafter:

| Resource | Width | Purpose |
|----------|-------|---------|
| Public key (Ed25519) | 256 bits | Trusted signature anchor |
| Anti-rollback counter | 16–32 bits | Increment-only; old fw refused |
| Device unique ID | 128 bits | Per-robot identity |
| Debug-disable | 1 bit | Permanently off-line JTAG/SWD |
| Encrypted-flash key | 128 bits | AES key for XIP flash decrypt |

**Provisioning workflow** (factory line; documented in a follow-up
deployment RFC):

1. HSM-protected master key signs unique device key.
2. OTP burner writes public anchor + device ID + debug-disable +
   AES-flash key.
3. Production test: verify signature path, rollback monotonic, debug
   disabled.
4. Logged provenance (which device shipped with which key) signed
   into supply chain SBOM (RFC-0012).

### Layer D — Encrypted flash (Execute-In-Place encrypted)

On supported SoCs, the flash content is encrypted; decryption uses
the OTP-stored AES key. Even if the flash chip is physically removed
and read, the content is ciphertext.

| SoC | XIP encrypted flash support |
|-----|-----------------------------|
| i.MX HABv4 + CAAM | ✅ Phase 1 |
| STM32MP1 SECURABLE memory | ✅ Phase 2 |
| K1 / RK3588 (AHB encryption engine) | ✅ Phase 2 |
| JH7110 (VF2) | limited; software AES on read | Phase 3 |

### Layer E — Secure Element (~$1–3 BOM)

Commercial off-the-shelf chips that store keys in tamper-resistant
silicon and perform crypto operations without exposing keys:

- Microchip **ATECC608A** — Ed25519, ECDSA, AES, HMAC. Mature.
- Infineon **OPTIGA Trust M** — RSA, ECC, side-channel resistant.
- NXP **A71CH** — TLS, key protection.

PHANES exposes a `SecureElement` trait following the modular
pattern (RFC-0002):

```rust
// crates/drivers/src/se/api.rs
pub trait SecureElement {
    fn device_id(&self) -> [u8; 16];
    fn sign(&self, key_id: u8, data: &[u8]) -> Result<Signature, SeErr>;
    fn random(&self, out: &mut [u8]) -> Result<(), SeErr>;
    fn verify(&self, key_id: u8, sig: &Signature, data: &[u8]) -> Result<(), SeErr>;
}
```

Per-vendor implementations under `crates/drivers/src/se/impls/`.

### Layer F — Tamper detection and response

Detection sources:

- **JTAG / SWD line activation** monitor (some SoCs expose a tamper
  pin).
- **Case-open switch** (microswitch or hall sensor on chassis).
- **Voltage glitch detection** (PMIC supports under/over-voltage
  brown-out).
- **Watchdog mismatch** (kernel doesn't kick → reset → forensics
  log).
- **OTP read-back integrity** (read what was burned; mismatch =
  tampered or hardware fault).

Response policies (configurable in `SCHED.TOML` and per-deployment):

| Policy | Behaviour |
|--------|-----------|
| `brick` | Erase derived key in OTP-shadow → robot inoperable until factory recovery |
| `diminished` | Disable motors + payload; keep comms for forensics |
| `log_only` | Append to immutable audit log; no operational change |
| `attest_alert` | Send tamper signal to fleet brain via secure channel |

For a hobby robot the right default is `log_only` + `attest_alert`.
For an automotive ECU, `brick` may be required.

### Layer G — Remote attestation (Phase 3)

The robot proves to the fleet brain (or operator) that it is running
known-good firmware:

```
Fleet → Robot: "send attestation"
Robot computes: PCR-style hash of (TF-A || U-Boot || kernel || topology)
Robot signs:    sig = SE.sign(device_key, hash || nonce)
Robot replies:  (hash, sig)
Fleet verifies: device_key recognised + hash matches known-good list
```

Implementation uses TPM 2.0 if available, or software emulation via
the Secure Element. The hash chain is computed at boot and stored
in PCR-equivalent registers.

## Integration with existing PHANES code

| Existing code | Status |
|---------------|--------|
| `crates/ota/src/secure_boot.rs` (Ed25519 sig verify) | Already in place; this RFC extends to the chain. |
| `tools/gen_prod_key.py` (key generation) | Already in place; extend to generate OTP-burnable artefacts. |
| `tools/sign_ota.py` | Already in place; extend with TF-A signing flow. |
| `crates/ota/build.rs` (embed pubkey) | Already in place; key still embedded for in-kernel verification, but the *anchor* is OTP, not flash. |
| Recovery slot `KERN_R.BIN` (OT04) | Already in place; RFC-0011 makes it the BROM-fallback path. |

## SoC support roadmap

| Phase | SoC | What's delivered |
|-------|-----|------------------|
| 1 | NXP i.MX 8M Plus | Full HABv4 + TF-A + signed boot + KERN_R.BIN fallback |
| 1 | RV64 (VF2 / K1) | TF-A-equivalent (hardened OpenSBI) + signed boot |
| 2 | RK3588 | Rockchip secure boot integration |
| 2 | OTP provisioning tooling | Factory-line workflow + HSM integration |
| 2 | ATECC608A driver + reference impl | Layer E, smallest BOM impact |
| 2 | Encrypted flash for i.MX, STM32MP1 | Layer D |
| 3 | Tamper detection + response policies | Layer F + topology config |
| 3 | Remote attestation protocol | Layer G + fleet-brain verification |

## Drawbacks

- **Per-SoC engineering effort.** Each new SoC takes 2–3
  ing-months for the full chain. Pragmatic limit: 3–4 reference
  SoCs in Phase 1–2, more on customer demand.
- **Factory-line provisioning is a real operations cost.** HSM
  setup, key custody, audit, training. Industry-standard but not
  zero.
- **Recovery vs. anti-tamper tension.** If JTAG is permanently
  fused off, debugging field failures is hard. Mitigation: a
  documented "RMA recovery" workflow with controlled re-flashing
  via a vendor key (signed with a separate anchor).
- **Documentation is uneven across SoCs.** JH7110 / K1 docs are
  worse than NXP / ARM. We start with the well-documented
  platforms and write portable docs ourselves for the rest.

## Rationale and alternatives

**Alternative A — software-only secure boot.** Insufficient for
cert. Rejected.

**Alternative B — license a commercial vendor's secure boot.** Cost
+ vendor-lock + closed-source. Incompatible with foundation hosting.
Rejected.

**Alternative C (chosen) — open-source standard layers (TF-A,
HABv4, Ed25519) + per-SoC integration.** Industry-aligned,
reproducible, auditable.

## Prior art

- **TF-A (Trusted Firmware-A)** — ARM-canonical secure boot
  reference. Open-source, multi-vendor. We adopt as ARM standard.
- **NXP HABv4** — well-documented implementation; Phase 1 model.
- **iPhone secure boot** — gold standard for tamper-resistant
  consumer devices. Inspires the chain shape.
- **Tesla vehicle security** — open writeups on signed boot, OTA,
  tamper. Inspires automotive-grade thinking.
- **PSA Certified** — ARM's security certification framework. We'll
  target compliance.
- **TCG TPM 2.0 specs** — for the attestation layer (G).
- **OpenSSF / Sigstore** — for non-HW supply chain tie-in.

## Unresolved questions

- **OTP rotation policy.** What if the trusted public key is
  compromised? Working assumption: per-device sub-key derivation
  from a master OTP value, with revocation lists. Detailed in a
  follow-up RFC during Phase 2.
- **Recovery key escrow.** Which entity holds the recovery /
  master key? Working assumption: split via Shamir Secret Sharing
  among PHANES Foundation officers + customer ops. Decided per
  customer, documented in deployment RFC.
- **Tamper response default.** `log_only` for hobby, `brick` for
  automotive — but what's the right default for "general"
  deployment? Working assumption: `diminished` (motors off, comms
  on) — safest middle.

## Future possibilities

- **Phase 3:** PSA Certified Level 2 / 3 attainment.
- **Phase 4:** TPM 2.0 integration for desktop / server-class
  PHANES deployments.
- **Phase 4:** Quantum-safe signature scheme migration (post-
  quantum Ed25519 successor; track NIST PQC standardisation).
- **Phase 5:** Hardware security partnership with a SoC vendor for
  PHANES-optimised silicon.
