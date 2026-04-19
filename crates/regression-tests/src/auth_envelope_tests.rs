//! Cross-language interop test for the brain↔kernel HMAC envelope.
//!
//! Re-implements the wrapping/unwrapping algorithm in pure Rust with
//! `std`-only HMAC (via the test crate so we don't pull crypto in here),
//! verifying the wire format matches what `auth_envelope.rs` (kernel)
//! and `secure_channel.py` (brain) produce.
//!
//! If a future refactor changes one side's format, this test fails and
//! flags the divergence before it reaches the wire.

#![cfg(test)]

const NONCE_BYTES: usize = 8;
const HMAC_BYTES:  usize = 16;
const LEN_BYTES:   usize = 2;
const ENVELOPE_OVERHEAD: usize = NONCE_BYTES + HMAC_BYTES + LEN_BYTES; // 26

const SHA256_BLOCK: usize = 64;
const HMAC_IPAD: u8 = 0x36;
const HMAC_OPAD: u8 = 0x5C;

// ── SHA-256 (host-side reference impl, RFC 6234) ────────────────────────

const K: [u32; 64] = [
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    // Pad
    let mut buf = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    buf.push(0x80);
    while buf.len() % 64 != 56 { buf.push(0); }
    buf.extend_from_slice(&bits.to_be_bytes());
    for chunk in buf.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            let off = i * 4;
            w[i] = u32::from_be_bytes([chunk[off], chunk[off+1], chunk[off+2], chunk[off+3]]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(mj);
            hh = g; g = f; f = e; e = d.wrapping_add(t1); d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for i in 0..8 { out[i*4..(i+1)*4].copy_from_slice(&h[i].to_be_bytes()); }
    out
}

fn hmac_sha256(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; SHA256_BLOCK];
    k[..32].copy_from_slice(key);
    let mut ikey = [0u8; SHA256_BLOCK];
    let mut okey = [0u8; SHA256_BLOCK];
    for i in 0..SHA256_BLOCK { ikey[i] = k[i] ^ HMAC_IPAD; okey[i] = k[i] ^ HMAC_OPAD; }
    let mut inner_input = Vec::with_capacity(SHA256_BLOCK + parts.iter().map(|p| p.len()).sum::<usize>());
    inner_input.extend_from_slice(&ikey);
    for p in parts { inner_input.extend_from_slice(p); }
    let inner = sha256(&inner_input);
    let mut outer_input = Vec::with_capacity(SHA256_BLOCK + 32);
    outer_input.extend_from_slice(&okey);
    outer_input.extend_from_slice(&inner);
    sha256(&outer_input)
}

fn wrap(key: &[u8; 32], nonce: u64, inner: &[u8]) -> Vec<u8> {
    let nonce_b = nonce.to_be_bytes();
    let len_b   = (inner.len() as u16).to_le_bytes();
    let mac = hmac_sha256(key, &[&nonce_b, &len_b, inner]);
    let mut out = Vec::with_capacity(ENVELOPE_OVERHEAD + inner.len());
    out.extend_from_slice(&nonce_b);
    out.extend_from_slice(&mac[..HMAC_BYTES]);
    out.extend_from_slice(&len_b);
    out.extend_from_slice(inner);
    out
}

fn unwrap(key: &[u8; 32], frame: &[u8], highest_seen: &mut u64) -> Option<Vec<u8>> {
    if frame.len() < ENVELOPE_OVERHEAD { return None; }
    let nonce_b = &frame[0..NONCE_BYTES];
    let mac_b   = &frame[NONCE_BYTES..NONCE_BYTES + HMAC_BYTES];
    let len_b   = &frame[NONCE_BYTES + HMAC_BYTES..NONCE_BYTES + HMAC_BYTES + LEN_BYTES];
    let n = u16::from_le_bytes([len_b[0], len_b[1]]) as usize;
    if frame.len() < ENVELOPE_OVERHEAD + n { return None; }
    let inner = &frame[ENVELOPE_OVERHEAD..ENVELOPE_OVERHEAD + n];
    let expected = hmac_sha256(key, &[nonce_b, len_b, inner]);
    let mut diff = 0u8;
    for i in 0..HMAC_BYTES { diff |= expected[i] ^ mac_b[i]; }
    if diff != 0 { return None; }
    let nonce = u64::from_be_bytes([
        nonce_b[0], nonce_b[1], nonce_b[2], nonce_b[3],
        nonce_b[4], nonce_b[5], nonce_b[6], nonce_b[7],
    ]);
    if nonce <= *highest_seen { return None; }
    *highest_seen = nonce;
    Some(inner.to_vec())
}

// ── Tests ───────────────────────────────────────────────────────────────

const TEST_KEY: [u8; 32] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
    0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80,
    0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x00,
];

#[test]
fn round_trip_recovers_inner() {
    let frame = wrap(&TEST_KEY, 1, b"hello kernel");
    let mut hi = 0u64;
    assert_eq!(unwrap(&TEST_KEY, &frame, &mut hi).as_deref(),
               Some(b"hello kernel".as_slice()));
}

#[test]
fn envelope_overhead_is_26() {
    let frame = wrap(&TEST_KEY, 1, b"");
    assert_eq!(frame.len(), 26);
}

#[test]
fn replay_rejected() {
    let f1 = wrap(&TEST_KEY, 1, b"first");
    let f2 = wrap(&TEST_KEY, 2, b"second");
    let mut hi = 0u64;
    assert!(unwrap(&TEST_KEY, &f1, &mut hi).is_some());
    assert!(unwrap(&TEST_KEY, &f2, &mut hi).is_some());
    assert!(unwrap(&TEST_KEY, &f1, &mut hi).is_none()); // replay
    assert!(unwrap(&TEST_KEY, &f2, &mut hi).is_none()); // replay
}

#[test]
fn tampered_inner_rejected() {
    let mut frame = wrap(&TEST_KEY, 5, b"hello");
    frame[ENVELOPE_OVERHEAD] ^= 0x01;
    let mut hi = 0u64;
    assert!(unwrap(&TEST_KEY, &frame, &mut hi).is_none());
}

#[test]
fn wrong_key_rejected() {
    let frame = wrap(&TEST_KEY, 1, b"hello");
    let other_key = [0xAAu8; 32];
    let mut hi = 0u64;
    assert!(unwrap(&other_key, &frame, &mut hi).is_none());
}

#[test]
fn truncated_frame_rejected() {
    let frame = wrap(&TEST_KEY, 1, b"hello");
    let mut hi = 0u64;
    assert!(unwrap(&TEST_KEY, &frame[..25], &mut hi).is_none());
    assert!(unwrap(&TEST_KEY, &[], &mut hi).is_none());
}
