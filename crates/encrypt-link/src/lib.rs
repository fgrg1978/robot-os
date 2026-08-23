#![no_std]

//! RFC-0019 forward-secret link — handshake state machine.
//!
//! Pure `no_std`, host-buildable.  Cross-side compat with the brain
//! Python `secure_channel.SecureChannel` is verified by
//! `crates/encrypt-link-tests/` using deterministic ephemeral keys.
//!
//! ## Role split — the TCP role and the crypto role are INVERTED
//!
//! In the PHANES topology the kernel dials the brain, so the kernel is the
//! TCP *client*.  The crypto handshake runs the other way round: the brain
//! speaks first.  `robot-brain/protocol.py:294` calls `start_handshake()`
//! and `kernel/src/main.rs` calls `brain_responder_handshake()`, which
//! drives `handle_initiator_hello` / `handle_initiator_confirm`.  So:
//!
//! ```text
//!   crypto initiator = brain   (TCP server)
//!   crypto responder = kernel  (TCP client)
//! ```
//!
//! (An earlier version of this comment claimed the kernel was the
//! initiator.  It never was; the code has always been the other way.  The
//! distinction is now load-bearing — it selects the direction-bound key
//! pair below — so getting it wrong produces a link where nothing
//! decrypts.)
//!
//! ```text
//! initiator → responder: [0x02][HELLO=0x48][initiator_e_pub 32B]
//! responder → initiator: [0x02][HELLO=0x48][responder_e_pub 32B][proof_r 32B]
//! initiator → responder: [0x02][CONFIRM=0x43][proof_i 32B]
//! ```
//!
//! `proof_r = HMAC-SHA256(PSK, "RESP" || initiator_e_pub || responder_e_pub)`
//! `proof_i = HMAC-SHA256(PSK, "INIT" || responder_e_pub || initiator_e_pub)`
//!
//! Both directions verify their peer's proof via constant-time
//! comparison.  Either side rejects the handshake by transitioning to
//! `Rejected` (caller may send a `[0x02][REJECT]` byte to the peer if
//! desired — no detail leaked).
//!
//! ## After established
//!
//! All bulk frames are wrapped in AES-128-CTR + HMAC-SHA-256 using the
//! existing `robot_os_crypto::secure_channel::SecureChannel` primitives.
//! Nonce is 8 caller-supplied bytes + 4-byte little-endian counter; the
//! channel guards against counter wrap at `u32::MAX`.
//!
//! Each side derives a **separate** (enc, mac) pair per direction —
//! `C2S` = initiator→responder = brain→kernel, `S2C` = the reverse — so a
//! frame one side sent cannot be reflected back and still verify.  See
//! `SecureChannel::handshake_directional` for why the previous shared pair
//! was a real (if not-yet-exploitable) flaw.
//!
//! ## Forward secrecy: none, today
//!
//! The ephemeral keys are only as good as the entropy behind them, and
//! there is no TRNG driver in this tree.  See the note on
//! `robot_os_behavior::encrypt_link::derive_ephemeral_priv`.
//!
//! ## K-C5: this mode can be made MANDATORY
//!
//! Built with the `link-encrypt-enforced` feature, the brain link refuses to
//! produce or accept a frame that is not inside an established session here:
//! no plaintext mode, no HMAC-only mode, no runtime override. See the "Link
//! policy" section below for the mechanism and
//! `behavior::auth_envelope`'s `LINK_ENCRYPT_ENFORCED` for the gate itself.
//!
//! ## Entropy
//!
//! This crate is **pure**: it does NOT collect entropy.  Callers
//! generate the 32-byte ephemeral private key themselves (kernel uses
//! a mix of CLINT time + cycle counter + PSK + salt via SHA-256;
//! tests pass fixed keys) and pass it to [`EncryptLink::new`].  Same
//! for the 8-byte nonce prefix on `encrypt()`.

use core::sync::atomic::{AtomicUsize, Ordering};

use robot_os_crypto::ct::{ct_eq, secure_zero};
use robot_os_crypto::secure_channel::{
    Role,
    SecureChannel as CryptoChannel,
    MAX_PAYLOAD_SIZE as AEAD_MAX_PAYLOAD,
};
use robot_os_crypto::sha256::Sha256;
use robot_os_crypto::x25519;

// ── Wire constants (must match brain `secure_channel.py`) ──────────────

/// Mode byte for the encrypted (RFC-0019) link.
pub const MODE_ENCRYPTED: u8 = 0x02;
/// Frame label: HELLO carries an ephemeral public key.
pub const LABEL_HELLO: u8 = 0x48;
/// Frame label: CONFIRM carries the initiator's PSK proof.
pub const LABEL_CONFIRM: u8 = 0x43;
/// Frame label: REJECT signals handshake failure (no detail leaked).
pub const LABEL_REJECT: u8 = 0x52;

/// Size of an ephemeral X25519 public key.
pub const EPH_PUB_BYTES: usize = 32;
/// Size of a handshake proof (HMAC-SHA-256, full 32 B — NOT truncated).
pub const PROOF_BYTES: usize = 32;

/// Wire size of the responder's HELLO+proof reply.
pub const HELLO_REPLY_BYTES: usize = 2 + EPH_PUB_BYTES + PROOF_BYTES;
/// Wire size of the initiator's HELLO.
pub const HELLO_INIT_BYTES: usize = 2 + EPH_PUB_BYTES;
/// Wire size of the initiator's CONFIRM.
pub const CONFIRM_BYTES: usize = 2 + PROOF_BYTES;

/// Pre-shared key length (32 raw bytes), matches `auth_envelope::KEY_BYTES`.
pub const PSK_BYTES: usize = 32;

// Re-export the AEAD frame size constants so callers don't import from the
// crypto crate directly.
pub use robot_os_crypto::secure_channel::{
    NONCE_SIZE as ENC_NONCE_SIZE,
    HMAC_SIZE as ENC_HMAC_SIZE,
    PACKET_OVERHEAD as ENC_OVERHEAD,
    MAX_PAYLOAD_SIZE as ENC_MAX_PAYLOAD,
};

// ── KDF labels ─────────────────────────────────────────────────────────

const KDF_LABEL_RESP: &[u8] = b"RESP";
const KDF_LABEL_INIT: &[u8] = b"INIT";

// ── Link policy (K-C5): encrypted mode may be MANDATORY ────────────────
//
// The brain link has three historical modes:
//
//   1. plaintext   — no `/fat/LINK.KEY`; `auth_envelope::wrap`/`unwrap`
//                    degrade to the identity function.
//   2. HMAC-only   — key present, `CFG_LINK_ENCRYPT=0` (today's default).
//   3. encrypted   — key present, `CFG_LINK_ENCRYPT=1`; this crate's AEAD
//                    session wraps the HMAC envelope.
//
// Mode 2 carries a real hole: `auth_envelope::HIGHEST_RX_NONCE`, the only
// replay high-water mark in the stack, lives in RAM. A reboot zeroes it, and
// the brain derives its send nonces from `time_ns()`, so *any* recorded frame
// beats a zeroed mark. Between reboot and the first legitimate brain frame,
// a captured `PKT_ESTOP` — or a captured "FORWARD 100" — replays.
//
// Mode 3 closes that for free: each connection derives fresh session keys
// from fresh ephemeral X25519 keys, so a frame recorded before the reboot
// fails the AEAD MAC under the new `rx_mac_key` and never reaches the
// envelope layer at all. (Proven by
// `aead-link-tests::frame_from_a_previous_session_does_not_decrypt_in_a_new_one`.)
// The owner chose this over persisting the watermark: no flash wear, no
// rollback policy, and a keyless boot leaves the robot with no link —
// fail-closed, which is the correct side to be wrong on.
//
// `link-encrypt-enforced` makes modes 1 and 2 unreachable. Like
// `secure-boot-enforced` and `link-auth-enforced` it is fixed at COMPILE
// time and consults no runtime variable — `CFG_LINK_ENCRYPT` lives in
// `/fat/CONFIG.INI` on the FAT volume `msc_gadget.rs` also exports over USB
// mass storage, so a runtime knob here would be an attacker-writable
// downgrade switch, not a policy.

/// Number of `EncryptLink`s currently in [`HandshakeState::Established`].
///
/// Maintained as an invariant of the state machine: bumped at the two (and
/// only two) points that assign `Established`, dropped in `Drop`. There is
/// deliberately **no setter** — the only way to make this non-zero is to
/// complete a PSK-authenticated X25519 handshake, which is what makes the
/// gate in `auth_envelope` something other than a flag someone can flip.
static AEAD_SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// Was this binary built with `link-encrypt-enforced`?
///
/// `const fn` so callers can bind it to a `const` and have the policy
/// branches const-folded away rather than evaluated per packet — see
/// `LINK_ENCRYPT_ENFORCED` in `behavior/src/auth_envelope.rs`.
pub const fn link_encrypt_enforced() -> bool {
    cfg!(feature = "link-encrypt-enforced")
}

/// How many AEAD sessions are established right now.
pub fn aead_session_count() -> usize {
    AEAD_SESSIONS.load(Ordering::Acquire)
}

/// Is at least one RFC-0019 AEAD session established?
pub fn aead_session_established() -> bool {
    aead_session_count() != 0
}

/// **The gate.** May an `auth_envelope` frame be produced or accepted right
/// now?
///
/// With the policy off this is unconditionally `true` and the whole thing
/// const-folds to nothing. With it on, an envelope frame is only legitimate
/// when it is travelling inside an AEAD session — i.e. when `send_framed`
/// will encrypt what `wrap` returns, and when the bytes handed to `unwrap`
/// came out of `decrypt_consuming`.
///
/// Cost when enforced: one acquire load + one compare + one branch. The
/// envelope HMAC it guards is three SHA-256 compressions minimum (ikey
/// block, data, okey||inner) ≈ 192 rounds ≈ 1500 RV64 ops for the smallest
/// frame, before the AEAD layer's own AES-CTR and 32-byte HMAC. The gate is
/// under 0.2% of the packet it gates, which is why it sits on the hot path
/// per packet instead of once at establishment.
pub fn envelope_frame_permitted() -> bool {
    // Short-circuit on the const first: with the feature off, LLVM deletes
    // the atomic load entirely.
    !link_encrypt_enforced() || aead_session_established()
}

// ── Handshake state ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandshakeState {
    /// No bytes exchanged yet.
    Init,
    /// Initiator: sent HELLO, waiting for peer's HELLO+proof.
    AwaitPeerHello,
    /// Responder: sent HELLO+proof, waiting for peer's CONFIRM.
    AwaitConfirm,
    /// Keys derived, encrypt/decrypt available.
    Established,
    /// Proof mismatch or wire-format error.  Channel is terminal.
    Rejected,
}

/// HMAC-SHA-256 with a `PSK_BYTES` (= 32 B) key.  RFC 2104 zero-pads to
/// the hash's block size (64 B for SHA-256).
fn hmac_sha256_psk(key: &[u8; PSK_BYTES], data: &[u8]) -> [u8; PROOF_BYTES] {
    const BLOCK_SIZE: usize = 64;
    const IPAD: u8 = 0x36;
    const OPAD: u8 = 0x5C;

    let mut k_pad = [0u8; BLOCK_SIZE];
    k_pad[..PSK_BYTES].copy_from_slice(key);

    let mut inner_key = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE { inner_key[i] = k_pad[i] ^ IPAD; }
    let mut h = Sha256::new();
    h.update(&inner_key);
    h.update(data);
    let inner_hash = h.finalize();

    let mut outer_key = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE { outer_key[i] = k_pad[i] ^ OPAD; }
    let mut h = Sha256::new();
    h.update(&outer_key);
    h.update(&inner_hash);
    let out_digest = h.finalize();
    let mut out = [0u8; PROOF_BYTES];
    out.copy_from_slice(&out_digest);

    // `k_pad` is the raw PSK zero-padded; `inner_key`/`outer_key` are the
    // PSK XOR'd with a public constant, i.e. trivially reversible to it.
    // Leaving three recoverable copies of the *long-lived pre-shared key*
    // in a stack frame is worse than leaving a session key there — the PSK
    // does not rotate. Wipe them. This runs twice per handshake, never on
    // the packet path, so the cost is unmeasurable.
    secure_zero(&mut k_pad);
    secure_zero(&mut inner_key);
    secure_zero(&mut outer_key);

    out
}

/// `proof_r = HMAC-SHA256(PSK, "RESP" || initiator_pub || responder_pub)`.
pub fn proof_responder(psk: &[u8; PSK_BYTES],
                       initiator_pub: &[u8; EPH_PUB_BYTES],
                       responder_pub: &[u8; EPH_PUB_BYTES])
    -> [u8; PROOF_BYTES]
{
    let mut buf = [0u8; KDF_LABEL_RESP.len() + 2 * EPH_PUB_BYTES];
    let l = KDF_LABEL_RESP.len();
    buf[..l].copy_from_slice(KDF_LABEL_RESP);
    buf[l..l + EPH_PUB_BYTES].copy_from_slice(initiator_pub);
    buf[l + EPH_PUB_BYTES..].copy_from_slice(responder_pub);
    hmac_sha256_psk(psk, &buf)
}

/// `proof_i = HMAC-SHA256(PSK, "INIT" || responder_pub || initiator_pub)`.
pub fn proof_initiator(psk: &[u8; PSK_BYTES],
                       responder_pub: &[u8; EPH_PUB_BYTES],
                       initiator_pub: &[u8; EPH_PUB_BYTES])
    -> [u8; PROOF_BYTES]
{
    let mut buf = [0u8; KDF_LABEL_INIT.len() + 2 * EPH_PUB_BYTES];
    let l = KDF_LABEL_INIT.len();
    buf[..l].copy_from_slice(KDF_LABEL_INIT);
    buf[l..l + EPH_PUB_BYTES].copy_from_slice(responder_pub);
    buf[l + EPH_PUB_BYTES..].copy_from_slice(initiator_pub);
    hmac_sha256_psk(psk, &buf)
}

// The local `ct_eq` that used to live here guarded both PSK handshake
// proofs and was missing the `black_box` barrier that the copies in
// `auth_envelope.rs` and `ota/secure_boot.rs` already carried. It now comes
// from `robot_os_crypto::ct` (imported above) — one implementation, one
// answer to whether the barrier is needed.

// ── Public channel struct ──────────────────────────────────────────────

/// One handshake + AEAD session.
pub struct EncryptLink {
    state: HandshakeState,
    psk: [u8; PSK_BYTES],
    eph_pub: [u8; EPH_PUB_BYTES],
    peer_pub: [u8; EPH_PUB_BYTES],
    inner: CryptoChannel,
}

/// Errors a peer-driven handshake step can produce.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandshakeError {
    BadState,
    BadFrameLength,
    BadHeader,
    ProofMismatch,
    /// The peer's ephemeral public key produced a degenerate (all-zero)
    /// X25519 shared secret — a small-order point. See
    /// `robot_os_crypto::x25519::x25519_checked`.
    BadPeerKey,
}

impl EncryptLink {
    /// Build a fresh channel.  `eph_priv` is the X25519 private key
    /// caller-derived from a TRNG (production) or pinned (tests).
    pub fn new(psk: [u8; PSK_BYTES], eph_priv: [u8; 32]) -> Self {
        let inner = CryptoChannel::new(eph_priv);
        let eph_pub = inner.public_key;
        EncryptLink {
            state: HandshakeState::Init,
            psk,
            eph_pub,
            peer_pub: [0u8; EPH_PUB_BYTES],
            inner,
        }
    }

    pub fn state(&self) -> HandshakeState { self.state }
    pub fn is_established(&self) -> bool {
        self.state == HandshakeState::Established
    }
    pub fn is_rejected(&self) -> bool {
        self.state == HandshakeState::Rejected
    }

    /// Local ephemeral public key (exposed for handshake bookkeeping +
    /// test pinning).
    pub fn eph_pub(&self) -> &[u8; EPH_PUB_BYTES] { &self.eph_pub }

    /// The **only** way to reach `Established`, so `AEAD_SESSIONS` cannot
    /// drift from the number of live established links.
    ///
    /// Both callers below have already checked they are in a pre-`Established`
    /// state (`AwaitPeerHello` / `AwaitConfirm`), and no transition leads out
    /// of `Established` except `Drop` — so increments and decrements pair
    /// exactly one-to-one.
    fn mark_established(&mut self) {
        // Guard rather than `debug_assert!`: an assert here would be a
        // reachable panic in a non-release build, and `panic = "abort"` on a
        // robot means a board reset. More to the point, a double increment is
        // the mirror of the underflow guarded in `Drop` — one decrement would
        // then leave the count permanently non-zero and the gate permanently
        // OPEN. Refusing to double-count is the fail-closed choice, and it
        // costs a compare on a path that runs once per connection.
        if self.state == HandshakeState::Established {
            return;
        }
        self.state = HandshakeState::Established;
        AEAD_SESSIONS.fetch_add(1, Ordering::AcqRel);
    }

    // ── Initiator path ────────────────────────────────────────────

    /// Initiator step 1.  Write HELLO into `out`, advance to
    /// `AwaitPeerHello`.
    pub fn start_initiator(&mut self, out: &mut [u8; HELLO_INIT_BYTES])
        -> Result<usize, HandshakeError>
    {
        if self.state != HandshakeState::Init { return Err(HandshakeError::BadState); }
        out[0] = MODE_ENCRYPTED;
        out[1] = LABEL_HELLO;
        out[2..2 + EPH_PUB_BYTES].copy_from_slice(&self.eph_pub);
        self.state = HandshakeState::AwaitPeerHello;
        Ok(HELLO_INIT_BYTES)
    }

    /// Initiator step 2.  Parse the responder's HELLO+proof, verify,
    /// write CONFIRM into `out`.
    pub fn handle_peer_hello(&mut self, frame: &[u8],
                             out: &mut [u8; CONFIRM_BYTES])
        -> Result<usize, HandshakeError>
    {
        if self.state != HandshakeState::AwaitPeerHello {
            return Err(HandshakeError::BadState);
        }
        if frame.len() != HELLO_REPLY_BYTES {
            return Err(HandshakeError::BadFrameLength);
        }
        if frame[0] != MODE_ENCRYPTED || frame[1] != LABEL_HELLO {
            return Err(HandshakeError::BadHeader);
        }
        let mut peer = [0u8; EPH_PUB_BYTES];
        peer.copy_from_slice(&frame[2..2 + EPH_PUB_BYTES]);
        let proof_r = &frame[2 + EPH_PUB_BYTES..];
        let expected = proof_responder(&self.psk, &self.eph_pub, &peer);
        if !ct_eq(&expected, proof_r) {
            self.state = HandshakeState::Rejected;
            return Err(HandshakeError::ProofMismatch);
        }
        self.peer_pub = peer;
        // We spoke first → we are the crypto initiator → we transmit on
        // C2S and receive on S2C. (In deployment this half runs in the
        // Python brain; the kernel takes the responder path below.)
        if !self.inner.handshake_directional(&peer, Role::Initiator) {
            self.state = HandshakeState::Rejected;
            return Err(HandshakeError::BadPeerKey);
        }
        let proof_i = proof_initiator(&self.psk, &peer, &self.eph_pub);
        out[0] = MODE_ENCRYPTED;
        out[1] = LABEL_CONFIRM;
        out[2..2 + PROOF_BYTES].copy_from_slice(&proof_i);
        self.mark_established();
        Ok(CONFIRM_BYTES)
    }

    // ── Responder path ────────────────────────────────────────────

    /// Responder step 1.  Process initiator's HELLO, write HELLO+proof
    /// reply.
    pub fn handle_initiator_hello(&mut self, frame: &[u8],
                                  out: &mut [u8; HELLO_REPLY_BYTES])
        -> Result<usize, HandshakeError>
    {
        if self.state != HandshakeState::Init {
            return Err(HandshakeError::BadState);
        }
        if frame.len() != HELLO_INIT_BYTES {
            return Err(HandshakeError::BadFrameLength);
        }
        if frame[0] != MODE_ENCRYPTED || frame[1] != LABEL_HELLO {
            return Err(HandshakeError::BadHeader);
        }
        let mut peer = [0u8; EPH_PUB_BYTES];
        peer.copy_from_slice(&frame[2..2 + EPH_PUB_BYTES]);
        self.peer_pub = peer;
        // We answered → we are the crypto responder → we transmit on S2C
        // and receive on C2S. This is the kernel's path.
        //
        // NOTE: `peer` is UNAUTHENTICATED here — the PSK proof exchange has
        // not happened yet — so this is where a small-order point would be
        // injected. `handshake_directional` rejects the degenerate shared
        // secret; we must turn that into a terminal `Rejected` rather than
        // fall through, or we would send a proof and sit in `AwaitConfirm`
        // holding all-zero session keys.
        if !self.inner.handshake_directional(&peer, Role::Responder) {
            self.state = HandshakeState::Rejected;
            return Err(HandshakeError::BadPeerKey);
        }
        let proof_r = proof_responder(&self.psk, &peer, &self.eph_pub);
        out[0] = MODE_ENCRYPTED;
        out[1] = LABEL_HELLO;
        out[2..2 + EPH_PUB_BYTES].copy_from_slice(&self.eph_pub);
        out[2 + EPH_PUB_BYTES..].copy_from_slice(&proof_r);
        self.state = HandshakeState::AwaitConfirm;
        Ok(HELLO_REPLY_BYTES)
    }

    /// Responder step 2.  Verify the initiator's CONFIRM proof.
    pub fn handle_initiator_confirm(&mut self, frame: &[u8])
        -> Result<(), HandshakeError>
    {
        if self.state != HandshakeState::AwaitConfirm {
            return Err(HandshakeError::BadState);
        }
        if frame.len() != CONFIRM_BYTES {
            return Err(HandshakeError::BadFrameLength);
        }
        if frame[0] != MODE_ENCRYPTED || frame[1] != LABEL_CONFIRM {
            return Err(HandshakeError::BadHeader);
        }
        let proof_i = &frame[2..2 + PROOF_BYTES];
        let expected = proof_initiator(&self.psk, &self.eph_pub, &self.peer_pub);
        if !ct_eq(&expected, proof_i) {
            self.state = HandshakeState::Rejected;
            return Err(HandshakeError::ProofMismatch);
        }
        self.mark_established();
        Ok(())
    }

    // ── Bulk encrypt / decrypt ────────────────────────────────────

    /// Encrypt `plaintext` into `out`.  `nonce_rand` is 8 bytes the
    /// caller must source from a per-session counter or RNG.  Returns
    /// the wire byte count on success, 0 on failure.
    pub fn encrypt(&mut self, plaintext: &[u8],
                   nonce_rand: &[u8; 8],
                   out: &mut [u8]) -> usize
    {
        if !self.is_established() { return 0; }
        if plaintext.len() > AEAD_MAX_PAYLOAD { return 0; }
        self.inner.encrypt(plaintext, nonce_rand, out)
    }

    /// Decrypt the first AEAD frame in `buf`, returning
    /// `(plaintext_len, consumed)` so the caller can advance and decrypt
    /// the next one.  `(0, 0)` on failure.
    ///
    /// **Use this, not [`EncryptLink::decrypt`], when reading from TCP.**
    /// TCP hands you a byte stream, so two frames the brain sent with two
    /// separate `send()` calls routinely arrive in one `recv()`.  The
    /// single-value `decrypt` returns frame 1 and drops the rest of the
    /// buffer on the floor — no error, no log.  Because the brain sends
    /// `PKT_ESTOP` (0x88) exactly once, unacknowledged and never
    /// retransmitted, an emergency stop coalesced behind any other command
    /// was silently discarded.  See
    /// `robot_os_crypto::secure_channel::SecureChannel::decrypt_consuming`.
    ///
    /// Loop until this returns `(_, 0)`:
    ///
    /// ```text
    /// let mut off = 0;
    /// while off < buf.len() {
    ///     let (n, used) = link.decrypt_consuming(&buf[off..], &mut out);
    ///     if used == 0 { break; }          // torn / forged / done
    ///     handle(&out[..n]);
    ///     off += used;
    /// }
    /// ```
    pub fn decrypt_consuming(&self, buf: &[u8], plaintext_out: &mut [u8])
        -> (usize, usize)
    {
        if !self.is_established() { return (0, 0); }
        self.inner.decrypt_consuming(buf, plaintext_out)
    }

    /// Decrypt `frame` into `plaintext_out`.  Returns plaintext length
    /// or 0 on failure.
    ///
    /// Lossy on a coalesced TCP buffer — see
    /// [`EncryptLink::decrypt_consuming`], which callers reading a stream
    /// must use instead.
    pub fn decrypt(&self, frame: &[u8], plaintext_out: &mut [u8]) -> usize {
        if !self.is_established() { return 0; }
        self.inner.decrypt(frame, plaintext_out)
    }
}

/// Wipe the long-lived PSK copy this channel holds.
///
/// `EncryptLink` is constructed per TCP connection and dropped on every
/// disconnect (`link = None` in the kernel's brain loop), so without this
/// each reconnect leaves another copy of the *pre-shared* key — the one
/// secret in the system that never rotates — in released memory.  The
/// ephemeral private key and the four session keys are wiped by
/// `CryptoChannel`'s own `Drop`, which runs after this one.
impl Drop for EncryptLink {
    fn drop(&mut self) {
        // Deregister BEFORE overwriting `state`, or the check below reads
        // `Rejected` and the count never comes down — which under
        // `link-encrypt-enforced` would leave `envelope_frame_permitted()`
        // stuck at `true` after the brain disconnects. That is fail-OPEN:
        // the kernel would go back to emitting bare HMAC envelopes with a
        // dead session, exactly the mode the policy exists to forbid.
        if self.state == HandshakeState::Established {
            // `saturating_sub`, not `fetch_sub`: an unbalanced decrement on a
            // `usize` wraps to `usize::MAX` silently (no panic even with
            // `overflow-checks = true`, because atomics don't get the checked
            // lowering), and a saturated-high counter reads as "a session is
            // active" forever. The pairing is provable from the state guards
            // above, so this should be unreachable — but of the two ways to be
            // wrong, only one leaves the gate wedged open, and refusing to
            // take that one costs a CAS on a path that runs once per TCP
            // disconnect.
            let _ = AEAD_SESSIONS.fetch_update(
                Ordering::AcqRel, Ordering::Acquire,
                |n| Some(n.saturating_sub(1)));
        }
        secure_zero(&mut self.psk);
        secure_zero(&mut self.peer_pub);
        self.state = HandshakeState::Rejected;
    }
}

// Re-export the X25519 pubkey helper so callers / tests don't need to
// pull the crypto crate.
pub use x25519::x25519_pubkey;
