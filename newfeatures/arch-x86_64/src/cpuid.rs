//! CPUID-based CPU identification — vendor + brand string +
//! family/model decoding.
//!
//! Used by boot log + procfs to identify what hardware the
//! kernel is actually running on. Most calls return owned
//! arrays (no allocation needed in `no_std`) so callers can
//! print or compare without lifetimes.

#![allow(dead_code)]

/// 12-byte ASCII vendor identification string from CPUID leaf 0,
/// e.g. b"GenuineIntel", b"AuthenticAMD", b"KVMKVMKVM\0\0\0".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VendorId(pub [u8; 12]);

impl VendorId {
    pub fn as_bytes(&self) -> &[u8] { &self.0 }
    pub fn is_intel(&self) -> bool { &self.0 == b"GenuineIntel" }
    pub fn is_amd(&self)   -> bool { &self.0 == b"AuthenticAMD" }
    /// True if we're under any common hypervisor (KVM, Xen,
    /// VMware, Hyper-V) — useful for skipping HW-specific
    /// quirks that don't apply in virt.
    pub fn is_hypervisor(&self) -> bool {
        matches!(
            &self.0,
            b"KVMKVMKVM\0\0\0"
                | b"VMwareVMware"
                | b"Microsoft Hv"
                | b"XenVMMXenVMM"
                | b"TCGTCGTCGTCG"
        )
    }
}

/// 48-byte ASCII brand string from CPUID leaves 0x80000002-4,
/// e.g. b"Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz\0\0\0\0\0\0\0\0".
#[derive(Clone, Copy)]
pub struct BrandString(pub [u8; 48]);

impl BrandString {
    pub fn as_bytes(&self) -> &[u8] { &self.0 }
    /// Trim trailing NULs + spaces (the BIOS pads with both).
    pub fn trimmed(&self) -> &[u8] {
        let mut end = self.0.len();
        while end > 0 {
            let b = self.0[end - 1];
            if b == 0 || b == b' ' { end -= 1; } else { break; }
        }
        &self.0[..end]
    }
}

/// Decoded family/model/stepping per Intel SDM Vol. 2 §3.3.
#[derive(Clone, Copy, Debug)]
pub struct CpuVersion {
    pub family:   u16,
    pub model:    u16,
    pub stepping: u8,
}

/// Read the vendor ID. CPUID leaf 0 returns EBX/EDX/ECX (NOT
/// EBX/ECX/EDX — Intel's classic gotcha).
#[cfg(target_arch = "x86_64")]
pub fn vendor_id() -> VendorId {
    let r = unsafe { core::arch::x86_64::__cpuid(0) };
    let mut out = [0u8; 12];
    out[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    out[4..8].copy_from_slice(&r.edx.to_le_bytes());
    out[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    VendorId(out)
}

/// Read the brand string — requires three CPUID calls
/// (0x80000002, 0x80000003, 0x80000004), each contributing
/// 16 bytes in EAX/EBX/ECX/EDX order.
#[cfg(target_arch = "x86_64")]
pub fn brand_string() -> Option<BrandString> {
    // First check the extended leaf range supports brand string.
    let ext_max = unsafe { core::arch::x86_64::__cpuid(0x8000_0000) }.eax;
    if ext_max < 0x8000_0004 {
        return None;
    }
    let mut out = [0u8; 48];
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004].iter().enumerate() {
        let r = unsafe { core::arch::x86_64::__cpuid(*leaf) };
        let base = i * 16;
        out[base + 0..base + 4 ].copy_from_slice(&r.eax.to_le_bytes());
        out[base + 4..base + 8 ].copy_from_slice(&r.ebx.to_le_bytes());
        out[base + 8..base + 12].copy_from_slice(&r.ecx.to_le_bytes());
        out[base + 12..base + 16].copy_from_slice(&r.edx.to_le_bytes());
    }
    Some(BrandString(out))
}

/// Decode CPUID leaf 1 EAX into family/model/stepping.
///
/// Spec gotcha — the formula is:
///   `family   = base_family + (if base_family == 0xF { ext_family } else { 0 })`
///   `model    = (ext_model << 4) | base_model` (when base_family ∈ {0x6, 0xF})
#[cfg(target_arch = "x86_64")]
pub fn cpu_version() -> CpuVersion {
    let r = unsafe { core::arch::x86_64::__cpuid(1) };
    let eax = r.eax;
    let base_family = ((eax >> 8) & 0xF) as u16;
    let base_model  = ((eax >> 4) & 0xF) as u16;
    let ext_family  = ((eax >> 20) & 0xFF) as u16;
    let ext_model   = ((eax >> 16) & 0xF) as u16;
    let stepping    = (eax & 0xF) as u8;

    let family = if base_family == 0xF { base_family + ext_family } else { base_family };
    let model  = if base_family == 0x6 || base_family == 0xF {
        (ext_model << 4) | base_model
    } else {
        base_model
    };
    CpuVersion { family, model, stepping }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "x86_64"))]
pub fn vendor_id() -> VendorId { VendorId([0; 12]) }
#[cfg(not(target_arch = "x86_64"))]
pub fn brand_string() -> Option<BrandString> { None }
#[cfg(not(target_arch = "x86_64"))]
pub fn cpu_version() -> CpuVersion {
    CpuVersion { family: 0, model: 0, stepping: 0 }
}
