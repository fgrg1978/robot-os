# Revisar antes de reactivar aarch64 / x86_64

Aparcadas el 2026-08-20. Estado al aparcarlas: **compilaban como crates
sueltos; nunca hubo un kernel arrancable**.

## Qué falta, por arquitectura

Lo mismo en ambas, y ninguna de las tres cosas es trivial:

1. **Ensamblador de arranque.** `kernel/src/entry/` solo tiene `riscv64/`
   (`boot.S`, `trap_entry.S`, `context_switch.S`, más las variantes EFI,
   sin-OpenSBI y RVV). Hace falta el equivalente completo, incluido el
   arranque de harts secundarios.
2. **Linker script del kernel.** Existen `kernel/linker{,-vf2,-k1,-fleet}.ld`,
   todos RV64. Los `.ld` que se van con estas carpetas son de los "hola mundo",
   no del kernel.
3. **Entrada en `tools/ci_check.sh`.** **Esto es lo que decide si el trabajo
   sigue vivo dentro de seis meses.** Sin gate, se repite exactamente la
   historia del esp32c3.

## Lo que hay hecho y sirve

- **aarch64** (2.313 líneas): GIC, MMU VMSAv8-64, PSCI, timer, sysregs.
- **x86_64** (3.009 líneas): ACPI (parser MADT con sus 13 tests), APIC, IOAPIC,
  GDT, IDT, TSS, xsave, PVH.
- Ambas implementan `robot_os_arch_api`, que **sigue viva en el árbol** — el
  contrato no ha cambiado bajo ellas mientras estén aquí, porque `arch-riscv64`
  lo implementa y sus 17 tests lo fijan.

## Trampa al reactivar

`crates/arch/src/lib.rs` era una fachada con `cfg(target_arch)` para las tres,
y sus dependencias en `Cargo.toml` eran condicionales por arquitectura. Al
aparcarlas se dejó solo la rama riscv64. **Restaurar las dos ramas y las dos
dependencias condicionales es el primer paso**, y es fácil de olvidar porque
todo sigue compilando sin ellas.

Comprobar además que `arch-api` no ha divergido: si `api_impl.rs` de riscv64
ganó métodos nuevos, estas dos implementaciones estarán incompletas y el error
saldrá como un fallo de trait, no como algo evidente.
