//! HMAC-SHA-256 authenticated envelope for the brain↔kernel TCP stream.
//!
//! Pairs with `robot-brain/secure_channel.py` byte-for-byte. Wrapping
//! a brain protocol packet adds 26 bytes of overhead (8B nonce + 16B
//! truncated HMAC + 2B inner length).
//!
//! ## Threat model
//!
//! Without this layer the brain↔kernel TCP stream is plaintext on a
//! presumed-trusted LAN. Any peer on the segment can:
//! - Forge an ESTOP packet (`PKT_ESTOP=0x88`) and stop every robot.
//! - Replay an old "FORWARD 100" actuator command indefinitely.
//! - Inject malformed packets to crash the kernel parser.
//!
//! With this layer the attacker would need the pre-shared key to
//! produce a valid HMAC. The replay-protection (monotonically
//! increasing nonce) ensures even captured-and-replayed authentic
//! packets are rejected.
//!
//! ## Wire format
//!
//! ```text
//! Offset  Size  Field
//! 0x00    8     Nonce (monotonic u64, big-endian)
//! 0x08    16    HMAC-SHA-256 over (nonce || len || inner) truncated to 16
//! 0x18    2     Inner length (LE u16)
//! 0x1A    N     Inner brain protocol packet
//! ```
//!
//! ## Key management
//!
//! The 32-byte symmetric key is loaded from `/fat/LINK.KEY`
//! (raw bytes). When the file is missing, the channel falls back to
//! plaintext mode and logs a warning — explicit opt-in.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU64, Ordering};
use robot_os_crypto::sha256::{Sha256, Digest};

// ── Constants ───────────────────────────────────────────────────────────

pub const KEY_BYTES:         usize = 32;
pub const NONCE_BYTES:       usize = 8;
pub const HMAC_BYTES:        usize = 16;
pub const LEN_BYTES:         usize = 2;
pub const ENVELOPE_OVERHEAD: usize = NONCE_BYTES + HMAC_BYTES + LEN_BYTES;
pub const MAX_INNER_BYTES:   usize = 8 * 1024;

const SHA256_BLOCK_SIZE: usize = 64;
const SHA256_DIGEST_LEN: usize = 32;
const HMAC_IPAD: u8 = 0x36;
const HMAC_OPAD: u8 = 0x5C;

// ── HMAC-SHA-256 over arbitrary-length key ──────────────────────────────
//
// Performance optimisation: the (K ⊕ ipad) and (K ⊕ opad) blocks depend
// only on the key, not on the data. We precompute them once at init()
// time and reuse on every wrap/unwrap. With a 32-byte key padded to 64
// bytes, this saves 128 byte XORs per HMAC call (256 cycles on RV64),
// which adds up: at 100 packets/sec and 2 HMACs/packet (wrap+verify),
// the kernel was burning ~50K cycles/sec just rebuilding ipad/opad.

/// Pre-computed (K ⊕ ipad) — populated by `init()`.
static mut IKEY_BLOCK: [u8; SHA256_BLOCK_SIZE] = [0u8; SHA256_BLOCK_SIZE];
/// Pre-computed (K ⊕ opad) — populated by `init()`.
static mut OKEY_BLOCK: [u8; SHA256_BLOCK_SIZE] = [0u8; SHA256_BLOCK_SIZE];

/// HMAC-SHA-256 using the pre-computed ikey/okey blocks. Caller must
/// only use this AFTER `init()` has been called (gated by KEY_LOADED).
fn hmac_sha256_precomputed(data_parts: &[&[u8]]) -> Digest {
    // SAFETY: IKEY_BLOCK/OKEY_BLOCK are written once at init() before
    // any wrap/unwrap can be issued; from then on read-only.
    let ikey = unsafe { &*core::ptr::addr_of!(IKEY_BLOCK) };
    let okey = unsafe { &*core::ptr::addr_of!(OKEY_BLOCK) };

    // Inner: SHA-256(ikey || data_parts...)
    let mut h = Sha256::new();
    h.update(ikey);
    for part in data_parts { h.update(part); }
    let inner = h.finalize();

    // Outer: SHA-256(okey || inner)
    let mut h = Sha256::new();
    h.update(okey);
    h.update(&inner);
    h.finalize()
}

/// Constant-time byte-array comparison.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for i in 0..a.len() { diff |= a[i] ^ b[i]; }
    core::hint::black_box(diff) == 0
}

// ── State (key + replay window) ─────────────────────────────────────────

/// Pre-shared link key. All zeros = unconfigured (channel plaintext).
static mut LINK_KEY: [u8; KEY_BYTES] = [0u8; KEY_BYTES];

/// Highest accepted nonce (replay-protection high-water mark).
static HIGHEST_RX_NONCE: AtomicU64 = AtomicU64::new(0);

/// Send-side counter. Initialised from CLINT cycles to avoid collisions
/// across reboots without persisting the counter to disk.
static SEND_NONCE: AtomicU64 = AtomicU64::new(0);

/// True once `init()` has been called with a valid 32-byte key.
static mut KEY_LOADED: bool = false;

// ── Public API ──────────────────────────────────────────────────────────

/// Install the link key. Returns true if successful, false if `key.len() != 32`.
///
/// SAFETY: callers must ensure no in-flight wrap/unwrap is racing with
/// this call (typically only invoked at boot).
pub unsafe fn init(key: &[u8]) -> bool {
    if key.len() != KEY_BYTES { return false; }
    let dst = &mut *core::ptr::addr_of_mut!(LINK_KEY);
    dst.copy_from_slice(key);

    // Pre-compute (K ⊕ ipad) and (K ⊕ opad) once — see optimisation note
    // above hmac_sha256_precomputed. Saves ~256 cycles per HMAC call.
    let mut k_pad = [0u8; SHA256_BLOCK_SIZE];
    k_pad[..KEY_BYTES].copy_from_slice(key);
    let ikey = &mut *core::ptr::addr_of_mut!(IKEY_BLOCK);
    let okey = &mut *core::ptr::addr_of_mut!(OKEY_BLOCK);
    for i in 0..SHA256_BLOCK_SIZE {
        ikey[i] = k_pad[i] ^ HMAC_IPAD;
        okey[i] = k_pad[i] ^ HMAC_OPAD;
    }

    KEY_LOADED = true;
    // Seed send nonce from cycle counter so reboot doesn't reuse low values.
    SEND_NONCE.store(robot_os_drivers::clint::get_time(), Ordering::Release);
    HIGHEST_RX_NONCE.store(0, Ordering::Release);
    true
}

/// True if the channel has been keyed (otherwise wrap/unwrap fall back
/// to identity for backwards compatibility).
pub fn is_authenticated() -> bool {
    // SAFETY: KEY_LOADED is set only by init() at boot; all subsequent
    // reads are racy-but-safe (a slightly-stale view is correctness-equiv).
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(KEY_LOADED)) }
}

/// Wrap an inner brain protocol packet with the HMAC envelope.
///
/// Writes nonce + hmac + len + inner into `out`, returning the number of
/// bytes written. Returns 0 if `out` is too small or `inner` exceeds the
/// max inner-packet size. If the channel has no key configured,
/// returns the inner unchanged (legacy plaintext mode).
pub fn wrap(inner: &[u8], out: &mut [u8]) -> usize {
    if !is_authenticated() {
        if out.len() < inner.len() { return 0; }
        out[..inner.len()].copy_from_slice(inner);
        return inner.len();
    }
    if inner.len() > MAX_INNER_BYTES { return 0; }
    let total = ENVELOPE_OVERHEAD + inner.len();
    if out.len() < total { return 0; }

    let nonce = SEND_NONCE.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    let nonce_b = nonce.to_be_bytes();
    let len_b   = (inner.len() as u16).to_le_bytes();

    let mac = hmac_sha256_precomputed(&[&nonce_b, &len_b, inner]);

    out[0..NONCE_BYTES].copy_from_slice(&nonce_b);
    out[NONCE_BYTES..NONCE_BYTES + HMAC_BYTES].copy_from_slice(&mac[..HMAC_BYTES]);
    out[NONCE_BYTES + HMAC_BYTES..NONCE_BYTES + HMAC_BYTES + LEN_BYTES]
        .copy_from_slice(&len_b);
    out[ENVELOPE_OVERHEAD..ENVELOPE_OVERHEAD + inner.len()]
        .copy_from_slice(inner);
    total
}

/// Unwrap an envelope. Returns `Some(inner_len)` and writes the inner
/// packet into `out`, or `None` on:
///   - frame too short
///   - HMAC mismatch
///   - replay (nonce <= highest accepted)
///
/// When the channel has no key configured, returns `Some(frame.len())`
/// and copies frame → out unchanged (legacy plaintext mode).
pub fn unwrap<'a>(frame: &'a [u8], out: &mut [u8]) -> Option<usize> {
    if !is_authenticated() {
        if out.len() < frame.len() { return None; }
        out[..frame.len()].copy_from_slice(frame);
        return Some(frame.len());
    }
    if frame.len() < ENVELOPE_OVERHEAD { return None; }

    let nonce_b: &[u8] = &frame[0..NONCE_BYTES];
    let mac_b:   &[u8] = &frame[NONCE_BYTES..NONCE_BYTES + HMAC_BYTES];
    let len_b:   &[u8] = &frame[NONCE_BYTES + HMAC_BYTES..NONCE_BYTES + HMAC_BYTES + LEN_BYTES];
    let n = u16::from_le_bytes([len_b[0], len_b[1]]) as usize;
    if n > MAX_INNER_BYTES { return None; }
    if frame.len() < ENVELOPE_OVERHEAD + n { return None; }
    if out.len() < n { return None; }

    let inner = &frame[ENVELOPE_OVERHEAD..ENVELOPE_OVERHEAD + n];

    let expected = hmac_sha256_precomputed(&[nonce_b, len_b, inner]);
    if !ct_eq(&expected[..HMAC_BYTES], mac_b) { return None; }

    // Replay defence — strictly monotonic nonce.
    let mut nonce_arr = [0u8; 8];
    nonce_arr.copy_from_slice(nonce_b);
    let nonce = u64::from_be_bytes(nonce_arr);
    let high = HIGHEST_RX_NONCE.load(Ordering::Acquire);
    if nonce <= high { return None; }
    HIGHEST_RX_NONCE.store(nonce, Ordering::Release);

    out[..n].copy_from_slice(inner);
    Some(n)
}
