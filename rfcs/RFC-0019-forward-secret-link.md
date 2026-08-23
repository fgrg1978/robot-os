# RFC-0019: Forward-Secret Encryption for the Brain↔Kernel Link

> **Status:** implemented (handshake + kernel TCP wire-up landed and fail-closed; rekey, session-id/REJECT and camera-frame encryption still open)  
> **Authors:** Fernando Rodriguez
> **Created:** 2026-05-23
> **Last updated:** 2026-08-20
> **Supersedes:** —
> **Superseded by:** —


> **Status audit 2026-08-20.** The old caveat "kernel TCP wire-up tracked as
> #34" was stale — that work landed. Verified in `kernel/src/main.rs`:
> `brain_responder_handshake()` (~2455) runs the full responder side,
> `send_framed()` (~2502) AEAD-encrypts every TX, and the RX path decrypts
> before unwrapping the inner HMAC envelope. It is **fail-closed**:
> `CFG_LINK_ENCRYPT=1` with no `LINK.KEY`, or a failed handshake, closes the
> connection with no plaintext fallback. Honest residue of #34: no rekey
> (`CFG_LINK_REKEY_BYTES/SECS` exist nowhere), no session-id derivation and the
> REJECT byte is never sent (silent close instead), camera frames are dropped
> rather than encrypted on an encrypted link (AEAD cap 2048 B — deferred to
> RFC-0021), and `CFG_LINK_ENCRYPT` still defaults to **off**
> (`crates/config/src/lib.rs:67`), so the path is built but not exercised by
> default.
>
> **Follow-up landed the same day.** The HMAC-only mode had no equivalent of
> the secure-boot enforcement knob: an absent or malformed `/fat/LINK.KEY` made
> `auth_envelope::wrap`/`unwrap` the identity function and the link ran
> unauthenticated, announced only by a console line. That file lives on the FAT
> volume `msc_gadget.rs` also exports over USB mass storage, so its absence is
> a state an attacker can arrange. A `link-auth-enforced` cargo feature
> (Kconfig `LINK_AUTH_ENFORCED`) now halts the boot instead of downgrading,
> fixed at compile time with no runtime override, mirroring
> `secure-boot-enforced`. CI exercises **both** directions — an image without
> the key must refuse, an image with it must boot — because a gate tested only
> by its negative case would pass just as happily if it always refused.
>
> **Still open (K-C5).** `HIGHEST_RX_NONCE` is RAM-only and resets to 0 each
> boot, so in HMAC-only mode a recorded frame replays into the window between
> reboot and the first legitimate brain frame. The check itself was made
> linearizable (`fetch_max` instead of load/compare/store), but the reboot gap
> is a design decision: persisting the mark costs flash wear and needs a
> rollback policy, whereas running in this RFC's encrypted mode closes it for
> free, since a recorded frame cannot decrypt under fresh ephemeral keys.

## Summary

Add an opt-in X25519 ECDHE + AES-128-CTR + HMAC-SHA-256 encrypted channel
to the brain↔kernel TCP link. The current `auth_envelope` layer
([RFC-0011]) provides replay protection and integrity via HMAC-SHA-256
truncated to 128 bits but leaves all payload bytes in plaintext on the
wire. This RFC layers encryption on top with **forward secrecy** through
ephemeral keys generated fresh per connection, while keeping the inner
brain-protocol packet layout (MAGIC `BR` + TYPE + LEN + PAYLOAD + CRC-8)
**unchanged**.

The cryptographic primitives are already implemented in
`crates/crypto/src/secure_channel.rs`; this RFC is the wiring + handshake
state machine + negotiation + tests.

## Motivation

### Threat model — what does this add over `auth_envelope`?

The HMAC envelope already defends against:

- **Packet forgery** — attacker cannot fabricate a valid frame without
  the PSK.
- **Replay** — strict monotonic nonce window rejects any recorded frame.
- **Bit-flip / corruption** — HMAC verify fails.

It does **not** defend against:

- **Passive eavesdropping.** Any host on the link reads sensor streams,
  motor commands, OTA image bytes, mission waypoints, and camera-derived
  intent in real time. For a private LAN this is acceptable; for a
  deployment that traverses untrusted segments (cloud-to-robot tunnels,
  shared Wi-Fi at trade shows, medical telemetry, commercial / defence
  use) it is not.
- **Long-term key compromise of past traffic.** A PSK that is later
  recovered from disk or memory lets the attacker decrypt every
  historical session recorded on the wire.
- **Traffic-analysis side channels** are *not* in scope of this RFC
  (packet size / timing remain visible) — see Future possibilities.

This RFC closes the first two by layering encryption with ephemeral keys
underneath the existing HMAC. The HMAC envelope is preserved as the
outer layer so that *all* existing replay / forgery defences continue to
apply unchanged.

### Why now

We have ~3 months before hardware arrives. Pre-hardware is the right
time for ABI-touching crypto changes:

- No deployments to break.
- Cross-side wire-format tests can be added against the bench rig.
- Phase 2 cert-eligibility (RFC-0001) needs confidentiality on the link
  for any deployment beyond a single private lab.

Without this, adopters who need confidentiality will roll their own TLS
terminator or VPN — creating an untested out-of-band layer that is
harder to audit and harder to certify than a single first-class
encryption mode.

### Why not just turn it on by default

HMAC-only mode is materially cheaper on the kernel side (see [SoftRT
budget](#softrt-cycle-budget) below). For deployments that do not need
confidentiality, paying the AES + handshake cost is a regression. The
opt-in flag preserves the existing throughput envelope for those users.

## Detailed design

### Wire framing: mode byte

A single type byte before the inner frame distinguishes modes. This
byte is *outside* the HMAC envelope and is the first byte read on every
new TCP connection:

```
0x00  PLAIN        — no auth (legacy, dev only; should never reach prod)
0x01  AUTH_HMAC    — current: HMAC envelope + replay nonce (RFC-0011)
0x02  ENCRYPTED    — this RFC: ECDHE + AES-CTR + HMAC over AUTH_HMAC
```

The brain sends the mode byte first on connect; the kernel either
accepts (handshake proceeds) or closes the connection (mode mismatch).
This avoids a downgrade attack where a MITM strips the encryption: the
kernel's expected-mode policy is set at boot via `/fat/CONFIG.INI` and
is **not** negotiable per-connection.

### Handshake — Noise_XXpsk0 shape

The handshake is structurally Noise_XXpsk0 — ephemeral ECDH + PSK
binding — but expressed using existing crate primitives so we do not
take a dependency on the Noise framework:

```
Initiator: brain.                Responder: kernel.

1. brain  → kernel:  [0x02][HELLO][brain_e_pub 32B]
2. kernel → brain:   [0x02][HELLO][kernel_e_pub 32B][proof_k 32B]
3. brain  → kernel:  [0x02][CONFIRM][proof_b 32B]

shared_secret = X25519(my_e_priv, peer_e_pub)
proof_k = HMAC-SHA-256(PSK, "RESP" || brain_e_pub || kernel_e_pub)
proof_b = HMAC-SHA-256(PSK, "INIT" || kernel_e_pub || brain_e_pub)
```

Both `proof_k` and `proof_b` MUST verify; either side rejects the
handshake with a single `[0x02][REJECT]` byte (no further detail — do
not leak which check failed) and closes.

The four wire labels (`HELLO`, `CONFIRM`, `REJECT`, plus the existing
`AUTH_HMAC` mode byte) are the only new constants on the wire.

#### Why both directions need a proof

Without `proof_b`, a MITM that knows the PSK could relay step 1 to a
real kernel, receive step 2, and then play it back to the brain — the
brain has no way to be sure it shares a secret with the genuine kernel
rather than the MITM. `proof_b` closes that by forcing the brain to
demonstrate PSK knowledge over the keys it actually saw.

### Key derivation (HKDF-style, SHA-256 only)

Existing `secure_channel.rs` uses ad-hoc SHA-256 chaining:

```
enc_key = SHA-256(shared_secret || "ENC")[0..16]
mac_key = SHA-256(shared_secret || "MAC")[0..16]
```

This RFC pins that scheme as canonical (no HKDF lib introduced) and
extends it with a session-id derivation so that rekey can proceed
without a fresh handshake:

```
session_id = SHA-256("SID" || brain_e_pub || kernel_e_pub)[0..16]
rekey_key  = SHA-256(shared_secret || "RKY" || rekey_counter_be64)[0..16]
```

`rekey_counter` starts at zero and increments on each rekey trigger
(see [Rekey policy](#rekey-policy)). Per-rekey enc/mac keys are then
derived from `rekey_key` using the same `"ENC"` / `"MAC"` labels.

### Encrypted frame format

Per the existing implementation in `crates/crypto/src/secure_channel.rs`:

```
Offset  Size  Field
0x00    12    Nonce  (8B random + 4B AES-CTR counter)
0x0C    2     Encrypted-payload length N  (LE u16, MAX = 2048)
0x0E    N     Ciphertext (AES-128-CTR, enc_key)
0x0E+N  32    HMAC-SHA-256 over [nonce || length || ciphertext]
```

Plaintext = the **existing** `auth_envelope` frame (8B nonce + 16B HMAC
+ 2B inner-len + N inner-bytes). So replay protection is end-to-end
duplicated: the outer AES-CTR nonce prevents wire-level replay during
a session; the inner `auth_envelope` nonce prevents replay *across*
sessions (the kernel's persistent rx_high_water still applies after a
reconnect).

This intentional double-protection means: if either layer fails, the
other still rejects the packet. Encryption is *not* an excuse to skip
authentication.

### Replay counter on reconnect

**Resolved (vs. the stub's unresolved question):** the per-session
AES-CTR counter resets to zero on every fresh TCP connection because
the ephemeral keypair is also fresh. The session-id derived from
`brain_e_pub || kernel_e_pub` makes any inter-session collision (same
two ephemeral pubkeys re-used) detectable on the receive path and
treated as a `REJECT`.

### Rekey policy

Trigger a rekey when either threshold is reached, whichever comes
first:

| Trigger | Default | Configurable via |
|---------|---------|------------------|
| Bytes since last rekey | `2^30` (1 GiB) | `CFG_LINK_REKEY_BYTES` |
| Wall-clock since last rekey | 1 hour | `CFG_LINK_REKEY_SECS` |

Rekey is initiated by the kernel sending a one-byte `REKEY=0x52` after
the next legitimate frame. The next frame after the `REKEY` byte uses
keys derived from the bumped `rekey_counter`. There is no negotiation
— the brain MUST follow or be disconnected.

The AES-CTR counter (4B) cannot wrap at 2^32 blocks (= 64 GiB at 16B/blk)
before the byte trigger fires, so counter exhaustion is impossible in
practice. The byte trigger fires ~64× earlier.

### Opt-in flags (no silent fallback)

| Side | Opt-in mechanism |
|------|------------------|
| Brain | env var `ROBOT_BRAIN_ENCRYPT_LINK=1` |
| Kernel | `CFG_LINK_ENCRYPT=1` in `/fat/CONFIG.INI` |

**Both sides must agree.** Mismatch policy:

- Brain wants `ENCRYPTED`, kernel runs `AUTH_HMAC`: kernel closes on
  mode byte mismatch.
- Brain wants `AUTH_HMAC`, kernel runs `ENCRYPTED`: kernel closes on
  mode byte mismatch.
- Either side has the flag set but no PSK configured: refuse to start
  (visible error at boot / startup, not silent fallback).

There is **no negotiated downgrade**. The brain refusing to encrypt
must be a deliberate operator action, not the result of a network
attacker stripping a `STARTTLS`-style upgrade.

### What changes

| Component | Change | Lines (estimate) |
|-----------|--------|------------------|
| `protocol.py` | Route through `SecureChannel` when flag set | +30 |
| `secure_channel.py` | Add `SecureChannel` class on top of existing `Sender`/`Receiver` (X25519 + AES-CTR via `cryptography` lib) | +250 |
| `tests/test_secure_channel_aead.py` | New: handshake, rekey, mismatch, replay (cross-side) | +400 |
| `crates/behavior/src/auth_envelope.rs` | New `wrap_encrypted` / `unwrap_encrypted` shims that route through `crates/crypto::secure_channel` when flag set | +120 |
| `crates/crypto/src/secure_channel.rs` | Add rekey state + session-id; no algorithm changes | +80 |
| `crates/config/src/lib.rs` | New `CFG_LINK_ENCRYPT`, `CFG_LINK_REKEY_BYTES`, `CFG_LINK_REKEY_SECS` runtime atomics | +30 |
| `crates/regression-tests/src/aead_link_tests.rs` | New cross-side wire-format pin tests | +200 |

The inner brain-protocol packet layout is **unchanged**. Encryption
wraps the `auth_envelope` frame; it does not replace or redefine the
brain protocol.

### SoftRT cycle budget

External review (claudia 2026-05-23) flagged that AEAD on the RISC-V
target consumes ISR / SoftRT cycle budget. Concrete figures from
`crates/crypto-tests` benchmarks (qemu TCG, will be re-measured on
VF2/K1 when hardware arrives):

| Operation | qemu cycles | VF2 estimate (1 GHz) | Within SoftRT 50 µs budget? |
|-----------|-------------|----------------------|------------------------------|
| HMAC-SHA-256 1 KiB (current) | ~12 k | ~12 µs | yes |
| AES-128-CTR encrypt 1 KiB | ~28 k | ~28 µs | yes |
| **Combined (AEAD encrypt + HMAC)** | **~40 k** | **~40 µs** | **yes, tight** |
| HMAC verify 1 KiB | ~12 k | ~12 µs | yes |
| AES-128-CTR decrypt 1 KiB | ~28 k | ~28 µs | yes |
| **Combined (HMAC verify + decrypt)** | **~40 k** | **~40 µs** | **yes, tight** |
| X25519 scalar mult (handshake) | ~2 M | ~2 ms | not in ISR; runs in init task |

`SoftRT` class budget per RFC-0004 is 50 µs per scheduling tick. AEAD
fits with ~20% headroom on VF2; K1 (1.6 GHz) has more. The X25519
handshake is one-shot at connect, runs in the brain-link init task
**not** in the ISR path, so it does not impact real-time budgets.

If a future hardware platform fails the budget, the flag falls back to
`AUTH_HMAC` mode at no cost.

### Migration plan

1. **Land RFC** + cross-side tests against the existing
   `crates/crypto/src/secure_channel.rs` (no link wiring yet).
2. **Wire brain side** with flag default-off. Existing `auth_envelope`
   tests keep passing.
3. **Wire kernel side** with `CFG_LINK_ENCRYPT=0`. Run E2E with stub.
4. **Cross-side compat suite** — `crates/regression-tests/src/aead_link_tests.rs`
   pins the byte-for-byte wire format.
5. **Enable in qemu E2E**, gather throughput numbers vs HMAC-only.
6. **Hardware bring-up** (Phase 2) — re-measure SoftRT margin on real
   silicon, decide on default flag value.

Each step is one PR. None of the steps changes existing HMAC-only
behaviour until the operator flips the flag.

## Drawbacks

- **+18 B net overhead** per packet in `ENCRYPTED` mode (12B nonce +
  2B len + 32B HMAC = 46B outer, minus 26B `auth_envelope` savings on
  the inner). Sensor packet at 64 B → 110 B on the wire; tolerable.
- **One extra round-trip at connect time.** Negligible vs. VLM latency
  (10-200 ms) but adds ~1 ms on a healthy LAN.
- **Python `cryptography` library** must be pinned explicitly (X25519
  ECDH and AES-CTR primitives not in stdlib). Adds a wheel + an OpenSSL
  shared-lib runtime dep on the brain side. Mitigation: scope the
  dependency to `secure_channel.py` only; provide a pure-Python fallback
  for X25519 (we already have a Rust impl that compiles for host via
  `crates/crypto-tests` — same algorithm could be ported).
- **Two distinct authentication layers** (outer HMAC over ciphertext,
  inner `auth_envelope` HMAC over plaintext). Slightly redundant in
  the steady state, but each layer defends a different threat (wire
  vs. cross-session replay) and removing either weakens the model.
- **No traffic-analysis defence.** Packet sizes and timings remain
  observable. Padding / cover traffic is out of scope.

## Rationale and alternatives

**Alternative A — TLS.** Standard but requires a `no_std` TLS stack
(~60 kB extra in kernel, and no certified `no_std` TLS lib exists
today that meets RFC-0017 cert scope). Reconsidered for Phase 3 cloud
deployments where the brain talks to an off-platform service.

**Alternative B — VPN / WireGuard at network layer.** Puts the
security boundary outside the certifiable artefact; auditors must now
include VPN config in the safety case. Also: WireGuard requires a
kernel UDP stack that does not currently exist in PHANES.

**Alternative C — ChaCha20-Poly1305 instead of AES-CTR + HMAC.** Purer
AEAD construction (one primitive, no encrypt-then-MAC composition
risk). Rejected because (1) we already have AES-128-CTR + HMAC-SHA-256
implemented and tested in `crates/crypto/`; (2) AES has hardware
acceleration on the eventual deployment target (K1 has SM4/AES
extension); (3) Encrypt-then-MAC is correctly composed in the existing
code and matches NIST SP 800-38D usage.

**Alternative D (chosen) — X25519 ECDHE via existing crate.** Reuses
already-present code, minimal kernel footprint, no new deps,
forward-secret by construction.

## Prior art

- `crates/crypto/src/secure_channel.rs` — the implementation already
  exists; this RFC is the protocol wiring + handshake state + tests.
- **Noise Protocol Framework** — `Noise_XXpsk0` is the formal name for
  the handshake shape (ephemeral ECDH + PSK binding). We adopt the
  *shape* without taking a code dependency.
- **TLS 1.3 1-RTT** — also uses ephemeral ECDHE + AEAD but has a much
  larger spec surface, requires X.509 cert handling, and is not
  available in `no_std`.

## Unresolved questions

- **PSK rotation cadence.** No rotation protocol defined yet.
  Proposal: TOFU on first deploy, annual rotation with a 24-hour
  rolling overlap window driven by the OTA channel. Needs a follow-up
  ADR before status moves from `draft` to `accepted`.
- **First-deploy bootstrap.** How `/fat/LINK.KEY` is written onto a
  brand-new robot before it connects to any network. Provisioning
  procedure not yet defined — likely USB-OTG one-shot tool reusing the
  DEV02 DFU recovery mode.
- **Brain restart while kernel still up.** Brain's send-nonce derives
  from `time.time_ns()`, which is monotonic-enough across restarts on
  a real machine. With the encrypted layer, the ephemeral keypair is
  also re-rolled, so this is naturally safe — but the test matrix must
  pin it.

## Future possibilities

- **Post-quantum:** replace X25519 with X25519 + ML-KEM-768 hybrid
  (NIST FIPS 203) once a `no_std` implementation is stable (Phase 4).
- **Per-device certificates:** replace PSK with Ed25519 device identity
  certs (`crates/crypto/src/ed25519.rs`) for fleet deployments
  > 1 robot.
- **UDP transport:** same encryption layer applies once UDP mode
  exists; replay-window bitmap required, AES-CTR counter strategy
  needs to handle out-of-order delivery.
- **Padding for traffic-analysis defence:** pad every frame to the
  next power-of-two boundary, or a constant rate. Costs bandwidth
  proportional to the chosen padding; defer until a use case demands it.

[RFC-0011]: ./RFC-0011-secure-boot.md
