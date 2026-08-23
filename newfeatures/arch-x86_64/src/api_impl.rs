//! arch-api trait impls for x86_64 (AMD64 / Intel 64).
//!
//! Same pattern as the RISC-V and aarch64 adapters: ZST marker +
//! singleton + cfg-gated impl blocks. APIC programming + ACPI
//! parsing + APIC INIT/SIPI hart bring-up are deferred to B2.boot
//! / B2.apic / B2.acpi (the equivalents of B1.boot for aarch64).

#[cfg(target_arch = "x86_64")]
use robot_os_arch_api::{
    Boot, Cpu, HartStartError, InterruptState, Interrupts, Mmu, MmuError,
    PagePerms, Vector,
};

#[cfg(target_arch = "x86_64")]
use crate::mmu::{make_pte, PteFlags, PAGE_SIZE};

/// ZST marker satisfying every arch-api trait family for x86_64.
pub struct X86_64;

/// Singleton instance.
pub static X86_64_IMPL: X86_64 = X86_64;

// ──────────────────────────────────────────────────────────────────────────
// Cpu
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
impl Cpu for X86_64 {
    fn hart_id(&self) -> usize {
        crate::cpu::hart_id()
    }

    fn wfi(&self) {
        crate::cpu::hlt();
    }

    fn halt(&self) -> ! {
        // CLI + HLT guarantees we never wake. CLI alone wouldn't
        // help — HLT alone with IF=1 would resume on NMI/IRQ.
        crate::sysregs::cli();
        loop {
            crate::cpu::hlt();
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Interrupts
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
impl Interrupts for X86_64 {
    fn disable_all(&self) -> InterruptState {
        let prev = crate::sysregs::read_rflags();
        crate::sysregs::cli();
        InterruptState(prev)
    }

    fn restore(&self, prev: InterruptState) {
        // If IF was set in the prior RFLAGS, re-enable interrupts.
        // We don't touch other RFLAGS bits — restoring the full
        // RFLAGS would clobber arithmetic flags.
        if prev.0 & crate::sysregs::RFLAGS_IF != 0 {
            crate::sysregs::sti();
        }
    }

    fn set_timer_deadline(&self, deadline_ticks: u64) {
        // B2.apic: writes LVT_TIMER + Initial Count via the local
        // APIC at the address `init_local_apic` configured (default
        // 0xFEE00000). If the APIC hasn't been initialised yet the
        // write goes into an unmapped MMIO page — the kernel boot
        // path must call `crate::apic::init_local_apic` before
        // touching the scheduler.
        crate::apic::set_timer_deadline(deadline_ticks);
    }

    fn send_ipi(&self, target_hart: usize) {
        // `target_hart` is the destination APIC ID. xAPIC uses
        // the low 8 bits; x2APIC uses all 32. The apic module
        // picks the right path based on its enabled mode.
        crate::apic::send_ipi(target_hart as u32);
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Mmu
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
impl Mmu for X86_64 {
    const PAGE_SIZE: usize = PAGE_SIZE;

    fn encode_pte(&self, phys: usize, perms: PagePerms) -> Result<u64, MmuError> {
        if phys & (PAGE_SIZE - 1) != 0 {
            return Err(MmuError::NotAligned);
        }
        let mut flags = PteFlags::PRESENT;
        if perms.write {
            flags |= PteFlags::RW;
        }
        if perms.user {
            flags |= PteFlags::USER;
        }
        if !perms.cache {
            flags |= PteFlags::CACHE_DISABLE | PteFlags::WRITE_THROUGH;
        }
        // x86 page tables don't have a "readable" bit — Present
        // implies readable. `perms.read = false` is meaningless
        // (Present already covers it) so we silently accept any
        // value. There IS no execute-by-default — we have to set
        // NX explicitly for non-exec pages.
        if !perms.exec {
            flags |= PteFlags::NX;
        }
        // Kernel mappings get GLOBAL so they survive CR3 reloads.
        if !perms.user {
            flags |= PteFlags::GLOBAL | PteFlags::ACCESSED;
            if perms.write {
                flags |= PteFlags::DIRTY;
            }
        }
        Ok(make_pte(phys, flags))
    }

    fn switch_pt(&self, root_phys: usize, _asid: u16) {
        // x86 has no in-PT ASID; PCID is an optional CR4 feature.
        // For Phase 1 we just reload CR3 and accept the full TLB
        // flush. PCID + `INVPCID` are a B2.pcid follow-up.
        crate::sysregs::write_cr3(root_phys);
    }

    fn flush_tlb_all(&self) {
        crate::sysregs::flush_tlb_full();
    }

    fn flush_tlb_asid(&self, _asid: u16) {
        // No PCID yet — full flush.
        crate::sysregs::flush_tlb_full();
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Boot
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
impl Boot for X86_64 {
    fn shutdown(&self) -> ! {
        // QEMU q35 ACPI PM1a_CNT_BLK lives at port 0x604; writing
        // SLP_TYP=0 | SLP_EN sleeps to S5 (soft-off). For real
        // hardware we need to read the FADT for the real port +
        // value; that's B2.acpi.
        unsafe {
            core::arch::asm!(
                "mov dx, 0x604",
                "mov ax, 0x2000",
                "out dx, ax",
                out("dx") _,
                out("ax") _,
                options(nostack, preserves_flags),
            );
        }
        // If we somehow survive, park forever.
        loop {
            crate::sysregs::cli();
            crate::cpu::hlt();
        }
    }

    fn reboot(&self) -> ! {
        // Pulse the 8042 keyboard-controller reset line (port
        // 0x64, command 0xFE) — the universal fallback that
        // works on every PC since IBM AT.
        unsafe {
            core::arch::asm!(
                "mov al, 0xFE",
                "out 0x64, al",
                out("al") _,
                options(nostack, preserves_flags),
            );
        }
        loop {
            crate::sysregs::cli();
            crate::cpu::hlt();
        }
    }

    fn hart_start(
        &self,
        _hart_id: usize,
        _start_pc: usize,
        _opaque: usize,
    ) -> Result<(), HartStartError> {
        // x86_64 secondary-CPU bring-up uses APIC INIT/SIPI/SIPI
        // with a 16-bit real-mode trampoline. That whole dance —
        // and the trampoline page allocation — belongs in
        // B2.boot once we can actually boot. Until then return
        // a clear error.
        Err(HartStartError::Other(-1))
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Vector — SSE2 mandatory baseline (System V AMD64 ABI)
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
impl Vector for X86_64 {
    fn dot_f32(&self, a: &[f32], b: &[f32]) -> f32 {
        // Runtime dispatch: AVX (8-lane) when CPUID reports it
        // (and OSXSAVE is set), SSE2 (4-lane) otherwise. SSE2 is
        // x86_64 baseline so this is always safe — the AVX path
        // just isn't always faster on every µarch.
        crate::vector::dot_f32_best(a, b)
    }

    fn is_accelerated(&self) -> bool {
        true
    }
}
