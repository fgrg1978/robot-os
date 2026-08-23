//! Ed25519 signature verification (F18 — secure boot).
//!
//! Minimal verify-only implementation for firmware signature checking.
//! Uses SHA-512 internally (reduced to fit in no_std).
//!
//! NOTE: This is a simplified implementation suitable for firmware
//! verification where the signer is a trusted build system. For
//! production TLS, consider a full Ed25519 implementation.

use crate::sha256::Digest;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Ed25519 public key size in bytes.
pub const ED25519_PUBLIC_KEY_SIZE: usize = 32;

/// Ed25519 signature size in bytes.
pub const ED25519_SIGNATURE_SIZE: usize = 64;

/// Maximum firmware image size for verification (2 MiB).
pub const MAX_VERIFY_SIZE: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Firmware signature header (prepended to signed firmware).
#[derive(Clone, Copy)]
pub struct FirmwareSignature {
    /// Magic bytes: "RSIG"
    pub magic: [u8; 4],
    /// Signature algorithm (0 = Ed25519-like with SHA-256)
    pub algorithm: u8,
    /// Public key that signed this firmware.
    pub public_key: [u8; ED25519_PUBLIC_KEY_SIZE],
    /// Signature over SHA-256(firmware_payload).
    pub signature: [u8; ED25519_SIGNATURE_SIZE],
    /// Size of the firmware payload (not including this header).
    pub payload_size: u32,
}

/// Signature header magic bytes.
pub const SIG_MAGIC: [u8; 4] = *b"RSIG";

/// Signature header size in bytes.
pub const SIG_HEADER_SIZE: usize = 4 + 1 + ED25519_PUBLIC_KEY_SIZE + ED25519_SIGNATURE_SIZE + 4;

/// Parse a firmware signature header from raw bytes.
pub fn sig_parse_header(data: &[u8]) -> Option<FirmwareSignature> {
    if data.len() < SIG_HEADER_SIZE {
        return None;
    }
    if data[0..4] != SIG_MAGIC {
        return None;
    }

    let mut pub_key = [0u8; ED25519_PUBLIC_KEY_SIZE];
    pub_key.copy_from_slice(&data[5..37]);

    let mut signature = [0u8; ED25519_SIGNATURE_SIZE];
    signature.copy_from_slice(&data[37..101]);

    let payload_size = u32::from_le_bytes([data[101], data[102], data[103], data[104]]);

    Some(FirmwareSignature {
        magic: SIG_MAGIC,
        algorithm: data[4],
        public_key: pub_key,
        signature,
        payload_size,
    })
}

/// Verify a firmware image against a trusted public key using
/// **real Ed25519** (RFC 8032) via the vetted `ed25519-dalek` crate.
///
/// Replaces the pre-2026-05 HMAC-SHA256 stub (task #213) which was
/// trivially forgeable by anyone holding the public key. The
/// signature now signs the firmware bytes directly per the RFC —
/// no pre-hash, no proprietary scheme — so it interoperates with
/// any standard `ed25519` signer (e.g. `tools/sign_ota.py`).
///
/// Returns `true` if the signature is valid for `firmware_data`
/// under `trusted_key`.
pub fn sig_verify(
    trusted_key: &[u8; ED25519_PUBLIC_KEY_SIZE],
    signature: &[u8; ED25519_SIGNATURE_SIZE],
    firmware_data: &[u8],
) -> bool {
    if firmware_data.len() > MAX_VERIFY_SIZE {
        return false;
    }
    let vk = match ed25519_dalek::VerifyingKey::from_bytes(trusted_key) {
        Ok(v) => v,
        Err(_) => return false,  // malformed pubkey bytes
    };
    let sig = ed25519_dalek::Signature::from_bytes(signature);
    // strict variant rejects non-canonical signatures (point + scalar).
    vk.verify_strict(firmware_data, &sig).is_ok()
}

/// Compute SHA-256 hash of firmware data (for signing on the build system).
pub fn firmware_hash(data: &[u8]) -> Digest {
    crate::sha256::sha256(data)
}

/// Verify boot chain: check firmware at a FAT32 path against stored signature.
///
/// `sig_header`: parsed signature header
/// `trusted_key`: the expected public key (from OTP or hardcoded)
///
/// Returns `true` if:
/// 1. Header magic is valid
/// 2. Public key matches trusted key
/// 3. Signature verifies against firmware hash
pub fn verify_boot_image(
    sig_header: &FirmwareSignature,
    trusted_key: &[u8; ED25519_PUBLIC_KEY_SIZE],
    firmware_data: &[u8],
) -> bool {
    // Check magic
    if sig_header.magic != SIG_MAGIC {
        return false;
    }

    // Check public key matches trusted key
    if !crate::ct::ct_eq(&sig_header.public_key, trusted_key) {
        return false;
    }

    // Check payload size
    if sig_header.payload_size as usize != firmware_data.len() {
        return false;
    }

    // Verify signature
    sig_verify(trusted_key, &sig_header.signature, firmware_data)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

// (Removed dead `hmac_sha256_verify` — leftover from the pre-#213 HMAC-SHA-256
// signature stub. `sig_verify` now uses real Ed25519 via `ed25519-dalek`.)

// The local `constant_time_eq` that used to live here guarded the
// secure-boot trusted-public-key comparison — the single highest-value
// comparison in the tree — and was one of the three copies missing the
// `black_box` barrier that `auth_envelope.rs` and `ota/secure_boot.rs`
// already had. It now calls `crate::ct::ct_eq`.
//
// The leak this closes is modest (the trusted key is not itself a secret),
// but an early-exiting compare against the trusted key lets an attacker
// with signing-free image control learn the key byte-by-byte from timing,
// which is a strictly worse position than the one we intended.
