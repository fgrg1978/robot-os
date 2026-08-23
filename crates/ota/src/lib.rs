#![no_std]
// SC01 — safety coding standard lints (see docs/SAFETY_CODING_STANDARD.md).
#![warn(
    clippy::pedantic,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
)]
#![allow(
    clippy::module_name_repetitions, // re-exporting from `pure` is intentional
    clippy::missing_errors_doc,      // most fns return bool/Option, not Result
    clippy::missing_panics_doc,      // panics audited via SC-5
    clippy::cast_possible_truncation, // u32/usize casts audited at call sites
    clippy::cast_sign_loss,           // FS APIs return isize for n bytes
    clippy::similar_names,            // crc_a/crc_b etc. are intentional
    clippy::indexing_slicing,         // FS read/write paths check len() above
    clippy::manual_let_else,          // explicit `if x < 0 { return ... }` is clearer here
    clippy::match_same_arms,          // intentional duplication for readability
    clippy::large_stack_arrays,       // OTA_RECV_BUF_SIZE buffer is by design (no_std, no heap in safety path)
)]
//! OTA firmware update — A/B slot management, image format, CRC-32.
//!
//! ## On-wire format (TCP transfer)
//!
//! The OTA header (24 bytes) is sent over TCP before the payload.
//! It is **never** stored on disk — only the raw kernel binary is written.
//!
//! ```text
//! Offset  Size  Field
//! 0x00    4     Magic: "ROTA" (0x524F5441)
//! 0x04    4     Header version (1)
//! 0x08    4     Image size (payload only, excl. header)
//! 0x0C    4     CRC-32 of payload (IEEE 802.3)
//! 0x10    4     Firmware version (major.minor.patch packed u32)
//! 0x14    1     Platform ID (0=qemu, 1=vf2, 2=k1)
//! 0x15    1     Flags (bit0=compressed)
//! 0x16    2     Reserved (zero)
//! 0x18    --    Payload (raw kernel binary)
//! ```
//!
//! ## Disk layout
//!
//! ```text
//! /fat/KERN_A.BIN  — raw kernel binary (no header), directly bootable by U-Boot
//! /fat/KERN_B.BIN  — raw kernel binary (no header), directly bootable by U-Boot
//! /fat/BOOTMETA    — INI text with slot info + per-slot CRC/size/version
//! ```
//!
//! ## Boot metadata (`/fat/BOOTMETA`)
//!
//! ```text
//! active_slot=a
//! boot_count=0
//! last_good=a
//! fw_version_a=0
//! fw_version_b=0
//! fw_version_r=0
//! image_size_a=0
//! image_size_b=0
//! image_size_r=0
//! image_crc_a=0
//! image_crc_b=0
//! image_crc_r=0
//! min_fw_version=0
//! ```
//!
//! `active_slot`/`last_good` are one-letter codes: `a`/`b`/`r` (OT04 —
//! `r` selects the recovery slot). The `_r` fields exist so the recovery
//! slot can be CRC-verified like A/B, but nothing writes them today: R is
//! flashed at the factory and never touched by OTA, so on every BOOTMETA
//! in the field `image_size_r` reads back as 0 and `ota_verify_slot(SLOT_R)`
//! is unconditionally `false` until some future flashing tool populates it.
//! Old (pre-OT04) BOOTMETA files simply lack these keys — the INI parser
//! treats missing keys as 0, which is the same "R not verifiable" state.
//!
//! All pure (deterministic, side-effect-free) logic lives in [`pure`] —
//! keep it that way so the host test crate can `#[path]`-include it.

use core::sync::atomic::{AtomicU32, Ordering};

pub mod pure;
pub mod recovery;
pub mod secure_boot;

// Re-export all pure constants, types, and functions for backwards compat.
pub use pure::{
    BootMeta, BootMetaRecord, BootValidateOutcome, Crc32State, OtaHeader,
    OTA_FLAG_COMPRESSED, OTA_HEADER_SIZE, OTA_HEADER_VERSION, OTA_MAGIC,
    OTA_PLATFORM_K1, OTA_PLATFORM_QEMU, OTA_PLATFORM_VF2,
    SLOT_A, SLOT_B, SLOT_R,
    crc32, ota_boot_validate_pure, ota_check_rollback_pure,
    ota_encode_header, ota_inactive_slot_pure,
    ota_mark_boot_good_pure, ota_next_seq, ota_parse_header,
    ota_pick_boot_meta_record, ota_pick_meta_write_slot, ota_validate_header,
    parse_boot_meta, parse_boot_meta_record, serialize_boot_meta,
    serialize_boot_meta_record,
};

/// Maximum firmware payload size the OTA receiver will accept, straight from
/// Kconfig (`OTA_MAX_IMAGE_SIZE_MB`, range 1-64, default 8 MiB).
///
/// This used to be a hardcoded `2 * 1024 * 1024` inside [`pure`] that ignored
/// the Kconfig symbol entirely — the symbol was declared, emitted as
/// `OTA_MAX_IMAGE_SIZE_BYTES`, and referenced by nobody. `Kconfig.ota`'s own
/// help text documented the intent to migrate it and it never happened.
///
/// It lives here rather than in [`pure`] because that module must stay
/// dependency-free for the host test crate; [`pure::ota_validate_header`]
/// takes the ceiling as a parameter instead.
///
/// NOTE: this is the *acceptance* limit only. Secure-boot verification has a
/// separate, smaller ceiling — see [`secure_boot::SECURE_BOOT_MAX_IMAGE_SIZE`]
/// for why the two cannot simply be the same number today.
pub const OTA_MAX_IMAGE_SIZE: usize = robot_os_limits::OTA_MAX_IMAGE_SIZE_BYTES;

pub use secure_boot::{
    BootTrust, BootTrustReason, SecureBootInfo,
    secure_boot_verify_slot, secure_boot_verify_slot_detailed,
    secure_boot_verify_staged_detailed, secure_boot_verify_image_detailed,
    secure_boot_require_signature, secure_boot_enforced_at_compile_time,
    secure_boot_set_require_signature, secure_boot_info,
    secure_boot_sig_path, secure_boot_tmp_path, SECURE_BOOT_PUBKEY,
    SECURE_BOOT_PUBKEY_LEN, SECURE_BOOT_SIG_LEN,
    SECURE_BOOT_SIG_PATH_A, SECURE_BOOT_SIG_PATH_B, SECURE_BOOT_SIG_PATH_R,
    SECURE_BOOT_TMP_PATH_A, SECURE_BOOT_TMP_PATH_B,
};

// ── Network defaults ───────────────────────────────────────────────────────

/// Default TCP port for OTA receive.
pub const OTA_DEFAULT_PORT: u16 = 8080;

/// Size of the streaming receive buffer (bytes per chunk).
pub const OTA_RECV_BUF_SIZE: usize = 4096;

// ── Boot loop detection ────────────────────────────────────────────────────

/// Maximum consecutive boot attempts before rollback.
pub const OTA_DEFAULT_MAX_BOOT_ATTEMPTS: u32 = 3;

/// Seconds of successful uptime before marking boot as good.
pub const OTA_BOOT_GOOD_DELAY_S: u32 = 30;

// ── Runtime atomics (populated from BOOTMETA at boot) ──────────────────────

/// Active boot slot (0 = A, 1 = B).
pub static CFG_OTA_ACTIVE_SLOT: AtomicU32 = AtomicU32::new(0);

/// Current boot count (incremented each boot, reset on success).
pub static CFG_OTA_BOOT_COUNT: AtomicU32 = AtomicU32::new(0);

/// Maximum boot attempts before automatic rollback.
pub static CFG_OTA_MAX_BOOT_ATTEMPTS: AtomicU32 =
    AtomicU32::new(OTA_DEFAULT_MAX_BOOT_ATTEMPTS);

/// One-shot latch for the "fell back to the unauthenticated legacy BOOTMETA"
/// warning in [`ota_read_boot_meta`]. 0 = not yet warned, 1 = warned.
/// See the comment at the use site for why the print must not repeat.
static LEGACY_META_WARNED: AtomicU32 = AtomicU32::new(0);

// ── File paths on FAT32 ────────────────────────────────────────────────────

pub const OTA_SLOT_A_PATH: &[u8] = b"/fat/KERN_A.BIN";
pub const OTA_SLOT_B_PATH: &[u8] = b"/fat/KERN_B.BIN";
/// OT04 — immutable recovery slot. Read-only; OTA never writes here.
/// Boot fallback: if both A and B fail to load, U-Boot tries this path.
pub const OTA_SLOT_R_PATH: &[u8] = b"/fat/KERN_R.BIN";

// OT02.A — atomic-write staging files for OTA payload.
// Receiver writes to `*.TMP` first, validates CRC, then promotes to `.BIN`.
pub const OTA_SLOT_A_TMP_PATH: &[u8] = b"/fat/KERN_A.TMP";
pub const OTA_SLOT_B_TMP_PATH: &[u8] = b"/fat/KERN_B.TMP";

// OT02.B — dual BOOTMETA records (power-loss safe).
// Two physical files; reader picks the higher-seq valid CRC; writer
// always targets the older/invalid one so a torn write loses at most
// one generation.
pub const OTA_META_PATH_A: &[u8] = b"/fat/BOOTMETA.A";
pub const OTA_META_PATH_B: &[u8] = b"/fat/BOOTMETA.B";

// Legacy single-file path — read-only fallback for one-time migration
// from the pre-OT02.B format. New writes never touch this path.
pub const OTA_META_PATH:   &[u8] = b"/fat/BOOTMETA";

/// Maximum size of one BOOTMETA record on disk.
///
/// Our serialized records are ~260 bytes (OT04 added the `_r` fields);
/// 512 keeps generous headroom and matches the FAT32 sector size.
pub const OTA_META_RECORD_MAX_BYTES: usize = 512;

// ── Slot management (FS-aware wrappers) ────────────────────────────────────

/// Return the inactive slot (the slot we write new firmware to).
pub fn ota_inactive_slot() -> u8 {
    pure::ota_inactive_slot_pure(CFG_OTA_ACTIVE_SLOT.load(Ordering::Acquire) as u8)
}

/// Return the FAT32 path for the given slot.
#[must_use]
pub fn ota_slot_path(slot: u8) -> &'static [u8] {
    match slot {
        SLOT_A => OTA_SLOT_A_PATH,
        SLOT_B => OTA_SLOT_B_PATH,
        SLOT_R => OTA_SLOT_R_PATH,
        _      => OTA_SLOT_A_PATH, // defensive default
    }
}

/// Return the active slot index.
pub fn ota_active_slot() -> u8 {
    CFG_OTA_ACTIVE_SLOT.load(Ordering::Acquire) as u8
}

// ── Boot metadata read/write — OT02.B dual-file power-loss-safe ────────────

/// Read a single BOOTMETA record from disk, validating its embedded CRC.
///
/// Returns `None` if the file is missing, empty, or fails CRC validation
/// (torn-write detection).
fn fs_read_meta_record(path: &[u8]) -> Option<BootMetaRecord> {
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path, robot_os_fs::O_RDONLY);
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; OTA_META_RECORD_MAX_BYTES];
    let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), buf.len());
    robot_os_fs::vfs_close(&mut fd_table, fd);
    if n <= 0 {
        return None;
    }
    parse_boot_meta_record(&buf[..n as usize])
}

/// Write a BOOTMETA record to disk and request a flush.
fn fs_write_meta_record(path: &[u8], record: &BootMetaRecord) {
    let mut buf = [0u8; OTA_META_RECORD_MAX_BYTES];
    let n = serialize_boot_meta_record(record, &mut buf);

    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path,
        robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC);
    if fd >= 0 {
        robot_os_fs::vfs_write(&mut fd_table, fd, buf.as_ptr(), n);
        robot_os_fs::vfs_close(&mut fd_table, fd);
        // Flush the FAT32 dirty cache so the record is durable on disk
        // before we return. A torn write here is exactly the case
        // OT02.B is designed to survive — but we still want each record
        // to be as durable as possible.
        let _ = robot_os_fs::fat32_sync();
    }
}

/// Write the plain, single-file `/fat/BOOTMETA` that U-Boot's `boot.cmd`
/// reads via `env import -t`. This is the kernel's dual-file `.A`/`.B`
/// scheme's *view* projected into the legacy format U-Boot understands —
/// U-Boot has no knowledge of the CRC/seq dual-file protocol. A torn
/// write here is accepted: `.A`/`.B` remain the kernel's own source of
/// truth and recover independently; this file only has to be *eventually*
/// consistent for U-Boot's next boot decision.
fn fs_write_plain_boot_meta(meta: &BootMeta) {
    let mut buf = [0u8; OTA_META_RECORD_MAX_BYTES];
    let n = serialize_boot_meta(meta, &mut buf);

    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, OTA_META_PATH,
        robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC);
    if fd >= 0 {
        robot_os_fs::vfs_write(&mut fd_table, fd, buf.as_ptr(), n);
        robot_os_fs::vfs_close(&mut fd_table, fd);
        let _ = robot_os_fs::fat32_sync();
    }
}

/// Read both dual-file records and the legacy single-file fallback.
///
/// Returns a tuple `(rec_a, rec_b)` with `None` for any file that
/// is missing or has a CRC mismatch.
#[must_use]
pub fn ota_read_boot_meta_records() -> (Option<BootMetaRecord>, Option<BootMetaRecord>) {
    (fs_read_meta_record(OTA_META_PATH_A),
     fs_read_meta_record(OTA_META_PATH_B))
}

/// Read effective boot metadata.
///
/// Selection order:
/// 1. The valid record with the higher `seq` from `BOOTMETA.A`/`BOOTMETA.B`.
/// 2. (Migration) the legacy `/fat/BOOTMETA` parsed as raw `BootMeta`
///    (no seq, no CRC) — only consulted on first boot after upgrade.
/// 3. `BootMeta::default()` if nothing is available.
#[must_use]
pub fn ota_read_boot_meta() -> BootMeta {
    let (rec_a, rec_b) = ota_read_boot_meta_records();
    if let Some(picked) = ota_pick_boot_meta_record(rec_a, rec_b) {
        return picked.meta;
    }

    // Legacy migration path: try the old single-file format.
    //
    // SECURITY — this record is UNAUTHENTICATED. Unlike `BOOTMETA.A`/`.B`,
    // the legacy file carries no `crc=` line, so `parse_boot_meta` accepts
    // whatever bytes are on disk: there is no torn-write detection and, more
    // to the point, no way to tell an old file left by an upgrade from one an
    // attacker planted. The FAT volume is exported over USB mass storage by
    // `msc_gadget.rs`, and reaching this branch only requires deleting or
    // corrupting both dual-file records — so "the legacy file is what we
    // read" is a state that can be *caused*, not merely inherited.
    //
    // What that buys an attacker is bounded but real: they choose
    // `active_slot`, `last_good`, and in particular `min_fw_version`, which
    // they will set to 0 to flatten the OT03 anti-rollback floor.
    // Deliberately NOT hardened into a rejection: refusing here falls through
    // to `BootMeta::default()`, whose `min_fw_version` is *also* 0, so
    // rejecting buys no security at all and costs the one-time migration this
    // branch exists for. `parse_slot_char` already clamps any unrecognised
    // slot code to `SLOT_A`, so there is no out-of-range slot to guard
    // against either.
    //
    // What actually contains this is secure boot: the anti-rollback floor is
    // advisory, and an image that reaches the slot still has to carry a
    // signature `secure_boot_verify_slot_detailed()` accepts. The warning
    // below exists so that a fleet operator sees the downgrade in the boot
    // log rather than discovering it forensically.
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, OTA_META_PATH,
                                    robot_os_fs::O_RDONLY);
    if fd >= 0 {
        let mut buf = [0u8; OTA_META_RECORD_MAX_BYTES];
        let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), buf.len());
        robot_os_fs::vfs_close(&mut fd_table, fd);
        if n > 0 {
            let meta = parse_boot_meta(&buf[..n as usize]);
            // Warn once per boot, not once per call. `ota_read_boot_meta()` is
            // on hot-ish paths (`ota_slot_info` → `secure_boot_verify_*`, the
            // shell's `ota status`), and the QEMU disk image built by the
            // Makefile ships exactly this layout — a plain `::BOOTMETA` with
            // no `.A`/`.B` — so an unlatched print here would repeat on every
            // call and drown the console the smoke tests read.
            if LEGACY_META_WARNED
                .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                robot_os_drivers::kprintln!(
                "[OTA] WARNING: both BOOTMETA.A/.B are missing or failed CRC — \
                 falling back to the UNAUTHENTICATED legacy /fat/BOOTMETA \
                 (active_slot={}, min_fw_version={}). Anti-rollback floor from \
                 this file is NOT trustworthy; secure boot is the only gate \
                 left. Expected once after upgrade — otherwise investigate.",
                    match meta.active_slot { SLOT_A => 'A', SLOT_B => 'B', _ => 'R' },
                    meta.min_fw_version);
            }
            return meta;
        }
    }

    BootMeta::default()
}

/// Write boot metadata using the dual-file scheme.
///
/// Picks the older (or invalid) of `BOOTMETA.A`/`BOOTMETA.B` as the
/// target, increments the sequence number, and writes the new record.
/// A torn write here is recoverable on next boot — the previous record
/// in the *other* file remains valid.
pub fn ota_write_boot_meta(meta: &BootMeta) {
    let (rec_a, rec_b) = ota_read_boot_meta_records();
    let current_best = ota_pick_boot_meta_record(rec_a, rec_b);
    let next_seq = ota_next_seq(current_best);
    let target_slot = ota_pick_meta_write_slot(rec_a, rec_b);
    let target_path = if target_slot == SLOT_A {
        OTA_META_PATH_A
    } else {
        OTA_META_PATH_B
    };
    let record = BootMetaRecord { meta: *meta, seq: next_seq };
    fs_write_meta_record(target_path, &record);
    // Keep U-Boot's view (boot.cmd's `env import -t` of the plain file)
    // in sync with what the kernel just decided.
    fs_write_plain_boot_meta(meta);
}

/// Apply boot metadata to runtime atomics.
pub fn ota_apply_meta(meta: &BootMeta) {
    CFG_OTA_ACTIVE_SLOT.store(u32::from(meta.active_slot), Ordering::Release);
    CFG_OTA_BOOT_COUNT.store(meta.boot_count, Ordering::Release);
}

/// Mark the current boot as successful (reset `boot_count`, set `last_good`).
pub fn ota_mark_boot_good() {
    let mut meta = ota_read_boot_meta();
    ota_mark_boot_good_pure(&mut meta);
    ota_write_boot_meta(&meta);
    CFG_OTA_BOOT_COUNT.store(0, Ordering::Release);
    robot_os_drivers::kprintln!("[OTA] Boot marked good (slot={})",
        match meta.active_slot {
            SLOT_A => 'A',
            SLOT_B => 'B',
            _ => 'R', // SLOT_R
        });
}

// ── Slot CRC verification (FS) ─────────────────────────────────────────────

/// Verify CRC-32 of a raw firmware file against the expected CRC from BOOTMETA.
///
/// The .BIN file is a raw kernel binary (no header). The expected CRC and
/// size come from BOOTMETA, which was set during the OTA receive.
///
/// Returns `true` if the file exists, size matches, and CRC matches.
#[must_use] 
pub fn ota_verify_slot(slot: u8) -> bool {
    let meta = ota_read_boot_meta();
    let expected_crc = meta.slot_crc(slot);
    let expected_size = meta.slot_size(slot);

    if expected_size == 0 {
        return false; // no firmware recorded for this slot
    }

    let path = ota_slot_path(slot);
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path, robot_os_fs::O_RDONLY);
    if fd < 0 { return false; }

    // Stream file and compute CRC-32
    let mut crc_state = Crc32State::new();
    let mut total_read = 0u32;
    let mut chunk = [0u8; OTA_RECV_BUF_SIZE];

    loop {
        let got = robot_os_fs::vfs_read(&mut fd_table, fd,
                                         chunk.as_mut_ptr(), chunk.len());
        if got <= 0 { break; }
        crc_state.update(&chunk[..got as usize]);
        total_read += got as u32;
    }
    robot_os_fs::vfs_close(&mut fd_table, fd);

    if total_read != expected_size {
        return false;
    }

    crc_state.finalize() == expected_crc
}

/// Get the slot info (version, size, crc) from BOOTMETA for display.
#[must_use] 
pub fn ota_slot_info(slot: u8) -> (u32, u32, u32) {
    let meta = ota_read_boot_meta();
    (meta.slot_version(slot), meta.slot_size(slot), meta.slot_crc(slot))
}

// ── Boot-time validation (called from kernel_main) ─────────────────────────

/// Boot-time OTA validation: increment `boot_count`, rollback if stuck.
///
/// Call after FAT32 is mounted and config is loaded, before starting tasks.
/// Returns the boot metadata (possibly updated with rollback).
pub fn ota_boot_validate() -> BootMeta {
    let mut meta = ota_read_boot_meta();
    let max_attempts = CFG_OTA_MAX_BOOT_ATTEMPTS.load(Ordering::Relaxed);

    // Pure FSM does the increment + possible rollback.
    let outcome = ota_boot_validate_pure(&mut meta, max_attempts);

    if outcome == BootValidateOutcome::RolledBack {
        robot_os_drivers::kprintln!(
            "[OTA] Boot loop detected (max={}) — rolling back to slot {}",
            max_attempts,
            if meta.last_good == SLOT_A { 'A' } else { 'B' });
    }

    // Persist updated metadata
    ota_write_boot_meta(&meta);
    ota_apply_meta(&meta);

    robot_os_drivers::kprintln!("[OTA] Boot: slot={} count={}/{} last_good={}",
        if meta.active_slot == SLOT_A { 'A' } else { 'B' },
        meta.boot_count, max_attempts,
        if meta.last_good == SLOT_A { 'A' } else { 'B' });

    meta
}

// ── Current platform detection ─────────────────────────────────────────────

/// Return the platform ID for the currently running kernel.
#[must_use] 
pub fn ota_current_platform() -> u8 {
    #[cfg(feature = "vf2")]
    { return OTA_PLATFORM_VF2; }
    #[cfg(feature = "k1")]
    { return OTA_PLATFORM_K1; }
    #[cfg(not(any(feature = "vf2", feature = "k1")))]
    { OTA_PLATFORM_QEMU }
}
