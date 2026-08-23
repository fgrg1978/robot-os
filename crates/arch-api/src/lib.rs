//! Cross-ISA arch trait surface — PHANES Phase 2 prep (B0).
//!
//! This crate defines **what** the kernel needs from any
//! architecture, not **how** any specific architecture provides
//! it. Per RFC-0002 modular pattern, every ISA ships an impl
//! crate (e.g. `robot_os_arch_riscv64`, `robot_os_arch_aarch64`,
//! `robot_os_arch_x86_64`) that satisfies these traits.
//!
//! # Why a new crate now
//!
//! The existing `robot_os_arch` (~1.3 kL) is 100% RISC-V: bare
//! `pub fn read_satp()`, `pub const SSTATUS_SIE`, etc. There is no
//! abstraction layer separating "what the kernel asks" from "what
//! RISC-V provides". Phase 2 adds `aarch64` and `x86_64` ports;
//! without this trait surface every consumer would have to grow
//! `cfg(target_arch = "...")` branches.
//!
//! # Scope of THIS commit (B0)
//!
//! - Trait + value-type **API only**. Zero implementations.
//! - Existing `robot_os_arch` is **untouched**. Migration of
//!   RISC-V impls to satisfy this trait surface is a follow-up
//!   (B0.2) so this commit cannot break the 5 existing build
//!   configs.
//! - `aarch64` and `x86_64` skeletons (B1, B2) implement these
//!   traits from day one.
//!
//! # The five trait families
//!
//! Worked through by grepping every `robot_os_arch::*` reference
//! across `crates/` and `kernel/`:
//!
//! | Family       | Captures                                           |
//! |--------------|----------------------------------------------------|
//! | [`Cpu`]      | hart id, wfi, halt, number of harts                |
//! | [`Interrupts`] | enable/disable all, timer deadline, IPI send      |
//! | [`Mmu`]      | PAGE_SIZE, map/unmap, switch_pt, TLB shootdown     |
//! | [`Boot`]     | platform shutdown / reboot / hart start            |
//! | [`Vector`]   | optional SIMD kernels (RVV / SVE / AVX2 / fallback)|
//!
//! ISA-specific surfaces (CSR bit layout, PMP regions, SBI HSM
//! call numbers) stay private to each impl crate; they don't
//! belong in a cross-ISA API.

#![no_std]

// ──────────────────────────────────────────────────────────────────────────
// Cpu
// ──────────────────────────────────────────────────────────────────────────

/// Per-hart / per-core / per-thread identity + low-level halt.
///
/// On RISC-V `hart_id()` reads `mhartid` (via early-boot stash);
/// on aarch64 it reads `MPIDR_EL1`; on x86 it reads the APIC ID.
/// The kernel uses `hart_id()` to index per-CPU statics — the
/// only requirement is that distinct cores return distinct values
/// in `0..max_active_harts`.
///
/// **Note on `num_harts`**: deliberately NOT in this trait. The
/// number of available harts is a *platform discovery* fact (FDT
/// on RISC-V, ACPI MADT on x86, device tree on aarch64) — not
/// an ISA fact. The kernel reads it once at boot from the
/// appropriate platform interface and stores it; arch impls
/// should not duplicate that logic.
pub trait Cpu: Send + Sync {
    /// Identifier of the calling hart / core. Stable for the
    /// lifetime of the kernel.
    fn hart_id(&self) -> usize;

    /// Wait For Interrupt — low-power idle. Returns when any
    /// pending interrupt is delivered to this hart.
    fn wfi(&self);

    /// Park this hart forever (used in panic-handler paths). Does
    /// not return.
    fn halt(&self) -> !;
}

// ──────────────────────────────────────────────────────────────────────────
// Interrupts
// ──────────────────────────────────────────────────────────────────────────

/// Cross-ISA interrupt control + per-hart timer scheduling.
///
/// Only the *kernel's* notion of interrupt enable is exposed — the
/// individual interrupt-controller programming (PLIC vs GIC vs
/// I/O-APIC) lives in driver code, not here.
///
/// **Note on `now_ticks`**: NOT in this trait. Reading the
/// monotonic counter is the timer-driver's job (CLINT on RISC-V,
/// generic timer on aarch64, HPET/TSC on x86) — `arch` just
/// programs the next deadline via the SBI / PSCI / APIC interface,
/// it doesn't own the time source.
pub trait Interrupts: Send + Sync {
    /// Disable all maskable interrupts on the calling hart;
    /// returns the previous enable state (caller restores via
    /// [`Self::restore`]).
    fn disable_all(&self) -> InterruptState;

    /// Restore the interrupt-enable state previously returned by
    /// [`Self::disable_all`].
    fn restore(&self, prev: InterruptState);

    /// Program the next per-hart timer interrupt to fire at
    /// absolute `deadline_ticks`. Tick units are platform-defined
    /// (mtime on RISC-V, generic timer on aarch64, APIC timer on
    /// x86) — the kernel only needs monotonicity.
    fn set_timer_deadline(&self, deadline_ticks: u64);

    /// Send an Inter-Processor Interrupt to the target hart.
    fn send_ipi(&self, target_hart: usize);
}

/// Opaque previous-state token returned by
/// [`Interrupts::disable_all`]. ISA-specific layouts go in the
/// impl crate; consumers treat it as a transparent token.
#[derive(Clone, Copy, Debug)]
pub struct InterruptState(pub u64);

// ──────────────────────────────────────────────────────────────────────────
// Mmu
// ──────────────────────────────────────────────────────────────────────────

/// Cross-ISA virtual-memory operations.
///
/// `PAGE_SIZE` differs per ISA *in principle* (4 KiB on most
/// architectures we target, but ARM allows 16 KiB / 64 KiB
/// variants). Each impl exports its own const.
///
/// **Note on `map_page` / `unmap_page`**: NOT in this trait. The
/// page-table walk + page allocator live in `crates/mm`; arch
/// only owns the architectural *encoding* of a PTE word (via
/// [`Self::encode_pte`]). This keeps `arch → mm` from creating a
/// dependency cycle and makes the PTE format the single
/// audit-worthy surface per ISA.
pub trait Mmu: Send + Sync {
    /// Page size in bytes.
    const PAGE_SIZE: usize;

    /// Encode a leaf PTE word for `phys` with `perms`. The result
    /// is the raw `u64` (or `usize`-sized) value mm writes into
    /// the architectural page table. Returns `Err` if `phys` is
    /// not page-aligned or perms are nonsensical for this ISA.
    fn encode_pte(&self, phys: usize, perms: PagePerms) -> Result<u64, MmuError>;

    /// Switch the current address space to `root_phys` (page
    /// table root physical address). The ASID slot is provided
    /// separately so the impl can re-use it for TLB tagging.
    fn switch_pt(&self, root_phys: usize, asid: u16);

    /// Invalidate the entire TLB. Used on PT teardown.
    fn flush_tlb_all(&self);

    /// Invalidate TLB entries tagged with `asid`. Used on
    /// per-task address-space tear-down to avoid blowing away
    /// other tasks' translations.
    fn flush_tlb_asid(&self, asid: u16);
}

/// Page permissions in the cross-ISA model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagePerms {
    pub read:  bool,
    pub write: bool,
    pub exec:  bool,
    /// Userspace may access. False = kernel-only.
    pub user:  bool,
    /// May be cached. False = device / strongly-ordered.
    pub cache: bool,
}

impl PagePerms {
    /// Kernel R/W data; no exec.
    pub const KERNEL_RW: Self = Self {
        read: true, write: true, exec: false, user: false, cache: true,
    };
    /// Kernel code; R+X.
    pub const KERNEL_RX: Self = Self {
        read: true, write: false, exec: true, user: false, cache: true,
    };
    /// User R/W data; no exec.
    pub const USER_RW: Self = Self {
        read: true, write: true, exec: false, user: true, cache: true,
    };
    /// User code; R+X.
    pub const USER_RX: Self = Self {
        read: true, write: false, exec: true, user: true, cache: true,
    };
    /// MMIO region; uncached.
    pub const MMIO: Self = Self {
        read: true, write: true, exec: false, user: false, cache: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmuError {
    /// `phys` was not page-aligned to [`Mmu::PAGE_SIZE`].
    NotAligned,
    /// Requested perms are not representable on this ISA (e.g.
    /// `exec + !read` which RISC-V allows but some ISAs don't).
    UnrepresentablePerms,
    /// `phys` is outside the platform's physical address range.
    BadPhys,
}

// ──────────────────────────────────────────────────────────────────────────
// Boot
// ──────────────────────────────────────────────────────────────────────────

/// Platform-level lifecycle: shut down the machine, reboot, and
/// bring secondary harts up.
pub trait Boot: Send + Sync {
    /// Stop the whole machine. Does not return.
    fn shutdown(&self) -> !;

    /// Reboot the whole machine. Does not return.
    fn reboot(&self) -> !;

    /// Start `hart_id` executing at `start_pc` with `opaque`
    /// passed in the first argument register. On RISC-V this maps
    /// to SBI HSM `hart_start`; on aarch64 to PSCI CPU_ON; on x86
    /// to APIC INIT/SIPI sequence.
    fn hart_start(
        &self,
        hart_id: usize,
        start_pc: usize,
        opaque: usize,
    ) -> Result<(), HartStartError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HartStartError {
    AlreadyOn,
    InvalidHartId,
    Denied,
    Other(i32),
}

// ──────────────────────────────────────────────────────────────────────────
// Vector (optional)
// ──────────────────────────────────────────────────────────────────────────

/// Optional SIMD kernels. ISA impls expose a fast path via
/// RVV / SVE / NEON / AVX2 / etc., plus a scalar fallback. The
/// kernel selects between them at boot via a feature flag or
/// CPUID-style probe.
///
/// Only the operations the kernel actually uses appear here — no
/// general-purpose matrix library. Today: the ML inner loops.
pub trait Vector: Send + Sync {
    /// Dot product of two equal-length `f32` slices. Returns the
    /// scalar fallback result if no SIMD is available.
    fn dot_f32(&self, a: &[f32], b: &[f32]) -> f32;

    /// `true` if the impl is actually using SIMD. Diagnostic — the
    /// kernel never branches on this.
    fn is_accelerated(&self) -> bool;
}

// ──────────────────────────────────────────────────────────────────────────
// Where the impls will live (Phase 2 follow-ups)
// ──────────────────────────────────────────────────────────────────────────

/// Compile-time identifier of the active ISA impl, surfaced for
/// `procfs` and diagnostic prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchId {
    Riscv64,
    Aarch64,
    X86_64,
    /// Test stub (host) — used by host-side unit tests of
    /// kernel logic that depend on the arch trait shape.
    Stub,
}

/// Returns a stable short name for the [`ArchId`] (for logging,
/// procfs, manifest fields).
pub const fn arch_name(id: ArchId) -> &'static str {
    match id {
        ArchId::Riscv64 => "riscv64",
        ArchId::Aarch64 => "aarch64",
        ArchId::X86_64 => "x86_64",
        ArchId::Stub => "stub",
    }
}
