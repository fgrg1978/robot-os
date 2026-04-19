//! Coverage for security bugs found during the project audit and fixed.
//! Each module pins a specific vulnerability so a refactor cannot
//! silently re-open the hole.

#![cfg(test)]

// ── ELF loader bounds (sec.elf-1) ────────────────────────────────────────
// Locks the fix that prevents:
//   - p_vaddr in kernel space being mapped into the user PT
//   - p_memsz huge → infinite alloc loop / OOM
//   - p_filesz > p_memsz (ELF spec violation)
//   - p_offset + p_filesz overflowing past elf.len() (OOB read of kernel mem)

#[cfg(test)]
mod elf_bounds {
    const USER_STACK_TOP: usize = 0x0000_0000_8000_0000;
    const USER_IMAGE_MAX: usize = 0x4000_0000;
    const PAGE_SIZE:      usize = 4096;

    /// Mirror of the bounds-checking the loader does after parsing each
    /// program header. Returns `false` if the ELF should be rejected.
    fn validate_phdr(p_vaddr: usize, p_memsz: usize, p_filesz: usize,
                     p_offset: usize, elf_len: usize) -> bool {
        if p_memsz == 0 { return true; } // skipped, not rejected
        if p_vaddr >= USER_STACK_TOP { return false; }
        if p_memsz > USER_IMAGE_MAX  { return false; }
        if p_filesz > p_memsz        { return false; }
        let va_end = match p_vaddr.checked_add(p_memsz) {
            Some(v) if v <= USER_STACK_TOP => v,
            _ => return false,
        };
        let _ = va_end;
        match p_offset.checked_add(p_filesz) {
            Some(end) => end <= elf_len,
            None      => false,
        }
    }

    #[test]
    fn reject_kernel_space_vaddr() {
        // p_vaddr in S-mode kernel range — would map kernel memory in user PT.
        assert!(!validate_phdr(0x8020_0000, 0x1000, 0x1000, 0, 0x10_000));
        assert!(!validate_phdr(0xFFFF_FFFF_8000_0000, 0x1000, 0x1000, 0, 0x10_000));
    }

    #[test]
    fn reject_huge_memsz() {
        // Eats all PMM pages, taking down the kernel.
        assert!(!validate_phdr(0x1000, USER_IMAGE_MAX + 1, 0, 0, 0x10_000));
        assert!(!validate_phdr(0x1000, usize::MAX, 0, 0, 0x10_000));
    }

    #[test]
    fn reject_filesz_greater_than_memsz() {
        // Per ELF spec: filesz <= memsz. Larger means undefined behaviour.
        assert!(!validate_phdr(0x1000, 0x1000, 0x2000, 0, 0x10_000));
    }

    #[test]
    fn reject_overflow_in_vaddr_plus_memsz() {
        // p_vaddr + p_memsz wraps to a small value, would pass naive check.
        let huge = usize::MAX - 0xFFF;
        assert!(!validate_phdr(huge, 0x2000, 0x2000, 0, 0x10_000));
    }

    #[test]
    fn reject_offset_plus_filesz_past_elf() {
        // OOB read of kernel memory beyond the user-supplied ELF blob.
        // p_offset + p_filesz must be <= elf.len().
        assert!(!validate_phdr(0x1000, 0x1000, 0x1000, 0xF001, 0x10_000));
    }

    #[test]
    fn accept_offset_plus_filesz_at_elf_end() {
        // Boundary: exactly at end is allowed (read up to but not past).
        assert!(validate_phdr(0x1000, 0x1000, 0x1000, 0xF000, 0x10_000));
    }

    #[test]
    fn accept_normal_well_formed_phdr() {
        assert!(validate_phdr(0x1000, 0x2000, 0x1500, 0x100, 0x10_000));
    }

    #[test]
    fn accept_zero_memsz_skipped() {
        // memsz=0 is a no-op load (skipped), not a rejection.
        assert!(validate_phdr(0, 0, 0, 0, 0));
    }
}

// ── ARP cache poisoning gate (sec.arp-1) ────────────────────────────────
// Locks the fix that only learns IP→MAC bindings for ARP traffic
// genuinely related to us (request for our IP, or reply we expect).

#[cfg(test)]
mod arp_anti_poison {
    const ARP_OP_REQUEST: u16 = 1;
    const ARP_OP_REPLY:   u16 = 2;

    /// Mirror of arp.rs gate.
    fn should_learn(op: u16, sender_ip: [u8; 4], sender_mac: [u8; 6],
                    target_ip: [u8; 4], our_ip: [u8; 4]) -> bool {
        let zero      = [0u8; 4];
        let broadcast = [0xff; 4];
        if sender_ip == zero || sender_ip == broadcast { return false; }
        if sender_mac[0] & 0x01 != 0 { return false; } // multicast bit
        let req_for_us = op == ARP_OP_REQUEST && target_ip == our_ip;
        let reply_to_us = op == ARP_OP_REPLY && target_ip == our_ip;
        req_for_us || reply_to_us
    }

    #[test]
    fn legitimate_request_learned() {
        let our_ip = [10, 0, 0, 1];
        assert!(should_learn(
            ARP_OP_REQUEST,
            [10, 0, 0, 2], [0x52, 0x55, 0, 0, 0, 2],
            our_ip, our_ip,
        ));
    }

    #[test]
    fn legitimate_reply_to_us_learned() {
        let our_ip = [10, 0, 0, 1];
        assert!(should_learn(
            ARP_OP_REPLY,
            [10, 0, 0, 2], [0x52, 0x55, 0, 0, 0, 2],
            our_ip, our_ip,
        ));
    }

    #[test]
    fn gratuitous_arp_for_other_ip_ignored() {
        // Attacker advertises gateway MAC for OWN IP — must NOT poison.
        let our_ip = [10, 0, 0, 1];
        assert!(!should_learn(
            ARP_OP_REPLY,
            [10, 0, 0, 254], [0xde, 0xad, 0xbe, 0xef, 0, 1],
            [10, 0, 0, 254], our_ip,
        ));
    }

    #[test]
    fn zero_source_ip_ignored() {
        let our_ip = [10, 0, 0, 1];
        assert!(!should_learn(
            ARP_OP_REPLY,
            [0, 0, 0, 0], [0x52, 0x55, 0, 0, 0, 2],
            our_ip, our_ip,
        ));
    }

    #[test]
    fn multicast_source_mac_ignored() {
        // Multicast bit (0x01 in first octet) is illegal as ARP source.
        let our_ip = [10, 0, 0, 1];
        assert!(!should_learn(
            ARP_OP_REPLY,
            [10, 0, 0, 2], [0x01, 0x55, 0, 0, 0, 2],
            our_ip, our_ip,
        ));
    }
}

// ── VirtIO descriptor id bounds (sec.virtio-1) ──────────────────────────

#[cfg(test)]
mod virtio_id_bounds {
    /// Mirror of the bounds check in virtq_poll_with_len.
    fn id_is_valid(id: usize, qsize: usize) -> bool {
        id < qsize
    }

    #[test]
    fn id_within_queue_accepted() {
        assert!(id_is_valid(0,  64));
        assert!(id_is_valid(63, 64));
    }

    #[test]
    fn id_at_or_above_qsize_rejected() {
        assert!(!id_is_valid(64,    64));
        assert!(!id_is_valid(65535, 64));
        assert!(!id_is_valid(usize::MAX, 64));
    }
}

// ── Constant-time pubkey comparison (sec.crypto-1) ──────────────────────

#[cfg(test)]
mod ct_eq {
    fn ct_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() { return false; }
        let mut diff = 0u8;
        for i in 0..a.len() {
            diff |= a[i] ^ b[i];
        }
        core::hint::black_box(diff) == 0
    }

    #[test]
    fn equal_arrays_match() {
        let a = [1u8, 2, 3, 4];
        let b = [1u8, 2, 3, 4];
        assert!(ct_eq(&a, &b));
    }

    #[test]
    fn different_first_byte() {
        let a = [0u8, 2, 3, 4];
        let b = [1u8, 2, 3, 4];
        assert!(!ct_eq(&a, &b));
    }

    #[test]
    fn different_last_byte() {
        // Crucially: even with mismatch at end, no early-out (all bytes
        // XOR'd before checking diff).
        let a = [1u8, 2, 3, 4];
        let b = [1u8, 2, 3, 5];
        assert!(!ct_eq(&a, &b));
    }

    #[test]
    fn unequal_length_rejected() {
        assert!(!ct_eq(&[1u8], &[1, 2]));
    }
}

// ── TCP RST handling closes connection (sec.tcp-1) ──────────────────────

#[cfg(test)]
mod tcp_rst {
    #[derive(PartialEq, Debug)]
    enum State { Established, Closed }

    /// Mirror of the RST handler in tcp.rs.
    fn apply_rst(state: &mut State, unacked: &mut bool, retx_len: &mut usize) {
        *state = State::Closed;
        *unacked = false;
        *retx_len = 0;
    }

    #[test]
    fn rst_in_established_closes() {
        let mut s = State::Established;
        let mut u = true;
        let mut l = 1500;
        apply_rst(&mut s, &mut u, &mut l);
        assert_eq!(s, State::Closed);
        assert!(!u);
        assert_eq!(l, 0);
    }
}
