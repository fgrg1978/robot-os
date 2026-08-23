//! Host-side tests for `robot_os_multi_stream` (RFC-0021).

#[cfg(test)]
mod tests {
    use robot_os_multi_stream::{
        wrap, unwrap, camera_stream_id, is_camera_stream,
        WrapError,
        HEADER_LEN, MAX_PAYLOAD_LEN,
        STREAM_CONTROL, STREAM_CAMERA_BASE, STREAM_CAMERA_LAST,
        STREAM_CAMERA_COUNT, STREAM_LIDAR, STREAM_AUDIO,
    };

    // ── Helper ────────────────────────────────────────────────────────────────

    /// Wrap then unwrap and check round-trip equality.
    fn roundtrip(stream_id: u8, payload: &[u8]) {
        let mut buf = vec![0u8; HEADER_LEN + payload.len()];
        let written = wrap(stream_id, payload, &mut buf).expect("wrap should succeed");
        assert_eq!(written, HEADER_LEN + payload.len());

        let (got_id, got_len, got_payload) = unwrap(&buf[..written]).expect("unwrap should succeed");
        assert_eq!(got_id, stream_id);
        assert_eq!(got_len, payload.len());
        assert_eq!(got_payload, payload);
    }

    // ── 1. Round-trip: STREAM_CONTROL ────────────────────────────────────────

    #[test]
    fn roundtrip_control_stream() {
        let payload = b"\x01\x00\x05\x00\xDE\xAD\xBE\xEF\x00";
        roundtrip(STREAM_CONTROL, payload);
    }

    // ── 2. Round-trip: first camera stream ───────────────────────────────────

    #[test]
    fn roundtrip_camera_stream_0() {
        let payload = vec![0xFFu8; 128];
        roundtrip(STREAM_CAMERA_BASE, &payload);
    }

    // ── 3. Round-trip: last camera stream ────────────────────────────────────

    #[test]
    fn roundtrip_camera_stream_last() {
        let payload = b"frame-data-last-cam";
        roundtrip(STREAM_CAMERA_LAST, payload);
    }

    // ── 4. Round-trip: LIDAR stream ──────────────────────────────────────────

    #[test]
    fn roundtrip_lidar_stream() {
        let payload = b"lidar-point-cloud";
        roundtrip(STREAM_LIDAR, payload);
    }

    // ── 5. Round-trip: AUDIO stream ──────────────────────────────────────────

    #[test]
    fn roundtrip_audio_stream() {
        let payload = b"pcm-samples";
        roundtrip(STREAM_AUDIO, payload);
    }

    // ── 6. Round-trip: zero-length payload ───────────────────────────────────

    #[test]
    fn roundtrip_zero_length_payload() {
        roundtrip(STREAM_CONTROL, b"");
    }

    // ── 7. Length-extension rejection ────────────────────────────────────────

    #[test]
    fn length_extension_rejected_by_unwrap() {
        // Build a frame that claims 10 bytes but only has 3 bytes of payload.
        let mut frame = vec![0u8; HEADER_LEN + 3];
        frame[0] = STREAM_CONTROL;
        // LEN field says 10.
        let len_le = 10u16.to_le_bytes();
        frame[1] = len_le[0];
        frame[2] = len_le[1];
        // Only 3 bytes of payload present — should be rejected.
        assert!(unwrap(&frame).is_none(), "must reject length-extension frame");
    }

    // ── 8. Too-short frame (no complete header) ───────────────────────────────

    #[test]
    fn malformed_frame_shorter_than_header() {
        assert!(unwrap(&[]).is_none(), "empty slice must be rejected");
        assert!(unwrap(&[STREAM_CONTROL]).is_none(), "1-byte slice must be rejected");
        assert!(unwrap(&[STREAM_CONTROL, 0]).is_none(), "2-byte slice must be rejected");
    }

    // ── 9. Header-only frame (LEN=0) ─────────────────────────────────────────

    #[test]
    fn header_only_frame_unwraps_to_empty_payload() {
        let frame = [STREAM_CAMERA_BASE, 0x00, 0x00]; // LEN = 0
        let (id, len, payload) = unwrap(&frame).expect("valid empty-payload frame");
        assert_eq!(id, STREAM_CAMERA_BASE);
        assert_eq!(len, 0);
        assert!(payload.is_empty());
    }

    // ── 10. PayloadTooLarge error from wrap ───────────────────────────────────

    #[test]
    fn wrap_rejects_payload_exceeding_max_len() {
        // MAX_PAYLOAD_LEN + 1 bytes.
        let oversized = vec![0u8; MAX_PAYLOAD_LEN + 1];
        let mut buf = vec![0u8; HEADER_LEN + MAX_PAYLOAD_LEN + 1];
        let err = wrap(STREAM_CONTROL, &oversized, &mut buf).unwrap_err();
        assert_eq!(err, WrapError::PayloadTooLarge);
    }

    // ── 11. OutputTooSmall error from wrap ────────────────────────────────────

    #[test]
    fn wrap_rejects_output_buffer_too_small() {
        let payload = b"hello";
        let mut buf = [0u8; HEADER_LEN]; // room for header only, not payload
        let err = wrap(STREAM_CONTROL, payload, &mut buf).unwrap_err();
        assert_eq!(err, WrapError::OutputTooSmall);
    }

    // ── 12. camera_stream_id() helper ─────────────────────────────────────────

    #[test]
    fn camera_stream_id_mapping() {
        assert_eq!(camera_stream_id(0), Some(STREAM_CAMERA_BASE));
        assert_eq!(camera_stream_id(STREAM_CAMERA_COUNT - 1), Some(STREAM_CAMERA_LAST));
        assert_eq!(camera_stream_id(STREAM_CAMERA_COUNT), None);
        assert_eq!(camera_stream_id(255), None);
    }

    // ── 13. is_camera_stream() helper ─────────────────────────────────────────

    #[test]
    fn is_camera_stream_boundaries() {
        assert!(!is_camera_stream(STREAM_CONTROL));
        assert!(is_camera_stream(STREAM_CAMERA_BASE));
        assert!(is_camera_stream(STREAM_CAMERA_LAST));
        assert!(!is_camera_stream(STREAM_LIDAR));
        assert!(!is_camera_stream(STREAM_AUDIO));
    }

    // ── 14. LEN field endianness ──────────────────────────────────────────────

    #[test]
    fn len_field_is_little_endian() {
        // A 256-byte payload → LEN = 0x0100 → little-endian bytes [0x00, 0x01].
        const PAYLOAD_LEN: usize = 256;
        let payload = vec![0xBBu8; PAYLOAD_LEN];
        let mut buf = vec![0u8; HEADER_LEN + PAYLOAD_LEN];
        wrap(STREAM_LIDAR, &payload, &mut buf).unwrap();
        assert_eq!(buf[1], 0x00, "len low byte"); // 256 & 0xFF = 0
        assert_eq!(buf[2], 0x01, "len high byte"); // 256 >> 8 = 1
    }

    // ── 15. stream_id passthrough for unknown IDs ─────────────────────────────

    #[test]
    fn unknown_stream_id_passthrough() {
        // The multiplexer does not validate stream_id values; unknown IDs
        // round-trip unchanged so future streams can be added without
        // updating the wrap/unwrap code.
        let future_id: u8 = 0xF0;
        let payload = b"future-stream";
        roundtrip(future_id, payload);
    }
}
