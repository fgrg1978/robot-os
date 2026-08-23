//! `arch-x86_64` — PHANES Phase 2 x86_64 (AMD64 / Intel 64) ISA
//! impl of the `robot_os_arch_api` trait surface.
//!
//! Mirrors the shape of `arch-aarch64`: workspace member, asm
//! bodies cfg-gated to `target_arch = "x86_64"`, struct +
//! singleton + trait impls live in `api_impl`. See that crate
//! for the design rationale; this is the same pattern for the
//! third ISA.
//!
//! # Scope of B2 (this commit)
//!
//! - [`Cpu`] via RDTSCP/CPUID for ID and `hlt` for idle.
//! - [`Interrupts`] via `cli` / `sti` / `pushfq;popfq` for the
//!   RFLAGS.IF bit; APIC timer + IPI deferred to B2.boot once
//!   the APIC driver lands.
//! - [`Mmu`] via 4-level paging (PML4 → PDP → PD → PT) PTE
//!   encoding; CR3 write for `switch_pt`; INVLPG / full TLB
//!   flush.
//! - [`Boot`] via ACPI reset register write for `reboot` (B2.acpi
//!   commit); `shutdown` via QEMU ACPI poweroff port 0x604.
//!   `hart_start` deferred to APIC INIT/SIPI in B2.boot.
//!
//! # Out of scope (follow-ups)
//!
//! - B2.boot: multiboot/UEFI entry, 32→64 mode switch,
//!   linker.ld for `qemu-system-x86_64 -M q35 -bios OVMF.fd`.
//! - B2.apic: local APIC + I/O APIC driver, x2APIC enable.
//! - B2.acpi: ACPI MADT parser for hart enumeration, FADT reset.
//! - B2.vec: AVX/AVX2 `impl Vector` with runtime CPUID probe.

#![no_std]
#![allow(dead_code)]

pub use robot_os_arch_api::{
    Boot, Cpu, HartStartError, InterruptState, Interrupts, Mmu, MmuError,
    PagePerms, Vector,
};

pub mod acpi;
pub mod apic;
pub mod api_impl;
pub mod barriers;
pub mod cpu;
pub mod cpuid;
pub mod gdt;
pub mod idt;
pub mod ioapic;
pub mod mmu;
pub mod msr;
pub mod pic;
pub mod pvh;
pub mod syscall;
pub mod sysregs;
pub mod tsc;
pub mod tss;
pub mod vector;
pub mod xsave;

/// Architectural identifier surfaced through arch-api.
pub const ARCH_ID: robot_os_arch_api::ArchId =
    robot_os_arch_api::ArchId::X86_64;
