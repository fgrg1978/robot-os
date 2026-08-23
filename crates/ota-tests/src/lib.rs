//! OT01 — host-side unit tests for the pure OTA logic.
//!
//! Pulls in `crates/ota/src/pure.rs` directly via `#[path]` so we test the
//! exact same source the kernel uses, without dragging in the FAT32 +
//! drivers crates (which can't be built for the host).

#[path = "../../ota/src/pure.rs"]
pub mod pure;

// DEV02 — recovery-mode entry trigger logic. Same #[path] pattern.
#[path = "../../ota/src/recovery.rs"]
pub mod recovery;

#[cfg(test)]
mod tests {
    use super::pure::*;

    // ── Test fixtures ──────────────────────────────────────────────────

    /// Acceptance ceiling used by these tests.
    ///
    /// The real ceiling is `robot_os_ota::OTA_MAX_IMAGE_SIZE`, which comes
    /// from Kconfig (`OTA_MAX_IMAGE_SIZE_MB`). It deliberately does NOT live
    /// in `pure`, because this crate `#[path]`-includes `pure.rs` directly and
    /// that file must stay dependency-free — so `ota_validate_header` takes
    /// the ceiling as a parameter. These tests exercise the boundary logic
    /// with a fixed value of their own; they are testing the comparison, not
    /// the configured number.
    const TEST_MAX_IMAGE_SIZE: usize = 2 * 1024 * 1024;

    /// Build a header that should validate against the QEMU platform.
    fn good_header() -> OtaHeader {
        OtaHeader {
            header_version: OTA_HEADER_VERSION,
            image_size:     1024,
            image_crc32:    0xDEADBEEF,
            fw_version:     0x00_01_02_03,
            platform_id:    OTA_PLATFORM_QEMU,
            flags:          0,
        }
    }

    fn encode(h: &OtaHeader) -> [u8; OTA_HEADER_SIZE] {
        let mut buf = [0u8; OTA_HEADER_SIZE];
        ota_encode_header(h, &mut buf);
        buf
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.A — Header roundtrip + brain-side encode/kernel-side decode sync
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn header_roundtrip_preserves_all_fields() {
        let h = good_header();
        let buf = encode(&h);
        let decoded = ota_parse_header(&buf).expect("valid header must parse");
        assert_eq!(h, decoded);
    }

    #[test]
    fn header_decode_rejects_buffer_smaller_than_header_size() {
        let buf = [0u8; OTA_HEADER_SIZE - 1];
        assert!(ota_parse_header(&buf).is_none());
    }

    #[test]
    fn header_magic_is_rota_ascii() {
        assert_eq!(&OTA_MAGIC, b"ROTA");
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.B — CRC32 vectors (IEEE 802.3, polynomial 0xEDB88320)
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn crc32_known_vectors() {
        // Standard test vectors for CRC-32/ISO-HDLC (== IEEE 802.3).
        // Source: https://reveng.sourceforge.io/crc-catalogue/all.htm
        assert_eq!(crc32(b""),               0x0000_0000);
        assert_eq!(crc32(b"a"),              0xE8B7_BE43);
        assert_eq!(crc32(b"123456789"),      0xCBF4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"),
                   0x414F_A339);
    }

    #[test]
    fn crc32_streaming_matches_oneshot() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let oneshot = crc32(data);

        // Feed in 4 chunks
        let mut state = Crc32State::new();
        state.update(&data[0..10]);
        state.update(&data[10..20]);
        state.update(&data[20..30]);
        state.update(&data[30..]);
        assert_eq!(state.finalize(), oneshot);
    }

    #[test]
    fn crc32_state_empty_matches_oneshot_empty() {
        let state = Crc32State::new();
        assert_eq!(state.finalize(), crc32(b""));
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.C — Header magic / version invalid → parse returns None
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn header_with_wrong_magic_is_rejected() {
        let mut buf = encode(&good_header());
        buf[0] = b'X'; // corrupt magic
        assert!(ota_parse_header(&buf).is_none());
    }

    #[test]
    fn header_with_wrong_version_is_rejected() {
        let h = OtaHeader { header_version: 99, ..good_header() };
        let buf = encode(&h);
        // Note: encode writes 99 into the version field; parse will reject
        // because OTA_HEADER_VERSION is 1.
        assert!(ota_parse_header(&buf).is_none());
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.D — Platform ID mismatch is rejected by validate
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn validate_rejects_platform_mismatch() {
        let h = OtaHeader { platform_id: OTA_PLATFORM_VF2, ..good_header() };
        // Running on QEMU, image targets VF2 → reject.
        assert!(!ota_validate_header(&h, OTA_PLATFORM_QEMU, TEST_MAX_IMAGE_SIZE));
        // Running on VF2 with VF2 image → accept.
        assert!(ota_validate_header(&h, OTA_PLATFORM_VF2, TEST_MAX_IMAGE_SIZE));
    }

    #[test]
    fn validate_accepts_matching_platform() {
        let h = good_header();
        assert!(ota_validate_header(&h, OTA_PLATFORM_QEMU, TEST_MAX_IMAGE_SIZE));
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.E — Image size > the acceptance ceiling is rejected
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn validate_rejects_oversize_image() {
        let h = OtaHeader {
            image_size: (TEST_MAX_IMAGE_SIZE + 1) as u32,
            ..good_header()
        };
        assert!(!ota_validate_header(&h, OTA_PLATFORM_QEMU, TEST_MAX_IMAGE_SIZE));
    }

    #[test]
    fn validate_accepts_image_at_exact_max_size() {
        let h = OtaHeader {
            image_size: TEST_MAX_IMAGE_SIZE as u32,
            ..good_header()
        };
        assert!(ota_validate_header(&h, OTA_PLATFORM_QEMU, TEST_MAX_IMAGE_SIZE));
    }

    #[test]
    fn validate_rejects_zero_size_image() {
        let h = OtaHeader {
            image_size: 0,
            ..good_header()
        };
        assert!(!ota_validate_header(&h, OTA_PLATFORM_QEMU, TEST_MAX_IMAGE_SIZE));
    }

    #[test]
    fn validate_rejects_compressed_flag_until_supported() {
        let h = OtaHeader { flags: OTA_FLAG_COMPRESSED, ..good_header() };
        assert!(!ota_validate_header(&h, OTA_PLATFORM_QEMU, TEST_MAX_IMAGE_SIZE));
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.F — Boot-loop simulation: panic 4× → automatic rollback
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn boot_count_increments_on_each_boot() {
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: 0,
            ..BootMeta::default()
        };
        let outcome = ota_boot_validate_pure(&mut meta, 3);
        assert_eq!(outcome, BootValidateOutcome::Normal);
        assert_eq!(meta.boot_count, 1);
        assert_eq!(meta.active_slot, SLOT_B); // not yet rolled back
    }

    /// Regression: `boot_count` at `u32::MAX` must saturate, not overflow.
    ///
    /// `parse_u32_simple` saturates rather than rejecting, so
    /// `boot_count=4294967295` in BOOTMETA survives parsing intact and
    /// arrives here as a real value. The kernel builds with
    /// `overflow-checks = true` and `panic = "abort"`, so the old
    /// `meta.boot_count += 1` was a board reset on every boot — and
    /// `ota_boot_validate()` runs unconditionally at boot, making it an
    /// unrecoverable reset loop. BOOTMETA is writable over USB mass storage,
    /// so this is reachable, not theoretical.
    ///
    /// NOTE: this test would abort the test process rather than fail if the
    /// fix regressed, because the `+=` panics. `cargo test` reports that as a
    /// failed test binary, which is the signal we want either way.
    #[test]
    fn boot_count_at_u32_max_saturates_instead_of_overflowing() {
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: u32::MAX,
            ..BootMeta::default()
        };
        let outcome = ota_boot_validate_pure(&mut meta, 3);
        // u32::MAX > max_attempts, so the correct response is the rollback
        // branch: return to last_good and restart the count at this attempt.
        assert_eq!(outcome, BootValidateOutcome::RolledBack);
        assert_eq!(meta.active_slot, SLOT_A);
        assert_eq!(meta.boot_count, 1);
    }

    /// The saturation must hold for a value one below the ceiling too — that
    /// is the case where a plain `+= 1` still fits and the *next* one does
    /// not, i.e. the boot before the brick.
    #[test]
    fn boot_count_near_u32_max_rolls_back_without_wrapping() {
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: u32::MAX - 1,
            ..BootMeta::default()
        };
        let outcome = ota_boot_validate_pure(&mut meta, 3);
        assert_eq!(outcome, BootValidateOutcome::RolledBack);
        assert_eq!(meta.boot_count, 1);
    }

    /// A BOOTMETA carrying an out-of-range `boot_count` must round-trip
    /// through the parser as `u32::MAX` (saturated) rather than wrapping —
    /// this is the input half of the pair above, and the reason the value
    /// can reach `ota_boot_validate_pure` at all.
    #[test]
    fn parser_saturates_absurd_boot_count_rather_than_wrapping() {
        let text = b"active_slot=b\nboot_count=99999999999999999999\nlast_good=a\n";
        let meta = parse_boot_meta(text);
        assert_eq!(meta.boot_count, u32::MAX);
    }

    #[test]
    fn boot_count_at_max_does_not_yet_rollback() {
        // boot_count starts at max-1 (2), increments to max (3), still OK.
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: 2,
            ..BootMeta::default()
        };
        let outcome = ota_boot_validate_pure(&mut meta, 3);
        assert_eq!(outcome, BootValidateOutcome::Normal);
        assert_eq!(meta.boot_count, 3);
        assert_eq!(meta.active_slot, SLOT_B);
    }

    #[test]
    fn boot_count_exceeding_max_triggers_rollback() {
        // boot_count is already at max (3), this would be the 4th attempt.
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: 3,
            ..BootMeta::default()
        };
        let outcome = ota_boot_validate_pure(&mut meta, 3);
        assert_eq!(outcome, BootValidateOutcome::RolledBack);
        // Active slot must have been swapped to last_good
        assert_eq!(meta.active_slot, SLOT_A);
        // Boot count resets to 1 (counts THIS boot attempt)
        assert_eq!(meta.boot_count, 1);
    }

    #[test]
    fn rollback_when_already_on_last_good_is_idempotent() {
        // Already on last_good but somehow boot_count is high — the
        // FSM still resets count and "rolls back" to itself.
        let mut meta = BootMeta {
            active_slot: SLOT_A, last_good: SLOT_A, boot_count: 5,
            ..BootMeta::default()
        };
        let outcome = ota_boot_validate_pure(&mut meta, 3);
        assert_eq!(outcome, BootValidateOutcome::RolledBack);
        assert_eq!(meta.active_slot, SLOT_A);
        assert_eq!(meta.boot_count, 1);
    }

    /// Full "boot-loop" simulation: 4 consecutive failed boots into slot B.
    #[test]
    fn boot_loop_simulation_4_panics_rolls_back() {
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: 0,
            ..BootMeta::default()
        };
        // Boot 1, 2, 3 — all Normal.
        for expected_count in 1u32..=3 {
            let o = ota_boot_validate_pure(&mut meta, 3);
            assert_eq!(o, BootValidateOutcome::Normal);
            assert_eq!(meta.boot_count, expected_count);
            assert_eq!(meta.active_slot, SLOT_B);
        }
        // Boot 4 — boot_count was 3, increments to 4, which exceeds max=3.
        let o = ota_boot_validate_pure(&mut meta, 3);
        assert_eq!(o, BootValidateOutcome::RolledBack);
        assert_eq!(meta.active_slot, SLOT_A);
        assert_eq!(meta.boot_count, 1);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.G — mark_boot_good resets boot_count and updates last_good
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn mark_boot_good_resets_count_to_zero() {
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: 2,
            ..BootMeta::default()
        };
        ota_mark_boot_good_pure(&mut meta);
        assert_eq!(meta.boot_count, 0);
        // last_good now tracks the (successful) active_slot
        assert_eq!(meta.last_good, SLOT_B);
        // active_slot itself unchanged
        assert_eq!(meta.active_slot, SLOT_B);
    }

    #[test]
    fn mark_boot_good_then_validate_starts_fresh_at_one() {
        let mut meta = BootMeta {
            active_slot: SLOT_B, last_good: SLOT_A, boot_count: 2,
            ..BootMeta::default()
        };
        ota_mark_boot_good_pure(&mut meta);
        let outcome = ota_boot_validate_pure(&mut meta, 3);
        assert_eq!(outcome, BootValidateOutcome::Normal);
        assert_eq!(meta.boot_count, 1);
        assert_eq!(meta.last_good, SLOT_B);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.H — BOOTMETA serialize/parse roundtrip
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn bootmeta_serialize_parse_roundtrip() {
        let original = BootMeta {
            active_slot:  SLOT_B,
            boot_count:   42,
            last_good:    SLOT_A,
            fw_version_a: 0x0100_0001,
            fw_version_b: 0x0100_0002,
            fw_version_r: 0x0100_0000,
            image_size_a: 524288,
            image_size_b: 524300,
            image_size_r: 524288,
            image_crc_a:  0xCAFE_BABE,
            image_crc_b:  0xDEAD_F00D,
            image_crc_r:  0x1234_5678,
            min_fw_version: 0x0100_0001,
        };

        let mut buf = [0u8; 512];
        let n = serialize_boot_meta(&original, &mut buf);
        assert!(n > 0 && n <= buf.len());

        let parsed = parse_boot_meta(&buf[..n]);
        assert_eq!(original, parsed);
    }

    #[test]
    fn bootmeta_parse_handles_unknown_keys_and_comments() {
        let text = b"# this is a comment\n\
                     active_slot=b\n\
                     unknown_key=hello\n\
                     boot_count=5\n\
                     last_good=a\n\
                     fw_version_a=1\n\
                     fw_version_b=2\n\
                     image_size_a=100\n\
                     image_size_b=200\n\
                     image_crc_a=300\n\
                     image_crc_b=400\n";
        let meta = parse_boot_meta(text);
        assert_eq!(meta.active_slot,  SLOT_B);
        assert_eq!(meta.boot_count,   5);
        assert_eq!(meta.last_good,    SLOT_A);
        assert_eq!(meta.fw_version_a, 1);
        assert_eq!(meta.fw_version_b, 2);
        assert_eq!(meta.image_size_a, 100);
        assert_eq!(meta.image_size_b, 200);
        assert_eq!(meta.image_crc_a,  300);
        assert_eq!(meta.image_crc_b,  400);
        // OT04 — this text predates the `_r` fields entirely (as every
        // BOOTMETA on disk today does). Absent keys must read back as 0,
        // not panic or pick up garbage.
        assert_eq!(meta.fw_version_r, 0);
        assert_eq!(meta.image_size_r, 0);
        assert_eq!(meta.image_crc_r,  0);
    }

    #[test]
    fn bootmeta_parse_empty_returns_default() {
        let meta = parse_boot_meta(b"");
        assert_eq!(meta, BootMeta::default());
    }

    #[test]
    fn bootmeta_parse_garbage_does_not_panic() {
        let garbage = b"\x00\xFF\xAB\x12==\nlolwut\n";
        let _ = parse_boot_meta(garbage); // must not panic
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT01.I — Slot inversion logic
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn inactive_slot_is_b_when_active_is_a() {
        assert_eq!(ota_inactive_slot_pure(SLOT_A), SLOT_B);
    }

    #[test]
    fn inactive_slot_is_a_when_active_is_b() {
        assert_eq!(ota_inactive_slot_pure(SLOT_B), SLOT_A);
    }

    #[test]
    fn slot_helpers_on_bootmeta_pick_correct_field() {
        let meta = BootMeta {
            fw_version_a: 1, fw_version_b: 2, fw_version_r: 3,
            image_size_a: 10, image_size_b: 20, image_size_r: 30,
            image_crc_a: 100, image_crc_b: 200, image_crc_r: 300,
            ..BootMeta::default()
        };
        assert_eq!(meta.slot_version(SLOT_A), 1);
        assert_eq!(meta.slot_version(SLOT_B), 2);
        assert_eq!(meta.slot_version(SLOT_R), 3);
        assert_eq!(meta.slot_size(SLOT_A),    10);
        assert_eq!(meta.slot_size(SLOT_B),    20);
        assert_eq!(meta.slot_size(SLOT_R),    30);
        assert_eq!(meta.slot_crc(SLOT_A),     100);
        assert_eq!(meta.slot_crc(SLOT_B),     200);
        assert_eq!(meta.slot_crc(SLOT_R),     300);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT02.B — BOOTMETA record with seq + CRC (dual-file power-loss safety)
    // ──────────────────────────────────────────────────────────────────────

    fn record(seq: u32, active: u8, last_good: u8, count: u32) -> BootMetaRecord {
        BootMetaRecord {
            meta: BootMeta {
                active_slot: active,
                last_good,
                boot_count: count,
                ..BootMeta::default()
            },
            seq,
        }
    }

    /// Serialize into a stack-allocated buffer, returning `(buf, len)`.
    fn serialize(rec: &BootMetaRecord) -> ([u8; 512], usize) {
        let mut buf = [0u8; 512];
        let n = serialize_boot_meta_record(rec, &mut buf);
        (buf, n)
    }

    #[test]
    fn record_roundtrip_preserves_meta_and_seq() {
        let rec = BootMetaRecord {
            meta: BootMeta {
                active_slot: SLOT_B,
                boot_count: 5,
                last_good: SLOT_A,
                fw_version_a: 1, fw_version_b: 2, fw_version_r: 3,
                image_size_a: 100, image_size_b: 200, image_size_r: 300,
                image_crc_a: 0xCAFE_BABE,
                image_crc_b: 0xDEAD_F00D,
                image_crc_r: 0xABCD_1234,
                min_fw_version: 1,
            },
            seq: 42,
        };

        let (buf, n) = serialize(&rec);
        let bytes = &buf[..n];
        let parsed = parse_boot_meta_record(bytes).expect("valid record must parse");
        assert_eq!(parsed, rec);
    }

    #[test]
    fn record_with_corrupted_payload_is_rejected() {
        let rec = record(7, SLOT_A, SLOT_A, 0);
        let (mut buf, n) = serialize(&rec);
        // Flip a byte inside the body — CRC must no longer match.
        buf[10] ^= 0x01;
        assert!(parse_boot_meta_record(&buf[..n]).is_none());
    }

    #[test]
    fn record_with_wrong_crc_line_is_rejected() {
        let rec = record(7, SLOT_A, SLOT_A, 0);
        let (buf, n) = serialize(&rec);
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        // Build the corrupted version into a fresh stack buffer.
        let mut bad = [0u8; 1024];
        let mut len = 0usize;
        for &b in text.as_bytes() {
            bad[len] = b;
            len += 1;
        }
        // Find the "crc=0x" prefix and rewrite the 8 hex digits to FFs.
        let needle = b"crc=0x";
        let mut i = 0;
        while i + needle.len() <= len && &bad[i..i + needle.len()] != needle {
            i += 1;
        }
        if i + needle.len() <= len {
            for j in 0..8 {
                bad[i + needle.len() + j] = b'F';
            }
        }
        assert!(parse_boot_meta_record(&bad[..len]).is_none());
    }

    #[test]
    fn record_missing_crc_line_is_rejected() {
        // Plain serialize_boot_meta output (no `crc=` line) must NOT parse
        // as a record.
        let m = BootMeta { active_slot: SLOT_B, ..BootMeta::default() };
        let mut buf = [0u8; 512];
        let n = serialize_boot_meta(&m, &mut buf);
        assert!(parse_boot_meta_record(&buf[..n]).is_none());
    }

    #[test]
    fn record_truncated_is_rejected() {
        let rec = record(3, SLOT_A, SLOT_A, 0);
        let (buf, n) = serialize(&rec);
        let truncated_len = n / 2;
        assert!(parse_boot_meta_record(&buf[..truncated_len]).is_none());
    }

    #[test]
    fn record_seq_zero_default_is_valid() {
        let rec = BootMetaRecord::default();
        assert_eq!(rec.seq, 0);
        assert_eq!(rec.meta, BootMeta::default());
        let (buf, n) = serialize(&rec);
        let parsed = parse_boot_meta_record(&buf[..n]).unwrap();
        assert_eq!(parsed, rec);
    }

    #[test]
    fn record_high_seq_roundtrips() {
        let rec = record(u32::MAX, SLOT_B, SLOT_B, 1);
        let (buf, n) = serialize(&rec);
        let parsed = parse_boot_meta_record(&buf[..n]).unwrap();
        assert_eq!(parsed.seq, u32::MAX);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT02.B — record-picker (read side)
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn picker_returns_higher_seq_when_both_valid() {
        let r1 = record(10, SLOT_A, SLOT_A, 0);
        let r2 = record(11, SLOT_B, SLOT_A, 0);
        let picked = ota_pick_boot_meta_record(Some(r1), Some(r2)).unwrap();
        assert_eq!(picked.seq, 11);
        assert_eq!(picked.meta.active_slot, SLOT_B);
    }

    #[test]
    fn picker_returns_a_on_seq_tie() {
        let r1 = record(5, SLOT_A, SLOT_A, 0);
        let r2 = record(5, SLOT_B, SLOT_A, 0);
        let picked = ota_pick_boot_meta_record(Some(r1), Some(r2)).unwrap();
        assert_eq!(picked.meta.active_slot, SLOT_A);
    }

    #[test]
    fn picker_returns_only_valid_when_other_is_corrupt() {
        let r = record(7, SLOT_B, SLOT_A, 0);
        assert_eq!(ota_pick_boot_meta_record(Some(r), None).unwrap().seq, 7);
        assert_eq!(ota_pick_boot_meta_record(None, Some(r)).unwrap().seq, 7);
    }

    #[test]
    fn picker_returns_none_when_both_invalid() {
        assert!(ota_pick_boot_meta_record(None, None).is_none());
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT02.B — write-slot picker (write side: target the older/invalid file)
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn write_slot_targets_a_when_a_missing() {
        let r = record(5, SLOT_B, SLOT_A, 0);
        assert_eq!(ota_pick_meta_write_slot(None, Some(r)), SLOT_A);
    }

    #[test]
    fn write_slot_targets_b_when_b_missing() {
        let r = record(5, SLOT_A, SLOT_A, 0);
        assert_eq!(ota_pick_meta_write_slot(Some(r), None), SLOT_B);
    }

    #[test]
    fn write_slot_targets_a_when_both_empty() {
        // First-ever write goes to A.
        assert_eq!(ota_pick_meta_write_slot(None, None), SLOT_A);
    }

    #[test]
    fn write_slot_targets_lower_seq_when_both_valid() {
        let r_a = record(11, SLOT_A, SLOT_A, 0);
        let r_b = record(10, SLOT_B, SLOT_A, 0);
        // B has lower seq → write should overwrite B.
        assert_eq!(ota_pick_meta_write_slot(Some(r_a), Some(r_b)), SLOT_B);
    }

    #[test]
    fn write_slot_targets_a_on_tie() {
        let r_a = record(5, SLOT_A, SLOT_A, 0);
        let r_b = record(5, SLOT_B, SLOT_A, 0);
        // On tie, the deterministic choice is A (it'll just bump seq+1
        // and B becomes the older one next round).
        assert_eq!(ota_pick_meta_write_slot(Some(r_a), Some(r_b)), SLOT_A);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT02.B — sequence helpers
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn next_seq_starts_at_one_when_no_record() {
        assert_eq!(ota_next_seq(None), 1);
    }

    #[test]
    fn next_seq_increments_existing() {
        let r = record(42, SLOT_A, SLOT_A, 0);
        assert_eq!(ota_next_seq(Some(r)), 43);
    }

    #[test]
    fn next_seq_saturates_at_u32_max() {
        let r = record(u32::MAX, SLOT_A, SLOT_A, 0);
        assert_eq!(ota_next_seq(Some(r)), u32::MAX);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT02.B — power-loss simulation: torn write produces invalid CRC
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn power_loss_during_write_simulated_as_truncation() {
        // Simulate writing a record but losing power at every byte boundary.
        // For each truncation length where the CRC value is not yet fully
        // on disk, the partial buffer MUST NOT parse as a valid record.
        // (This is the property OT02.B relies on.)
        //
        // Edge case: dropping only the very last byte (the trailing '\n'
        // after the crc value) is *not* a real torn write — the value was
        // fully written. The parser is allowed to accept that. We exclude
        // it from the loop.
        let rec = record(99, SLOT_B, SLOT_A, 1);
        let (buf, n) = serialize(&rec);
        let upper = n.saturating_sub(1);

        for truncated_len in 0..upper {
            let result = parse_boot_meta_record(&buf[..truncated_len]);
            assert!(
                result.is_none(),
                "truncated record at len={truncated_len} unexpectedly parsed; partial torn write must be detectable"
            );
        }
        // And the full buffer parses cleanly.
        assert_eq!(parse_boot_meta_record(&buf[..n]), Some(rec));
    }

    #[test]
    fn power_loss_keeps_other_file_valid() {
        // Scenario: file A has seq=10 (valid), file B is being written
        // with seq=11 when power is lost mid-write (B becomes corrupt).
        // Read picker must return A's record, not crash, not return B.
        let a = record(10, SLOT_A, SLOT_A, 0);
        let b_corrupt: Option<BootMetaRecord> = None; // CRC mismatch on read returns None

        let picked = ota_pick_boot_meta_record(Some(a), b_corrupt).unwrap();
        assert_eq!(picked.seq, 10);
        assert_eq!(picked.meta.active_slot, SLOT_A);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT03 — Anti-rollback floor (min_fw_version)
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn rollback_check_accepts_newer_version() {
        assert!(ota_check_rollback_pure(10, 5));
        assert!(ota_check_rollback_pure(0xFFFF_FFFF, 0));
    }

    #[test]
    fn rollback_check_accepts_equal_version() {
        // Same version is allowed — recovery / re-install is a valid op.
        assert!(ota_check_rollback_pure(7, 7));
    }

    #[test]
    fn rollback_check_rejects_older_version() {
        assert!(!ota_check_rollback_pure(4, 5));
        assert!(!ota_check_rollback_pure(0, 1));
    }

    #[test]
    fn mark_boot_good_advances_min_fw_version() {
        let mut meta = BootMeta {
            active_slot: SLOT_B,
            fw_version_a: 1,
            fw_version_b: 7,
            min_fw_version: 1,
            ..BootMeta::default()
        };
        ota_mark_boot_good_pure(&mut meta);
        assert_eq!(meta.min_fw_version, 7);
    }

    #[test]
    fn mark_boot_good_does_not_lower_floor() {
        // active slot has version older than the floor (shouldn't happen
        // in practice, but the floor must remain monotonic).
        let mut meta = BootMeta {
            active_slot: SLOT_A,
            fw_version_a: 3,
            min_fw_version: 9,
            ..BootMeta::default()
        };
        ota_mark_boot_good_pure(&mut meta);
        assert_eq!(meta.min_fw_version, 9);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT04 — Recovery slot SLOT_R = 2 (read-only, never written by OTA)
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn slot_r_constant_is_distinct_from_a_and_b() {
        assert_ne!(SLOT_R, SLOT_A);
        assert_ne!(SLOT_R, SLOT_B);
        assert_eq!(SLOT_R, 2);
    }

    #[test]
    fn ota_inactive_slot_never_returns_r_when_active_is_a_or_b() {
        // OTA writes always target the inactive A/B slot — never R.
        assert_eq!(ota_inactive_slot_pure(SLOT_A), SLOT_B);
        assert_eq!(ota_inactive_slot_pure(SLOT_B), SLOT_A);
    }

    #[test]
    fn ota_inactive_slot_when_unexpectedly_r_falls_back_to_a_or_b() {
        // Defensive: SLOT_R should never be the active slot, but if BOOTMETA
        // is somehow corrupted to claim it is, the inactive slot should be
        // a writable A/B slot (not R itself).
        let inactive = ota_inactive_slot_pure(SLOT_R);
        assert!(inactive == SLOT_A || inactive == SLOT_B);
    }

    // ──────────────────────────────────────────────────────────────────────
    // OT04.B — BOOTMETA can now represent `active_slot`/`last_good` = R
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn active_slot_r_serializes_as_lowercase_r() {
        let meta = BootMeta { active_slot: SLOT_R, ..BootMeta::default() };
        let mut buf = [0u8; 512];
        let n = serialize_boot_meta(&meta, &mut buf);
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("active_slot=r\n"),
            "expected 'active_slot=r' line, got: {text}");
    }

    #[test]
    fn last_good_r_serializes_as_lowercase_r() {
        let meta = BootMeta { last_good: SLOT_R, ..BootMeta::default() };
        let mut buf = [0u8; 512];
        let n = serialize_boot_meta(&meta, &mut buf);
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("last_good=r\n"),
            "expected 'last_good=r' line, got: {text}");
    }

    #[test]
    fn parse_accepts_lowercase_and_uppercase_r() {
        let lower = parse_boot_meta(b"active_slot=r\n");
        let upper = parse_boot_meta(b"active_slot=R\n");
        assert_eq!(lower.active_slot, SLOT_R);
        assert_eq!(upper.active_slot, SLOT_R);
    }

    #[test]
    fn parse_accepts_r_for_last_good_too() {
        let meta = parse_boot_meta(b"last_good=r\n");
        assert_eq!(meta.last_good, SLOT_R);
    }

    #[test]
    fn bootmeta_r_serialize_parse_roundtrip() {
        // Full roundtrip with active_slot=R AND populated `_r` fields —
        // the state a hypothetical future factory-flashing tool would
        // produce.
        let original = BootMeta {
            active_slot: SLOT_R,
            last_good:   SLOT_R,
            boot_count:  0,
            fw_version_r: 0x0200_0000,
            image_size_r: 1_048_576,
            image_crc_r:  0x0BAD_F00D,
            ..BootMeta::default()
        };
        let mut buf = [0u8; 512];
        let n = serialize_boot_meta(&original, &mut buf);
        let parsed = parse_boot_meta(&buf[..n]);
        assert_eq!(original, parsed);
        assert_eq!(parsed.slot_version(SLOT_R), 0x0200_0000);
        assert_eq!(parsed.slot_size(SLOT_R),    1_048_576);
        assert_eq!(parsed.slot_crc(SLOT_R),     0x0BAD_F00D);
    }

    #[test]
    fn old_bootmeta_text_without_r_fields_parses_as_zero_and_stays_unselectable() {
        // OT04 backward compatibility: a BOOTMETA written by pre-OT04 code
        // (or any BOOTMETA where nothing has ever populated R, which is
        // every BOOTMETA on disk today) has no `_r` keys at all. The
        // parser must not choke on their absence, and the resulting
        // `image_size_r == 0` is exactly the state that keeps
        // `ota_verify_slot(SLOT_R)` (guarded by `expected_size == 0` in
        // `crates/ota/src/lib.rs`) from ever treating R as verified.
        let legacy_text = b"active_slot=b\n\
                             boot_count=1\n\
                             last_good=a\n\
                             fw_version_a=1\n\
                             fw_version_b=2\n\
                             image_size_a=100\n\
                             image_size_b=200\n\
                             image_crc_a=10\n\
                             image_crc_b=20\n";
        let meta = parse_boot_meta(legacy_text);
        assert_eq!(meta.fw_version_r, 0);
        assert_eq!(meta.image_size_r, 0);
        assert_eq!(meta.image_crc_r,  0);
        assert_eq!(meta.slot_size(SLOT_R), 0);
    }

    #[test]
    fn active_slot_r_read_by_pre_ot04_style_parser_would_be_a() {
        // Documents the forward-compat / downgrade risk (not something
        // this parser can fix): a NEW BOOTMETA with `active_slot=r`,
        // read by an OLD parser that only recognizes "b" (else assumes
        // "a"), would silently resolve to SLOT_A. We can't test the old
        // parser here (it no longer exists in this tree), but we can
        // pin the exact byte the new serializer emits, since that byte
        // is the input the old parser's `val == b"b"` check would see.
        let meta = BootMeta { active_slot: SLOT_R, ..BootMeta::default() };
        let mut buf = [0u8; 512];
        let n = serialize_boot_meta(&meta, &mut buf);
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        let line = text.lines().find(|l| l.starts_with("active_slot=")).unwrap();
        assert_eq!(line, "active_slot=r");
        // An old `if val == b"b" {SLOT_B} else {SLOT_A}` parser reading
        // "r" takes the `else` branch: SLOT_A. That's the downgrade risk.
    }

    #[test]
    fn min_fw_version_persists_through_serialize() {
        let original = BootMeta {
            min_fw_version: 0xCAFEBABE,
            ..BootMeta::default()
        };
        let mut buf = [0u8; 512];
        let n = serialize_boot_meta(&original, &mut buf);
        let parsed = parse_boot_meta(&buf[..n]);
        assert_eq!(parsed.min_fw_version, 0xCAFEBABE);
    }
}
