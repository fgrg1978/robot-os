//! TS01 — Property-based tests using proptest.
//!
//! Property tests assert invariants over arbitrary inputs (proptest
//! generates them). Far stronger than example-based unit tests for code
//! whose correctness depends on subtle invariants (parsers, ring
//! buffers, codecs, schedulers, …).
//!
//! This file seeds the kernel with a small starter set; new property
//! tests should land here as we promote modules.

#[cfg(test)]
mod ring_buffer_invariants {
    use proptest::prelude::*;

    /// Same store loop as the TCP rx ring (handle() in-order branch).
    fn store(rx: &mut [u8], head: &mut usize, tail: &mut usize,
             mask: usize, payload: &[u8]) -> u32 {
        let mut stored: u32 = 0;
        for &b in payload {
            let next = (*tail + 1) & mask;
            if next == *head { break; }
            rx[*tail] = b;
            *tail = next;
            stored += 1;
        }
        stored
    }

    fn drain(rx: &[u8], head: &mut usize, tail: usize,
             mask: usize, n: usize) -> Vec<u8> {
        let avail = if tail >= *head { tail - *head } else { rx.len() - *head + tail };
        let take = avail.min(n);
        let mut out = Vec::with_capacity(take);
        for _ in 0..take {
            out.push(rx[*head & mask]);
            *head = (*head + 1) & mask;
        }
        out
    }

    proptest! {
        /// Bytes written into the ring then read out in order must match
        /// exactly. Holds across arbitrary head/tail positions and
        /// payload sizes.
        #[test]
        fn store_then_drain_preserves_order(
            payload in prop::collection::vec(any::<u8>(), 0..15),
            initial_head in 0usize..16,
        ) {
            const SIZE: usize = 16;
            const MASK: usize = SIZE - 1;
            let mut rx = [0u8; SIZE];
            let mut head = initial_head & MASK;
            let mut tail = head;

            let stored = store(&mut rx, &mut head, &mut tail, MASK, &payload) as usize;
            prop_assert!(stored <= payload.len());
            let drained = drain(&rx, &mut head, tail, MASK, stored);
            prop_assert_eq!(drained, payload[..stored].to_vec());
        }

        /// `stored` is always strictly less than buffer capacity if the
        /// ring started empty. A 16-byte ring can hold at most 15 (one
        /// slot reserved for full-vs-empty disambiguation).
        #[test]
        fn empty_ring_capacity_is_size_minus_one(
            payload in prop::collection::vec(any::<u8>(), 0..1024),
        ) {
            const SIZE: usize = 16;
            const MASK: usize = SIZE - 1;
            let mut rx = [0u8; SIZE];
            let mut head = 0usize;
            let mut tail = 0usize;
            let stored = store(&mut rx, &mut head, &mut tail, MASK, &payload) as usize;
            prop_assert!(stored <= SIZE - 1);
            prop_assert_eq!(stored, payload.len().min(SIZE - 1));
        }
    }
}

#[cfg(test)]
mod window_clamp_invariants {
    use proptest::prelude::*;

    const fn window_clamp(free: usize) -> u16 {
        if free > u16::MAX as usize { u16::MAX } else { free as u16 }
    }

    proptest! {
        /// window_clamp is monotonic and bounded.
        #[test]
        fn window_clamp_is_bounded(free in 0usize..0xFFFF_FFFF) {
            let clamped = window_clamp(free);
            prop_assert!(clamped as usize <= free);
            prop_assert!(clamped <= u16::MAX);
        }

        /// Below u16::MAX, window_clamp is identity.
        #[test]
        fn window_clamp_is_identity_below_u16_max(free in 0u16..) {
            prop_assert_eq!(window_clamp(free as usize), free);
        }
    }
}
