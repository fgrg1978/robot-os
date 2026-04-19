//! F18 — Secure boot: Ed25519 signature verification for OTA slots.
//!
//! # Design
//!
//! - The OTA header layout is unchanged — we keep CRC-32 there so old
//!   images still boot, and store Ed25519 signatures as a separate
//!   sidecar file per slot (`/fat/KERN_A.SIG` and `/fat/KERN_B.SIG`).
//! - The signature file uses the `RSIG` format already provided by
//!   `robot_os_crypto::ed25519` (`FirmwareSignature`): magic + version
//!   + pubkey + signature + length.
//! - The kernel compares the `pubkey` field against the embedded
//!   trusted `SECURE_BOOT_PUBKEY`. Mismatch → rejected, regardless of
//!   whether the signature itself is mathematically valid.
//! - Missing signature file ⇒ `BootTrust::Unverified` (warning). When
//!   `secure_boot_require_signature()` is true, Unverified becomes
//!   fatal (rollback to `last_good`).
//!
//! # Production vs development
//!
//! - Dev key is embedded at `SECURE_BOOT_PUBKEY` in this file (ALL ZEROS
//!   by default). A production build replaces it via linker override or
//!   an eFuse/OTP read at boot (not implemented here).
//! - Sign images with `tools/sign_ota.py` using the matching private key
//!   (see `tools/gen_dev_key.py`).

use core::sync::atomic::{AtomicU32, Ordering};
use robot_os_crypto::ed25519::{
    sig_parse_header, sig_verify,
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, SIG_HEADER_SIZE,
};

// ───────────────────────────────────────────────────────────────────────────
// Named constants — no magic numbers.
// ───────────────────────────────────────────────────────────────────────────

/// Length of the Ed25519 public key (bytes).
pub const SECURE_BOOT_PUBKEY_LEN: usize = ED25519_PUBLIC_KEY_SIZE;
/// Length of the Ed25519 signature (bytes).
pub const SECURE_BOOT_SIG_LEN: usize = ED25519_SIGNATURE_SIZE;

/// Maximum image size that the verifier will read into its hashing buffer
/// from disk. Matches `OTA_MAX_IMAGE_SIZE` (2 MiB) — declared here to avoid a
/// circular reference with the parent module.
pub const SECURE_BOOT_MAX_IMAGE_SIZE: usize = 2 * 1024 * 1024;

/// Slot signature file paths (alongside `KERN_A.BIN` / `KERN_B.BIN`).
pub const SECURE_BOOT_SIG_PATH_A: &[u8] = b"/fat/KERN_A.SIG";
pub const SECURE_BOOT_SIG_PATH_B: &[u8] = b"/fat/KERN_B.SIG";
/// OT04 — recovery slot signature (read-only, signed at flash time).
pub const SECURE_BOOT_SIG_PATH_R: &[u8] = b"/fat/KERN_R.SIG";

/// Max size of a signature file on disk (header + slack).
pub const SECURE_BOOT_SIG_FILE_MAX: usize = SIG_HEADER_SIZE + 16;

// ───────────────────────────────────────────────────────────────────────────
// Trusted public key.
//
// OT05 — the array contents come from `build.rs`, which reads
// `tools/keys/prod_pub.bin` at compile time (or `$PROD_PUBKEY_PATH`). When
// no prod key file is present, the array is all zeros and the kernel treats
// every signature as Unverified (dev default). To rotate to a real key:
//
//   1. python3 tools/gen_prod_key.py        # writes prod_priv.bin + prod_pub.bin
//   2. cargo clean -p robot_os_ota          # force build.rs to re-run
//   3. cargo build --release --features qemu
//
// The kernel binary now embeds the real pubkey; signed firmware images
// produced by `tools/sign_ota.py --priv tools/keys/prod_priv.bin` will
// verify, all others will fail with BootTrust::Failed.
// ───────────────────────────────────────────────────────────────────────────

include!(concat!(env!("OUT_DIR"), "/secure_boot_pubkey.rs"));

/// Trusted public key. Override at link time or via OTP in production.
#[no_mangle]
#[link_section = ".secure_boot_pubkey"]
pub static SECURE_BOOT_PUBKEY: [u8; SECURE_BOOT_PUBKEY_LEN] =
    SECURE_BOOT_PUBKEY_BYTES;

// ───────────────────────────────────────────────────────────────────────────
// Enforcement policy.
// ───────────────────────────────────────────────────────────────────────────

/// 0 = dev (warn on missing/bad sig, still boot); 1 = production (refuse).
pub static CFG_SECURE_BOOT_REQUIRE_SIG: AtomicU32 = AtomicU32::new(0);

#[inline]
pub fn secure_boot_require_signature() -> bool {
    CFG_SECURE_BOOT_REQUIRE_SIG.load(Ordering::Relaxed) != 0
}

#[inline]
pub fn secure_boot_set_require_signature(require: bool) {
    CFG_SECURE_BOOT_REQUIRE_SIG.store(u32::from(require), Ordering::Relaxed);
}

// ───────────────────────────────────────────────────────────────────────────
// Boot trust level.
// ───────────────────────────────────────────────────────────────────────────

/// Trust level returned by `secure_boot_verify_slot()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootTrust {
    /// Signature present, matches embedded pubkey, passes verification.
    Verified,
    /// No .SIG file found (dev mode). Boot allowed with warning.
    Unverified,
    /// .SIG present but pubkey mismatch or signature verification failed.
    Failed,
}

impl BootTrust {
    #[must_use] 
    pub fn is_bootable(self) -> bool {
        match self {
            BootTrust::Verified   => true,
            BootTrust::Unverified => !secure_boot_require_signature(),
            BootTrust::Failed     => false,
        }
    }

    #[must_use] 
    pub fn as_str(self) -> &'static str {
        match self {
            BootTrust::Verified   => "verified",
            BootTrust::Unverified => "unverified",
            BootTrust::Failed     => "failed",
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Verification.
// ───────────────────────────────────────────────────────────────────────────

/// Return the signature-file path for a given slot index.
#[must_use]
pub fn secure_boot_sig_path(slot: u8) -> &'static [u8] {
    match slot {
        crate::SLOT_A => SECURE_BOOT_SIG_PATH_A,
        crate::SLOT_B => SECURE_BOOT_SIG_PATH_B,
        crate::SLOT_R => SECURE_BOOT_SIG_PATH_R,
        _             => SECURE_BOOT_SIG_PATH_A,
    }
}

/// Read the whole signature-file contents into `out`. Returns length or 0.
///
/// The `.SIG` file on disk is a single `FirmwareSignature` structure
/// followed by arbitrary padding to the next cluster boundary.
fn read_sig_file(path: &[u8], out: &mut [u8]) -> usize {
    use robot_os_fs::{
        fat32_mount_volume, fat32_open, fat32_read, fat32_close, open_flags,
    };

    let vol = match fat32_mount_volume() {
        Ok(v)  => v,
        Err(_) => return 0,
    };
    let file = match fat32_open(vol, path, open_flags::READ) {
        Ok(f)  => f,
        Err(_) => return 0,
    };
    let n = fat32_read(file, out).unwrap_or(0);
    let _ = fat32_close(file);
    n
}

/// Read the kernel image of `slot` into `out`. Returns actual bytes read.
fn read_slot_image(slot: u8, out: &mut [u8]) -> usize {
    use robot_os_fs::{
        fat32_mount_volume, fat32_open, fat32_read, fat32_close, open_flags,
    };

    let path = crate::ota_slot_path(slot);
    let vol = match fat32_mount_volume() {
        Ok(v)  => v,
        Err(_) => return 0,
    };
    let file = match fat32_open(vol, path, open_flags::READ) {
        Ok(f)  => f,
        Err(_) => return 0,
    };
    let n = fat32_read(file, out).unwrap_or(0);
    let _ = fat32_close(file);
    n
}

/// Verify the Ed25519 signature of a slot's kernel image.
///
/// # Behaviour
/// 1. If `SECURE_BOOT_PUBKEY` is all zeros, return `Unverified`
///    (dev build — no signing key installed).
/// 2. Attempt to read `/fat/KERN_{A,B}.SIG` into a local buffer.
/// 3. Parse the header; fail if magic/version wrong.
/// 4. Compare the embedded key in the header against the trusted
///    `SECURE_BOOT_PUBKEY`. Mismatch → `Failed`.
/// 5. Read the kernel image, verify the Ed25519 signature against
///    SHA-256(image).
#[must_use] 
pub fn secure_boot_verify_slot(slot: u8) -> BootTrust {
    // Dev early-out: all-zero pubkey means no trusted key yet.
    if SECURE_BOOT_PUBKEY.iter().all(|b| *b == 0) {
        return BootTrust::Unverified;
    }

    let path = secure_boot_sig_path(slot);
    let mut sig_buf = [0u8; SECURE_BOOT_SIG_FILE_MAX];
    let n = read_sig_file(path, &mut sig_buf);
    if n == 0 {
        return BootTrust::Unverified;
    }

    let sig = match sig_parse_header(&sig_buf[..n]) {
        Some(s) => s,
        None    => return BootTrust::Failed,
    };

    // Trust check: signature's embedded pubkey must match the trusted one.
    // Constant-time comparison — `!=` on byte arrays short-circuits on the
    // first mismatching byte and leaks the trusted key bit-by-bit through
    // observable timing/power side channels (one boot per byte recovered).
    if !ct_eq(&sig.public_key, &SECURE_BOOT_PUBKEY) {
        return BootTrust::Failed;
    }

    // Verification check.
    let mut img_buf = [0u8; SECURE_BOOT_MAX_IMAGE_SIZE];
    let img_len = read_slot_image(slot, &mut img_buf);
    if img_len == 0 {
        return BootTrust::Failed;
    }

    if sig_verify(&SECURE_BOOT_PUBKEY, &sig.signature, &img_buf[..img_len]) {
        BootTrust::Verified
    } else {
        BootTrust::Failed
    }
}

/// Best-effort verification that adds the trust string to a text buffer.
/// Intended for kprintln: "secure boot: verified/unverified/failed".
#[must_use] 
pub fn secure_boot_status_str(slot: u8) -> &'static str {
    secure_boot_verify_slot(slot).as_str()
}

// ───────────────────────────────────────────────────────────────────────────
// Constant-time byte-array comparison — used for the pubkey check above
// to avoid timing-side-channel leakage of the trusted key.
// ───────────────────────────────────────────────────────────────────────────

#[inline]
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    // Use volatile-ish read to discourage the compiler from short-circuiting
    // (RISC-V branch prediction shouldn't matter for u8, but be defensive).
    core::hint::black_box(diff) == 0
}

// ───────────────────────────────────────────────────────────────────────────
// Re-exports from crypto crate for callers' convenience.
// ───────────────────────────────────────────────────────────────────────────

pub use robot_os_crypto::ed25519::{
    verify_boot_image as secure_boot_verify_raw,
    firmware_hash as secure_boot_hash,
    FirmwareSignature,
};

pub const SECURE_BOOT_HEADER_SIZE: usize = SIG_HEADER_SIZE;

// ───────────────────────────────────────────────────────────────────────────
// Diagnostic helper used by the boot path / shell.
// ───────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct SecureBootInfo {
    pub trust:   BootTrust,
    pub require: bool,
    pub pubkey:  [u8; SECURE_BOOT_PUBKEY_LEN],
}

#[must_use] 
pub fn secure_boot_info(slot: u8) -> SecureBootInfo {
    SecureBootInfo {
        trust:   secure_boot_verify_slot(slot),
        require: secure_boot_require_signature(),
        pubkey:  SECURE_BOOT_PUBKEY,
    }
}
