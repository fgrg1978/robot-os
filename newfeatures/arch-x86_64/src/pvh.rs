//! PVH boot hand-off — `hvm_start_info` struct + helpers.
//!
//! The PVH spec (§4.1) defines a hand-off block that the loader
//! (QEMU's emulated PVH path, Xen's PVH guest loader, etc.) puts
//! in RAM and passes to the kernel's entry point in `%rbx`
//! (PVH-direct boot) or `%rdi` (after the trampoline in
//! `x86_64-hello/src/main.rs` translates rbx → rdi to match the
//! SysV AMD64 ABI for `rust_main`).
//!
//! The most load-bearing field is `rsdp_paddr` — feeding that to
//! [`super::acpi::parse_madt`] gives the kernel its CPU
//! enumeration + LAPIC base without having to scan low memory
//! for the RSDP signature.
//!
//! Layout per <https://xenbits.xenproject.org/docs/4.18-testing/misc/pvh.html>
//! and Linux's `arch/x86/include/uapi/asm/bootparam.h`
//! (`struct hvm_start_info`).

#![allow(dead_code)]

use core::mem::size_of;

/// Magic value `b"xEn3"` little-endian — distinguishes a valid
/// PVH hand-off from random memory.
pub const HVM_START_MAGIC: u32 = 0x336E_C578;

/// PVH `hvm_start_info` block — 56 bytes (4×u32 + 4×u64 + 2×u32),
/// 8-byte alignment (the u64 fields).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HvmStartInfo {
    /// Always [`HVM_START_MAGIC`].
    pub magic:           u32,
    /// PVH ABI version (currently 1).
    pub version:         u32,
    /// Implementation-defined flags. Bit 0 = PVH_FLAGS_BSP (this
    /// CPU is the boot processor); not relevant for single-CPU
    /// hand-off.
    pub flags:           u32,
    /// Number of pre-loaded modules described by `modlist_paddr`.
    pub nr_modules:      u32,
    /// PA of an `hvm_modlist_entry` array (we don't use modules).
    pub modlist_paddr:   u64,
    /// PA of an ASCII cmdline string, NUL-terminated.
    pub cmdline_paddr:   u64,
    /// **PA of the ACPI RSDP**. Feeds [`super::acpi::parse_madt`].
    pub rsdp_paddr:      u64,
    /// PA of an `hvm_memmap_table_entry` array (firmware memory
    /// map — we currently ignore in favour of E820 / UEFI when
    /// available, but it's the only memory map PVH supplies).
    pub memmap_paddr:    u64,
    /// Number of valid entries in the memmap above.
    pub memmap_entries:  u32,
    pub _reserved:       u32,
}

/// Memory-map entry pointed at by `memmap_paddr`. 24 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HvmMemmapEntry {
    pub addr:  u64,
    pub size:  u64,
    /// E820-style type: 1 = RAM, 2 = Reserved, 3 = ACPI Reclaim,
    /// 4 = ACPI NVS, 5 = Unusable.
    pub kind:  u32,
    pub _pad:  u32,
}

/// Errors validating a candidate `hvm_start_info` pointer.
#[derive(Debug, PartialEq, Eq)]
pub enum PvhError {
    NullPointer,
    BadMagic,
    UnsupportedVersion,
}

/// Validate + dereference an `hvm_start_info` pointer. The
/// kernel calls this with the value the loader put in RDI/RBX.
///
/// # Safety
/// `ptr` must be either null or a valid pointer into firmware-
/// supplied memory.
#[cfg(target_arch = "x86_64")]
pub unsafe fn read_start_info(ptr: usize) -> Result<HvmStartInfo, PvhError> {
    if ptr == 0 {
        return Err(PvhError::NullPointer);
    }
    let info = unsafe {
        core::ptr::read_unaligned(ptr as *const HvmStartInfo)
    };
    if info.magic != HVM_START_MAGIC {
        return Err(PvhError::BadMagic);
    }
    if info.version == 0 {
        return Err(PvhError::UnsupportedVersion);
    }
    Ok(info)
}

/// Read the firmware memory map entries pointed at by
/// [`HvmStartInfo::memmap_paddr`] into a caller-provided slice.
/// Returns the number of entries copied (≤ `entries.len()`).
#[cfg(target_arch = "x86_64")]
pub unsafe fn read_memmap(
    info: &HvmStartInfo,
    entries: &mut [HvmMemmapEntry],
) -> usize {
    if info.memmap_paddr == 0 {
        return 0;
    }
    let n = (info.memmap_entries as usize).min(entries.len());
    for i in 0..n {
        entries[i] = unsafe {
            core::ptr::read_unaligned(
                (info.memmap_paddr as usize + i * size_of::<HvmMemmapEntry>())
                    as *const HvmMemmapEntry,
            )
        };
    }
    n
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn read_start_info(_ptr: usize) -> Result<HvmStartInfo, PvhError> {
    Err(PvhError::NullPointer)
}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn read_memmap(_info: &HvmStartInfo, _entries: &mut [HvmMemmapEntry]) -> usize { 0 }

// ── Compile-time sanity ─────────────────────────────────────

const _: () = {
    // The PVH spec is firm on these sizes — the loader writes
    // matching bytes, mismatched repr breaks the hand-off.
    if size_of::<HvmStartInfo>() != 56 {
        panic!("hvm_start_info must be 56 bytes per PVH §4.1 \
                (4×u32 + 4×u64 + 2×u32)");
    }
    if size_of::<HvmMemmapEntry>() != 24 {
        panic!("hvm_memmap_table_entry must be 24 bytes");
    }
};
