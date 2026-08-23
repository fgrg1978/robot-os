//! Host-side unit tests for `arch-x86_64::acpi`.
//!
//! `parse_madt_bytes` is a pure byte-slice parser — no raw-pointer
//! dereferences, no x86_64 asm — so we exercise it on the host
//! with synthetic MADT blobs. Covers the bounds-checking branches
//! that are most likely to hide bugs (truncation in three places,
//! zero-length entries, capacity overflow) plus the three entry
//! types we actually decode.
//!
//! Pulls in `crates/arch-x86_64/src/acpi.rs` directly via `#[path]`
//! so we test the same source the kernel uses, without depending
//! on `robot_os_arch_api` (which is `no_std` + cfg-gated and won't
//! build cleanly on the host under the workspace build-std setup).
//! The acpi.rs file is `#![allow(dead_code)]` and only uses a
//! handful of items from `core` + `bitflags` (the latter via Cargo
//! dep), so direct inclusion works.

// acpi.rs is `#![allow(dead_code)]` and the host build of the file
// activates the `#[cfg(not(target_arch = "x86_64"))]` stub branch
// for `parse_madt` — exactly what we want.
#[path = "../../arch-x86_64/src/acpi.rs"]
pub mod acpi;

#[cfg(test)]
mod tests {
    use super::acpi::*;

    // ── Constants & helpers re-declared here so the tests don't
    //    reach into private items of `acpi`.  These mirror the
    //    private constants in the source file.

    const MADT_SIG: [u8; 4] = *b"APIC";
    const MADT_TYPE_LOCAL_APIC: u8 = 0;
    const MADT_TYPE_IO_APIC: u8 = 1;
    const MADT_TYPE_LAPIC_ADDR_OVR: u8 = 5;
    const LOCAL_APIC_ENABLED: u32 = 1 << 0;

    /// Hand-rolled MADT builder.  Lays down a 36-byte SDT header
    /// (signature + length placeholder + zeroed metadata), the
    /// 8-byte `{lapic_addr, flags}` fixed prefix, and any entries
    /// the test appended via `push_*`.  Fixes up `length` in the
    /// header on `finish`.
    struct MadtBuilder {
        buf: Vec<u8>,
    }

    impl MadtBuilder {
        fn new(lapic_addr: u32) -> Self {
            let mut buf = Vec::new();
            buf.extend_from_slice(&MADT_SIG);              //  0..4  sig
            buf.extend_from_slice(&[0u8; 4]);              //  4..8  length placeholder
            buf.extend_from_slice(&[0u8; 28]);             //  8..36 rest of SdtHeader
            buf.extend_from_slice(&lapic_addr.to_le_bytes()); // body: lapic_addr
            buf.extend_from_slice(&[0u8; 4]);              // body: flags
            Self { buf }
        }

        fn push_local_apic(&mut self, processor_id: u8, apic_id: u8, flags: u32) {
            self.buf.push(MADT_TYPE_LOCAL_APIC);
            self.buf.push(8);
            self.buf.push(processor_id);
            self.buf.push(apic_id);
            self.buf.extend_from_slice(&flags.to_le_bytes());
        }

        fn push_io_apic(&mut self, ioapic_id: u8, ioapic_addr: u32, gsi_base: u32) {
            self.buf.push(MADT_TYPE_IO_APIC);
            self.buf.push(12);
            self.buf.push(ioapic_id);
            self.buf.push(0);
            self.buf.extend_from_slice(&ioapic_addr.to_le_bytes());
            self.buf.extend_from_slice(&gsi_base.to_le_bytes());
        }

        fn push_lapic_addr_override(&mut self, addr: u64) {
            self.buf.push(MADT_TYPE_LAPIC_ADDR_OVR);
            self.buf.push(12);
            self.buf.extend_from_slice(&[0u8; 2]);
            self.buf.extend_from_slice(&addr.to_le_bytes());
        }

        fn push_raw(&mut self, entry_type: u8, forged_len: u8, payload: &[u8]) {
            self.buf.push(entry_type);
            self.buf.push(forged_len);
            self.buf.extend_from_slice(payload);
        }

        fn finish(mut self) -> Vec<u8> {
            let len = self.buf.len() as u32;
            self.buf[4..8].copy_from_slice(&len.to_le_bytes());
            self.buf
        }
    }

    #[test]
    fn happy_path_one_enabled_cpu() {
        let mut b = MadtBuilder::new(0xFEE0_0000);
        b.push_local_apic(0, 7, LOCAL_APIC_ENABLED);
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.lapic_pa, 0xFEE0_0000);
        assert_eq!(s.cpu_count, 1);
        assert_eq!(s.cpus[0], 7);
    }

    #[test]
    fn disabled_cpu_is_skipped() {
        let mut b = MadtBuilder::new(0xFEE0_0000);
        b.push_local_apic(0, 1, 0);
        b.push_local_apic(1, 2, LOCAL_APIC_ENABLED);
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.cpu_count, 1);
        assert_eq!(s.cpus[0], 2);
    }

    #[test]
    fn multiple_cpus_in_order() {
        let mut b = MadtBuilder::new(0);
        for id in 0u8..4 {
            b.push_local_apic(id, id * 10, LOCAL_APIC_ENABLED);
        }
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.cpu_count, 4);
        assert_eq!(&s.cpus[..4], &[0, 10, 20, 30]);
    }

    #[test]
    fn cpu_capacity_caps_at_max() {
        let mut b = MadtBuilder::new(0);
        for id in 0u8..(MAX_CPUS as u8 + 5) {
            b.push_local_apic(id, id, LOCAL_APIC_ENABLED);
        }
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.cpu_count, MAX_CPUS);
    }

    #[test]
    fn ioapic_entry_recorded() {
        let mut b = MadtBuilder::new(0);
        b.push_io_apic(0, 0xFEC0_0000, 0);
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.ioapic_pa, 0xFEC0_0000);
    }

    #[test]
    fn lapic_addr_override_takes_precedence() {
        let mut b = MadtBuilder::new(0xFEE0_0000);
        b.push_lapic_addr_override(0x0000_FEDC_FEE0_0000);
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.lapic_pa, 0x0000_FEDC_FEE0_0000);
    }

    #[test]
    fn mixed_entries_all_parsed() {
        let mut b = MadtBuilder::new(0xFEE0_0000);
        b.push_local_apic(0, 0, LOCAL_APIC_ENABLED);
        b.push_io_apic(0, 0xFEC0_0000, 0);
        b.push_local_apic(1, 1, LOCAL_APIC_ENABLED);
        b.push_lapic_addr_override(0xFFFF_FEE0_0000);
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.cpu_count, 2);
        assert_eq!(s.cpus[0], 0);
        assert_eq!(s.cpus[1], 1);
        assert_eq!(s.ioapic_pa, 0xFEC0_0000);
        assert_eq!(s.lapic_pa, 0xFFFF_FEE0_0000);
    }

    #[test]
    fn rejects_short_buffer() {
        let buf = [0u8; 10];
        assert_eq!(parse_madt_bytes(&buf), Err(AcpiError::Truncated));
    }

    #[test]
    fn rejects_bad_signature() {
        let mut b = MadtBuilder::new(0).finish();
        b[0] = b'X';
        assert_eq!(parse_madt_bytes(&b), Err(AcpiError::MadtNotFound));
    }

    #[test]
    fn rejects_zero_length_entry() {
        let mut b = MadtBuilder::new(0);
        b.push_raw(MADT_TYPE_LOCAL_APIC, 0, &[]);
        assert_eq!(parse_madt_bytes(&b.finish()), Err(AcpiError::Truncated));
    }

    #[test]
    fn rejects_entry_overrunning_buffer() {
        let mut b = MadtBuilder::new(0);
        // Forged 200-byte entry with only 6 bytes of payload —
        // would walk far past the end if the bounds check was
        // missing.  Catches the `cursor + entry_len > end` guard.
        b.push_raw(MADT_TYPE_LOCAL_APIC, 200, &[0u8; 6]);
        assert_eq!(parse_madt_bytes(&b.finish()), Err(AcpiError::Truncated));
    }

    #[test]
    fn declared_length_longer_than_bytes_rejected() {
        let mut b = MadtBuilder::new(0).finish();
        // Lie about the declared length — pretend MADT is 1 KiB.
        b[4..8].copy_from_slice(&1024u32.to_le_bytes());
        assert_eq!(parse_madt_bytes(&b), Err(AcpiError::Truncated));
    }

    #[test]
    fn unknown_entry_type_skipped() {
        let mut b = MadtBuilder::new(0);
        b.push_raw(42, 4, &[0u8; 2]);
        b.push_local_apic(0, 99, LOCAL_APIC_ENABLED);
        let s = parse_madt_bytes(&b.finish()).unwrap();
        assert_eq!(s.cpu_count, 1);
        assert_eq!(s.cpus[0], 99);
    }
}
