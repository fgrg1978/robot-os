//! Host-side tests for `robot_os_cam_ring` (S1/S6).

#[cfg(test)]
mod tests {
    use robot_os_cam_ring::FrameRing;

    // ── Constants ─────────────────────────────────────────────────────────────

    /// Small ring size used for most tests: 4 slots.
    const TEST_N: usize = 4;
    /// Slot byte capacity for most tests.
    const TEST_SZ: usize = 64;
    /// A smaller payload used as test frame content.
    const TEST_PAYLOAD_LEN: usize = 16;
    /// Test byte pattern for frame content.
    const TEST_BYTE_PATTERN: u8 = 0xA5;

    // ── 1. Claim/commit roundtrip ─────────────────────────────────────────────

    #[test]
    fn claim_commit_peek_release_roundtrip() {
        let ring: FrameRing<TEST_N, TEST_SZ> = FrameRing::new();

        // Ring starts empty.
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);

        // Producer claims a slot and writes a pattern.
        let slot = ring.claim_write().expect("ring should have free slot");
        for b in slot[..TEST_PAYLOAD_LEN].iter_mut() {
            *b = TEST_BYTE_PATTERN;
        }
        ring.commit_write(TEST_PAYLOAD_LEN);

        // Ring now has one frame.
        assert_eq!(ring.len(), 1);
        assert!(!ring.is_empty());

        // Consumer peeks and validates.
        let (len, data) = ring.peek_read().expect("ring should have one frame");
        assert_eq!(len, TEST_PAYLOAD_LEN);
        for b in &data[..len] {
            assert_eq!(*b, TEST_BYTE_PATTERN);
        }

        // Release and verify ring is empty again.
        ring.release_read();
        assert!(ring.is_empty());
    }

    // ── 2. Full-ring back-pressure ─────────────────────────────────────────────

    #[test]
    fn full_ring_claim_returns_none() {
        let ring: FrameRing<TEST_N, TEST_SZ> = FrameRing::new();

        // Fill all N slots.
        for i in 0..TEST_N {
            let slot = ring.claim_write()
                .unwrap_or_else(|| panic!("slot {} should be available", i));
            slot[0] = i as u8;
            ring.commit_write(1);
        }

        // Ring is now full.
        assert!(ring.is_full());
        assert_eq!(ring.len(), TEST_N);

        // Further claim must fail.
        assert!(ring.claim_write().is_none());

        // After releasing one slot the producer can write again.
        ring.release_read();
        assert!(!ring.is_full());
        assert!(ring.claim_write().is_some());
    }

    // ── 3. Multi-frame ordering ───────────────────────────────────────────────

    #[test]
    fn frames_are_delivered_in_fifo_order() {
        const FRAME_COUNT: usize = 4;
        const SLOT_SZ: usize = 8;
        let ring: FrameRing<FRAME_COUNT, SLOT_SZ> = FrameRing::new();

        // Produce FRAME_COUNT frames, each tagged with its sequence number.
        for seq in 0..FRAME_COUNT {
            let slot = ring.claim_write().expect("slot must be free");
            slot[0] = seq as u8;
            ring.commit_write(1);
        }

        // Consume and verify order.
        for expected_seq in 0..FRAME_COUNT {
            let (len, data) = ring.peek_read().expect("frame must be available");
            assert_eq!(len, 1);
            assert_eq!(data[0], expected_seq as u8,
                "expected frame {} got {}", expected_seq, data[0]);
            ring.release_read();
        }

        assert!(ring.is_empty());
    }

    // ── 4. Empty ring peek returns None ───────────────────────────────────────

    #[test]
    fn peek_on_empty_ring_returns_none() {
        let ring: FrameRing<TEST_N, TEST_SZ> = FrameRing::new();
        assert!(ring.peek_read().is_none());
    }

    // ── 5. commit_write clamps oversized len ─────────────────────────────────

    #[test]
    fn commit_clamps_len_to_slot_size() {
        const SMALL_SZ: usize = 8;
        let ring: FrameRing<2, SMALL_SZ> = FrameRing::new();

        let slot = ring.claim_write().expect("slot free");
        // Write the whole slot.
        slot.fill(0xFF);
        // Commit with a length larger than the slot — must be clamped.
        ring.commit_write(SMALL_SZ + 99);

        let (len, _data) = ring.peek_read().expect("frame must be readable");
        assert_eq!(len, SMALL_SZ, "len must be clamped to SZ");
        ring.release_read();
    }

    // ── 6. Producer/consumer after wrap-around ────────────────────────────────

    #[test]
    fn indices_wrap_around_correctly() {
        const RING_N: usize = 4;
        const RING_SZ: usize = 4;
        let ring: FrameRing<RING_N, RING_SZ> = FrameRing::new();

        // Cycle through the ring more than once to exercise index wrap-around.
        const CYCLES: usize = 3;
        for cycle in 0..CYCLES {
            for slot_no in 0..RING_N {
                // Produce.
                let slot = ring.claim_write()
                    .expect("slot must be free at start of round");
                slot[0] = (cycle * RING_N + slot_no) as u8;
                ring.commit_write(1);

                // Consume immediately so ring never exceeds depth 1.
                let (len, data) = ring.peek_read().expect("frame must appear immediately");
                assert_eq!(len, 1);
                assert_eq!(data[0], (cycle * RING_N + slot_no) as u8);
                ring.release_read();
            }
        }
        assert!(ring.is_empty());
    }

    // ── 7. Capacity and slot_size accessors ───────────────────────────────────

    #[test]
    fn capacity_and_slot_size_accessors() {
        type Ring = FrameRing<8, 256>;
        assert_eq!(Ring::capacity(), 8);
        assert_eq!(Ring::slot_size(), 256);
    }

    // ── 8. Multiple frames queued before any release ─────────────────────────

    #[test]
    fn batch_produce_then_batch_consume() {
        const BATCH: usize = 4;
        const SZ: usize = 16;
        let ring: FrameRing<BATCH, SZ> = FrameRing::new();

        // Produce all frames before consuming any.
        for i in 0u8..BATCH as u8 {
            let slot = ring.claim_write().expect("slot free");
            slot[0] = i;
            slot[1] = i.wrapping_mul(2);
            ring.commit_write(2);
        }
        assert_eq!(ring.len(), BATCH);

        // Now consume them all.
        for i in 0u8..BATCH as u8 {
            let (len, data) = ring.peek_read().expect("frame present");
            assert_eq!(len, 2);
            assert_eq!(data[0], i);
            assert_eq!(data[1], i.wrapping_mul(2));
            ring.release_read();
        }
        assert!(ring.is_empty());
    }

    // ── 9. Single-slot ring edge case ─────────────────────────────────────────

    #[test]
    fn single_slot_ring_alternates_correctly() {
        // N=1 is the minimal valid power-of-two size.
        let ring: FrameRing<1, 32> = FrameRing::new();

        for round in 0u8..4 {
            let slot = ring.claim_write().expect("slot free");
            slot[0] = round;
            ring.commit_write(1);

            assert!(ring.is_full());
            assert!(ring.claim_write().is_none(), "ring must be full");

            let (len, data) = ring.peek_read().expect("frame present");
            assert_eq!(len, 1);
            assert_eq!(data[0], round);
            ring.release_read();

            assert!(ring.is_empty());
        }
    }

    // ── 10. is_full reflects ring state correctly ─────────────────────────────

    #[test]
    fn is_full_transitions() {
        let ring: FrameRing<2, 8> = FrameRing::new();

        assert!(!ring.is_full());

        let slot = ring.claim_write().unwrap();
        slot[0] = 1;
        ring.commit_write(1);
        assert!(!ring.is_full());

        let slot = ring.claim_write().unwrap();
        slot[0] = 2;
        ring.commit_write(1);
        assert!(ring.is_full());

        ring.release_read();
        assert!(!ring.is_full());
        ring.release_read();
        assert!(ring.is_empty());
    }
}
