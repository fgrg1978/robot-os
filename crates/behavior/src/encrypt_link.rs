//! Kernel-side adapter for the RFC-0019 encrypt-link.
//!
//! The handshake state machine + AEAD wrapper live in the standalone
//! `robot_os_encrypt_link` crate (host-buildable, byte-pinned against the
//! brain by `crates/encrypt-link-tests/`).  This module re-exports that
//! API and adds the kernel-only entropy helpers — the standalone crate is
//! deliberately pure so its bytes can be diffed against Python.

#![allow(unused_imports)]

pub use robot_os_encrypt_link::{
    EncryptLink, HandshakeState, HandshakeError,
    MODE_ENCRYPTED, LABEL_HELLO, LABEL_CONFIRM, LABEL_REJECT,
    EPH_PUB_BYTES, PROOF_BYTES, PSK_BYTES,
    HELLO_INIT_BYTES, HELLO_REPLY_BYTES, CONFIRM_BYTES,
    ENC_NONCE_SIZE, ENC_HMAC_SIZE, ENC_OVERHEAD, ENC_MAX_PAYLOAD,
    proof_responder, proof_initiator,
    x25519_pubkey,
};

use robot_os_crypto::sha256::Sha256;

/// Derive a deterministic ephemeral X25519 private key from kernel entropy
/// sources.
///
/// # This link has NO forward secrecy, and will not until a TRNG exists
///
/// Read the inputs: the PSK, `clint::get_time`, `wcet::read_cycles`, and a
/// `salt` that is itself another `get_time` sample XOR a small counter.
/// Forward secrecy only means anything under the compromised-PSK model —
/// an attacker who recorded traffic and later obtained the pre-shared key.
/// Give that attacker the PSK and every remaining input collapses to
/// *timing on two monotonic counters*, both of which start near zero at
/// boot and advance predictably. The reachable entropy is on the order of
/// 2^15–2^40 depending on how tightly the handshake's boot-relative timing
/// can be bounded — not 128 bits. Recorded sessions are recoverable by
/// brute-forcing the ephemeral key.
///
/// This is **not** fixable in software. There is no TRNG driver anywhere in
/// this tree, and a home-made PRNG here would be actively worse than the
/// present state: it would look like a fix and stop anyone from asking the
/// real question. Closing this requires a hardware entropy source (RISC-V
/// `Zkr`/`seed` CSR where the SoC implements it, or an on-board TRNG
/// peripheral) plus a decision about what the kernel does when entropy is
/// unavailable at boot. That is an owner-level design decision.
///
/// Until then: the AEAD layer still gives confidentiality and integrity
/// against an attacker who never learns the PSK. It does not give forward
/// secrecy against one who does. Do not describe the link as
/// forward-secret in any doc, RFC status line, or commit message.
///
/// (RFC-0019 calls this "forward secret". That claim is not currently
/// true.)
pub fn derive_ephemeral_priv(psk: &[u8; PSK_BYTES], salt: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"PHANES-EPH-V1");
    h.update(psk);
    let now = robot_os_drivers::clint::get_time().to_le_bytes();
    h.update(&now);
    let cyc = robot_os_drivers::wcet::read_cycles().to_le_bytes();
    h.update(&cyc);
    h.update(&salt.to_le_bytes());
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

/// Convenience: kernel-side 8-byte nonce-randomness from current cycles
/// + a caller-supplied salt counter.  Production should plug a real RNG.
pub fn fresh_nonce_rand(salt: u64) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(b"PHANES-NONCE-V1");
    h.update(&salt.to_le_bytes());
    let t = robot_os_drivers::clint::get_time().to_le_bytes();
    h.update(&t);
    let c = robot_os_drivers::wcet::read_cycles().to_le_bytes();
    h.update(&c);
    let d = h.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&d[..8]);
    out
}
