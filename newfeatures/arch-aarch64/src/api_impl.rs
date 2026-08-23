//! arch-api trait impls for ARMv8-A.
//!
//! See `crates/arch/src/api_impl.rs` for the RISC-V reference
//! version. Trait shapes match exactly; the only thing that
//! changes is the backing instructions.

// The arch-api + mmu items are only referenced inside the
// `#[cfg(target_arch = "aarch64")]` impl blocks below. Gate the
// imports too so the workspace's riscv64 target doesn't generate
// unused-import warnings.
#[cfg(target_arch = "aarch64")]
use robot_os_arch_api::{
    Boot, Cpu, HartStartError, InterruptState, Interrupts, Mmu, MmuError,
    PagePerms, Vector,
};

#[cfg(target_arch = "aarch64")]
use crate::mmu::{make_pte, PteAttrs, PAGE_SIZE};

/// ZST marker satisfying every arch-api trait family for ARMv8-A.
pub struct Aarch64;

/// Singleton instance — `&AARCH64` plugs into any
/// `&dyn Cpu` / `&dyn Mmu` / etc. slot.
pub static AARCH64: Aarch64 = Aarch64;

// ──────────────────────────────────────────────────────────────────────────
// Cpu
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
impl Cpu for Aarch64 {
    fn hart_id(&self) -> usize {
        crate::cpu::hart_id()
    }

    fn wfi(&self) {
        crate::cpu::wfi();
    }

    fn halt(&self) -> ! {
        loop {
            crate::cpu::wfi();
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Interrupts
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
impl Interrupts for Aarch64 {
    fn disable_all(&self) -> InterruptState {
        InterruptState(crate::sysregs::disable_irq_fiq())
    }

    fn restore(&self, prev: InterruptState) {
        crate::sysregs::write_daif(prev.0);
    }

    fn set_timer_deadline(&self, deadline_ticks: u64) {
        // Generic-timer ticks are CNTFRQ_EL0 Hz. The kernel
        // already converts its scheduler ticks to the platform
        // timebase, so this is a direct write.
        crate::sysregs::write_cntp_cval_el0(deadline_ticks);
        crate::sysregs::enable_phys_timer();
    }

    fn send_ipi(&self, target_hart: usize) {
        // GIC v3 `ICC_SGI1R_EL1` write. Correct field layout:
        //   [15:0]  TargetList (one bit per affinity-0 PE within the cluster)
        //   [23:16] Aff1
        //   [27:24] INTID (SGI 0..15)
        //   [39:32] Aff2   [55:48] Aff3
        // TargetList lives in bits [15:0] and must NOT be shifted — the old
        // `target_list << 32` put it in the Aff2 field, leaving TargetList=0
        // so the SGI targeted no PE and every IPI was silently dropped.
        // We use SGI #0 as the kernel-wide IPI; multi-affinity grouping is a
        // follow-up alongside the GIC driver.
        let aff0 = (target_hart & 0xFF) as u64;
        let aff1 = ((target_hart >> 8) & 0xFF) as u64;
        // Mask aff0 to the 16-bit TargetList range (avoids a shift ≥ 64 panic).
        let target_list: u64 = 1 << (aff0 & 0xF);
        let intid: u64 = 0;
        let sgi1r = target_list | (aff1 << 16) | (intid << 24);
        unsafe {
            core::arch::asm!(
                "msr ICC_SGI1R_EL1, {0}",
                "isb",
                in(reg) sgi1r,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Mmu
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
impl Mmu for Aarch64 {
    const PAGE_SIZE: usize = PAGE_SIZE;

    fn encode_pte(&self, phys: usize, perms: PagePerms) -> Result<u64, MmuError> {
        if phys & (PAGE_SIZE - 1) != 0 {
            return Err(MmuError::NotAligned);
        }

        // Start from "Valid + AF + Inner Shareable + Normal mem"
        // and refine based on `perms`.
        let mut attrs = PteAttrs::TYPE_PAGE
            | PteAttrs::AF
            | PteAttrs::SH_INNER;

        // Memory type: cacheable Normal vs Device-nGnRE.
        if perms.cache {
            attrs |= PteAttrs::ATTRIDX_NORMAL;
        } else {
            // Device memory: SH bits are ignored per VMSAv8 §D8;
            // we leave them but they have no effect.
            attrs |= PteAttrs::ATTRIDX_DEVICE;
        }

        // Access permissions. AP encoding is the only place
        // ARMv8 differs noticeably from RISC-V: there's no
        // separate R/W/X tri-state — the AP field combines R/W
        // with EL0/EL1 reach, and X is encoded separately via
        // PXN/UXN.
        attrs |= match (perms.user, perms.write) {
            (false, false) => PteAttrs::AP_EL1_RO,
            (false, true)  => PteAttrs::AP_EL1_RW,
            (true,  false) => PteAttrs::AP_EL0_RO,
            (true,  true)  => PteAttrs::AP_EL0_RW,
        };

        // Reject perms that aren't representable on ARMv8: we
        // don't currently support !read (ARMv8 has no "X-only"
        // EL0 mapping equivalent to RISC-V's X=1,R=0,W=0).
        if !perms.read {
            return Err(MmuError::UnrepresentablePerms);
        }

        // Execute permission: ARM defaults to executable, so we
        // *set* the XN bits to make a page non-executable. Kernel
        // data pages get PXN; user data pages get UXN; both for
        // a non-exec page accessible from either ring.
        if !perms.exec {
            attrs |= PteAttrs::PXN;
            if perms.user {
                attrs |= PteAttrs::UXN;
            }
        }

        // User pages are nG so they don't pollute the global TLB.
        if perms.user {
            attrs |= PteAttrs::NG;
        }

        Ok(make_pte(phys, attrs))
    }

    fn switch_pt(&self, root_phys: usize, asid: u16) {
        crate::sysregs::write_ttbr0_el1(root_phys, asid);
        // ISB so subsequent fetches see the new translation regime.
        unsafe {
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }

    fn flush_tlb_all(&self) {
        crate::sysregs::tlbi_vmalle1is();
    }

    fn flush_tlb_asid(&self, asid: u16) {
        crate::sysregs::tlbi_aside1is(asid);
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Boot
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
impl Boot for Aarch64 {
    fn shutdown(&self) -> ! {
        crate::psci::system_off()
    }

    fn reboot(&self) -> ! {
        crate::psci::system_reset()
    }

    fn hart_start(
        &self,
        hart_id: usize,
        start_pc: usize,
        opaque: usize,
    ) -> Result<(), HartStartError> {
        // PSCI CPU_ON takes MPIDR affinity directly — the caller
        // passes the logical hart_id and we trust it matches the
        // platform's MPIDR layout (kernel boot code builds the
        // mapping when parsing the device tree).
        let rc = crate::psci::cpu_on(
            hart_id as u64,
            start_pc as u64,
            opaque as u64,
        );
        match rc {
            crate::psci::PSCI_OK => Ok(()),
            crate::psci::PSCI_ALREADY_ON => Err(HartStartError::AlreadyOn),
            crate::psci::PSCI_INVALID_PARAMS
            | crate::psci::PSCI_INVALID_ADDRESS => {
                Err(HartStartError::InvalidHartId)
            }
            crate::psci::PSCI_DENIED | crate::psci::PSCI_DISABLED => {
                Err(HartStartError::Denied)
            }
            other => Err(HartStartError::Other(other)),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Vector (B1.vec)
// ──────────────────────────────────────────────────────────────────────────
//
// NEON is mandatory on ARMv8 — no probe, no fallback path
// selection. `is_accelerated` returns `true` unconditionally
// because we *always* go through NEON intrinsics.

#[cfg(target_arch = "aarch64")]
impl Vector for Aarch64 {
    fn dot_f32(&self, a: &[f32], b: &[f32]) -> f32 {
        // Runtime dispatcher — SVE detection lives in vector.rs;
        // path falls back to NEON unconditionally until we have
        // SVE-capable hardware to validate a real `dot_f32_sve`.
        // Same shape as x86_64's AVX/SSE2 dispatch.
        crate::vector::dot_f32_best(a, b)
    }

    fn is_accelerated(&self) -> bool {
        true
    }
}
