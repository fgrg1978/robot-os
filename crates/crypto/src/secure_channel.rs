//! Secure channel — encrypted brain↔robot protocol (F07.4/F07.5).
//!
//! Wraps the binary brain protocol in AES-128-CTR encryption with
//! SHA-256 HMAC for integrity. Key agreement via X25519.
//!
//! ## Handshake (simplified PSK + ECDH)
//!
//! 1. Robot → Brain: `HELLO` + robot_public_key (32 bytes)
//! 2. Brain → Robot: `HELLO` + brain_public_key (32 bytes)
//! 3. Both compute: shared_secret = X25519(my_private, their_public)
//! 4. Derive keys: enc_key = SHA-256(shared_secret || "ENC")[0..16]
//!                  mac_key = SHA-256(shared_secret || "MAC")[0..16]
//! 5. All subsequent packets: AES-CTR(enc_key, nonce) + HMAC(mac_key)
//!
//! ## Encrypted packet format
//!
//! ```text
//! Offset  Size  Field
//! 0x00    12    Nonce (random 8 bytes + 4-byte counter)
//! 0x0C    2     Encrypted payload length (LE)
//! 0x0E    N     Encrypted payload (AES-128-CTR)
//! 0x0E+N  32    HMAC-SHA-256 over [nonce || length || ciphertext]
//! ```

use crate::sha256::{Sha256, Digest};
use crate::aes::{Aes128, AES_KEY_SIZE};
use crate::x25519;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Nonce size for AES-CTR (12 bytes: 8 random + 4 counter).
pub const NONCE_SIZE: usize = 12;

/// HMAC tag size (SHA-256 output).
pub const HMAC_SIZE: usize = 32;

/// Overhead per encrypted packet: nonce + length + HMAC.
pub const PACKET_OVERHEAD: usize = NONCE_SIZE + 2 + HMAC_SIZE;

/// Maximum plaintext payload size.
pub const MAX_PAYLOAD_SIZE: usize = 2048;

/// Handshake magic byte.
pub const HANDSHAKE_HELLO: u8 = 0x48; // 'H'

/// Key derivation label for encryption key.
const KDF_LABEL_ENC: &[u8] = b"ENC";
/// Key derivation label for MAC key.
const KDF_LABEL_MAC: &[u8] = b"MAC";

// ---------------------------------------------------------------------------
// Secure Channel State
// ---------------------------------------------------------------------------

/// State of the secure channel.
#[derive(Clone, Copy, PartialEq)]
pub enum ChannelState {
    /// No handshake performed yet.
    Init,
    /// Handshake complete, keys derived.
    Established,
}

/// Secure channel context.
pub struct SecureChannel {
    /// Channel state.
    pub state: ChannelState,
    /// Our private key (32 bytes).
    private_key: [u8; 32],
    /// Our public key (32 bytes).
    pub public_key: [u8; 32],
    /// Peer's public key (32 bytes).
    peer_public_key: [u8; 32],
    /// Derived encryption key (16 bytes for AES-128).
    enc_key: [u8; AES_KEY_SIZE],
    /// Derived MAC key (16 bytes).
    mac_key: [u8; AES_KEY_SIZE],
    /// Send nonce counter (incremented per packet).
    tx_counter: u32,
}

impl SecureChannel {
    /// Create a new secure channel with the given private key.
    pub fn new(private_key: [u8; 32]) -> Self {
        let public_key = x25519::x25519_pubkey(&private_key);
        Self {
            state: ChannelState::Init,
            private_key,
            public_key,
            peer_public_key: [0u8; 32],
            enc_key: [0u8; AES_KEY_SIZE],
            mac_key: [0u8; AES_KEY_SIZE],
            tx_counter: 0,
        }
    }

    /// Complete handshake with peer's public key. Derives encryption and MAC keys.
    pub fn handshake(&mut self, peer_public_key: &[u8; 32]) {
        self.peer_public_key = *peer_public_key;

        // Compute shared secret via X25519
        let shared_secret = x25519::x25519(&self.private_key, peer_public_key);

        // Derive encryption key: SHA-256(shared_secret || "ENC")[0..16]
        let mut h = Sha256::new();
        h.update(&shared_secret);
        h.update(KDF_LABEL_ENC);
        let digest = h.finalize();
        self.enc_key.copy_from_slice(&digest[..AES_KEY_SIZE]);

        // Derive MAC key: SHA-256(shared_secret || "MAC")[0..16]
        let mut h = Sha256::new();
        h.update(&shared_secret);
        h.update(KDF_LABEL_MAC);
        let digest = h.finalize();
        self.mac_key.copy_from_slice(&digest[..AES_KEY_SIZE]);

        self.state = ChannelState::Established;
        self.tx_counter = 0;
    }

    /// Encrypt a plaintext payload into the output buffer.
    ///
    /// Returns the total encrypted packet size, or 0 on error.
    /// `nonce_rand`: 8 bytes of randomness for the nonce.
    pub fn encrypt(&mut self, plaintext: &[u8], nonce_rand: &[u8; 8],
                   out: &mut [u8]) -> usize {
        if self.state != ChannelState::Established { return 0; }
        if plaintext.len() > MAX_PAYLOAD_SIZE { return 0; }

        let total = NONCE_SIZE + 2 + plaintext.len() + HMAC_SIZE;
        if out.len() < total { return 0; }

        // Build nonce: 8 random bytes + 4-byte counter
        let mut nonce = [0u8; NONCE_SIZE];
        nonce[..8].copy_from_slice(nonce_rand);
        nonce[8..12].copy_from_slice(&self.tx_counter.to_le_bytes());
        self.tx_counter += 1;

        // Write nonce
        out[..NONCE_SIZE].copy_from_slice(&nonce);

        // Write length (LE)
        let len_bytes = (plaintext.len() as u16).to_le_bytes();
        out[NONCE_SIZE..NONCE_SIZE + 2].copy_from_slice(&len_bytes);

        // Encrypt payload with AES-128-CTR
        let ct_start = NONCE_SIZE + 2;
        let ct_end = ct_start + plaintext.len();
        out[ct_start..ct_end].copy_from_slice(plaintext);
        let aes = Aes128::new(&self.enc_key);
        // Use first 12 bytes of nonce for CTR
        aes.ctr_encrypt(&nonce, &mut out[ct_start..ct_end]);

        // Compute HMAC over [nonce || length || ciphertext]
        let hmac = hmac_sha256(&self.mac_key, &out[..ct_end]);
        out[ct_end..ct_end + HMAC_SIZE].copy_from_slice(&hmac);

        total
    }

    /// Decrypt an encrypted packet. Returns plaintext length, or 0 on error.
    pub fn decrypt(&self, packet: &[u8], plaintext_out: &mut [u8]) -> usize {
        if self.state != ChannelState::Established { return 0; }
        if packet.len() < PACKET_OVERHEAD { return 0; }

        // Parse nonce
        let nonce: [u8; NONCE_SIZE] = {
            let mut n = [0u8; NONCE_SIZE];
            n.copy_from_slice(&packet[..NONCE_SIZE]);
            n
        };

        // Parse length
        let payload_len = u16::from_le_bytes([
            packet[NONCE_SIZE], packet[NONCE_SIZE + 1]
        ]) as usize;

        let ct_start = NONCE_SIZE + 2;
        let ct_end = ct_start + payload_len;
        let expected_total = ct_end + HMAC_SIZE;

        if packet.len() < expected_total { return 0; }
        if payload_len > MAX_PAYLOAD_SIZE { return 0; }
        if plaintext_out.len() < payload_len { return 0; }

        // Verify HMAC
        let expected_hmac = hmac_sha256(&self.mac_key, &packet[..ct_end]);
        let packet_hmac = &packet[ct_end..ct_end + HMAC_SIZE];
        if !constant_time_eq(&expected_hmac, packet_hmac) {
            return 0; // integrity check failed
        }

        // Decrypt
        plaintext_out[..payload_len].copy_from_slice(&packet[ct_start..ct_end]);
        let aes = Aes128::new(&self.enc_key);
        aes.ctr_decrypt(&nonce, &mut plaintext_out[..payload_len]);

        payload_len
    }
}

// ---------------------------------------------------------------------------
// HMAC-SHA-256 (RFC 2104)
// ---------------------------------------------------------------------------

/// Compute HMAC-SHA-256(key, data).
fn hmac_sha256(key: &[u8; AES_KEY_SIZE], data: &[u8]) -> Digest {
    /// SHA-256 block size.
    const BLOCK_SIZE: usize = 64;
    /// Inner padding byte.
    const IPAD: u8 = 0x36;
    /// Outer padding byte.
    const OPAD: u8 = 0x5C;

    // Key is 16 bytes, pad to 64 bytes with zeros
    let mut k_padded = [0u8; BLOCK_SIZE];
    k_padded[..key.len()].copy_from_slice(key);

    // Inner hash: SHA-256((K ⊕ ipad) || data)
    let mut inner_key = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        inner_key[i] = k_padded[i] ^ IPAD;
    }
    let mut h = Sha256::new();
    h.update(&inner_key);
    h.update(data);
    let inner_hash = h.finalize();

    // Outer hash: SHA-256((K ⊕ opad) || inner_hash)
    let mut outer_key = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        outer_key[i] = k_padded[i] ^ OPAD;
    }
    let mut h = Sha256::new();
    h.update(&outer_key);
    h.update(&inner_hash);
    h.finalize()
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}
