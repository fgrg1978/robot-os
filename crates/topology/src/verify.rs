//! Topology TOML signature verification — RFC-0005 + RFC-0011.
//!
//! Each signed TOML file ships with a `<file>.SIG` sidecar containing a
//! single Ed25519 signature over the raw TOML bytes. The trusted public
//! key lives in OTP / eFuse on real hardware (RFC-0011) and in a
//! compile-time constant for QEMU / dev builds.
//!
//! On boot:
//!
//! ```text
//!     verify_signature(&toml_bytes, &sig, &TRUSTED_PUBKEY)?;
//! ```
//!
//! Failure ⇒ kernel halts before parsing.

use robot_os_crypto::ed25519::{
    sig_verify, ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE,
};

/// Errors returned by [`verify_signature`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VerifyError {
    /// Signature buffer is the wrong length (must be 64 bytes).
    BadSignatureLen,
    /// Trusted-key buffer is the wrong length (must be 32 bytes).
    BadKeyLen,
    /// Cryptographic verification failed.
    InvalidSignature,
    /// TOML payload exceeds the verification size limit.
    PayloadTooLarge,
}

/// Verify an Ed25519 signature over a TOML byte slice.
///
/// `toml_bytes`     — the raw bytes of `CAPS.TOML` or `SCHED.TOML`.
/// `signature`      — exactly 64 bytes from the `.SIG` sidecar.
/// `trusted_pubkey` — exactly 32 bytes; production = OTP-anchored key.
///
/// # Ordering contract for whoever wires the loader
///
/// This function is **real** verification, not a stub: `sig_verify` in
/// `crates/crypto/src/ed25519.rs` runs `verify_strict` against the
/// supplied key. What is missing is a *caller*. As of today nothing
/// outside this crate and its tests calls `verify_signature`,
/// `parser::parse_caps` or `parser::parse_sched`; the kernel installs
/// `topology::default_minimal()` — a hardcoded Rust function — so no
/// capability grant currently originates from the FAT volume.
///
/// When that loader is written it MUST call `verify_signature` on the
/// raw bytes **before** handing them to `parse_*`, and halt on error.
/// The order is the whole point: the parser is a byte-level state
/// machine fed straight from an attacker-writable FAT32 partition, and
/// parsing first would expose it to unsigned input — every parser bug
/// (and F3's unterminated-array hang was one) becomes reachable by
/// anyone who can write the SD card. Verify, then parse, then admit.
/// Do not "optimise" this by parsing to find the signature: the
/// signature lives in a separate `.SIG` sidecar precisely so that no
/// part of the signed payload has to be interpreted before the
/// signature over it has been checked.
pub fn verify_signature(
    toml_bytes: &[u8],
    signature: &[u8],
    trusted_pubkey: &[u8],
) -> Result<(), VerifyError> {
    if signature.len() != ED25519_SIGNATURE_SIZE {
        return Err(VerifyError::BadSignatureLen);
    }
    if trusted_pubkey.len() != ED25519_PUBLIC_KEY_SIZE {
        return Err(VerifyError::BadKeyLen);
    }
    let mut sig_buf = [0u8; ED25519_SIGNATURE_SIZE];
    sig_buf.copy_from_slice(signature);
    let mut key_buf = [0u8; ED25519_PUBLIC_KEY_SIZE];
    key_buf.copy_from_slice(trusted_pubkey);

    if sig_verify(&key_buf, &sig_buf, toml_bytes) {
        Ok(())
    } else {
        // The crypto crate's `sig_verify` returns `false` both on
        // payload-too-large and on signature-mismatch. We surface the
        // most common case.
        Err(VerifyError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_signature_len_rejected() {
        let toml = b"[task.a]\ncaps = []\n";
        let key = [0u8; ED25519_PUBLIC_KEY_SIZE];
        let too_short = [0u8; 32];
        assert_eq!(
            verify_signature(toml, &too_short, &key),
            Err(VerifyError::BadSignatureLen)
        );
    }

    #[test]
    fn bad_key_len_rejected() {
        let toml = b"x";
        let sig = [0u8; ED25519_SIGNATURE_SIZE];
        let bad_key = [0u8; 16];
        assert_eq!(
            verify_signature(toml, &sig, &bad_key),
            Err(VerifyError::BadKeyLen)
        );
    }

    #[test]
    fn zero_signature_does_not_validate() {
        // A zero signature against random data should not validate.
        let toml = b"[task.a]\ncaps = []\n";
        let sig = [0u8; ED25519_SIGNATURE_SIZE];
        let key = [0u8; ED25519_PUBLIC_KEY_SIZE];
        assert_eq!(
            verify_signature(toml, &sig, &key),
            Err(VerifyError::InvalidSignature)
        );
    }
}
