//! Host-side tests for `robot_os_config` (INI parser + runtime
//! atomic config).
//!
//! The config crate keeps its entries in a `static mut` table —
//! by design, because it's read by the kernel boot path before
//! the heap exists.  That means our tests share state, so each
//! one grabs `TEST_LOCK` and calls `cfg_load(b"")` first to start
//! from a clean table.

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use robot_os_config::{
        cfg_apply, cfg_count, cfg_get, cfg_get_i32, cfg_get_u32, cfg_load,
        cfg_serialize, cfg_set,
        unpack_ip, MAX_ENTRIES, MAX_KEY, MAX_VAL,
    };

    /// Tests touch a static-mut table inside `robot_os_config`, so
    /// they must run serialised.  Each test acquires this lock for
    /// its whole body.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the global lock and reset the config table.
    /// Returns the lock guard the test must hold for its lifetime.
    fn fresh<'a>() -> std::sync::MutexGuard<'a, ()> {
        // `lock()` returns Err only if a previous holder panicked
        // while holding the lock; we recover via `into_inner` so a
        // single failing test doesn't poison every later test.
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        cfg_load(b"");
        g
    }

    // ── Pure helpers (no shared state, no lock needed) ─────────

    #[test]
    fn unpack_ip_be_order() {
        // Packed (a<<24)|(b<<16)|(c<<8)|d → [a, b, c, d].
        assert_eq!(unpack_ip(0xC0_A8_01_FE), [0xC0, 0xA8, 0x01, 0xFE]);
        assert_eq!(unpack_ip(0), [0, 0, 0, 0]);
        assert_eq!(unpack_ip(0xFFFF_FFFF), [0xFF, 0xFF, 0xFF, 0xFF]);
    }

    // ── INI parser ─────────────────────────────────────────────

    #[test]
    fn loads_single_kv_pair() {
        let _g = fresh();
        cfg_load(b"name=value\n");
        assert_eq!(cfg_get(b"name"), Some(b"value".as_slice()));
        assert_eq!(cfg_count(), 1);
    }

    #[test]
    fn loads_multiple_kv_pairs() {
        let _g = fresh();
        cfg_load(b"a=1\nb=2\nc=3\n");
        assert_eq!(cfg_get(b"a"), Some(b"1".as_slice()));
        assert_eq!(cfg_get(b"b"), Some(b"2".as_slice()));
        assert_eq!(cfg_get(b"c"), Some(b"3".as_slice()));
        assert_eq!(cfg_count(), 3);
    }

    #[test]
    fn skips_blank_lines() {
        let _g = fresh();
        cfg_load(b"\n\nfoo=bar\n\n");
        assert_eq!(cfg_get(b"foo"), Some(b"bar".as_slice()));
        assert_eq!(cfg_count(), 1);
    }

    #[test]
    fn skips_comments() {
        let _g = fresh();
        cfg_load(b"# this is a comment\nkey=val\n# another\n");
        assert_eq!(cfg_get(b"key"), Some(b"val".as_slice()));
        assert_eq!(cfg_count(), 1);
    }

    #[test]
    fn handles_crlf_line_endings() {
        let _g = fresh();
        cfg_load(b"k1=v1\r\nk2=v2\r\n");
        assert_eq!(cfg_get(b"k1"), Some(b"v1".as_slice()));
        assert_eq!(cfg_get(b"k2"), Some(b"v2".as_slice()));
    }

    #[test]
    fn skips_lines_without_equals() {
        let _g = fresh();
        cfg_load(b"no_equals_here\nkey=val\n");
        assert_eq!(cfg_get(b"key"), Some(b"val".as_slice()));
        assert_eq!(cfg_count(), 1);
    }

    #[test]
    fn skips_lines_with_empty_key() {
        let _g = fresh();
        cfg_load(b"=lonely_value\nkey=val\n");
        assert_eq!(cfg_get(b"key"), Some(b"val".as_slice()));
        assert_eq!(cfg_count(), 1);
    }

    #[test]
    fn key_longer_than_max_is_truncated() {
        let _g = fresh();
        // 30-char key, MAX_KEY = 24 → truncated to 24.
        let mut line = Vec::new();
        for _ in 0..30 { line.push(b'x'); }
        line.push(b'=');
        line.extend_from_slice(b"v\n");
        cfg_load(&line);
        let key_24 = vec![b'x'; MAX_KEY];
        assert_eq!(cfg_get(&key_24), Some(b"v".as_slice()),
            "first MAX_KEY ({}) bytes of key must round-trip", MAX_KEY);
    }

    #[test]
    fn value_longer_than_max_is_truncated() {
        let _g = fresh();
        // Derive the over-long length from MAX_VAL instead of hardcoding it.
        // This test asserted truncation with a literal 30 bytes, which stopped
        // being "longer than max" the moment MAX_VAL went 16 -> 48 and the
        // test failed for the wrong reason. Anything expressed in terms of a
        // tunable constant has to be written in terms of it.
        let mut line = b"k=".to_vec();
        for _ in 0..(MAX_VAL + 10) { line.push(b'V'); }
        line.push(b'\n');
        cfg_load(&line);
        let want = vec![b'V'; MAX_VAL];
        assert_eq!(cfg_get(b"k"), Some(want.as_slice()),
            "a value longer than MAX_VAL ({}) must be cut to exactly MAX_VAL",
            MAX_VAL);
    }

    #[test]
    fn truncation_is_counted_not_silent() {
        let _g = fresh();
        // The cut itself is only half the contract. A silently truncated
        // value is a configuration nobody wrote, and the symptom shows up
        // somewhere unrelated -- a cut autorun path reads as "file not
        // found", a cut IP as an unreachable host. Boot warns off this
        // counter, so the counter has to actually move.
        let mut line = b"short=v\nlong=".to_vec();
        for _ in 0..(MAX_VAL + 1) { line.push(b'V'); }
        line.push(b'\n');
        cfg_load(&line);
        assert_eq!(robot_os_config::cfg_truncated_count(), 1,
            "exactly the one over-long value must be counted");
    }

    #[test]
    fn value_of_exactly_max_is_not_counted_as_truncated() {
        let _g = fresh();
        // Boundary that bit us for real: /fat/GPIODRV.ELF and
        // /fat/SYSTEST.ELF are exactly 16 bytes and fit under the old
        // MAX_VAL by a single byte, while /fat/BRAINCLI.ELF (17) did not and
        // was silently cut to /fat/BRAINCLI.EL. A value of exactly MAX_VAL
        // must round-trip whole and must NOT be reported as truncated.
        let mut line = b"k=".to_vec();
        for _ in 0..MAX_VAL { line.push(b'V'); }
        line.push(b'\n');
        cfg_load(&line);
        let want = vec![b'V'; MAX_VAL];
        assert_eq!(cfg_get(b"k"), Some(want.as_slice()));
        assert_eq!(robot_os_config::cfg_truncated_count(), 0,
            "a value of exactly MAX_VAL is not truncated");
    }

    #[test]
    fn entries_capped_at_max_entries() {
        let _g = fresh();
        let mut blob = Vec::new();
        for i in 0..(MAX_ENTRIES + 10) {
            // Keys k0, k1, ... k41.  All ≤ MAX_KEY.
            blob.extend_from_slice(format!("k{:02}=v\n", i).as_bytes());
        }
        cfg_load(&blob);
        assert_eq!(cfg_count(), MAX_ENTRIES,
            "must stop at MAX_ENTRIES (={}), got {}",
            MAX_ENTRIES, cfg_count());
        // First entry kept, post-cap entry dropped.
        assert_eq!(cfg_get(b"k00"), Some(b"v".as_slice()));
        let beyond_cap = format!("k{:02}", MAX_ENTRIES);
        assert_eq!(cfg_get(beyond_cap.as_bytes()), None);
    }

    #[test]
    fn rtrim_strips_trailing_whitespace_from_key_and_val() {
        let _g = fresh();
        cfg_load(b"key   =  val   \n");
        // Leading whitespace in value is NOT trimmed by this impl
        // (only trailing); pin behaviour so we notice if it changes.
        assert_eq!(cfg_get(b"key"), Some(b"  val".as_slice()));
    }

    // ── Typed getters ──────────────────────────────────────────

    #[test]
    fn cfg_get_u32_falls_back_to_default_when_missing() {
        let _g = fresh();
        assert_eq!(cfg_get_u32(b"absent", 42), 42);
    }

    #[test]
    fn cfg_get_u32_parses_decimal() {
        let _g = fresh();
        cfg_load(b"port=8080\nbig=4294967295\n");
        assert_eq!(cfg_get_u32(b"port", 0), 8080);
        assert_eq!(cfg_get_u32(b"big", 0), u32::MAX);
    }

    #[test]
    fn cfg_get_u32_uses_default_on_non_numeric() {
        let _g = fresh();
        cfg_load(b"not_a_number=hello\n");
        assert_eq!(cfg_get_u32(b"not_a_number", 7), 7);
    }

    #[test]
    fn cfg_get_i32_handles_negative() {
        let _g = fresh();
        cfg_load(b"offset=-123\npositive=456\n");
        assert_eq!(cfg_get_i32(b"offset", 0), -123);
        assert_eq!(cfg_get_i32(b"positive", 0), 456);
    }

    // ── cfg_set ─────────────────────────────────────────────────

    #[test]
    fn cfg_set_inserts_new_key() {
        let _g = fresh();
        assert!(cfg_set(b"hello", b"world"));
        assert_eq!(cfg_get(b"hello"), Some(b"world".as_slice()));
    }

    #[test]
    fn cfg_set_updates_existing_key() {
        let _g = fresh();
        cfg_load(b"x=old\n");
        assert!(cfg_set(b"x", b"new"));
        assert_eq!(cfg_get(b"x"), Some(b"new".as_slice()));
        assert_eq!(cfg_count(), 1, "update must not add a row");
    }

    #[test]
    fn cfg_set_rejects_oversized_key() {
        let _g = fresh();
        let huge = vec![b'k'; MAX_KEY + 1];
        assert!(!cfg_set(&huge, b"v"));
    }

    #[test]
    fn cfg_set_rejects_oversized_value() {
        let _g = fresh();
        let huge = vec![b'v'; MAX_VAL + 1];
        assert!(!cfg_set(b"k", &huge));
    }

    // ── Round-trip via cfg_serialize ───────────────────────────

    #[test]
    fn serialize_then_reload_round_trips() {
        let _g = fresh();
        cfg_load(b"a=1\nb=hello\nc=42\n");
        let mut buf = [0u8; 256];
        let n = cfg_serialize(&mut buf);
        assert!(n > 0);

        // Reload from the serialised form and check all three keys
        // survived.
        let serialised = &buf[..n];
        cfg_load(serialised);
        assert_eq!(cfg_get(b"a"), Some(b"1".as_slice()));
        assert_eq!(cfg_get(b"b"), Some(b"hello".as_slice()));
        assert_eq!(cfg_get(b"c"), Some(b"42".as_slice()));
    }

    // ── cfg_apply IP parsing ───────────────────────────────────

    #[test]
    fn cfg_apply_parses_dotted_ip_into_atomic() {
        use robot_os_config::BEHAVIOR_SERVER_IP;
        use std::sync::atomic::Ordering;
        let _g = fresh();
        cfg_load(b"behavior_server_ip=192.168.1.254\n");
        cfg_apply();
        let packed = BEHAVIOR_SERVER_IP.load(Ordering::Acquire);
        assert_eq!(unpack_ip(packed), [192, 168, 1, 254]);
    }

    #[test]
    fn cfg_apply_skips_malformed_ip() {
        use robot_os_config::BEHAVIOR_SERVER_IP;
        use std::sync::atomic::Ordering;
        let _g = fresh();
        // First load a known-good IP so we can see whether the
        // malformed one overwrites it.
        cfg_load(b"behavior_server_ip=10.0.0.1\n");
        cfg_apply();
        let baseline = BEHAVIOR_SERVER_IP.load(Ordering::Acquire);
        assert_eq!(unpack_ip(baseline), [10, 0, 0, 1]);

        // Now load garbage. Impl must silently keep the baseline.
        cfg_load(b"behavior_server_ip=not.an.ip.address\n");
        cfg_apply();
        let after = BEHAVIOR_SERVER_IP.load(Ordering::Acquire);
        assert_eq!(unpack_ip(after), [10, 0, 0, 1],
            "malformed IP must not clobber the previous value");
    }
}
