//! SHA-256 hash function (FIPS 180-4).
//!
//! Pure software, no_std, no allocations. Processes data in 64-byte blocks.

// ---------------------------------------------------------------------------
// Constants (FIPS 180-4 §4.2.2)
// ---------------------------------------------------------------------------

/// Round constants K[0..63].
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
    0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash values H0 (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Block size in bytes (512 bits).
const BLOCK_SIZE: usize = 64;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// SHA-256 digest (32 bytes).
pub type Digest = [u8; 32];

/// Incremental SHA-256 hasher.
pub struct Sha256 {
    state: [u32; 8],
    buf:   [u8; BLOCK_SIZE],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    /// Create a new SHA-256 hasher.
    pub const fn new() -> Self {
        Self {
            state: H0,
            buf: [0u8; BLOCK_SIZE],
            buf_len: 0,
            total_len: 0,
        }
    }

    /// Feed data into the hasher.
    pub fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        self.total_len += data.len() as u64;

        // Fill buffer first
        if self.buf_len > 0 {
            let space = BLOCK_SIZE - self.buf_len;
            let take = data.len().min(space);
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            offset += take;

            if self.buf_len == BLOCK_SIZE {
                let block = self.buf;
                compress(&mut self.state, &block);
                self.buf_len = 0;
            }
        }

        // Process full blocks
        while offset + BLOCK_SIZE <= data.len() {
            let mut block = [0u8; BLOCK_SIZE];
            block.copy_from_slice(&data[offset..offset + BLOCK_SIZE]);
            compress(&mut self.state, &block);
            offset += BLOCK_SIZE;
        }

        // Buffer remainder
        let remaining = data.len() - offset;
        if remaining > 0 {
            self.buf[..remaining].copy_from_slice(&data[offset..]);
            self.buf_len = remaining;
        }
    }

    /// Finalize and return the 32-byte digest.
    pub fn finalize(mut self) -> Digest {
        let bit_len = self.total_len * 8;

        // Pad: append 0x80, then zeros, then 64-bit big-endian bit length
        let mut pad = [0u8; BLOCK_SIZE];
        pad[0] = 0x80;

        let pad_len = if self.buf_len < 56 {
            56 - self.buf_len
        } else {
            120 - self.buf_len // two blocks needed
        };
        self.update(&pad[..pad_len]);
        self.update(&bit_len.to_be_bytes());

        // Produce output
        let mut digest = [0u8; 32];
        for i in 0..8 {
            digest[i * 4..(i + 1) * 4].copy_from_slice(&self.state[i].to_be_bytes());
        }
        digest
    }
}

/// One-shot SHA-256: hash `data` and return the 32-byte digest.
pub fn sha256(data: &[u8]) -> Digest {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

// ---------------------------------------------------------------------------
// Compression function (FIPS 180-4 §6.2.2)
// ---------------------------------------------------------------------------

fn compress(state: &mut [u32; 8], block: &[u8; BLOCK_SIZE]) {
    // Prepare message schedule W[0..63]
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],     block[i * 4 + 1],
            block[i * 4 + 2], block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        w[i] = small_sigma1(w[i - 2])
            .wrapping_add(w[i - 7])
            .wrapping_add(small_sigma0(w[i - 15]))
            .wrapping_add(w[i - 16]);
    }

    // Initialize working variables
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

    // 64 rounds
    for i in 0..64 {
        let t1 = h
            .wrapping_add(big_sigma1(e))
            .wrapping_add(ch(e, f, g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    // Update state
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

// ---------------------------------------------------------------------------
// SHA-256 logic functions (FIPS 180-4 §4.1.2)
// ---------------------------------------------------------------------------

#[inline(always)]
fn ch(x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (!x & z) }

#[inline(always)]
fn maj(x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (x & z) ^ (y & z) }

#[inline(always)]
fn big_sigma0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}

#[inline(always)]
fn big_sigma1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}

#[inline(always)]
fn small_sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}

#[inline(always)]
fn small_sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}
