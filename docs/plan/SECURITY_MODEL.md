# PHANES — Security Model

> **Audience:** security engineers, auditors, integrators  
> **Pre-requisites:** RFC-0003 (caps), RFC-0011 (secure boot), RFC-0012 (supply chain)  
> **Last updated:** 2026-05-10

This document specifies the threat model PHANES defends against,
the security properties it guarantees, the invariants the kernel
enforces, and the boundaries beyond which security is the
deployer's responsibility.

---

## 1. Adversaries — who we defend against

We model threats by adversary capability, not by attacker
identity:

| Adversary | Capability | In scope? |
|-----------|------------|-----------|
| **A1 — Remote network attacker** | Can send arbitrary packets to the robot's network interfaces (TCP, UDP, link-layer). | ✅ Phase 1 |
| **A2 — Compromised brain / fleet operator** | Can issue arbitrary commands over the brain↔kernel link. | ✅ Phase 1 |
| **A3 — Compromised user-space task** | Holds capabilities for one set of resources; attempts to access others. | ✅ Phase 1 |
| **A4 — Local but unprivileged user** | Can SSH or shell into a deployed robot's user account; cannot write to flash. | ✅ Phase 2 |
| **A5 — Physical attacker (transient)** | Has the device for minutes; tries firmware extraction or signed-image swap. | ✅ Phase 2 (HW-ROT) |
| **A6 — Physical attacker (persistent)** | Has the device for hours/days; capable lab; tries glitching, side-channel. | ⚠️ Phase 3 (best-effort; cert-customer scope) |
| **A7 — Supply chain attacker** | Compromises an upstream Rust crate or Python package. | ✅ Phase 1 (RFC-0012) |
| **A8 — Insider on PHANES Foundation** | Pushes malicious code to PHANES upstream. | ✅ Phase 1 (RFC-0009 + RFC-0012) |
| **A9 — Nation-state with full silicon access** | Modifies SoC mask ROM. | ❌ Out of scope |
| **A10 — User of `myrobots-stack`** | Operates within their own private repo. | ❌ Their responsibility |

PHANES defends from A1 through A8. A9 is acknowledged
out-of-scope (no software defends against silicon-level mask
modification). A10 is the deployer's responsibility.

---

## 2. Assets — what we protect

| Asset | Sensitivity |
|-------|-------------|
| Boot integrity (kernel image) | Critical |
| User-space task isolation | Critical |
| Cryptographic keys (Ed25519 device key, AES flash key) | Critical |
| OTA update integrity | Critical |
| Telemetry confidentiality + integrity | High |
| Mission / waypoint integrity | High |
| Sensor data | High |
| Capability table integrity | Critical |
| Anti-rollback counter | Critical |
| Topology (CAPS.TOML / SCHED.TOML) | Critical |
| Recovery slot kernel | Critical |
| AI model weights (signed) | High |
| Brain ↔ kernel session keys | High |

---

## 3. Trust boundaries

```
┌───────────────────────────────────────────────────┐
│ TRUST ANCHOR (immutable silicon)                  │
│  • SoC mask ROM, OTP / eFuse                      │
└─────────┬─────────────────────────────────────────┘
          │ verifies
          ▼
┌───────────────────────────────────────────────────┐
│ ROOT OF TRUST — TF-A / U-Boot / kernel.bin chain  │  ←─ HW-ROT (RFC-0011)
└─────────┬─────────────────────────────────────────┘
          │ enforces
          ▼
┌───────────────────────────────────────────────────┐
│ KERNEL — capability table, scheduler, IPC        │  ←─ RFC-0003 / RFC-0004
└─────────┬─────────────────────────────────────────┘
          │ exposes typed handles
          ▼
┌───────────────────────────────────────────────────┐
│ USER-SPACE TASKS — skills, mission, AI inference  │
│ Each task isolated; its caps are its only powers  │
└─────────┬─────────────────────────────────────────┘
          │ via brain link (auth_envelope HMAC)
          ▼
┌───────────────────────────────────────────────────┐
│ BRAIN — Python orchestrator (host)               │
│ NOT in cert scope; treated as untrusted by kernel │
└───────────────────────────────────────────────────┘
```

The brain is treated as **untrusted** from the kernel's view.
Every brain→kernel command is authenticated (HMAC envelope) and
authorised (capability check). A compromised brain cannot
exfiltrate keys, brick the device, or violate safety bounds.

---

## 4. Guaranteed properties

This is what PHANES (the kernel + signed OTA + HW-ROT) **commits
to deliver** to deployers. Each is paired with the mechanism that
provides it.

### S1 — Boot integrity

**Property:** No code executes on the application processor that
isn't either (a) signed by a key in OTP, or (b) the immutable
mask-ROM.

**Mechanism:** TF-A signed by OTP key → U-Boot signed by TF-A →
kernel signed by U-Boot's anchor → recovery fallback is
signed-immutable. RFC-0011.

**Verifiable by:** running `slsa-verifier` against released
artefacts; running provided "verify chain" tool against a
deployed device's measurements (Phase 3 attestation).

### S2 — Capability unforgeability

**Property:** A user-space task cannot construct, guess, or
overwrite a capability handle it wasn't given.

**Mechanism:** `Cap<T>` is `#[repr(transparent)]` over `u32` but
the kernel's `cap_table[t][h]` validates kind + generation +
target on every dereference. Generations are monotonic; freed
caps' handles return `EBADF` on reuse. RFC-0003.

**Verifiable by:** Kani harness `cap_forge_impossible.rs`,
proptest sweep, mutation tests.

### S3 — IPC authentication

**Property:** A message received on a typed channel was sent by a
holder of a write-cap to that channel.

**Mechanism:** Send takes `&Cap<W>`; receive yields a typed
message tagged with sender ID. Channel ownership is set in
CAPS.TOML and unforgeable. RFC-0003 + RFC-0005.

### S4 — Scheduler partition isolation

**Property:** A best-effort task cannot starve a safety-critical
task.

**Mechanism:** Adaptive Partitioning Scheduler — each class has a
guaranteed budget; CBS prevents over-run; safety-critical class
has lowest preemption latency. RFC-0004.

**Verifiable by:** TLA+ partition spec + worst-case soak (TS02).

### S5 — Geofence + safety profile enforcement

**Property:** A safety-bound (geofence, max speed, ESTOP) cannot
be bypassed by user-space code.

**Mechanism:** Safety check is in kernel `behavior/safety.rs`; the
actuator command path goes through the kernel safety filter
*after* the policy module produced the command. User-space cannot
bypass; brain cannot bypass.

### S6 — OTA safety

**Property:** An attacker who controls the network cannot install
arbitrary firmware. Power loss during update never bricks.

**Mechanism:** Image signature (Ed25519, OTP-anchored), anti-
rollback counter (OTP), atomic A/B slot (RFC-0011 + OT02),
recovery slot fallback. RFC-0011 + OT01–OT05.

### S7 — Topology authenticity

**Property:** The set of capabilities a task starts with is
exactly what the signed CAPS.TOML / SCHED.TOML specified.

**Mechanism:** TOML files signed Ed25519, verified on boot,
parsed into immutable cap table. Any modification → boot fail.
RFC-0005.

### S8 — Communication confidentiality + integrity (brain link)

**Property:** Brain ↔ kernel link cannot be replayed, MitM'd, or
tampered.

**Mechanism:** `auth_envelope` HMAC + nonce + monotonic counter +
per-session key (X25519 ECDH on connect). Kernel rejects packets
with bad MAC, repeated nonces, or counter regressions.

### S9 — Crypto primitive correctness

**Property:** Ed25519 signing/verifying matches RFC 8032; AES-GCM
matches NIST SP 800-38D; HMAC-SHA256 matches RFC 2104.

**Mechanism:** Wycheproof test vectors run in CI; cargo-mutants
on `crates/crypto`; loom on the channel state machine.

### S10 — Memory safety

**Property:** No `unsafe` block can corrupt memory it doesn't
explicitly own.

**Mechanism:** Rust language guarantees + audited `unsafe` blocks
+ Miri-tested host-portable parts + Kani harness on critical
unsafe.

### S11 — Supply chain integrity

**Property:** The kernel binary on a deployed robot was built
from the published source by the published toolchain.

**Mechanism:** Reproducible builds + SBOM + Sigstore signing +
SLSA L3 provenance. RFC-0012. Verifiable by anyone with
`slsa-verifier`.

---

## 5. System-wide invariants

These are the invariants the kernel **maintains continuously**.
Any code path that could violate one is a P0 bug.

| ID | Invariant | Enforced by |
|----|-----------|-------------|
| INV-1 | The capability table is monotonic in generation per slot. | RFC-0003; mutation test |
| INV-2 | No user task holds a write-cap to the kernel's address space. | Sv39 + cap discipline |
| INV-3 | All scheduler class budgets sum to ≤ 100% per partition window. | RFC-0004; TLA+ |
| INV-4 | The OTA active slot has a valid signature and rollback ≥ stored. | RFC-0011; boot check |
| INV-5 | The recovery slot is read-only (write-protected at boot). | HW + flash partitioning |
| INV-6 | ESTOP, when triggered, takes effect within 50 ms (hard real-time). | sched class + behavior/safety |
| INV-7 | No allocation in the safety-class scheduler path. | SC01 lint |
| INV-8 | All loops in safety crates have static bounds. | SC01 lint |
| INV-9 | All `unsafe` blocks have `// SAFETY:` justification. | SC01 lint |
| INV-10 | Brain link nonces never repeat within a session. | auth_envelope counter |
| INV-11 | The kernel's text section is immutable post-boot (W^X). | MMU + PMP/TF-A |
| INV-12 | The active session key is rotated at least every 24 h. | secure_channel rekey |
| INV-13 | Geofence violation triggers `safety::estop()` within 1 navigation cycle. | behavior/safety |
| INV-14 | Anti-rollback counter is monotonic at OTP write. | RFC-0011 |
| INV-15 | Topology load failure prevents user-space spawn. | RFC-0005 |

The full list is exported as `formal/proofs/INVARIANTS.md` and
each invariant has at least one test or proof.

---

## 6. Cryptographic posture

| Use | Algorithm | Key length |
|-----|-----------|------------|
| Code signing | Ed25519 | 256 bits |
| Brain ↔ kernel session | HKDF-SHA256 from X25519 ECDH | 256 bits derived |
| Brain link integrity | HMAC-SHA256 | 256 bits |
| Encrypted flash (where SoC supports) | AES-128-XTS | 128 bits |
| Secure-element attestation | Ed25519 + ECDSA P-256 (per device) | 256 bits |
| OTA chunk hashing | SHA-256 | — |

**No** legacy / weak primitives:

- ❌ MD5 (banned)
- ❌ SHA-1 (banned)
- ❌ DES / 3DES (banned)
- ❌ AES-CBC without HMAC (banned; we use AES-GCM or AES-XTS)
- ❌ ECDSA without strict-mode (we use Ed25519, deterministic)

**Key rotation:**

- Brain session keys: 24 h (S12).
- OTA signing key: per-release; revocable via OTP-stored revocation
  list (Phase 3).
- Device unique key: at provisioning, via HSM-protected master.

**Quantum readiness:** Phase 4 — track NIST PQC; implement
ML-DSA / SLH-DSA where signature primitives are needed; XMSS for
firmware signing pre-quantum-safe migration.

---

## 7. Disclosure & response

See RFC-0009 (governance + PSIRT) and RFC-0016 (operational
excellence) for:

- `security@phanes-project.org` PGP-encrypted intake
- Triage SLAs by severity
- Embargo policy + customer pre-disclosure list
- CVE coordination
- Bug bounty program

---

## 8. Out-of-scope assumptions

PHANES makes the following **assumptions** and explicitly does
**not** defend against violations:

- The OTP key was burned correctly at factory (provenance =
  factory operator).
- The Rust compiler (`rustc` or Ferrocene) is not malicious or
  backdoored. Mitigated by reproducible builds + diverse
  rebuilders Phase 4.
- The SoC mask ROM is what the vendor says it is.
- The deployer doesn't ship `myrobots-stack` code that bypasses
  topology constraints by re-implementing kernel functions in
  user-space.
- Power and thermal envelopes are within hardware spec.

---

## 9. What PHANES does **not** promise

To set expectations honestly:

- **PHANES is not malware-proof.** A privileged user-space task
  with appropriate caps can still misbehave within those caps.
- **PHANES does not solve social engineering.** A deployer who
  hands their signing keys to an attacker is compromised.
- **PHANES does not certify your application code.** ASIL applies
  to the kernel + behaviour layers; your skills + mission code
  must be certified separately if you want a full ASIL ECU.
- **PHANES is not a TEE / TrustZone replacement.** It interoperates
  with TEEs (we'll integrate OP-TEE Phase 3) but doesn't replace
  them for high-assurance secrets like banking keys.

---

## 10. Verification cross-reference

| Property | Test / proof |
|----------|---------------|
| S1 | OTA sig regression + reproducible build verifier |
| S2 | Kani `cap_forge.rs` + proptest |
| S3 | proptest channel test + Loom |
| S4 | TLA+ APS spec + soak tests |
| S5 | unit test in `behavior/safety.rs` + integration tests |
| S6 | OTA E2E suite (OT01) |
| S7 | TOML signature regression |
| S8 | auth_envelope unit tests + Loom + replay attack regression |
| S9 | Wycheproof + cargo-mutants |
| S10 | Miri (host-portable) + Kani (kernel critical) |
| S11 | reproducible-builds CI + slsa-verifier |
