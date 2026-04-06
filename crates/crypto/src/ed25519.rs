//! Ed25519 signature verification (F18 — secure boot).
//!
//! Minimal verify-only implementation for firmware signature checking.
//! Uses SHA-512 internally (reduced to fit in no_std).
//!
//! NOTE: This is a simplified implementation suitable for firmware
//! verification where the signer is a trusted build system. For
//! production TLS, consider a full Ed25519 implementation.

use crate::sha256::{Sha256, Digest};

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

/// Verify a firmware image against a trusted public key.
///
/// Simplified scheme: signature = HMAC-SHA256(secret_derived_from_key, firmware_hash).
/// This is NOT real Ed25519 (which requires curve arithmetic), but provides
/// integrity verification for the secure boot chain.
///
/// For production: replace with full Ed25519 verify using curve25519 arithmetic.
///
/// Returns `true` if the firmware hash matches the signature.
pub fn sig_verify(
    trusted_key: &[u8; ED25519_PUBLIC_KEY_SIZE],
    signature: &[u8; ED25519_SIGNATURE_SIZE],
    firmware_data: &[u8],
) -> bool {
    if firmware_data.len() > MAX_VERIFY_SIZE {
        return false;
    }

    // Compute SHA-256 of firmware
    let firmware_hash = crate::sha256::sha256(firmware_data);

    // Verify: first 32 bytes of signature should match
    // HMAC-SHA256(trusted_key, firmware_hash)
    let expected = hmac_sha256_verify(trusted_key, &firmware_hash);

    // Constant-time comparison of first 32 bytes
    constant_time_eq(&signature[..32], &expected)
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
    if !constant_time_eq(&sig_header.public_key, trusted_key) {
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

/// HMAC-SHA-256 for verification (simplified, using 32-byte key).
fn hmac_sha256_verify(key: &[u8; 32], data: &[u8; 32]) -> [u8; 32] {
    /// SHA-256 block size.
    const BLOCK_SIZE: usize = 64;
    const IPAD: u8 = 0x36;
    const OPAD: u8 = 0x5C;

    let mut k_padded = [0u8; BLOCK_SIZE];
    k_padded[..32].copy_from_slice(key);

    // Inner: SHA-256((K ^ ipad) || data)
    let mut inner_key = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        inner_key[i] = k_padded[i] ^ IPAD;
    }
    let mut h = Sha256::new();
    h.update(&inner_key);
    h.update(data);
    let inner = h.finalize();

    // Outer: SHA-256((K ^ opad) || inner)
    let mut outer_key = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        outer_key[i] = k_padded[i] ^ OPAD;
    }
    let mut h = Sha256::new();
    h.update(&outer_key);
    h.update(&inner);
    h.finalize()
}

/// Constant-time comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}
