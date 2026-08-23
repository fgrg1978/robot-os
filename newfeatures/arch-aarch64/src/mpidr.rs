//! MPIDR_EL1 — Multiprocessor Affinity Register decoder.
//!
//! 64-bit register that identifies *this* PE within the system
//! topology. Layout:
//!
//!   bits [7:0]    Aff0 — innermost level (typically thread / SMT)
//!   bits [15:8]   Aff1 — core within cluster
//!   bits [23:16]  Aff2 — cluster within socket
//!   bit  [24]     MT   — 1 if Aff0 is "thread" rather than "PE"
//!   bits [29:30]  RES0
//!   bit  [30]     U    — 1 if uniprocessor (no other PEs)
//!   bit  [31]     RES1 — always 1
//!   bits [39:32]  Aff3 — outermost (socket / package)
//!
//! Already used implicitly by `aarch64-hello`: the PSCI CPU_ON
//! target argument is "Aff0..3 packed", and the ICC_SGI1R_EL1
//! `TargetList` field lives within `Aff0` of a specific Aff1/2/3
//! cluster. Until this module landed callers were poking the
//! sysreg directly + bit-twiddling on the result.

#![allow(dead_code)]

/// Decoded MPIDR_EL1.
#[derive(Clone, Copy, Debug)]
pub struct Mpidr {
    pub raw: u64,
    /// Innermost affinity level — thread/SMT when `MT == 1`,
    /// PE otherwise.
    pub aff0: u8,
    pub aff1: u8,
    pub aff2: u8,
    pub aff3: u8,
    /// True when Aff0 selects a hardware thread (vs a PE) — set
    /// on big.LITTLE designs with SMT cores.
    pub multi_thread: bool,
    /// True when this is the only PE in the system.
    pub uniprocessor: bool,
}

impl Mpidr {
    /// Build a `target_cpu` argument for PSCI CPU_ON from the
    /// caller's chosen affinity values. Aff0-2 occupy bits
    /// [7:0]/[15:8]/[23:16] and Aff3 jumps to [39:32].
    pub const fn pack_for_psci(aff0: u8, aff1: u8, aff2: u8, aff3: u8) -> u64 {
        (aff0 as u64)
            | ((aff1 as u64) << 8)
            | ((aff2 as u64) << 16)
            | ((aff3 as u64) << 32)
    }

    /// Build an ICC_SGI1R_EL1 value for an SGI targeting the
    /// specific PE matching this MPIDR's Aff1/2/3 cluster.
    /// `intid` is the SGI ID (0..=15).
    ///
    /// Bit layout (ICC_SGI1R_EL1):
    ///   bits [15:0]  TargetList — bit N = PE with Aff0=N
    ///   bits [23:16] Aff1
    ///   bits [27:24] INTID
    ///   bits [39:32] Aff2
    ///   bit  [40]    IRM (1 = all-but-self)
    ///   bits [55:48] Aff3
    pub fn sgi_to_self_aff0(&self, intid: u8) -> u64 {
        let target_list: u64 = 1 << (self.aff0 & 0xF);
        target_list
            | ((self.aff1 as u64) << 16)
            | ((intid as u64 & 0xF) << 24)
            | ((self.aff2 as u64) << 32)
            | ((self.aff3 as u64) << 48)
    }
}

/// Read MPIDR_EL1 and decode.
#[cfg(target_arch = "aarch64")]
pub fn read_mpidr() -> Mpidr {
    let raw: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, MPIDR_EL1",
            out(reg) raw,
            options(nomem, nostack, preserves_flags),
        );
    }
    Mpidr {
        raw,
        aff0:        ( raw         & 0xFF) as u8,
        aff1:        ((raw >> 8)   & 0xFF) as u8,
        aff2:        ((raw >> 16)  & 0xFF) as u8,
        aff3:        ((raw >> 32)  & 0xFF) as u8,
        multi_thread: ((raw >> 24) & 1) != 0,
        uniprocessor: ((raw >> 30) & 1) != 0,
    }
}

// ── Host-build stubs ────────────────────────────────────────

#[cfg(not(target_arch = "aarch64"))]
pub fn read_mpidr() -> Mpidr {
    Mpidr {
        raw: 0, aff0: 0, aff1: 0, aff2: 0, aff3: 0,
        multi_thread: false, uniprocessor: false,
    }
}
