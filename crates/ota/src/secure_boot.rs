//! F18 — Secure boot: Ed25519 signature verification for OTA slots.
//!
//! # Design
//!
//! - The OTA header layout is unchanged — we keep CRC-32 there so old
//!   images still boot, and store Ed25519 signatures as a separate
//!   sidecar file per slot (`/KERN_A.SIG` and `/KERN_B.SIG`, at the FAT32
//!   volume root — same convention `tools/boot.cmd` uses for `KERN_A.BIN`).
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

/// Maximum image size the verifier can hold in its `.bss` hashing buffer.
///
/// This is DELIBERATELY NOT the same as `OTA_MAX_IMAGE_SIZE` (the acceptance
/// limit, which comes from Kconfig and defaults to 8 MiB). The two are
/// different concerns that used to share one hardcoded constant:
///
/// * `OTA_MAX_IMAGE_SIZE` — "how big an image will the OTA receiver accept?"
///   Pure header validation, no buffer involved, so Kconfig can set it freely.
/// * `SECURE_BOOT_MAX_IMAGE_SIZE` — "how big an image can we *verify*?" This
///   one is bounded by physical RAM, because pure Ed25519 (RFC 8032, what
///   `tools/sign_ota.py` and `verify_strict` both use) signs the raw message
///   and therefore needs the whole image contiguous in memory at once. Only
///   Ed25519ph would allow streaming, and that is a different scheme.
///
/// The binding constraint chain today:
/// ```text
///   kernel window (linker.ld MEMORY) ... 8 MiB
///   kernel image + .bss already uses ... ~4.3 MiB (~6.3 with this buffer)
///                                        ─────────
///   room left for this buffer .......... ~3.6 MiB
/// ```
/// Raising `linker.ld`'s `LENGTH` (the VF2 has 8 GB of RAM; the 8 MiB window
/// is a ceiling, not a reservation — the PMM starts at `_kernel_end`) is what
/// would let this match the 8 MiB acceptance limit.
///
/// An image larger than this but within `OTA_MAX_IMAGE_SIZE` is accepted by
/// the OTA receiver and then rejected by secure boot with
/// `BootTrustReason::ImageTooLargeToVerify` — fail-closed and explicit,
/// rather than silently truncating and reporting a bogus signature failure.
pub const SECURE_BOOT_MAX_IMAGE_SIZE: usize = 2 * 1024 * 1024;

// The verification buffer may be smaller than the acceptance limit (that gap
// is handled explicitly at runtime), but never larger — a buffer bigger than
// anything we would ever accept is pure wasted `.bss`.
const _: () = assert!(
    SECURE_BOOT_MAX_IMAGE_SIZE <= crate::OTA_MAX_IMAGE_SIZE,
    "SECURE_BOOT_MAX_IMAGE_SIZE exceeds OTA_MAX_IMAGE_SIZE (Kconfig OTA_MAX_IMAGE_SIZE_MB) — \
     the verification buffer would be larger than any image the OTA receiver accepts"
);

/// Slot signature file paths (alongside `KERN_A.BIN` / `KERN_B.BIN`).
///
/// Root-relative, no `/fat` prefix: unlike `crate::OTA_SLOT_A_PATH` (used
/// via the VFS layer, where `/fat` is a *mount point* stripped before
/// lookup), these paths go straight to `fat32_open()` (see
/// `read_sig_file`/`read_image_file` below), which resolves them directly
/// against the mounted volume — a leading `/fat/` there would be looked
/// up as a literal subdirectory named "fat", which doesn't exist. Real
/// hardware confirms the root-relative convention is correct: `boot.cmd`
/// (`tools/boot.cmd`) loads `BOOTMETA`/`KERN_A.BIN` via `fatload mmc 0:1`
/// with no subdirectory either. Found and fixed 2026-08 by actually
/// booting a signed image in QEMU (D2) instead of trusting this by
/// inspection — with the old `/fat/...` paths, `secure-boot-enforced`
/// could never find the `.SIG` file on a disk laid out the way U-Boot
/// actually expects, and would halt at the fail-closed `loop { wfi() }`
/// on every real boot once that feature was ever turned on.
pub const SECURE_BOOT_SIG_PATH_A: &[u8] = b"/KERN_A.SIG";
pub const SECURE_BOOT_SIG_PATH_B: &[u8] = b"/KERN_B.SIG";
/// OT04 — recovery slot signature (read-only, signed at flash time).
pub const SECURE_BOOT_SIG_PATH_R: &[u8] = b"/KERN_R.SIG";

/// Slot kernel-image paths for direct `fat32_open()` access (see
/// `read_image_file` below) — root-relative for the same reason as
/// `SECURE_BOOT_SIG_PATH_*` above. Deliberately separate from
/// `crate::OTA_SLOT_A_PATH`/`ota_slot_path()`, which stay `/fat`-prefixed
/// because their callers (`ota_verify_slot` and friends) go through the
/// VFS layer, where that prefix is the mount point, not a literal path
/// component.
pub const SECURE_BOOT_BIN_PATH_A: &[u8] = b"/KERN_A.BIN";
pub const SECURE_BOOT_BIN_PATH_B: &[u8] = b"/KERN_B.BIN";
pub const SECURE_BOOT_BIN_PATH_R: &[u8] = b"/KERN_R.BIN";

/// Root-relative staging paths, for verifying an OTA image *before* it is
/// promoted over the live slot binary.
///
/// Same root-relative convention as `SECURE_BOOT_BIN_PATH_*` above (these go
/// straight to `fat32_open()`), and deliberately separate from
/// `crate::OTA_SLOT_A_TMP_PATH` / `OTA_SLOT_B_TMP_PATH`, which keep the
/// `/fat` mount-point prefix because the OTA receiver reaches them through
/// the VFS layer.
///
/// These exist because verifying *after* promotion is not good enough: the
/// promotion is what destroys the rollback target. `cmd_ota_recv` writes into
/// the inactive slot, which is normally `last_good` — the exact image
/// `ota_boot_validate_pure()` and `ota rollback` fall back to. If a rejected
/// update had already overwritten it, an attacker who can merely *reach* the
/// OTA port would destroy the fallback without ever flipping `active_slot`,
/// turning the next failure of the active slot into an unrecoverable brick on
/// an enforced build. Verifying the `.TMP` leaves `KERN_{A,B}.BIN`
/// byte-identical when the image is refused.
pub const SECURE_BOOT_TMP_PATH_A: &[u8] = b"/KERN_A.TMP";
pub const SECURE_BOOT_TMP_PATH_B: &[u8] = b"/KERN_B.TMP";

/// Return the kernel-image path for a given slot index, for direct
/// `fat32_open()` access — see `SECURE_BOOT_BIN_PATH_*` doc comment.
#[must_use]
fn secure_boot_bin_path(slot: u8) -> &'static [u8] {
    match slot {
        crate::SLOT_A => SECURE_BOOT_BIN_PATH_A,
        crate::SLOT_B => SECURE_BOOT_BIN_PATH_B,
        crate::SLOT_R => SECURE_BOOT_BIN_PATH_R,
        _             => SECURE_BOOT_BIN_PATH_A,
    }
}

/// Return the *staging* (`.TMP`) path for a given slot index, for direct
/// `fat32_open()` access — see `SECURE_BOOT_TMP_PATH_*` doc comment.
///
/// `SLOT_R` has no staging file (the recovery slot is flashed at the factory
/// and OTA never writes it), so it falls through to slot A's path exactly the
/// way `secure_boot_bin_path` handles an out-of-range slot: defensively, not
/// meaningfully. Callers only ever pass `ota_inactive_slot()`, which is A or B.
#[must_use]
pub fn secure_boot_tmp_path(slot: u8) -> &'static [u8] {
    match slot {
        crate::SLOT_B => SECURE_BOOT_TMP_PATH_B,
        _             => SECURE_BOOT_TMP_PATH_A,
    }
}

/// Max size of a signature file on disk (header + slack).
pub const SECURE_BOOT_SIG_FILE_MAX: usize = SIG_HEADER_SIZE + 16;

/// Chunk size used when streaming a slot's kernel image off FAT32 into the
/// verification buffer (see `read_image_file`). Ed25519 (pure, RFC 8032 —
/// what `tools/sign_ota.py` and `ed25519_dalek::verify_strict` both use)
/// signs the raw message directly rather than a pre-hash, so the full image
/// still has to land in one contiguous buffer before `sig_verify()` can run;
/// this constant only bounds each individual `fat32_read()` call instead of
/// requesting the whole image in one frame. 4 KiB matches the chunk size
/// already used by the OTA receive path (`OTA_CHUNK` in
/// `crates/shell/src/lib.rs`).
pub const SECURE_BOOT_READ_CHUNK_SIZE: usize = 4096;

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

/// 0 = dev (warn on missing/bad sig, still boot); 1 = production (refuse to
/// run an unsigned image). Defaulted to 0 in dev builds; release builds with
/// `--features secure-boot-enforced` default this to 1 so a production binary
/// can't ship with sig-enforcement off if someone forgets to flip the runtime
/// flag. Either mode can still be flipped at runtime via the setter below.
#[cfg(not(feature = "secure-boot-enforced"))]
pub static CFG_SECURE_BOOT_REQUIRE_SIG: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "secure-boot-enforced")]
pub static CFG_SECURE_BOOT_REQUIRE_SIG: AtomicU32 = AtomicU32::new(1);

#[inline]
pub fn secure_boot_require_signature() -> bool {
    CFG_SECURE_BOOT_REQUIRE_SIG.load(Ordering::Relaxed) != 0
}

#[inline]
pub fn secure_boot_set_require_signature(require: bool) {
    CFG_SECURE_BOOT_REQUIRE_SIG.store(u32::from(require), Ordering::Relaxed);
}

/// Is the `secure-boot-enforced` feature compiled into this build?
///
/// This is deliberately NOT `secure_boot_require_signature()`. The two answer
/// different questions and only one of them is safe for a gate that must
/// agree with the boot path:
///
/// * `secure_boot_require_signature()` reads `CFG_SECURE_BOOT_REQUIRE_SIG`,
///   an atomic any caller can flip at runtime via
///   `secure_boot_set_require_signature()`. Advisory / soft callers only.
/// * this function is a pure `cfg!` — it cannot be relaxed at runtime, which
///   is exactly the property `kernel/src/main.rs`'s boot gate relies on
///   ("Policy is fixed at COMPILE TIME ... never by a runtime flag").
///
/// Any code that decides whether to *install* firmware must use this one, so
/// that it agrees with the gate that later decides whether to *boot* it. If
/// the installer consulted the relaxable runtime flag while the boot gate
/// consulted the feature, an enforced build could be talked into staging an
/// image it will then refuse to boot — which on a device whose only recovery
/// is physical access is a brick, not a refusal.
///
/// Exposed from this crate (rather than each caller writing its own
/// `#[cfg(feature = ...)]`) because the feature lives on `robot_os_ota`;
/// downstream crates such as `robot_os_shell` do not declare it, and cargo
/// feature unification means this answers for the whole build.
#[must_use]
pub const fn secure_boot_enforced_at_compile_time() -> bool {
    cfg!(feature = "secure-boot-enforced")
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
// Detailed failure reason (diagnostics only).
// ───────────────────────────────────────────────────────────────────────────

/// Fine-grained reason behind a `BootTrust::Unverified` / `BootTrust::Failed`
/// result. This exists purely for console diagnostics at boot (which slot,
/// why) — the pass/fail *decision* is entirely owned by `BootTrust` (and by
/// the caller's own policy for what to do with an `Unverified`/`Failed`
/// result); nothing reads `BootTrustReason` to decide whether to boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootTrustReason {
    /// Matches `BootTrust::Verified` — nothing to report.
    Verified,
    /// `SECURE_BOOT_PUBKEY` is all zeros (dev build, no signing key installed).
    NoTrustedKey,
    /// No `.SIG` file found (or unreadable) for this slot.
    SignatureAbsent,
    /// `.SIG` file present but its header is malformed (bad magic/version).
    SignatureMalformed,
    /// `.SIG` file's embedded pubkey doesn't match the trusted `SECURE_BOOT_PUBKEY`.
    PubkeyMismatch,
    /// The slot's kernel image could not be read from disk.
    ImageUnreadable,
    /// Ed25519 signature verification failed against the image contents.
    SignatureInvalid,
    /// The recorded image is larger than `SECURE_BOOT_MAX_IMAGE_SIZE`, so it
    /// cannot be held contiguously for pure-Ed25519 verification. Accepted by
    /// the OTA receiver (which uses the larger Kconfig `OTA_MAX_IMAGE_SIZE`)
    /// but not verifiable here. Fail-closed on purpose: reported as its own
    /// reason rather than silently truncating and blaming the signature.
    ImageTooLargeToVerify,
}

impl BootTrustReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BootTrustReason::Verified          => "verified",
            BootTrustReason::NoTrustedKey       => "no trusted signing key embedded (dev build)",
            BootTrustReason::SignatureAbsent    => "signature file absent",
            BootTrustReason::SignatureMalformed => "signature file malformed",
            BootTrustReason::PubkeyMismatch     => "signature key does not match trusted key",
            BootTrustReason::ImageUnreadable    => "kernel image unreadable from disk",
            BootTrustReason::SignatureInvalid   => "signature invalid for image contents",
            BootTrustReason::ImageTooLargeToVerify =>
                "image larger than the secure-boot verification buffer (raise linker.ld's \
                 kernel window, or lower Kconfig OTA_MAX_IMAGE_SIZE_MB)",
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

/// Read the kernel image at `path` into `out`. Returns actual bytes read.
///
/// Takes a path rather than a slot index because the same reader has to serve
/// two callers: the boot gate, which verifies the live `KERN_{A,B}.BIN`, and
/// the OTA receiver, which verifies the staged `KERN_{A,B}.TMP` before
/// promoting it (see `SECURE_BOOT_TMP_PATH_*`).
///
/// Reads happen in `SECURE_BOOT_READ_CHUNK_SIZE`-sized calls to
/// `fat32_read()` rather than one request spanning all of `out` — `out`
/// itself is still sized to hold the whole image (Ed25519 verification
/// needs it contiguous, see `SECURE_BOOT_READ_CHUNK_SIZE` doc comment),
/// but bounding each individual read keeps this symmetric with the OTA
/// receive path instead of asking the filesystem for a multi-MiB transfer
/// in a single call.
fn read_image_file(path: &[u8], out: &mut [u8]) -> usize {
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

    let mut total = 0usize;
    while total < out.len() {
        let end = (total + SECURE_BOOT_READ_CHUNK_SIZE).min(out.len());
        let n = fat32_read(file, &mut out[total..end]).unwrap_or(0);
        if n == 0 {
            break;
        }
        total += n;
    }

    let _ = fat32_close(file);
    total
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
    secure_boot_verify_slot_detailed(slot).0
}

/// As [`secure_boot_verify_slot`], but also returns the specific
/// [`BootTrustReason`] behind a non-`Verified` result, for boot-time
/// diagnostics. A thin wrapper over
/// [`secure_boot_verify_image_detailed`] so there is exactly one place the
/// checks are performed.
#[must_use]
pub fn secure_boot_verify_slot_detailed(slot: u8) -> (BootTrust, BootTrustReason) {
    secure_boot_verify_image_detailed(
        secure_boot_bin_path(slot),
        secure_boot_sig_path(slot),
        crate::ota_slot_info(slot).1,
    )
}

/// Verify a *staged* OTA image (`KERN_{A,B}.TMP`) against the signature
/// sidecar its promoted form would be checked against at boot.
///
/// The OTA receiver calls this BEFORE `ota_promote_tmp_to_bin`, so that a
/// refused image never touches the live slot binary — see
/// `SECURE_BOOT_TMP_PATH_*` for why overwriting the inactive slot with an
/// unverified image is itself the attack, independent of `active_slot`.
///
/// `image_size` comes from the OTA header the receiver just validated, not
/// from BOOTMETA: at this point BOOTMETA still describes the *previous*
/// occupant of the slot, so `ota_slot_info(slot)` would answer the wrong
/// question for the size-vs-verification-buffer check.
#[must_use]
pub fn secure_boot_verify_staged_detailed(
    slot: u8,
    image_size: u32,
) -> (BootTrust, BootTrustReason) {
    secure_boot_verify_image_detailed(
        secure_boot_tmp_path(slot),
        secure_boot_sig_path(slot),
        image_size,
    )
}

/// Verify the Ed25519 signature at `sig_path` over the image at `bin_path`.
///
/// `image_size` is the expected payload length, used only for the
/// "too large to hold contiguously" early-out; pass 0 if unknown, in which
/// case the check is skipped and the read is bounded by the buffer instead.
/// Both paths are root-relative (`fat32_open()` convention) — see
/// `SECURE_BOOT_SIG_PATH_*`.
#[must_use]
pub fn secure_boot_verify_image_detailed(
    bin_path: &[u8],
    sig_path: &[u8],
    image_size: u32,
) -> (BootTrust, BootTrustReason) {
    // Dev early-out: all-zero pubkey means no trusted key yet.
    if SECURE_BOOT_PUBKEY.iter().all(|b| *b == 0) {
        return (BootTrust::Unverified, BootTrustReason::NoTrustedKey);
    }

    let mut sig_buf = [0u8; SECURE_BOOT_SIG_FILE_MAX];
    let n = read_sig_file(sig_path, &mut sig_buf);
    if n == 0 {
        return (BootTrust::Unverified, BootTrustReason::SignatureAbsent);
    }

    let sig = match sig_parse_header(&sig_buf[..n]) {
        Some(s) => s,
        None    => return (BootTrust::Failed, BootTrustReason::SignatureMalformed),
    };

    // Trust check: signature's embedded pubkey must match the trusted one.
    // Constant-time comparison — `!=` on byte arrays short-circuits on the
    // first mismatching byte and leaks the trusted key bit-by-bit through
    // observable timing/power side channels (one boot per byte recovered).
    if !ct_eq(&sig.public_key, &SECURE_BOOT_PUBKEY) {
        return (BootTrust::Failed, BootTrustReason::PubkeyMismatch);
    }

    // The OTA receiver accepts up to `OTA_MAX_IMAGE_SIZE` (Kconfig), which can
    // legitimately exceed what fits in the verification buffer. Detect that up
    // front and say so, instead of letting `read_image_file` fill the buffer to
    // the brim and then reporting a signature failure that would send whoever
    // debugs it looking for a key problem that doesn't exist.
    if image_size as usize > SECURE_BOOT_MAX_IMAGE_SIZE {
        return (BootTrust::Failed, BootTrustReason::ImageTooLargeToVerify);
    }

    // Verification check.
    //
    // `SECURE_BOOT_MAX_IMAGE_SIZE` is 2 MiB — kernel task stacks are 2-16 KiB
    // (the boot stack is 64 KiB), so this buffer must NOT be a stack local.
    // It lives in `.bss` instead and is filled in bounded chunks by
    // `read_image_file()` (see `SECURE_BOOT_READ_CHUNK_SIZE`). Same pattern
    // as `ELF_BUF` in `crates/shell/src/lib.rs`.
    //
    // CONCURRENCY: this `static mut` now has two callers, where it used to
    // have one. The boot gate (`kernel/src/main.rs`) runs it single-hart,
    // before `task_create` and before `smp_start_secondary_harts()`, so it
    // cannot overlap with anything. The OTA receiver (`cmd_ota_recv`) runs
    // much later on the shell task — but strictly *after* the boot gate has
    // returned, and the shell is the only task that performs an OTA install,
    // so the two never run concurrently and two OTA installs cannot either.
    // OWNER DECISION: that is an argument from call-graph, not one the
    // compiler enforces. If a second concurrent verifier is ever added (a
    // DFU-side verify, a background "re-attest the slots" task), this buffer
    // needs a `SpinLock` — it is 2 MiB of shared mutable state with no guard.
    static mut IMG_BUF: [u8; SECURE_BOOT_MAX_IMAGE_SIZE] =
        [0u8; SECURE_BOOT_MAX_IMAGE_SIZE];
    let img_buf = unsafe { &mut *core::ptr::addr_of_mut!(IMG_BUF) };
    let img_len = read_image_file(bin_path, img_buf);
    if img_len == 0 {
        return (BootTrust::Failed, BootTrustReason::ImageUnreadable);
    }

    if sig_verify(&SECURE_BOOT_PUBKEY, &sig.signature, &img_buf[..img_len]) {
        (BootTrust::Verified, BootTrustReason::Verified)
    } else {
        (BootTrust::Failed, BootTrustReason::SignatureInvalid)
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
