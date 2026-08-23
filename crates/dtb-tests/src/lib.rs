//! Host-side tests for `robot_os_dtb` — the Flattened Device
//! Tree (FDT) parser called from kernel boot.
//!
//! FDT is big-endian. The 40-byte header layout (Devicetree
//! Specification v0.4 §5.2):
//!
//! ```text
//!  +0  magic              u32  must be 0xd00dfeed
//!  +4  totalsize          u32  whole blob size
//!  +8  off_dt_struct      u32  offset to structure block
//! +12  off_dt_strings     u32
//! +16  off_mem_rsvmap     u32
//! +20  version            u32  must be >= 16
//! +24  last_comp_version  u32
//! +28  boot_cpuid_phys    u32
//! +32  size_dt_strings    u32
//! +36  size_dt_struct     u32
//! ```

#[cfg(test)]
mod tests {
    use robot_os_dtb::{dtb_compatible_str, dtb_parse, DtbInfo};

    const FDT_MAGIC: u32 = 0xd00d_feed;

    /// Build a minimal valid header (40 bytes) with the given
    /// version + totalsize. Struct + strings blocks are empty
    /// (offsets point past the header into a zeroed area).
    fn minimal_header(version: u32, totalsize: u32) -> [u8; 40] {
        let mut hdr = [0u8; 40];
        hdr[0..4].copy_from_slice(&FDT_MAGIC.to_be_bytes());
        hdr[4..8].copy_from_slice(&totalsize.to_be_bytes());
        // off_dt_struct = 40 (immediately after header).
        hdr[8..12].copy_from_slice(&40u32.to_be_bytes());
        // off_dt_strings = 40 (also empty).
        hdr[12..16].copy_from_slice(&40u32.to_be_bytes());
        // off_mem_rsvmap = 40.
        hdr[16..20].copy_from_slice(&40u32.to_be_bytes());
        // version
        hdr[20..24].copy_from_slice(&version.to_be_bytes());
        // last_comp_version
        hdr[24..28].copy_from_slice(&16u32.to_be_bytes());
        // boot_cpuid_phys
        hdr[28..32].copy_from_slice(&0u32.to_be_bytes());
        // size_dt_strings
        hdr[32..36].copy_from_slice(&0u32.to_be_bytes());
        // size_dt_struct
        hdr[36..40].copy_from_slice(&0u32.to_be_bytes());
        hdr
    }

    /// Build a buffer with the minimal header + an FDT_END token
    /// so the walker has something to terminate on without
    /// reading into uninitialised memory.
    fn minimal_blob(version: u32) -> Vec<u8> {
        const FDT_END: u32 = 9;
        let totalsize: u32 = 40 + 4; // header + one 4-byte END token
        let mut buf = minimal_header(version, totalsize).to_vec();
        buf.extend_from_slice(&FDT_END.to_be_bytes());
        buf
    }

    // ── Rejection cases (bounds & sanity) ──────────────────────

    #[test]
    fn rejects_null_pointer() {
        // SAFETY: passing a null pointer is the documented "this
        // is bogus" entry path; impl returns None.
        let result = unsafe { dtb_parse(core::ptr::null()) };
        assert!(result.is_none());
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut buf = minimal_blob(17);
        // Corrupt magic.
        buf[0] = 0xAA;
        let info = unsafe { dtb_parse(buf.as_ptr()) };
        assert!(info.is_none());
    }

    #[test]
    fn rejects_old_version() {
        // Version 15 is below the minimum (16).
        let buf = minimal_blob(15);
        let info = unsafe { dtb_parse(buf.as_ptr()) };
        assert!(info.is_none(),
            "version 15 must be rejected (impl requires >= 16)");
    }

    #[test]
    fn accepts_minimum_supported_version() {
        let buf = minimal_blob(16);
        let info = unsafe { dtb_parse(buf.as_ptr()) };
        assert!(info.is_some(),
            "version 16 is the documented minimum and must parse");
    }

    #[test]
    fn rejects_totalsize_below_header() {
        // totalsize < 40 is structurally impossible (header is
        // 40 bytes). Impl explicitly guards against this.
        let mut hdr = minimal_header(17, 30);
        // Patch totalsize back to a too-small value (minimal_header
        // already used 30, but make the rest of the field clear).
        hdr[4..8].copy_from_slice(&30u32.to_be_bytes());
        let info = unsafe { dtb_parse(hdr.as_ptr()) };
        assert!(info.is_none());
    }

    // ── Header offsets must lie inside the blob ────────────────
    //
    // The header fields are firmware-supplied u32 parsed from the raw `a1`
    // register at boot, before the trap handler is useful. `panic = "abort"`
    // makes any fault here a silent board reset, so a blob whose offsets
    // point outside itself must be rejected, not walked.

    #[test]
    fn rejects_strings_block_past_end_of_blob() {
        // The exact reported case: off_dt_strings = 0xFFFF_F000 with
        // size_dt_strings = 0x1000 makes strings_end wrap up to
        // 0x1_0000_0000. The walker's only strings guard was
        // "off < strings_end", which such a value passes trivially — so the
        // first FDT_PROP resolved its name ~4 GiB past the blob, outside
        // physical RAM on every target board.
        let mut buf = minimal_blob(17);
        buf[12..16].copy_from_slice(&0xFFFF_F000u32.to_be_bytes()); // off_dt_strings
        buf[32..36].copy_from_slice(&0x1000u32.to_be_bytes());      // size_dt_strings
        let info = unsafe { dtb_parse(buf.as_ptr()) };
        assert!(info.is_none(),
            "strings block outside totalsize must be rejected");
    }

    #[test]
    fn rejects_strings_block_overrunning_end_by_one() {
        // Boundary: a strings block ending exactly at totalsize is the
        // normal dtc layout and must be ACCEPTED (see the happy-path
        // tests); one byte more must not be.
        let mut buf = minimal_blob(17);
        // totalsize is 44, off_dt_strings is 40 → size 4 fits exactly,
        // size 5 does not.
        buf[32..36].copy_from_slice(&4u32.to_be_bytes());
        assert!(unsafe { dtb_parse(buf.as_ptr()) }.is_some(),
            "strings ending exactly at totalsize is a valid dtc layout");

        buf[32..36].copy_from_slice(&5u32.to_be_bytes());
        assert!(unsafe { dtb_parse(buf.as_ptr()) }.is_none(),
            "strings overrunning totalsize by one byte must be rejected");
    }

    #[test]
    fn rejects_struct_block_past_end_of_blob() {
        // walk() bounds its cursor relative to off_dt_struct; if that base
        // is itself outside the blob, every later bound is meaningless.
        let mut buf = minimal_blob(17);
        buf[8..12].copy_from_slice(&0xFFFF_F000u32.to_be_bytes());
        assert!(unsafe { dtb_parse(buf.as_ptr()) }.is_none());
    }

    #[test]
    fn rejects_absurd_totalsize() {
        // totalsize is the walker's only extent bound. Unclamped, a blob
        // claiming 4 GiB licenses a 4 GiB march past the end of RAM. Real
        // DTBs are tens of KiB; the parser caps at a few MiB.
        let mut buf = minimal_blob(17);
        buf[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        assert!(unsafe { dtb_parse(buf.as_ptr()) }.is_none(),
            "a totalsize far beyond any real DTB must be rejected");
    }

    // ── Unterminated strings must not scan out of the blob ─────

    #[test]
    fn unterminated_node_name_aborts_walk_instead_of_scanning_past_blob() {
        // A blob whose last token is FDT_BEGIN_NODE with a name that has no
        // NUL before the end of the blob. The old strlen() had no bound and
        // no totalsize, so it scanned RAM until it happened to hit a zero
        // byte.
        //
        // The buffer is sized so the blob ends exactly at the Vec's last
        // initialised byte: any read past `totalsize` is also past the
        // allocation, so Miri/ASAN would flag a regression here even though
        // a plain host run cannot observe the difference directly. What the
        // assertions pin is the contract — an unterminated name aborts the
        // walk and yields the zeroed defaults rather than guessing.
        const FDT_BEGIN_NODE: u32 = 1;
        let name = b"memory@"; // deliberately NOT NUL-terminated
        let totalsize = (40 + 4 + name.len()) as u32;

        let mut buf = minimal_header(17, totalsize).to_vec();
        // off_dt_strings = totalsize, size_dt_strings = 0 → empty strings
        // block sitting exactly at the end, which is legal.
        buf[12..16].copy_from_slice(&totalsize.to_be_bytes());
        buf[32..36].copy_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&FDT_BEGIN_NODE.to_be_bytes());
        buf.extend_from_slice(name);
        assert_eq!(buf.len(), totalsize as usize);

        let info = unsafe { dtb_parse(buf.as_ptr()) }
            .expect("header is well-formed; only the node name is truncated");
        assert_eq!(info.mem_base, 0);
        assert_eq!(info.mem_size, 0);
        assert_eq!(info.num_cpus, 0);
        assert_eq!(info.uart_base, 0);
    }

    // ── Happy-path minimal blob ────────────────────────────────

    #[test]
    fn minimal_valid_blob_parses_with_zeroed_fields() {
        let buf = minimal_blob(17);
        let info = unsafe { dtb_parse(buf.as_ptr()) }.unwrap();
        // No /memory or /cpu nodes → fields stay at their zero
        // defaults. This pins the contract that a structurally
        // valid but content-empty blob is NOT a parse error.
        assert_eq!(info.mem_base, 0);
        assert_eq!(info.mem_size, 0);
        assert_eq!(info.num_cpus, 0);
        assert_eq!(info.timer_freq, 0);
        assert_eq!(info.uart_base, 0);
        assert_eq!(info.plic_base, 0);
    }

    // ── dtb_compatible_str ─────────────────────────────────────

    #[test]
    fn compatible_str_empty_when_no_root_compatible() {
        let info = DtbInfo {
            mem_base: 0, mem_size: 0, timer_freq: 0,
            uart_base: 0, plic_base: 0, num_cpus: 0,
            compatible: [0u8; 64],
        };
        assert_eq!(dtb_compatible_str(&info), b"");
    }

    #[test]
    fn compatible_str_truncates_at_first_nul() {
        let mut info = DtbInfo {
            mem_base: 0, mem_size: 0, timer_freq: 0,
            uart_base: 0, plic_base: 0, num_cpus: 0,
            compatible: [0u8; 64],
        };
        let want = b"riscv-virtio";
        info.compatible[..want.len()].copy_from_slice(want);
        // Anything after the NUL must be hidden.
        info.compatible[want.len() + 5] = b'X';
        assert_eq!(dtb_compatible_str(&info), want);
    }

    #[test]
    fn compatible_str_caps_at_buffer_when_unterminated() {
        let info = DtbInfo {
            mem_base: 0, mem_size: 0, timer_freq: 0,
            uart_base: 0, plic_base: 0, num_cpus: 0,
            compatible: [b'A'; 64], // no NUL anywhere
        };
        let out = dtb_compatible_str(&info);
        assert_eq!(out.len(), 64);
        assert!(out.iter().all(|&b| b == b'A'));
    }
}
