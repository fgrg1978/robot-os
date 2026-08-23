//! x86_64 Interrupt Descriptor Table (IDT) — 256 gate descriptors
//! + a `lidt`-compatible IDTR.
//!
//! Mirror of how `arch-aarch64::sysregs::set_vbar_el1` programs the
//! exception vector table on ARMv8. Without an IDT loaded, any
//! interrupt or CPU exception triple-faults.
//!
//! What lives here:
//!   - The 16-byte 64-bit gate-descriptor format (offset split
//!     across three fields, code selector, IST index, type+DPL).
//!   - A static 256-entry IDT_TABLE plus the matching 10-byte
//!     IDTR pseudo-descriptor.
//!   - `set_handler(vector, addr)` to wire one entry.
//!   - `load_idt()` to issue `lidt`.
//!
//! What does *not* live here (kernel-tier work):
//!   - Per-vector wrapper asm that pushes a TrapFrame and calls
//!     a Rust `trap_handler` — the kernel has its own opinions
//!     about register-save layout; this module only programs the
//!     hardware table.
//!   - TSS + IST setup for ring3→ring0 transitions and
//!     double-fault stacks (B2.tss follow-up).

#![allow(dead_code)]

use core::mem::size_of;

// ── Gate-descriptor layout (Intel SDM Vol. 3 §6.14.1) ────────
//
// Bits   Field
//   0–15  offset_low      low 16 bits of handler address
//  16–31  selector        code-segment selector (typically 0x08)
//  32–34  ist             interrupt stack table index (0 = no IST)
//  35–39  reserved (0)
//  40–43  type            0xE = 64-bit interrupt gate, 0xF = trap gate
//      44  reserved (0)
//  45–46  dpl             descriptor privilege level (0 = kernel)
//      47  present
//  48–63  offset_mid      middle 16 bits of handler address
//  64–95  offset_high     high 32 bits of handler address
//  96–127 reserved (0)
//
// Total 16 bytes per entry; 256 entries = 4 KiB IDT.

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IdtEntry {
    offset_low:  u16,
    selector:    u16,
    /// Bits [2:0] = IST index, [7:3] = reserved.
    ist:         u8,
    /// Bits [3:0] = type, [4] = 0, [6:5] = DPL, [7] = present.
    type_attr:   u8,
    offset_mid:  u16,
    offset_high: u32,
    _reserved:   u32,
}

impl IdtEntry {
    /// Bit pattern for an unused entry — present=0 so the CPU
    /// reports a #NP fault if the vector is taken.
    pub const NULL: IdtEntry = IdtEntry {
        offset_low: 0, selector: 0, ist: 0, type_attr: 0,
        offset_mid: 0, offset_high: 0, _reserved: 0,
    };

    /// Build a 64-bit interrupt-gate entry for `handler_addr`.
    /// `selector` is the kernel code-segment selector (usually
    /// `0x08`), `ist` is 0 for "use the current stack" or 1..=7
    /// to switch to the matching TSS.IST entry on entry.
    pub const fn interrupt_gate(handler_addr: u64, selector: u16, ist: u8) -> Self {
        IdtEntry {
            offset_low:  handler_addr        as u16,
            selector,
            ist:         ist & 0b0000_0111,
            type_attr:   0b1000_1110,        // P=1, DPL=0, type=0xE (intr gate)
            offset_mid:  (handler_addr >> 16) as u16,
            offset_high: (handler_addr >> 32) as u32,
            _reserved:   0,
        }
    }

    /// Trap-gate variant — same as interrupt gate but doesn't
    /// automatically clear RFLAGS.IF on entry (handler may be
    /// preempted by higher-priority interrupts).
    pub const fn trap_gate(handler_addr: u64, selector: u16, ist: u8) -> Self {
        let mut g = Self::interrupt_gate(handler_addr, selector, ist);
        g.type_attr = 0b1000_1111;            // type=0xF
        g
    }
}

/// Pseudo-descriptor consumed by `lidt`. 10 bytes total.
#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base:  u64,
}

/// Number of IDT entries — fixed at the architectural 256.
pub const IDT_ENTRIES: usize = 256;

/// The IDT itself. Kept in `.bss`; populated via [`set_handler`]
/// during kernel boot before [`load_idt`].
static mut IDT_TABLE: [IdtEntry; IDT_ENTRIES] = [IdtEntry::NULL; IDT_ENTRIES];

/// Default kernel code-segment selector (GDT index 1, RPL 0).
/// Matches the x86_64-hello demo's GDT and the typical
/// post-multiboot setup.
pub const DEFAULT_CODE_SELECTOR: u16 = 0x08;

/// Set a single IDT entry. `vector` is 0..=255; the kernel
/// programs APIC IPI vectors here too.
///
/// # Safety
/// `IDT_TABLE` is a global mutable static. Concurrent writes
/// from multiple harts are races; the kernel must call this
/// from a single thread during early boot or hold a lock.
#[cfg(target_arch = "x86_64")]
pub unsafe fn set_handler(vector: u8, handler_addr: u64) {
    unsafe {
        IDT_TABLE[vector as usize] =
            IdtEntry::interrupt_gate(handler_addr, DEFAULT_CODE_SELECTOR, 0);
    }
}

/// Issue `lidt` to point the CPU at [`IDT_TABLE`]. Idempotent;
/// safe to call again after re-populating entries.
#[cfg(target_arch = "x86_64")]
pub unsafe fn load_idt() {
    let ptr = IdtPointer {
        limit: (size_of::<[IdtEntry; IDT_ENTRIES]>() - 1) as u16,
        base:  unsafe { core::ptr::addr_of!(IDT_TABLE) } as u64,
    };
    unsafe {
        core::arch::asm!(
            "lidt [{0}]",
            in(reg) &ptr,
            options(nostack, preserves_flags, readonly),
        );
    }
}

/// Convenience init: clear all entries to NULL, then load the
/// table. After this, every vector still traps to #NP — the
/// kernel must `set_handler` each vector it actually wants to
/// service before that vector fires.
#[cfg(target_arch = "x86_64")]
pub unsafe fn init_idt() {
    unsafe {
        for e in IDT_TABLE.iter_mut() { *e = IdtEntry::NULL; }
        load_idt();
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn set_handler(_vector: u8, _handler_addr: u64) {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn load_idt() {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn init_idt() {}

// ── Compile-time sanity ─────────────────────────────────────

const _: () = {
    if size_of::<IdtEntry>() != 16 {
        panic!("IdtEntry must be 16 bytes — see Intel SDM Vol. 3 §6.14.1");
    }
    if size_of::<IdtPointer>() != 10 {
        panic!("IdtPointer must be 10 bytes (limit:u16 + base:u64)");
    }
};
