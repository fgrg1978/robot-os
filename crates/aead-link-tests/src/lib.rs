//! Cross-side AEAD wire-format pin tests (RFC-0019).
//!
//! This crate runs on the developer host (aarch64-apple-darwin or x86_64-linux).
//! It verifies that the byte sequence produced by `robot-brain/secure_channel.py`
//! is decryptable by the kernel-side `crates/crypto::secure_channel::SecureChannel`,
//! and that both sides derive byte-identical X25519 keys + KDF outputs.
//!
//! The vector below was computed by `robot-brain/secure_channel.py` using
//! deterministic private keys and a fixed `nonce_rand` injected via the
//! `_testing_*` test hooks.  Both implementations MUST produce these exact
//! bytes; if either drifts, the test fails and the cross-side break is
//! caught at CI time.
//!
//! Run:
//!   cd crates/aead-link-tests && cargo +stable test --release -- --nocapture
//!
//! # ⚠ TWO TESTS HERE ARE `#[ignore]`d — PINNED VECTORS ARE STALE
//!
//! Binding a direction label into the KDF changed the wire format:
//!
//! ```text
//!   before:  enc_key = SHA-256(shared || "ENC")[0..16]      (both ways)
//!   after:   enc_c2s = SHA-256(shared || "ENC" || "C2S")[0..16]
//!            mac_c2s = SHA-256(shared || "MAC" || "C2S")[0..16]
//!            enc_s2c = SHA-256(shared || "ENC" || "S2C")[0..16]
//!            mac_s2c = SHA-256(shared || "MAC" || "S2C")[0..16]
//!   initiator (brain)  sends with *_c2s, receives with *_s2c
//!   responder (kernel) sends with *_s2c, receives with *_c2s
//! ```
//!
//! `EXPECTED_ENC_KEY_HEX`, `EXPECTED_MAC_KEY_HEX` and `FRAME_HEX` below are
//! the OLD, pre-direction-binding values.  They cannot be recomputed from
//! this side: regenerating them is the *whole point* of a cross-side pin,
//! and deriving the expectation in Rust would turn the pin into a
//! tautology that passes no matter how far the two implementations drift.
//!
//! **To re-enable:** update `robot-brain/secure_channel.py::_derive_keys`
//! to the four-key form above, re-run `robot-brain/tests/test_aead_link.py`
//! with the same deterministic keys (`ALICE_PRIV = 0xAA*32`,
//! `BOB_PRIV = 0xBB*32`, `nonce_rand = 0x01..08`), paste the new hex here,
//! and drop the `#[ignore]`.  Until that happens these two tests assert
//! nothing — which is why the direction property itself is covered by the
//! self-contained tests at the bottom of this file instead.

#[cfg(test)]
mod tests {
    use robot_os_crypto::secure_channel::{Role, SecureChannel, PACKET_OVERHEAD};
    use robot_os_crypto::x25519;

    // ── Deterministic key material ────────────────────────────────────────
    //
    // Same constants the Python side uses in tests/test_aead_link.py.
    // Production code MUST NEVER use deterministic keys.

    const ALICE_PRIV: [u8; 32] = [0xAA; 32];
    const BOB_PRIV:   [u8; 32] = [0xBB; 32];

    // Public keys after X25519 clamping + base-point multiplication.
    // Produced by Python's `cryptography` lib X25519PrivateKey; the dalek
    // crate's `MontgomeryPoint::mul_base_clamped` must produce the same.
    const ALICE_PUB_HEX: &str =
        "14ca9e4d387bccf35746e0407daaacc6b28a4f8445ef5a5158894db983e24070";
    const BOB_PUB_HEX:   &str =
        "6b0b616d718e53691236d3be3ce6d44f9d28836426d81305d131f488206f8d2b";

    // STALE — pre-direction-binding. See the ⚠ note in the module header.
    // Derived shared keys (SHA-256(shared || "ENC")[0..16] and "MAC").
    const EXPECTED_ENC_KEY_HEX: &str = "f23883bd9a90eb144359442d0d50acac";
    const EXPECTED_MAC_KEY_HEX: &str = "31ede4093436ce648c47815a56c8a41b";

    // The deterministic encrypted frame: 12B nonce + 2B len(LE) + 21B ct + 32B mac
    // for plaintext "hello AEAD cross-side" with nonce_rand = 0x01..08.
    const PLAINTEXT: &[u8] = b"hello AEAD cross-side";
    const FRAME_HEX: &str = concat!(
        "0102030405060708",            // 8B nonce_rand
        "00000000",                    // 4B AES-CTR counter LE (starts at 0; the
                                       //   crate's ctr_encrypt bumps the in-block
                                       //   counter starting from 1, so nonce[8..12]
                                       //   here is the per-packet tx_counter)
        "1500",                        // payload length LE (= 21)
        "4450a8a6b8a69b90da9ceacda97c334e2025db9f50", // 21B ciphertext
        "07c4db407b0f10f86c9e12ed0ded88860df50215da860906fe7288f68fdec0df", // 32B mac
    );

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i+2], 16).unwrap())
            .collect()
    }

    fn hex_arr32(s: &str) -> [u8; 32] {
        let v = hex(s);
        assert_eq!(v.len(), 32);
        let mut a = [0u8; 32];
        a.copy_from_slice(&v);
        a
    }

    fn hex_arr16(s: &str) -> [u8; 16] {
        let v = hex(s);
        assert_eq!(v.len(), 16);
        let mut a = [0u8; 16];
        a.copy_from_slice(&v);
        a
    }

    // ── Tests ──────────────────────────────────────────────────────────────

    #[test]
    fn x25519_pubkey_matches_python_alice() {
        let pub_rust = x25519::x25519_pubkey(&ALICE_PRIV);
        let pub_py   = hex_arr32(ALICE_PUB_HEX);
        assert_eq!(pub_rust, pub_py,
            "Rust x25519_pubkey(0xAA*32) differs from Python's. \
             RFC 7748 clamping mismatch?");
    }

    #[test]
    fn x25519_pubkey_matches_python_bob() {
        let pub_rust = x25519::x25519_pubkey(&BOB_PRIV);
        let pub_py   = hex_arr32(BOB_PUB_HEX);
        assert_eq!(pub_rust, pub_py);
    }

    #[test]
    fn shared_secret_symmetric() {
        // Alice computes X25519(a_priv, b_pub) == Bob computes X25519(b_priv, a_pub).
        let a_pub = hex_arr32(ALICE_PUB_HEX);
        let b_pub = hex_arr32(BOB_PUB_HEX);
        let shared_a = x25519::x25519(&ALICE_PRIV, &b_pub);
        let shared_b = x25519::x25519(&BOB_PRIV,   &a_pub);
        assert_eq!(shared_a, shared_b, "ECDH symmetry broken");
    }

    #[test]
    #[ignore = "pinned vector is pre-direction-binding; regenerate from \
                robot-brain/secure_channel.py after its _derive_keys is \
                updated to the four-key form (see module header)"]
    fn handshake_derives_python_pinned_keys() {
        // Drive the kernel side's SecureChannel.handshake with Bob (responder)
        // role: priv = BOB_PRIV, peer = alice_pub.  After handshake, enc_key
        // and mac_key must match the Python-pinned hex constants.
        let mut bob = SecureChannel::new(BOB_PRIV);
        let a_pub = hex_arr32(ALICE_PUB_HEX);
        assert!(bob.handshake_directional(&a_pub, Role::Responder));

        // SecureChannel doesn't expose enc_key/mac_key, so we derive them
        // independently the same way and use the pinned vector.  This still
        // verifies that the *X25519* output is right, which is the only thing
        // that varies between implementations.
        let shared = x25519::x25519(&BOB_PRIV, &a_pub);
        let exp_enc = hex_arr16(EXPECTED_ENC_KEY_HEX);
        let exp_mac = hex_arr16(EXPECTED_MAC_KEY_HEX);

        // Re-derive enc_key = SHA-256(shared || "ENC")[0..16].
        use robot_os_crypto::sha256::Sha256;
        let mut h = Sha256::new();
        h.update(&shared);
        h.update(b"ENC");
        let d = h.finalize();
        let mut got_enc = [0u8; 16];
        got_enc.copy_from_slice(&d[..16]);

        let mut h = Sha256::new();
        h.update(&shared);
        h.update(b"MAC");
        let d = h.finalize();
        let mut got_mac = [0u8; 16];
        got_mac.copy_from_slice(&d[..16]);

        assert_eq!(got_enc, exp_enc, "ENC KDF drift");
        assert_eq!(got_mac, exp_mac, "MAC KDF drift");
    }

    #[test]
    #[ignore = "pinned frame is pre-direction-binding; regenerate from \
                robot-brain/tests/test_aead_link.py after the brain's KDF is \
                updated to the four-key form (see module header)"]
    fn decrypts_python_pinned_frame() {
        // Bob (responder) decrypts the frame Python's Alice (initiator)
        // produced.  This is the load-bearing cross-side compat check.
        let mut bob = SecureChannel::new(BOB_PRIV);
        let a_pub = hex_arr32(ALICE_PUB_HEX);
        assert!(bob.handshake_directional(&a_pub, Role::Responder));

        let frame = hex(FRAME_HEX);
        assert_eq!(frame.len(), 12 + 2 + PLAINTEXT.len() + 32);

        let mut out = vec![0u8; PLAINTEXT.len()];
        let n = bob.decrypt(&frame, &mut out);
        assert_eq!(n, PLAINTEXT.len(),
            "kernel SecureChannel rejected the Python-produced frame");
        assert_eq!(&out, PLAINTEXT,
            "kernel decrypted plaintext drift");
    }

    /// Build an established initiator/responder pair with the deterministic
    /// keys. Alice = initiator (= the Python brain's role), Bob = responder
    /// (= the kernel's role).
    fn established_pair() -> (SecureChannel, SecureChannel) {
        let a_pub = hex_arr32(ALICE_PUB_HEX);
        let b_pub = hex_arr32(BOB_PUB_HEX);
        let mut alice = SecureChannel::new(ALICE_PRIV);
        let mut bob = SecureChannel::new(BOB_PRIV);
        assert!(alice.handshake_directional(&b_pub, Role::Initiator));
        assert!(bob.handshake_directional(&a_pub, Role::Responder));
        (alice, bob)
    }

    #[test]
    fn roundtrip_through_kernel_with_opposite_roles() {
        // Kernel (responder) encrypts; brain (initiator) decrypts. This is
        // the direction the old shared-key design and the new
        // direction-bound design must BOTH get right — it is the check that
        // the S2C pair is actually mirrored on the two sides.
        let (alice, mut bob) = established_pair();

        let plain = b"kernel->brain reply";
        let nonce_rand = [0x09u8, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02];
        let mut out = vec![0u8; plain.len() + PACKET_OVERHEAD];
        let n = bob.encrypt(plain, &nonce_rand, &mut out);
        assert!(n > 0, "kernel encrypt failed");
        out.truncate(n);

        let mut plain_out = vec![0u8; plain.len()];
        let m = alice.decrypt(&out, &mut plain_out);
        assert_eq!(m, plain.len(), "initiator rejected responder's frame");
        assert_eq!(&plain_out, plain);
    }

    // ── Direction binding (finding 2) ──────────────────────────────────────

    #[test]
    fn peer_cannot_reflect_our_own_frame_back_at_us() {
        // THE point of direction binding. Before it, both peers used one
        // (enc, mac) pair for both directions, so a frame the responder
        // SENT verified as a frame the responder RECEIVED. An attacker who
        // can echo a TCP segment — no key needed — could replay the
        // kernel's own traffic into the kernel. It was only unexploitable
        // because kernel→brain and brain→kernel packet TYPE bytes happen to
        // be disjoint, i.e. a naming convention was holding a crypto flaw
        // shut. Now it fails at the MAC.
        let (_alice, mut bob) = established_pair();

        let plain = b"kernel->brain sensor frame";
        let nonce_rand = [0x11u8; 8];
        let mut frame = vec![0u8; plain.len() + PACKET_OVERHEAD];
        let n = bob.encrypt(plain, &nonce_rand, &mut frame);
        assert!(n > 0);
        frame.truncate(n);

        let mut out = vec![0u8; plain.len()];
        assert_eq!(
            bob.decrypt(&frame, &mut out), 0,
            "responder accepted a reflection of its OWN frame — direction \
             binding is not in effect"
        );
    }

    #[test]
    fn same_role_on_both_sides_fails_closed() {
        // The legacy one-argument `handshake()` hardcodes Role::Initiator.
        // If both peers end up there they claim the same direction, so
        // neither can read the other. Assert that this fails CLOSED (no
        // traffic) rather than open (silently reinstating the shared-key
        // behaviour) — that is the only reason keeping the legacy wrapper
        // for out-of-lane callers is acceptable.
        let a_pub = hex_arr32(ALICE_PUB_HEX);
        let b_pub = hex_arr32(BOB_PUB_HEX);
        let mut alice = SecureChannel::new(ALICE_PRIV);
        let mut bob = SecureChannel::new(BOB_PRIV);
        assert!(alice.handshake(&b_pub));
        assert!(bob.handshake(&a_pub));

        let plain = b"both claim initiator";
        let mut frame = vec![0u8; plain.len() + PACKET_OVERHEAD];
        let n = alice.encrypt(plain, &[0x22u8; 8], &mut frame);
        assert!(n > 0);
        frame.truncate(n);

        let mut out = vec![0u8; plain.len()];
        assert_eq!(bob.decrypt(&frame, &mut out), 0,
                   "two same-role peers interoperated — the legacy wrapper \
                    fails OPEN, which is the bug it was supposed to avoid");
    }

    // ── Coalesced frames (finding 1) ───────────────────────────────────────

    #[test]
    fn coalesced_frames_are_all_recovered_via_decrypt_consuming() {
        // TCP send boundaries are not recv boundaries: two frames the brain
        // wrote with two send() calls routinely arrive in one recv(). The
        // old decrypt() returned frame 1 and threw the rest away silently.
        // Because the brain sends PKT_ESTOP exactly once, with no ack and
        // no retransmit, that dropped an emergency stop coalesced behind
        // any other command.
        let (mut alice, bob) = established_pair();

        let msgs: [&[u8]; 3] = [b"CONFIG", b"ACTUATOR fwd 100", b"ESTOP"];
        let mut stream: Vec<u8> = Vec::new();
        for m in msgs.iter() {
            let mut f = vec![0u8; m.len() + PACKET_OVERHEAD];
            let n = alice.encrypt(m, &[0x33u8; 8], &mut f);
            assert!(n > 0);
            stream.extend_from_slice(&f[..n]);
        }

        // The single-value API still sees only the first frame — kept as a
        // regression pin so nobody "simplifies" the loop away later.
        let mut one = vec![0u8; 64];
        assert_eq!(bob.decrypt(&stream, &mut one), msgs[0].len());

        // The consuming API recovers all three, ESTOP included.
        let mut got: Vec<Vec<u8>> = Vec::new();
        let mut off = 0usize;
        while off < stream.len() {
            let mut out = vec![0u8; 64];
            let (n, used) = bob.decrypt_consuming(&stream[off..], &mut out);
            if used == 0 { break; }
            out.truncate(n);
            got.push(out);
            off += used;
        }
        assert_eq!(got.len(), 3, "coalesced frames lost");
        assert_eq!(off, stream.len(), "consumed length does not tile the stream");
        for (g, m) in got.iter().zip(msgs.iter()) {
            assert_eq!(g.as_slice(), *m);
        }
    }

    #[test]
    fn torn_trailing_frame_stops_the_loop_without_consuming() {
        // A frame split across two recv() calls must report (0, 0) so the
        // caller keeps the bytes and retries, rather than consuming a
        // partial frame or spinning.
        let (mut alice, bob) = established_pair();
        let msg: &[u8] = b"ACTUATOR";
        let mut f = vec![0u8; msg.len() + PACKET_OVERHEAD];
        let n = alice.encrypt(msg, &[0x44u8; 8], &mut f);
        f.truncate(n);

        let mut stream = f.clone();
        stream.extend_from_slice(&f[..f.len() - 4]); // second frame truncated

        let mut out = vec![0u8; 64];
        let (n1, used1) = bob.decrypt_consuming(&stream, &mut out);
        assert_eq!(n1, msg.len());
        assert_eq!(used1, f.len());
        let (n2, used2) = bob.decrypt_consuming(&stream[used1..], &mut out);
        assert_eq!((n2, used2), (0, 0), "torn frame was partially consumed");
    }

    // ── Small-order peer points (finding 5) ────────────────────────────────

    #[test]
    fn all_zero_peer_point_is_rejected() {
        // The all-zero u-coordinate is the canonical small-order point: the
        // product is the identity, so BOTH sides would derive keys from 32
        // zero bytes regardless of their private keys.
        let mut bob = SecureChannel::new(BOB_PRIV);
        assert!(!bob.handshake_directional(&[0u8; 32], Role::Responder),
                "all-zero peer point accepted");
        assert!(x25519::x25519_checked(&BOB_PRIV, &[0u8; 32]).is_none());
    }

    #[test]
    fn other_small_order_points_are_rejected() {
        // u = 1 and the two order-8 points from RFC 7748 §6.1 / the
        // curve25519 small-order list. All must produce an all-zero
        // product, which is why testing the OUTPUT beats blacklisting
        // the input encodings.
        let mut u1 = [0u8; 32];
        u1[0] = 1;
        let order8_a: [u8; 32] = [
            0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3,
            0xfa, 0xf1, 0x9f, 0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32,
            0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49, 0xb8, 0x00,
        ];
        let order8_b: [u8; 32] = [
            0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1,
            0x55, 0x9c, 0x83, 0xef, 0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c,
            0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f, 0x11, 0x57,
        ];
        for p in [u1, order8_a, order8_b].iter() {
            assert!(x25519::x25519_checked(&BOB_PRIV, p).is_none(),
                    "small-order point accepted: {:02x?}", &p[..4]);
            let mut ch = SecureChannel::new(BOB_PRIV);
            assert!(!ch.handshake_directional(p, Role::Responder));
        }
    }

    // ── K-C5: what the AEAD layer does and does NOT do about replay ───────
    //
    // These two tests are the evidence behind the K-C5 decision recorded in
    // `behavior/src/auth_envelope.rs`. Read them as a pair: they say the
    // encrypted mode closes the reboot replay window and *only* the reboot
    // replay window, which is exactly why the envelope's RAM watermark stays.

    /// **This is why requiring encrypted mode answers K-C5.**
    ///
    /// The envelope's `HIGHEST_RX_NONCE` high-water mark lives in RAM and
    /// resets to 0 on reboot, so in HMAC-only mode a frame captured before
    /// the reboot replays into the window before the first legitimate brain
    /// frame — the brain derives its nonces from `time_ns()`, so any recorded
    /// nonce beats a zeroed mark.
    ///
    /// A reboot means a new TCP connection, which means a new handshake with
    /// new ephemeral keys, which means new `enc`/`mac` session keys. The
    /// captured frame is now MAC'd under a key the new session does not hold.
    /// It is rejected before a single byte reaches the envelope layer — no
    /// persisted state, no flash wear, no rollback policy.
    ///
    /// The second session below reuses Bob's *long-term* private key on
    /// purpose: only the peer's ephemeral changes, which is the weakest form
    /// of the property and therefore the honest thing to assert.
    #[test]
    fn frame_from_a_previous_session_does_not_decrypt_in_a_new_one() {
        // Session 1 — "before the reboot". Alice (brain) sends a command.
        let (mut alice1, _bob1) = established_pair();
        let captured = {
            let plain = b"BR\x88 ESTOP";
            let mut out = vec![0u8; plain.len() + PACKET_OVERHEAD];
            let n = alice1.encrypt(plain, &[0x11u8; 8], &mut out);
            assert!(n > 0);
            out.truncate(n);
            out
        };

        // Session 2 — "after the reboot". Same PSK world, different
        // ephemeral on the initiator side, so different shared secret and a
        // different rx_mac_key at the kernel.
        let carol_priv = [0xCCu8; 32];
        let carol_pub = x25519::x25519_pubkey(&carol_priv);
        let b_pub = hex_arr32(BOB_PUB_HEX);
        let mut carol = SecureChannel::new(carol_priv);
        let mut bob2 = SecureChannel::new(BOB_PRIV);
        assert!(carol.handshake_directional(&b_pub, Role::Initiator));
        assert!(bob2.handshake_directional(&carol_pub, Role::Responder));

        let mut out = vec![0u8; captured.len()];
        let (len, eaten) = bob2.decrypt_consuming(&captured, &mut out);
        assert_eq!((len, eaten), (0, 0),
            "a frame recorded in a previous session decrypted in a new one — \
             the K-C5 argument for requiring encrypted mode is void");

        // And the live session still works, so the rejection above is the
        // key change and not a broken channel.
        let plain = b"BR\x83 fresh";
        let mut fresh = vec![0u8; plain.len() + PACKET_OVERHEAD];
        let n = carol.encrypt(plain, &[0x22u8; 8], &mut fresh);
        assert!(n > 0);
        let mut got = vec![0u8; plain.len()];
        assert_eq!(bob2.decrypt(&fresh[..n], &mut got), plain.len());
        assert_eq!(&got, plain);
    }

    /// **This is why the envelope's RAM watermark must NOT be deleted.**
    ///
    /// `SecureChannel::decrypt_consuming` keeps no receive counter and no
    /// nonce window: a frame replayed inside the *same* live session decrypts
    /// and verifies every single time. On this link that frame can be
    /// `PKT_ESTOP` or an actuator command.
    ///
    /// The only thing that rejects it is `auth_envelope`'s
    /// `HIGHEST_RX_NONCE` `fetch_max`, one layer in. If a future change adds
    /// counter tracking here this test starts failing — which is the signal
    /// to go re-read the K-C5 note in `auth_envelope.rs` before removing
    /// anything, and to check what the brain's rekey path
    /// (`_tx_counter` resets to 0) does to a strict-monotonic check.
    #[test]
    fn replayed_frame_within_one_session_still_decrypts() {
        let (mut alice, bob) = established_pair();
        let plain = b"BR\x88 ESTOP";
        let mut wire = vec![0u8; plain.len() + PACKET_OVERHEAD];
        let n = alice.encrypt(plain, &[0x33u8; 8], &mut wire);
        assert!(n > 0);
        wire.truncate(n);

        for attempt in 0..4 {
            let mut out = vec![0u8; plain.len()];
            let (len, _) = bob.decrypt_consuming(&wire, &mut out);
            assert_eq!(len, plain.len(),
                "attempt {attempt}: the AEAD layer grew replay rejection — \
                 re-read the K-C5 note in behavior/src/auth_envelope.rs");
            assert_eq!(&out[..len], plain);
        }
    }
}
