#![no_std]

//! Fachada de arquitectura: reexporta la implementación de la ISA activa.
//!
//! Hoy solo existe RISC-V 64. Las implementaciones de aarch64 y x86_64
//! estaban escritas (GIC/MMU/PSCI y APIC/ACPI/paginación de 4 niveles) pero
//! **nunca se construyó un kernel para ellas**: no hay ensamblador de
//! arranque, ni linker script, ni entrada en CI. Se aparcaron en
//! `newfeatures/` el 2026-08-20 para retomarlas tras las pruebas de hardware
//! — ver `newfeatures/README.md` y `docs/POST_HARDWARE_DEFERRED.md`.
//!
//! La abstracción `robot_os_arch_api` **no** se aparcó: `arch-riscv64` la
//! implementa en `api_impl.rs`, así que el contrato cross-ISA sigue vivo y
//! con sus 17 tests en el gate. Eso es lo que permitirá que reactivar una
//! ISA sea añadir un crate, no rehacer el diseño.

#[cfg(target_arch = "riscv64")]
pub use robot_os_arch_riscv64::*;
