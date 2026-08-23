//! x86_64 Global Descriptor Table (GDT) — minimal kernel/user
//! code+data segments, ready for ring0↔ring3 transitions.
//!
//! In 64-bit long mode most descriptor fields (base, limit) are
//! ignored — only the access byte + flags actually matter. The
//! kernel needs at least:
//!
//!   index 0  null
//!   index 1  kernel code (selector 0x08, RPL=0, type=code+exec)
//!   index 2  kernel data (selector 0x10, RPL=0, type=data+write)
//!   index 3  user   code (selector 0x1B, RPL=3, type=code+exec)
//!   index 4  user   data (selector 0x23, RPL=3, type=data+write)
//!
//! For ring3 → ring0 transitions a TSS (Task State Segment) is
//! also needed.  The TSS descriptor type itself lives in
//! [`super::tss::TssDescriptor`]; this module reserves the GDT
//! slot for it at indices 5+6 (TSS is a 16-byte system
//! descriptor that takes two normal entry slots in 64-bit mode)
//! and exposes [`install_tss`] for the boot path to populate it
//! once the per-CPU TSS has been allocated.
//!
//! Counterpart in aarch64 land: nothing direct. Aarch64 has no
//! segment selectors — privilege is governed by SPSR mode bits +
//! page-table AP fields. This module is one of the few "no
//! equivalent" pieces between the two ISAs.

#![allow(dead_code)]

use core::mem::size_of;

/// 8-byte 64-bit-mode segment descriptor. Most fields are
/// architecturally ignored; the access byte + the long-mode
/// (L) flag are the only ones the CPU consults.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GdtEntry {
    limit_low:   u16,  // ignored in long mode
    base_low:    u16,  // ignored in long mode
    base_mid:    u8,   // ignored in long mode
    /// Access byte:
    ///   bit 7    P (present)
    ///   bits 6-5 DPL (privilege level)
    ///   bit 4    S (1 = code/data, 0 = system)
    ///   bit 3    E (1 = code, 0 = data)
    ///   bit 2    DC (direction/conforming)
    ///   bit 1    RW (1 = readable code OR writable data)
    ///   bit 0    A (accessed; CPU sets, ignored on write)
    pub access:  u8,
    /// Flags (high nibble) + limit_high (low nibble):
    ///   bit 7    G (granularity)
    ///   bit 6    DB (1 = 32-bit, 0 = 16-bit OR 64-bit)
    ///   bit 5    L (1 = 64-bit code segment)
    ///   bit 4    reserved
    ///   bits 3-0 limit[19:16] (ignored in long mode)
    pub flags:   u8,
    base_high:   u8,   // ignored in long mode
}

impl GdtEntry {
    pub const NULL: GdtEntry = GdtEntry {
        limit_low: 0, base_low: 0, base_mid: 0,
        access: 0, flags: 0, base_high: 0,
    };

    /// Kernel code segment (DPL=0, code, executable, readable,
    /// long-mode = L=1). Matches selector 0x08 if at index 1.
    pub const KERNEL_CODE: GdtEntry = GdtEntry {
        limit_low: 0xFFFF, base_low: 0, base_mid: 0,
        // P=1, DPL=0, S=1, E=1, RW=1 → 0b1001_1010
        access: 0b1001_1010,
        // L=1 (long mode), G=1, limit_high=0xF
        flags:  0b1010_1111,
        base_high: 0,
    };

    /// Kernel data segment (DPL=0, data, writable). Selector
    /// 0x10 if at index 2.
    pub const KERNEL_DATA: GdtEntry = GdtEntry {
        limit_low: 0xFFFF, base_low: 0, base_mid: 0,
        // P=1, DPL=0, S=1, E=0, RW=1 → 0b1001_0010
        access: 0b1001_0010,
        // G=1, limit_high=0xF; DB/L don't matter for data.
        flags:  0b1100_1111,
        base_high: 0,
    };

    /// User code segment (DPL=3, code, executable, readable,
    /// long-mode). Selector 0x1B if at index 3 (3 << 3 | 3 = 0x1B,
    /// RPL=3).
    pub const USER_CODE: GdtEntry = GdtEntry {
        limit_low: 0xFFFF, base_low: 0, base_mid: 0,
        // P=1, DPL=3, S=1, E=1, RW=1 → 0b1111_1010
        access: 0b1111_1010,
        flags:  0b1010_1111,
        base_high: 0,
    };

    /// User data segment (DPL=3, data, writable). Selector 0x23
    /// if at index 4.
    pub const USER_DATA: GdtEntry = GdtEntry {
        limit_low: 0xFFFF, base_low: 0, base_mid: 0,
        // P=1, DPL=3, S=1, E=0, RW=1 → 0b1111_0010
        access: 0b1111_0010,
        flags:  0b1100_1111,
        base_high: 0,
    };
}

/// Number of entries in the kernel GDT.
///
/// Layout (indices):
///   0  null
///   1  kernel code
///   2  kernel data
///   3  user   code
///   4  user   data
///   5  TSS low  (8 bytes — first half of the 16-byte TSS desc)
///   6  TSS high (8 bytes — second half; together (5,6) = 1 TSS)
///
/// = 7 entries.  The TSS is a 16-byte system descriptor that
/// straddles two normal-entry slots in 64-bit mode because the
/// `base_high` field is 64-bit; see [`super::tss::TssDescriptor`].
pub const GDT_ENTRIES: usize = 7;

/// Index of the TSS descriptor within [`GDT_TABLE`] (low half).
pub const TSS_GDT_INDEX: usize = 5;

/// The GDT itself — populated by [`init_gdt`] at boot and
/// usually never mutated after `lgdt` except via [`install_tss`]
/// which writes the TSS slots before `ltr` is issued.
static mut GDT_TABLE: [GdtEntry; GDT_ENTRIES] = [
    GdtEntry::NULL,
    GdtEntry::KERNEL_CODE,
    GdtEntry::KERNEL_DATA,
    GdtEntry::USER_CODE,
    GdtEntry::USER_DATA,
    GdtEntry::NULL,  // TSS low — patched by install_tss()
    GdtEntry::NULL,  // TSS high — patched by install_tss()
];

/// Selector constants — `index << 3 | RPL`. Match the layout
/// of [`GDT_TABLE`] above; if you reorder one, update both.
pub mod selector {
    /// Kernel code at index 1, RPL=0.
    pub const KERNEL_CODE: u16 = 1 << 3;        // 0x08
    /// Kernel data at index 2, RPL=0.
    pub const KERNEL_DATA: u16 = 2 << 3;        // 0x10
    /// User code at index 3, RPL=3.
    pub const USER_CODE:   u16 = (3 << 3) | 3;  // 0x1B
    /// User data at index 4, RPL=3.
    pub const USER_DATA:   u16 = (4 << 3) | 3;  // 0x23
    /// TSS at index 5, RPL=0 (TSS is always kernel-privileged).
    pub const TSS:         u16 = 5 << 3;        // 0x28
}

/// 10-byte pseudo-descriptor consumed by `lgdt`.
#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base:  u64,
}

/// Load the kernel GDT and update the segment selectors. After
/// this returns the CPU uses our long-mode descriptors instead
/// of whatever the bootloader left in place.
///
/// # Safety
/// Must be called once during early boot, after the GDT bytes
/// land in writable memory. Reloading CS via the far-return
/// trick clobbers an arbitrary kernel-local label, so caller
/// must be ready to lose the current code-segment cache.
#[cfg(target_arch = "x86_64")]
pub unsafe fn load_gdt() {
    let ptr = GdtPointer {
        limit: (size_of::<[GdtEntry; GDT_ENTRIES]>() - 1) as u16,
        base:  unsafe { core::ptr::addr_of!(GDT_TABLE) } as u64,
    };
    unsafe {
        core::arch::asm!(
            "lgdt [{0}]",
            // Reload data segments — long mode mostly ignores
            // them but writing here forces the cached selectors
            // to match our GDT.
            "mov ax, {ds}",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            in(reg) &ptr,
            ds  = const selector::KERNEL_DATA,
            out("ax") _,
            options(nostack, preserves_flags),
        );
    }
    // CS reload (the actual `jmp far` trick) is left to the
    // kernel caller because the target label is kernel-local.
}

/// Convenience init — load the table. The static is already
/// initialised at compile time so there's nothing to populate.
#[cfg(target_arch = "x86_64")]
pub unsafe fn init_gdt() {
    unsafe { load_gdt(); }
}

/// Patch the 16-byte TSS descriptor into [`GDT_TABLE`] slots
/// `TSS_GDT_INDEX` + `TSS_GDT_INDEX + 1`.  Call once per CPU
/// after the per-CPU TSS has been allocated; immediately follow
/// with `ltr(selector::TSS)` so the CPU picks up the new RSP0
/// on the next ring3→ring0 transition.
///
/// # Safety
/// Requires that [`load_gdt`] has already run (otherwise lgdt
/// would re-load with stale TSS = null and the next interrupt
/// from ring3 would trap with #GP).  Caller must also ensure
/// `tss` outlives the GDT.
#[cfg(target_arch = "x86_64")]
pub unsafe fn install_tss(tss: *const super::tss::TaskStateSegment) {
    let desc = super::tss::TssDescriptor::new_for(tss);
    // TssDescriptor is 16 bytes = two consecutive GdtEntry slots.
    // Reinterpret-cast via transmute so each 8-byte half lands
    // in the right slot.
    let halves: [GdtEntry; 2] = unsafe {
        core::mem::transmute::<super::tss::TssDescriptor, [GdtEntry; 2]>(desc)
    };
    unsafe {
        let table = core::ptr::addr_of_mut!(GDT_TABLE);
        (*table)[TSS_GDT_INDEX]     = halves[0];
        (*table)[TSS_GDT_INDEX + 1] = halves[1];
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn load_gdt() {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn init_gdt() {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn install_tss(_tss: *const super::tss::TaskStateSegment) {}

// ── Compile-time sanity ─────────────────────────────────────

const _: () = {
    if size_of::<GdtEntry>() != 8 {
        panic!("GdtEntry must be 8 bytes");
    }
    if size_of::<GdtPointer>() != 10 {
        panic!("GdtPointer must be 10 bytes");
    }
};
