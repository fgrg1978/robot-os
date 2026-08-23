//! Host-side tests for `robot_os_arch_api` — the cross-ISA trait
//! surface every per-ISA impl crate satisfies.
//!
//! The traits themselves are abstract — the value-adds we can pin
//! from the host are:
//!
//! - `PagePerms` named constants represent the layouts the kernel
//!   actually requests, so changing one would silently break MMU
//!   programming on every ISA.
//! - `arch_name(ArchId)` is the string the kernel prints in procfs
//!   and the OTA manifest — a typo here breaks operational tools.
//! - `ArchId::Stub` exists specifically for host tests of code
//!   that depends on the trait shape; we exercise it here so the
//!   variant doesn't get accidentally removed.
//! - A `Stub` impl that satisfies all five traits proves the trait
//!   surface is dyn-compatible and self-consistent (no
//!   unintentionally-required `Self: Sized` or duplicated methods).

#[cfg(test)]
mod tests {
    use robot_os_arch_api::{
        arch_name, ArchId, Boot, Cpu, HartStartError, InterruptState,
        Interrupts, Mmu, MmuError, PagePerms, Vector,
    };

    // ── PagePerms named constants ──────────────────────────────

    #[test]
    fn page_perms_kernel_rw_is_no_exec_no_user() {
        let p = PagePerms::KERNEL_RW;
        assert!(p.read);
        assert!(p.write);
        assert!(!p.exec, "KERNEL_RW must not be executable (W^X)");
        assert!(!p.user, "KERNEL_RW must not be userspace-accessible");
        assert!(p.cache, "KERNEL_RW is normal cacheable memory");
    }

    #[test]
    fn page_perms_kernel_rx_is_no_write() {
        let p = PagePerms::KERNEL_RX;
        assert!(p.read);
        assert!(!p.write, "KERNEL_RX must not be writable (W^X)");
        assert!(p.exec);
        assert!(!p.user);
        assert!(p.cache);
    }

    #[test]
    fn page_perms_user_rw_is_no_exec_with_user() {
        let p = PagePerms::USER_RW;
        assert!(p.read);
        assert!(p.write);
        assert!(!p.exec, "USER_RW must not be executable (W^X)");
        assert!(p.user, "USER_RW must allow userspace access");
        assert!(p.cache);
    }

    #[test]
    fn page_perms_user_rx_is_no_write_with_user() {
        let p = PagePerms::USER_RX;
        assert!(p.read);
        assert!(!p.write, "USER_RX must not be writable (W^X)");
        assert!(p.exec);
        assert!(p.user);
        assert!(p.cache);
    }

    #[test]
    fn page_perms_mmio_is_uncached() {
        let p = PagePerms::MMIO;
        assert!(p.read);
        assert!(p.write);
        assert!(!p.exec, "MMIO must not be executable");
        assert!(!p.user, "MMIO must be kernel-only");
        assert!(!p.cache, "MMIO must be uncached (device memory)");
    }

    #[test]
    fn page_perms_constants_are_distinct() {
        // Quickly catch a copy-paste accident: a constant ending
        // up structurally identical to another.
        let all = [
            PagePerms::KERNEL_RW,
            PagePerms::KERNEL_RX,
            PagePerms::USER_RW,
            PagePerms::USER_RX,
            PagePerms::MMIO,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j],
                    "PagePerms constants {} and {} are equal", i, j);
            }
        }
    }

    // ── ArchId + arch_name ─────────────────────────────────────

    #[test]
    fn arch_name_matches_uname_style_strings() {
        // These strings show up in procfs / OTA manifests / logs.
        // Changing them is operational-tool-breaking.
        assert_eq!(arch_name(ArchId::Riscv64), "riscv64");
        assert_eq!(arch_name(ArchId::Aarch64), "aarch64");
        assert_eq!(arch_name(ArchId::X86_64),  "x86_64");
        assert_eq!(arch_name(ArchId::Stub),    "stub");
    }

    #[test]
    fn arch_id_variants_are_distinct() {
        // PartialEq derive means we can compare; this proves the
        // enum hasn't degenerated to a single variant.
        let all = [
            ArchId::Riscv64, ArchId::Aarch64, ArchId::X86_64, ArchId::Stub,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j]);
            }
        }
    }

    // ── Error enums round-trip via derive ──────────────────────

    #[test]
    fn mmu_error_variants_distinct() {
        assert_ne!(MmuError::NotAligned, MmuError::UnrepresentablePerms);
        assert_ne!(MmuError::NotAligned, MmuError::BadPhys);
        assert_ne!(MmuError::UnrepresentablePerms, MmuError::BadPhys);
    }

    #[test]
    fn hart_start_error_other_inner_round_trip() {
        let e = HartStartError::Other(-7);
        assert_eq!(e, HartStartError::Other(-7));
        assert_ne!(e, HartStartError::Other(-8));
        assert_ne!(e, HartStartError::AlreadyOn);
    }

    // ── InterruptState transparency ────────────────────────────

    #[test]
    fn interrupt_state_is_a_thin_u64_wrapper() {
        // The doc says callers treat it as opaque; we pin that
        // it's a single u64 newtype so per-ISA impls don't grow
        // it into something heavier.
        let s = InterruptState(0xDEAD_BEEF);
        assert_eq!(s.0, 0xDEAD_BEEF);
        assert_eq!(core::mem::size_of::<InterruptState>(), 8);
    }

    // ── Stub impl: trait shape is satisfiable ──────────────────

    /// Stub satisfying all five traits.  If any of them grew a
    /// method we forgot to add here, this test would fail to compile
    /// — which is the point.
    struct Stub;

    impl Cpu for Stub {
        fn hart_id(&self) -> usize { 0 }
        fn wfi(&self) {}
        fn halt(&self) -> ! { loop {} }
    }

    impl Interrupts for Stub {
        fn disable_all(&self) -> InterruptState { InterruptState(0) }
        fn restore(&self, _prev: InterruptState) {}
        fn set_timer_deadline(&self, _deadline_ticks: u64) {}
        fn send_ipi(&self, _target_hart: usize) {}
    }

    impl Mmu for Stub {
        const PAGE_SIZE: usize = 4096;
        fn encode_pte(&self, phys: usize, _perms: PagePerms) -> Result<u64, MmuError> {
            if phys % Self::PAGE_SIZE != 0 { return Err(MmuError::NotAligned); }
            Ok(phys as u64)
        }
        fn switch_pt(&self, _root_phys: usize, _asid: u16) {}
        fn flush_tlb_all(&self) {}
        fn flush_tlb_asid(&self, _asid: u16) {}
    }

    impl Boot for Stub {
        fn shutdown(&self) -> ! { loop {} }
        fn reboot(&self) -> ! { loop {} }
        fn hart_start(&self, _hart_id: usize, _start_pc: usize, _opaque: usize)
            -> Result<(), HartStartError>
        {
            Ok(())
        }
    }

    impl Vector for Stub {
        fn dot_f32(&self, a: &[f32], b: &[f32]) -> f32 {
            a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
        }
        fn is_accelerated(&self) -> bool { false }
    }

    #[test]
    fn stub_cpu_hart_id() {
        let s = Stub;
        assert_eq!(s.hart_id(), 0);
        s.wfi(); // no-op; just confirm it's callable
    }

    #[test]
    fn stub_interrupts_disable_restore_round_trip() {
        let s = Stub;
        let prev = s.disable_all();
        // We can't observe state, just that the call sequence is
        // accepted and returns the documented opaque token type.
        s.restore(prev);
    }

    #[test]
    fn stub_mmu_encode_pte_rejects_unaligned() {
        let s = Stub;
        let err = s.encode_pte(0x1001, PagePerms::KERNEL_RW).unwrap_err();
        assert_eq!(err, MmuError::NotAligned);
    }

    #[test]
    fn stub_mmu_encode_pte_accepts_aligned() {
        let s = Stub;
        let pte = s.encode_pte(0x2000, PagePerms::KERNEL_RW).unwrap();
        assert_eq!(pte, 0x2000);
    }

    #[test]
    fn stub_vector_dot_f32_matches_scalar_oracle() {
        let s = Stub;
        let a = [1.0f32, 2.0, 3.0, 4.0];
        let b = [4.0f32, 3.0, 2.0, 1.0];
        // 1·4 + 2·3 + 3·2 + 4·1 = 20
        assert_eq!(s.dot_f32(&a, &b), 20.0);
        assert!(!s.is_accelerated(), "stub is the scalar fallback");
    }

    // ── Trait object dispatch ──────────────────────────────────
    //
    // Pin which traits are dyn-compatible. Cpu/Interrupts/Boot/
    // Vector ARE; Mmu is NOT because of its associated
    // `const PAGE_SIZE: usize` — associated consts make a trait
    // not dyn-compatible (Rust limitation).  Documented as task
    // #218: callers must use generics `<M: Mmu>` for the Mmu
    // surface, not `&dyn Mmu`.

    #[test]
    fn non_mmu_traits_are_dyn_compatible() {
        let s = Stub;
        let _cpu:        &dyn Cpu        = &s;
        let _interrupts: &dyn Interrupts = &s;
        let _boot:       &dyn Boot       = &s;
        let _vector:     &dyn Vector     = &s;
        // `let _mmu: &dyn Mmu = &s;` — intentionally NOT compiled.
        // See task #218; Mmu has an associated const, so dyn-dispatch
        // is forbidden by the language. Use `fn foo<M: Mmu>(m: &M)`
        // at call sites instead.
    }
}
