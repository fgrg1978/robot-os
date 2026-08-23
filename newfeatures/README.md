# newfeatures — trabajo aparcado, no muerto

Lo que vive aquí **está fuera del workspace**: no se construye, no se prueba y
no entra en `tools/ci_check.sh`. Se aparcó a propósito, con la intención de
retomarlo, y cada carpeta lleva un `REVISAR.md` con el estado real y lo que
falta.

La razón de que exista esta carpeta en vez de borrar el código: se aprendió por
las malas. `esp32c3` se quedó en el árbol meses sin estar en CI, y cuando por
fin se miró resultó que **ni compilaba** (`AtomicU64` en RV32) y que sus stubs
hacían que `fork`/`exec`/`mmap` devolvieran −1 en silencio. Código que nadie
construye no se conserva: se pudre. Fuera del árbol al menos no miente sobre su
estado.

| Carpeta | Qué es | Aparcado |
|---|---|---|
| `esp32c3/` | Soporte RV32 para ESP32-C3 | 2026-08-18 |
| `arch-aarch64/`, `aarch64-hello/` | ISA ARM64: GIC, MMU VMSAv8-64, PSCI, timer | 2026-08-20 |
| `arch-x86_64/`, `x86_64-hello/`, `arch-x86_64-tests/` | ISA x86-64: ACPI, APIC, GDT/IDT, TSS, xsave, PVH | 2026-08-20 |

## Sobre las dos arquitecturas (2026-08-20)

No se aparcaron por estar mal. Son **5.322 líneas de periféricos escritos y
razonables** — y `arch-x86_64-tests` traía 13 tests que pasaban. Se aparcaron
porque **nunca se construyó un kernel con ellas**: no hay ensamblador de
arranque, ni linker script del kernel, ni entrada en CI. Los únicos artefactos
de arranque que existían eran los `.ld` de los dos "hola mundo".

**La abstracción NO se aparcó.** `crates/arch-api` sigue en el árbol y sigue
con sus 17 tests en el gate, porque `arch-riscv64` la implementa de verdad en
`api_impl.rs`. Esa es la parte que importa conservar viva: mientras el contrato
cross-ISA se siga ejercitando contra una arquitectura real, reactivar otra es
añadir un crate. Si se hubiera aparcado también la abstracción, el contrato
dejaría de comprobarse contra nada y volvería a divergir en silencio.

Ver `docs/POST_HARDWARE_DEFERRED.md` para cuándo se retoman y qué hace falta.
