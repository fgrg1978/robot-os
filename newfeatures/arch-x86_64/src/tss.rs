//! Task State Segment (TSS) — kernel stacks + IST entries.
//!
//! On 64-bit x86 the TSS is mostly degenerate vs 32-bit (no
//! task-switch machinery), but two fields are still load-bearing:
//!
//!   - **RSP0..RSP2**: kernel stack pointer the CPU loads when
//!     transitioning *into* a higher privilege ring via an IDT
//!     gate. RSP0 is the one that matters for ring3→ring0; the
//!     others exist for completeness.
//!   - **IST1..IST7**: dedicated stacks the IDT can route certain
//!     vectors to (double fault, NMI, machine check). Each IDT
//!     entry's `ist` field selects which one; 0 = "use the
//!     current ring's stack". Without IST stacks a double-fault
//!     on a tiny stack triple-faults.
//!
//! The TSS lives in memory as a `TaskStateSegment` (104 bytes,
//! repr(C, packed)) and is pointed at by a 16-byte *system*
//! descriptor in the GDT — twice the size of a normal segment
//! descriptor, so it occupies two GDT slots. Once the GDT has
//! that descriptor, the CPU is told about it via the `ltr`
//! (Load Task Register) instruction with the TSS selector.
//!
//! Mirror on aarch64: nothing direct. ARMv8 doesn't have rings
//! or task gates — privilege transitions go through ELR/SPSR
//! and stack pointer banking (SP_EL0/EL1/EL2/EL3).

#![allow(dead_code)]

use core::mem::size_of;

/// 104-byte 64-bit-mode TSS. Field offsets per Intel SDM Vol. 3
/// §7.7. Many fields are reserved / unused in 64-bit mode but
/// kept in the layout because the CPU still reads from them.
#[repr(C, packed)]
pub struct TaskStateSegment {
    _reserved0:        u32,
    /// Kernel stack pointer for transitions to ring 0.
    pub rsp0:          u64,
    /// Ring-1 stack — unused in our kernel (we don't have ring 1).
    pub rsp1:          u64,
    /// Ring-2 stack — unused.
    pub rsp2:          u64,
    _reserved1:        u64,
    /// Interrupt Stack Table 1..=7. IDT entries whose `ist`
    /// field is non-zero land on the matching IST stack —
    /// canonical use: IST1 for double fault, IST2 for NMI.
    pub ist:           [u64; 7],
    _reserved2:        u64,
    _reserved3:        u16,
    /// Offset (from TSS base) to the I/O permission bitmap.
    /// Setting it past the end of the TSS disables port-by-port
    /// I/O permission checking — fine for a kernel that doesn't
    /// expose `in`/`out` to user mode.
    pub iomap_base:    u16,
}

impl TaskStateSegment {
    /// Zero-initialised TSS — all stacks null, no IST entries.
    /// The kernel must fill RSP0 before any ring3→ring0
    /// transition can take that stack.
    pub const fn zero() -> Self {
        TaskStateSegment {
            _reserved0: 0,
            rsp0: 0, rsp1: 0, rsp2: 0,
            _reserved1: 0,
            ist: [0; 7],
            _reserved2: 0,
            _reserved3: 0,
            // iomap_base = sizeof(TSS) → "no I/O bitmap follows".
            iomap_base: 0x68,
        }
    }
}

/// Load the Task Register with the GDT selector pointing at the
/// TSS descriptor. Must be called *after* the GDT containing
/// that descriptor has been loaded with `lgdt`.
///
/// # Safety
/// `selector` must point at a valid TSS system descriptor in
/// the currently loaded GDT.
#[cfg(target_arch = "x86_64")]
pub unsafe fn load_tr(selector: u16) {
    unsafe {
        core::arch::asm!(
            "ltr {0:x}",
            in(reg) selector,
            options(nostack, preserves_flags),
        );
    }
}

/// 16-byte TSS system descriptor — twice the size of a normal
/// GDT entry. Built from a TSS pointer + the standard "TSS
/// available" type (0x09).
///
/// The kernel allocates two consecutive GDT slots for this and
/// memcpy's the bytes in via [`encode_tss_descriptor`]; the
/// expected GDT selector is then `index << 3` of the first slot
/// (RPL = 0).
#[repr(C, packed)]
pub struct TssDescriptor {
    limit_low:   u16,
    base_low:    u16,
    base_mid_lo: u8,
    /// 0x89 = P=1 DPL=0 S=0 type=9 (TSS available).
    access:      u8,
    /// G=0, limit_high in low nibble.
    flags:       u8,
    base_mid_hi: u8,
    base_high:   u32,
    _reserved:   u32,
}

impl TssDescriptor {
    pub const NULL: TssDescriptor = TssDescriptor {
        limit_low: 0, base_low: 0, base_mid_lo: 0,
        access: 0, flags: 0, base_mid_hi: 0, base_high: 0,
        _reserved: 0,
    };

    /// Build a TSS descriptor pointing at `tss` (must outlive the
    /// GDT — typically a `static mut TaskStateSegment`).
    pub fn new_for(tss: *const TaskStateSegment) -> Self {
        let base = tss as u64;
        let limit = (size_of::<TaskStateSegment>() - 1) as u32;
        TssDescriptor {
            limit_low:   limit as u16,
            base_low:    base as u16,
            base_mid_lo: (base >> 16) as u8,
            access:      0x89, // P|S=0|Type=9 (TSS available)
            flags:       ((limit >> 16) & 0x0F) as u8, // G=0, AVL=0
            base_mid_hi: (base >> 24) as u8,
            base_high:   (base >> 32) as u32,
            _reserved:   0,
        }
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn load_tr(_selector: u16) {}

// ── Compile-time sanity ─────────────────────────────────────

const _: () = {
    if size_of::<TaskStateSegment>() != 104 {
        panic!("TSS must be 104 bytes per Intel SDM §7.7");
    }
    if size_of::<TssDescriptor>() != 16 {
        panic!("TSS system descriptor must be 16 bytes");
    }
};
