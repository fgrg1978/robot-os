//! Minimal ACPI Multiple APIC Description Table (MADT) parser.
//!
//! Just enough ACPI to feed [`apic::init_local_apic`] the right
//! base address and enumerate the per-CPU APIC IDs needed by
//! [`Boot::hart_start`]. We don't model DSDT, generic events,
//! power management, or anything else from the much larger ACPI
//! spec — those are kernel-tier concerns, not arch-tier.
//!
//! Scope of B2.acpi:
//!   - Walk an RSDP → RSDT/XSDT → MADT chain given the RSDP's
//!     physical address (the boot loader / PVH hand-off / multiboot
//!     info table is responsible for finding RSDP itself; UEFI
//!     systems pass it through the EFI system table).
//!   - Return [`MadtSummary`] with the LAPIC base + a fixed-size
//!     array of enumerated CPU APIC IDs.
//!   - Pure no_std, no allocation — host-testable on any target.
//!
//! Out of scope: I/O APIC interrupt routing (B2.ioapic), HPET,
//! ACPI 6+ extensions like local x2APIC entries (B2.acpi.x2 with
//! `apic::init_x2apic`).

#![allow(dead_code)]

use core::mem::size_of;

/// Maximum number of CPUs we'll surface from an MADT walk. xAPIC
/// itself only has room for 256; on x2APIC kernels the same field
/// becomes 32-bit and this constant grows.
pub const MAX_CPUS: usize = 32;

/// 8-byte ASCII signature for an SDT header.
type Signature = [u8; 4];

const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
const RSDT_SIG: Signature = *b"RSDT";
const XSDT_SIG: Signature = *b"XSDT";
const MADT_SIG: Signature = *b"APIC"; // MADT's table signature is "APIC"

// ── MADT entry types ─────────────────────────────────────────
const MADT_TYPE_LOCAL_APIC:      u8 = 0;
const MADT_TYPE_IO_APIC:         u8 = 1;
const MADT_TYPE_INT_SRC_OVERRIDE: u8 = 2;
const MADT_TYPE_LAPIC_ADDR_OVR:  u8 = 5;
// Type 9 (Local x2APIC) deferred to B2.acpi.x2.

// ── Local-APIC flags (MADT entry type 0) ─────────────────────
const LOCAL_APIC_ENABLED:         u32 = 1 << 0;
/// "Online Capable" — can be enabled later via ACPI methods. We
/// don't currently start these CPUs; they show up disabled.
const LOCAL_APIC_ONLINE_CAPABLE:  u32 = 1 << 1;

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
struct RsdpV1 {
    signature: [u8; 8],
    checksum:  u8,
    oem_id:    [u8; 6],
    revision:  u8,
    rsdt_addr: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
struct RsdpV2 {
    v1:           RsdpV1,
    length:       u32,
    xsdt_addr:    u64,
    ext_checksum: u8,
    _reserved:    [u8; 3],
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
struct SdtHeader {
    signature:    Signature,
    length:       u32,
    revision:     u8,
    checksum:     u8,
    oem_id:       [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id:   u32,
    creator_rev:  u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MadtSummary {
    /// Physical address of the local APIC (default 0xFEE00000;
    /// MADT can override via type-5 entry).
    pub lapic_pa: u64,
    /// Enumerated CPUs — only "enabled" ones. Each value is an
    /// APIC ID suitable for [`apic::send_ipi`] / SIPI bring-up.
    pub cpus: [u8; MAX_CPUS],
    /// Number of valid entries in `cpus`.
    pub cpu_count: usize,
    /// I/O APIC base, if a type-1 entry was found. 0 means none.
    pub ioapic_pa: u32,
}

/// Errors while walking the ACPI chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpiError {
    /// RSDP signature didn't match `b"RSD PTR "`.
    BadRsdpSignature,
    /// RSDP checksum failed.
    BadRsdpChecksum,
    /// XSDT/RSDT signature wrong.
    BadRootSdtSignature,
    /// MADT not present in the root SDT.
    MadtNotFound,
    /// Computed length would walk off the table.
    Truncated,
}

/// Parse the MADT starting from the RSDP physical address.
///
/// # Safety
///
/// `rsdp_pa` must be the physical address of a valid RSDP
/// structure, and the entire ACPI table tree (RSDT/XSDT + MADT)
/// must be safely readable as identity-mapped memory.
#[cfg(target_arch = "x86_64")]
pub unsafe fn parse_madt(rsdp_pa: usize) -> Result<MadtSummary, AcpiError> {
    let rsdp = unsafe { read_rsdp(rsdp_pa)? };
    let madt_pa = unsafe { find_madt(rsdp)? };
    unsafe { parse_madt_body(madt_pa) }
}

#[cfg(target_arch = "x86_64")]
unsafe fn read_rsdp(rsdp_pa: usize) -> Result<RsdpV2, AcpiError> {
    let v1 = unsafe { core::ptr::read_unaligned(rsdp_pa as *const RsdpV1) };
    if &v1.signature != RSDP_SIG {
        return Err(AcpiError::BadRsdpSignature);
    }
    let v1_bytes = unsafe {
        core::slice::from_raw_parts(rsdp_pa as *const u8, size_of::<RsdpV1>())
    };
    if !checksum_ok(v1_bytes) {
        return Err(AcpiError::BadRsdpChecksum);
    }
    let mut v2 = RsdpV2 {
        v1,
        length: size_of::<RsdpV1>() as u32,
        xsdt_addr: 0,
        ext_checksum: 0,
        _reserved: [0; 3],
    };
    if v1.revision >= 2 {
        v2 = unsafe { core::ptr::read_unaligned(rsdp_pa as *const RsdpV2) };
    }
    Ok(v2)
}

#[cfg(target_arch = "x86_64")]
unsafe fn find_madt(rsdp: RsdpV2) -> Result<usize, AcpiError> {
    // Prefer XSDT if revision >= 2 and addr non-zero.
    let xsdt_addr = rsdp.xsdt_addr;
    let rsdt_addr = rsdp.v1.rsdt_addr;
    if rsdp.v1.revision >= 2 && xsdt_addr != 0 {
        unsafe { walk_xsdt(xsdt_addr as usize) }
    } else {
        unsafe { walk_rsdt(rsdt_addr as usize) }
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn walk_rsdt(rsdt_pa: usize) -> Result<usize, AcpiError> {
    let hdr = unsafe { read_sdt_header(rsdt_pa)? };
    if hdr.signature != RSDT_SIG {
        return Err(AcpiError::BadRootSdtSignature);
    }
    let entries = (hdr.length as usize)
        .saturating_sub(size_of::<SdtHeader>())
        / size_of::<u32>();
    let base = rsdt_pa + size_of::<SdtHeader>();
    for i in 0..entries {
        let ptr = base + i * size_of::<u32>();
        let table_pa = unsafe { core::ptr::read_unaligned(ptr as *const u32) }
            as usize;
        if let Ok(h) = unsafe { read_sdt_header(table_pa) } {
            if h.signature == MADT_SIG {
                return Ok(table_pa);
            }
        }
    }
    Err(AcpiError::MadtNotFound)
}

#[cfg(target_arch = "x86_64")]
unsafe fn walk_xsdt(xsdt_pa: usize) -> Result<usize, AcpiError> {
    let hdr = unsafe { read_sdt_header(xsdt_pa)? };
    if hdr.signature != XSDT_SIG {
        return Err(AcpiError::BadRootSdtSignature);
    }
    let entries = (hdr.length as usize)
        .saturating_sub(size_of::<SdtHeader>())
        / size_of::<u64>();
    let base = xsdt_pa + size_of::<SdtHeader>();
    for i in 0..entries {
        let ptr = base + i * size_of::<u64>();
        let table_pa = unsafe { core::ptr::read_unaligned(ptr as *const u64) }
            as usize;
        if let Ok(h) = unsafe { read_sdt_header(table_pa) } {
            if h.signature == MADT_SIG {
                return Ok(table_pa);
            }
        }
    }
    Err(AcpiError::MadtNotFound)
}

#[cfg(target_arch = "x86_64")]
unsafe fn read_sdt_header(pa: usize) -> Result<SdtHeader, AcpiError> {
    let hdr = unsafe { core::ptr::read_unaligned(pa as *const SdtHeader) };
    if (hdr.length as usize) < size_of::<SdtHeader>() {
        return Err(AcpiError::Truncated);
    }
    Ok(hdr)
}

#[cfg(target_arch = "x86_64")]
unsafe fn parse_madt_body(madt_pa: usize) -> Result<MadtSummary, AcpiError> {
    let hdr = unsafe { read_sdt_header(madt_pa)? };
    if hdr.signature != MADT_SIG {
        return Err(AcpiError::MadtNotFound);
    }
    // Reborrow the whole MADT (header + body) as a byte slice and
    // hand off to the host-testable parser. This means the actual
    // entry-iteration logic — the part most likely to have bounds-
    // checking bugs — can be exercised from a regular `cargo test`
    // on the developer's macOS/Linux machine, no QEMU required.
    let len = hdr.length as usize;
    let bytes = unsafe { core::slice::from_raw_parts(madt_pa as *const u8, len) };
    parse_madt_bytes(bytes)
}

/// Pure-slice variant of [`parse_madt_body`]. Takes the full MADT
/// bytes (header + body) and returns the same summary. No raw
/// pointer dereferences — safely callable from host tests with
/// synthetic byte arrays. The caller is responsible for ensuring
/// `bytes` covers the entire MADT (i.e. is at least `hdr.length`
/// long).
pub fn parse_madt_bytes(bytes: &[u8]) -> Result<MadtSummary, AcpiError> {
    let header_len = size_of::<SdtHeader>();
    if bytes.len() < header_len {
        return Err(AcpiError::Truncated);
    }
    let signature: Signature = bytes[0..4].try_into().unwrap();
    if signature != MADT_SIG {
        return Err(AcpiError::MadtNotFound);
    }
    let declared_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if declared_len < header_len || declared_len > bytes.len() {
        return Err(AcpiError::Truncated);
    }
    // MADT body begins right after the header: u32 lapic_addr + u32 flags.
    if declared_len < header_len + 8 {
        return Err(AcpiError::Truncated);
    }
    let lapic_pa32 = u32::from_le_bytes(
        bytes[header_len..header_len + 4].try_into().unwrap(),
    );
    let mut summary = MadtSummary {
        lapic_pa: lapic_pa32 as u64,
        ..Default::default()
    };
    let mut cursor = header_len + 8; // past lapic_addr + flags
    let end = declared_len;
    while cursor + 2 <= end {
        let entry_type = bytes[cursor];
        let entry_len = bytes[cursor + 1] as usize;
        if entry_len < 2 || cursor + entry_len > end {
            return Err(AcpiError::Truncated);
        }
        match entry_type {
            MADT_TYPE_LOCAL_APIC if entry_len >= 8 => {
                let apic_id = bytes[cursor + 3];
                let flags = u32::from_le_bytes(
                    bytes[cursor + 4..cursor + 8].try_into().unwrap(),
                );
                if flags & LOCAL_APIC_ENABLED != 0
                    && summary.cpu_count < MAX_CPUS
                {
                    summary.cpus[summary.cpu_count] = apic_id;
                    summary.cpu_count += 1;
                }
            }
            MADT_TYPE_IO_APIC if entry_len >= 12 => {
                let ioapic_addr = u32::from_le_bytes(
                    bytes[cursor + 4..cursor + 8].try_into().unwrap(),
                );
                summary.ioapic_pa = ioapic_addr;
            }
            MADT_TYPE_LAPIC_ADDR_OVR if entry_len >= 12 => {
                let addr = u64::from_le_bytes(
                    bytes[cursor + 4..cursor + 12].try_into().unwrap(),
                );
                summary.lapic_pa = addr;
            }
            _ => {}
        }
        cursor += entry_len;
    }
    Ok(summary)
}

/// Byte-sum-mod-256 checksum used by every ACPI table.
fn checksum_ok(bytes: &[u8]) -> bool {
    let sum: u8 = bytes.iter().copied().fold(0u8, u8::wrapping_add);
    sum == 0
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn parse_madt(_rsdp_pa: usize) -> Result<MadtSummary, AcpiError> {
    Err(AcpiError::BadRsdpSignature)
}

// Host tests live in `crates/arch-x86_64-tests/` (workspace-excluded,
// builds for aarch64-apple-darwin). Run with:
//   cd crates/arch-x86_64-tests && cargo test
// They import `parse_madt_bytes` + `checksum_ok` + a handful of
// constants via `#[path]`, same pattern as `crates/ota-tests`.
