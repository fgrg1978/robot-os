//! Adapter — implements the cross-ISA [`robot_os_arch_api`]
//! traits in terms of the existing RISC-V modules in this crate.
//!
//! Pure additive: the legacy `cpu::*`, `csr::*`, `mmu::*`, `sbi::*`
//! free-function APIs stay in place for in-crate callers (the
//! kernel still calls `csr::read_satp()` directly today). This
//! adapter is the entry point for cross-ISA code that needs to
//! call arch through the trait surface — and for the upcoming
//! aarch64 / x86_64 ports, which will implement the same traits
//! over their own backends.
//!
//! See B0.2 commit message for the rationale.

use robot_os_arch_api::{
    Boot, Cpu, HartStartError, InterruptState, Interrupts, Mmu, MmuError,
    PagePerms, ArchId, Vector,
};

// ──────────────────────────────────────────────────────────────────────────
// Singleton marker type
// ──────────────────────────────────────────────────────────────────────────

/// ZST marker satisfying every arch-api trait family for RISC-V 64.
/// Held as a static so callers can pass `&RISCV64` wherever a
/// `&dyn Cpu` / `&dyn Mmu` / etc. is expected.
pub struct Riscv64;

/// Singleton instance. Use `&arch_impl::RISCV64` to drive the
/// cross-ISA API from in-tree code while the kernel continues to
/// call the legacy free-function modules directly.
pub static RISCV64: Riscv64 = Riscv64;

/// Architectural identifier surfaced through arch-api.
pub const ARCH_ID: ArchId = ArchId::Riscv64;

// ──────────────────────────────────────────────────────────────────────────
// Cpu
// ──────────────────────────────────────────────────────────────────────────

impl Cpu for Riscv64 {
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

impl Interrupts for Riscv64 {
    fn disable_all(&self) -> InterruptState {
        let prev = crate::csr::read_sstatus();
        crate::csr::write_sstatus(prev & !crate::csr::SSTATUS_SIE);
        InterruptState(prev as u64)
    }

    fn restore(&self, prev: InterruptState) {
        crate::csr::write_sstatus(prev.0 as usize);
    }

    fn set_timer_deadline(&self, deadline_ticks: u64) {
        crate::sbi::set_timer(deadline_ticks);
    }

    fn send_ipi(&self, target_hart: usize) {
        // RISC-V SBI IPI takes a hart-mask + base; for a single
        // target we set bit 0 of the mask at base = target_hart.
        let _ = crate::sbi::send_ipi(1, target_hart);
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Mmu
// ──────────────────────────────────────────────────────────────────────────

impl Mmu for Riscv64 {
    const PAGE_SIZE: usize = crate::mmu::PAGE_SIZE;

    fn encode_pte(&self, phys: usize, perms: PagePerms) -> Result<u64, MmuError> {
        if phys & (Self::PAGE_SIZE - 1) != 0 {
            return Err(MmuError::NotAligned);
        }
        let mut flags = crate::mmu::PteFlags::VALID;
        if perms.read {
            flags |= crate::mmu::PteFlags::READ;
        }
        if perms.write {
            flags |= crate::mmu::PteFlags::WRITE;
        }
        if perms.exec {
            flags |= crate::mmu::PteFlags::EXEC;
        }
        if perms.user {
            flags |= crate::mmu::PteFlags::USER;
        }
        // Pre-set A+D for kernel mappings to avoid software-managed
        // A/D faults on RISC-V implementations with ADUE=0 — matches
        // the convention in `mmu::PteFlags::KERNEL_RW` etc.
        if !perms.user {
            flags |= crate::mmu::PteFlags::ACCESSED;
            if perms.write {
                flags |= crate::mmu::PteFlags::DIRTY;
            }
        }
        // RISC-V allows X-only mappings, so PagePerms with
        // `exec && !read` is representable — no UnrepresentablePerms
        // case to reject here. `cache` is ignored: Sv39 has no
        // cacheable-bit in the PTE; cacheability is governed by
        // PMAs (Physical Memory Attributes) set up in PMP / DTB.
        let _ = perms.cache;
        Ok(crate::mmu::Pte::new(phys, flags).0)
    }

    fn switch_pt(&self, root_phys: usize, asid: u16) {
        let satp = crate::mmu::make_satp(root_phys, asid);
        crate::csr::write_satp(satp);
        crate::csr::sfence_vma();
    }

    fn flush_tlb_all(&self) {
        crate::csr::sfence_vma();
    }

    fn flush_tlb_asid(&self, _asid: u16) {
        // RISC-V SFENCE.VMA with rs2 != 0 selects by ASID, but the
        // existing `csr::sfence_vma()` flushes all. A per-ASID
        // variant is a follow-up; flushing all preserves
        // correctness at the cost of TLB pressure.
        crate::csr::sfence_vma();
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Boot
// ──────────────────────────────────────────────────────────────────────────

impl Boot for Riscv64 {
    fn shutdown(&self) -> ! {
        crate::sbi::shutdown()
    }

    fn reboot(&self) -> ! {
        crate::sbi::reboot()
    }

    fn hart_start(
        &self,
        hart_id: usize,
        start_pc: usize,
        opaque: usize,
    ) -> Result<(), HartStartError> {
        // SBI HSM hart_start returns 0 on success; negative on
        // error. RISC-V SBI doesn't surface a clean
        // AlreadyOn/InvalidHartId split, so we collapse all errors
        // into `Other(rc as i32)` so the caller can log the raw
        // SBI return code.
        let rc = crate::sbi::hart_start(hart_id, start_pc, opaque);
        if rc == 0 {
            Ok(())
        } else {
            Err(HartStartError::Other(rc as i32))
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Vector (B0.3)
// ──────────────────────────────────────────────────────────────────────────
//
// The Vector trait is a **build-time** dispatch on RISC-V: the
// `rvv` Cargo feature is set per platform (k1 enables it via the
// SpacemiT K1's RVV 1.0; QEMU rv64,v=true enables it via `rvv`).
// When the feature is off (QEMU default / VF2), we delegate to
// the scalar fallback that already exists in `crate::rvv`.
//
// A *runtime* probe (read `misa` CSR, check bit 'V') would let
// one binary run on both V-capable and V-less harts; that's a
// follow-up. The build-time form is correct today because every
// `--features qemu/vf2/k1/no-ml/no-mmu` config is for a single
// known target hart family.

impl Vector for Riscv64 {
    fn dot_f32(&self, a: &[f32], b: &[f32]) -> f32 {
        #[cfg(feature = "rvv")]
        {
            crate::rvv::dot_f32_rvv(a, b)
        }
        #[cfg(not(feature = "rvv"))]
        {
            crate::rvv::dot_f32_scalar(a, b)
        }
    }

    fn is_accelerated(&self) -> bool {
        cfg!(feature = "rvv")
    }
}