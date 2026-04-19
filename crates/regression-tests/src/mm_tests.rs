//! Coverage for `crates/mm/` — was 0 tests before.
//!
//! mm/addr.rs depends transitively on robot_os_arch (riscv asm), so we
//! can't directly #[path]-include it on the host. Instead we replicate
//! the EXACT bit-twiddling logic and lock in the invariants. If a
//! refactor changes the math here, sync both sides.

#![cfg(test)]

const PAGE_SIZE:  usize = 4096;
const PAGE_SHIFT: usize = 12;

// ── Mirror of mm/addr.rs primitives ───────────────────────────────────────

#[inline]
const fn page_align_up(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

#[inline]
const fn page_align_down(addr: usize) -> usize {
    addr & !(PAGE_SIZE - 1)
}

#[inline]
const fn page_offset(addr: usize) -> usize {
    addr & (PAGE_SIZE - 1)
}

#[inline]
const fn page_number(addr: usize) -> usize {
    addr >> PAGE_SHIFT
}

#[inline]
const fn is_page_aligned(addr: usize) -> bool {
    addr & (PAGE_SIZE - 1) == 0
}

// ── Tests ────────────────────────────────────────────────────────────────

#[test]
fn align_up_already_aligned_is_identity() {
    assert_eq!(page_align_up(0),         0);
    assert_eq!(page_align_up(0x1000),    0x1000);
    assert_eq!(page_align_up(0x8000_0000), 0x8000_0000);
}

#[test]
fn align_up_rounds_to_next_page() {
    assert_eq!(page_align_up(1),     0x1000);
    assert_eq!(page_align_up(0xfff), 0x1000);
    assert_eq!(page_align_up(0x1001), 0x2000);
    assert_eq!(page_align_up(0x8000_0fff), 0x8000_1000);
}

#[test]
fn align_down_truncates_to_page_boundary() {
    assert_eq!(page_align_down(0),        0);
    assert_eq!(page_align_down(0xfff),    0);
    assert_eq!(page_align_down(0x1234),   0x1000);
    assert_eq!(page_align_down(0x8000_0fff), 0x8000_0000);
}

#[test]
fn page_offset_is_low_12_bits() {
    assert_eq!(page_offset(0),      0);
    assert_eq!(page_offset(0xabc),  0xabc);
    assert_eq!(page_offset(0x1abc), 0xabc);
    assert_eq!(page_offset(0xfff),  0xfff);
    assert_eq!(page_offset(0x1000), 0);
}

#[test]
fn page_number_drops_low_12_bits() {
    assert_eq!(page_number(0),      0);
    assert_eq!(page_number(0xfff),  0);
    assert_eq!(page_number(0x1000), 1);
    assert_eq!(page_number(0x8000_0000), 0x8_0000);
}

#[test]
fn is_page_aligned_predicate() {
    assert!(is_page_aligned(0));
    assert!(is_page_aligned(0x1000));
    assert!(is_page_aligned(0x8000_0000));
    assert!(!is_page_aligned(1));
    assert!(!is_page_aligned(0xfff));
    assert!(!is_page_aligned(0x1001));
}

/// Property: align_up(x) ≥ x always.
/// Property: align_up(x) - x < PAGE_SIZE always.
/// Property: align_down(x) ≤ x always.
/// Property: x - align_down(x) < PAGE_SIZE always.
#[test]
fn alignment_bounds_are_tight() {
    for &x in &[0usize, 1, 0xabc, 0x1000, 0x1234, 0x80a0_0000] {
        let up = page_align_up(x);
        let down = page_align_down(x);
        assert!(up >= x, "align_up must not go down");
        assert!(up - x < PAGE_SIZE,
                "align_up should round up by < PAGE_SIZE, got {} for {:#x}", up - x, x);
        assert!(down <= x, "align_down must not go up");
        assert!(x - down < PAGE_SIZE,
                "align_down should round down by < PAGE_SIZE");
        assert!(is_page_aligned(up));
        assert!(is_page_aligned(down));
    }
}

/// align_up(align_up(x)) == align_up(x) — idempotence.
#[test]
fn align_up_is_idempotent() {
    for &x in &[0usize, 0xfff, 0x1234, 0xabcd_e000] {
        assert_eq!(page_align_up(page_align_up(x)), page_align_up(x));
    }
}

/// align_down(align_up(x)) — sometimes equal, sometimes a page lower.
/// Specifically: align_down(align_up(x)) == align_up(x) — both are page-aligned.
#[test]
fn align_up_then_down_keeps_alignment() {
    for &x in &[0x1usize, 0xfff, 0x1234, 0xabcd_e000] {
        let up = page_align_up(x);
        assert_eq!(page_align_down(up), up);
    }
}

/// Boundary: the last page in the address space is page-aligned by
/// construction. Lock in: align_up of an already-aligned address is
/// identity even at the top of the address space (no rounding-up
/// overflow because input == output).
#[test]
fn align_up_at_last_page_start_is_identity() {
    // Use a defensively-bounded value: highest valid kernel range we
    // realistically care about (Sv39 has 256 GiB / 39-bit VAs).
    let last_page_start = (1usize << 39) - PAGE_SIZE; // top of Sv39
    assert!(is_page_aligned(last_page_start));
    assert_eq!(page_align_up(last_page_start), last_page_start);
}
